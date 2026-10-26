use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex as StdMutex;

use async_trait::async_trait;
use base64::Engine as _;

use super::*;
use crate::backend::*;

#[derive(Default)]
struct FakeMail {
    boxes: StdMutex<HashMap<String, String>>,
    aliases: HashSet<String>,
    deleted: StdMutex<Vec<(String, bool)>>,
    waited: StdMutex<Vec<String>>,
}

#[async_trait]
impl MailBackend for FakeMail {
    async fn address_in_use(&self, a: &str) -> anyhow::Result<bool> {
        Ok(self.aliases.contains(a) || self.boxes.lock().unwrap().contains_key(a))
    }
    async fn verify(&self, a: &str, p: &str) -> anyhow::Result<Option<bool>> {
        Ok(self.boxes.lock().unwrap().get(a).map(|x| x == p))
    }
    async fn set_password(&self, a: &str, p: &str) -> anyhow::Result<Change> {
        let prev = self.boxes.lock().unwrap().insert(a.into(), p.into());
        Ok(if prev.is_some() {
            Change::Updated
        } else {
            Change::Created
        })
    }
    async fn wait_until_login(&self, a: &str, p: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.boxes.lock().unwrap().get(a).is_some_and(|x| x == p),
            "not live"
        );
        self.waited.lock().unwrap().push(a.into());
        Ok(())
    }
    async fn delete(&self, a: &str, purge: bool) -> anyhow::Result<Change> {
        self.deleted.lock().unwrap().push((a.into(), purge));
        Ok(match self.boxes.lock().unwrap().remove(a) {
            Some(_) => Change::Removed,
            None => Change::Unchanged,
        })
    }
}

#[derive(Default)]
struct CloudState {
    users: BTreeMap<String, (CloudUser, String)>,
    foreign: HashSet<String>,
    tokens: HashMap<String, String>,
    accounts: HashMap<String, Vec<(MailAccount, MailAccountSpec)>>,
    next_id: i64,
    fail_mail_calls: u32,
}

#[derive(Default)]
struct FakeCloud(StdMutex<CloudState>);

impl FakeCloud {
    fn authed(s: &CloudState, id: &str, token: &str) -> anyhow::Result<()> {
        let ok = s.tokens.get(token).is_some_and(|u| u == id)
            && s.users.get(id).is_some_and(|u| u.0.enabled);
        if ok { Ok(()) } else { anyhow::bail!("401") }
    }
}

#[async_trait]
impl CloudBackend for FakeCloud {
    async fn get_user(&self, id: &str) -> anyhow::Result<Option<CloudUser>> {
        Ok(self.0.lock().unwrap().users.get(id).map(|u| u.0.clone()))
    }
    async fn create_user(&self, spec: &UserSpec, pw: &str) -> anyhow::Result<CreateUser> {
        let mut s = self.0.lock().unwrap();
        if s.foreign.contains(&spec.id) || s.users.contains_key(&spec.id) {
            return Ok(CreateUser::AlreadyExists);
        }
        let user = CloudUser {
            id: spec.id.clone(),
            enabled: true,
            email: Some(spec.email.clone()),
            display_name: spec.display_name.clone(),
            groups: vec!["digital workforce".into()],
            quota: Some(5),
        };
        s.users.insert(spec.id.clone(), (user, pw.into()));
        Ok(CreateUser::Created)
    }
    async fn reconcile_user(&self, cur: &CloudUser, spec: &UserSpec) -> anyhow::Result<Change> {
        let mut s = self.0.lock().unwrap();
        let u = &mut s.users.get_mut(&cur.id).unwrap().0;
        if u.display_name == spec.display_name && u.email.as_deref() == Some(&spec.email) {
            return Ok(Change::Unchanged);
        }
        u.display_name = spec.display_name.clone();
        u.email = Some(spec.email.clone());
        Ok(Change::Updated)
    }
    async fn set_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<()> {
        self.0.lock().unwrap().users.get_mut(id).unwrap().0.enabled = enabled;
        Ok(())
    }
    async fn set_password(&self, id: &str, pw: &str) -> anyhow::Result<()> {
        self.0.lock().unwrap().users.get_mut(id).unwrap().1 = pw.into();
        Ok(())
    }
    async fn delete_user(&self, id: &str) -> anyhow::Result<Change> {
        let mut s = self.0.lock().unwrap();
        s.tokens.retain(|_, u| u != id);
        s.accounts.remove(id);
        Ok(if s.users.remove(id).is_some() {
            Change::Removed
        } else {
            Change::Unchanged
        })
    }
    async fn mint_app_password(
        &self,
        id: &str,
        login: &str,
        _label: &str,
    ) -> anyhow::Result<String> {
        let mut s = self.0.lock().unwrap();
        anyhow::ensure!(
            s.users.get(id).is_some_and(|u| u.1 == login && u.0.enabled),
            "bad login"
        );
        let t = crate::secret::generate_password();
        s.tokens.insert(t.clone(), id.into());
        Ok(t)
    }
    async fn app_password_valid(&self, id: &str, t: &str) -> anyhow::Result<bool> {
        Ok(Self::authed(&self.0.lock().unwrap(), id, t).is_ok())
    }
    async fn revoke_app_password(&self, _id: &str, t: &str) -> anyhow::Result<()> {
        self.0.lock().unwrap().tokens.remove(t);
        Ok(())
    }
    async fn list_mail_accounts(&self, id: &str, t: &str) -> anyhow::Result<Vec<MailAccount>> {
        let s = self.0.lock().unwrap();
        Self::authed(&s, id, t)?;
        Ok(s.accounts
            .get(id)
            .map(|v| v.iter().map(|a| a.0.clone()).collect())
            .unwrap_or_default())
    }
    async fn create_mail_account(
        &self,
        id: &str,
        t: &str,
        spec: &MailAccountSpec,
    ) -> anyhow::Result<()> {
        let mut s = self.0.lock().unwrap();
        Self::authed(&s, id, t)?;
        if s.fail_mail_calls > 0 {
            s.fail_mail_calls -= 1;
            anyhow::bail!("IMAP connection refused");
        }
        s.next_id += 1;
        let acct = MailAccount {
            id: s.next_id,
            email: spec.email.clone(),
        };
        s.accounts
            .entry(id.into())
            .or_default()
            .push((acct, spec.clone()));
        Ok(())
    }
    async fn update_mail_account(
        &self,
        id: &str,
        t: &str,
        account: i64,
        spec: &MailAccountSpec,
    ) -> anyhow::Result<()> {
        let mut s = self.0.lock().unwrap();
        Self::authed(&s, id, t)?;
        let a = s
            .accounts
            .get_mut(id)
            .unwrap()
            .iter_mut()
            .find(|a| a.0.id == account)
            .unwrap();
        a.1 = spec.clone();
        Ok(())
    }
    async fn delete_mail_account(&self, id: &str, t: &str, account: i64) -> anyhow::Result<()> {
        let mut s = self.0.lock().unwrap();
        Self::authed(&s, id, t)?;
        s.accounts
            .get_mut(id)
            .unwrap()
            .retain(|a| a.0.id != account);
        Ok(())
    }
}

#[derive(Default)]
struct FakeRegistry(StdMutex<BTreeMap<String, IdentityRecord>>);

impl Registry for FakeRegistry {
    fn get(&self, id: &str) -> anyhow::Result<Option<IdentityRecord>> {
        Ok(self.0.lock().unwrap().get(id).cloned())
    }
    fn get_by_name(&self, name: &str) -> anyhow::Result<Option<IdentityRecord>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .values()
            .find(|r| r.name == name)
            .cloned())
    }
    fn put(&self, r: &IdentityRecord) -> anyhow::Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert(r.external_id.clone(), r.clone());
        Ok(())
    }
    fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.0.lock().unwrap().remove(id);
        Ok(())
    }
    fn list(&self) -> anyhow::Result<Vec<IdentityRecord>> {
        Ok(self.0.lock().unwrap().values().cloned().collect())
    }
    fn now(&self) -> String {
        "2026-10-26T00:00:00Z".into()
    }
}

struct Env {
    engine: Engine,
    mail: Arc<FakeMail>,
    cloud: Arc<FakeCloud>,
    reg: Arc<FakeRegistry>,
}

fn env_with(mail: FakeMail, cloud: FakeCloud) -> Env {
    let mail = Arc::new(mail);
    let cloud = Arc::new(cloud);
    let reg = Arc::new(FakeRegistry::default());
    let cfg = EngineConfig {
        domain: "example.test".into(),
        reserved_names: vec!["contact".into()],
        shared_mailboxes: vec![SharedMailbox {
            key: "contact".into(),
            address: "contact@example.test".into(),
            label: "contact@ (shared)".into(),
            password: "contact-pw".into(),
        }],
        purge_mail: true,
        mail_retry_attempts: 3,
        mail_retry_delay: Duration::ZERO,
    };
    let key = base64::engine::general_purpose::STANDARD.encode([9u8; 32]);
    let engine = Engine::new(
        cfg,
        mail.clone(),
        cloud.clone(),
        reg.clone(),
        Sealer::from_base64(&key).unwrap(),
    );
    Env {
        engine,
        mail,
        cloud,
        reg,
    }
}

fn env() -> Env {
    env_with(FakeMail::default(), FakeCloud::default())
}

fn req(name: &str, shared: &[&str]) -> ProvisionRequest {
    ProvisionRequest {
        name: name.into(),
        display_name: "Ada".into(),
        shared_mailboxes: shared.iter().map(|s| s.to_string()).collect(),
    }
}

fn outcomes(r: &OperationResult) -> Vec<(String, Outcome)> {
    r.steps
        .iter()
        .map(|s| (s.step.clone(), s.outcome))
        .collect()
}

fn accounts(e: &Env, user: &str) -> Vec<String> {
    let s = e.cloud.0.lock().unwrap();
    let mut v: Vec<String> = s
        .accounts
        .get(user)
        .map(|v| v.iter().map(|a| a.0.email.clone()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

#[tokio::test]
async fn create_then_resync_is_all_unchanged() {
    let e = env();
    let r = e
        .engine
        .provision("agent-1", &req("ada", &["contact"]))
        .await
        .unwrap();
    assert!(
        r.steps.iter().all(|s| s.outcome == Outcome::Created),
        "{:?}",
        outcomes(&r)
    );
    assert!(r.credentials.nextcloud_login_password.is_some());
    assert!(r.credentials.mcp_app_password.is_some());
    assert_eq!(r.identity.unwrap().state, IdentityState::Active);
    assert_eq!(
        accounts(&e, "ada"),
        ["ada@example.test", "contact@example.test"]
    );

    let again = e
        .engine
        .provision("agent-1", &req("ada", &["contact"]))
        .await
        .unwrap();
    assert!(
        again.steps.iter().all(|s| s.outcome == Outcome::Unchanged),
        "{:?}",
        outcomes(&again)
    );
    assert!(again.credentials.is_empty());

    assert_eq!(
        *e.mail.waited.lock().unwrap(),
        ["ada@example.test"],
        "the mailbox password is confirmed live before Nextcloud Mail uses it"
    );

    let insp = e.engine.inspect("agent-1").await.unwrap();
    assert!(insp.checks.iter().all(|c| c.ok), "{:?}", insp.checks);
}

#[tokio::test]
async fn secrets_are_sealed_in_the_registry() {
    let e = env();
    let r = e
        .engine
        .provision("agent-1", &req("ada", &[]))
        .await
        .unwrap();
    let rec = e.reg.get("agent-1").unwrap().unwrap();
    let mcp = r.credentials.mcp_app_password.unwrap();
    assert_ne!(rec.mcp_token.as_deref(), Some(mcp.as_str()));
    assert!(!rec.mailbox_password.unwrap().is_empty());
}

#[tokio::test]
async fn shared_mailboxes_follow_the_request() {
    let e = env();
    e.engine
        .provision("agent-1", &req("ada", &[]))
        .await
        .unwrap();
    assert_eq!(accounts(&e, "ada"), ["ada@example.test"]);
    let r = e
        .engine
        .provision("agent-1", &req("ada", &["contact"]))
        .await
        .unwrap();
    assert!(outcomes(&r).contains(&("shared:contact".into(), Outcome::Created)));
    let r = e
        .engine
        .provision("agent-1", &req("ada", &[]))
        .await
        .unwrap();
    assert!(outcomes(&r).contains(&("shared:contact".into(), Outcome::Removed)));
    assert_eq!(accounts(&e, "ada"), ["ada@example.test"]);
    let err = e
        .engine
        .provision("agent-1", &req("ada", &["office"]))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "unknown_shared_mailbox");
}

#[tokio::test]
async fn collisions_are_refused() {
    let mut mail = FakeMail::default();
    mail.aliases.insert("bob@example.test".into());
    let cloud = FakeCloud::default();
    cloud.0.lock().unwrap().foreign.insert("eve".into());
    let e = env_with(mail, cloud);

    assert_eq!(
        e.engine
            .provision("a1", &req("bob", &[]))
            .await
            .unwrap_err()
            .code(),
        "address_in_use"
    );
    assert_eq!(
        e.engine
            .provision("a2", &req("eve", &[]))
            .await
            .unwrap_err()
            .code(),
        "nextcloud_user_exists"
    );
    assert!(
        e.reg.get("a2").unwrap().is_none(),
        "a refused identity leaves no registry record"
    );
    assert_eq!(
        e.engine
            .provision("a3", &req("contact", &[]))
            .await
            .unwrap_err()
            .code(),
        "invalid_name"
    );

    // A worker created by someone else (by hand, or by another installation) is not adopted.
    e.cloud.0.lock().unwrap().users.insert(
        "mallory".into(),
        (
            CloudUser {
                id: "mallory".into(),
                enabled: true,
                email: None,
                display_name: "M".into(),
                groups: vec![],
                quota: None,
            },
            "x".into(),
        ),
    );
    assert_eq!(
        e.engine
            .provision("a6", &req("mallory", &[]))
            .await
            .unwrap_err()
            .code(),
        "nextcloud_user_exists"
    );

    e.engine.provision("a4", &req("ada", &[])).await.unwrap();
    assert_eq!(
        e.engine
            .provision("a5", &req("ada", &[]))
            .await
            .unwrap_err()
            .code(),
        "name_taken"
    );
    assert_eq!(
        e.engine
            .provision("a4", &req("ada2", &[]))
            .await
            .unwrap_err()
            .code(),
        "rename_not_supported"
    );
}

#[tokio::test]
async fn disable_enable_purge_lifecycle() {
    let e = env();
    e.engine
        .provision("agent-1", &req("ada", &["contact"]))
        .await
        .unwrap();
    let mailbox_pw = e.mail.boxes.lock().unwrap()["ada@example.test"].clone();

    let r = e.engine.disable("agent-1").await.unwrap();
    assert_eq!(r.identity.unwrap().state, IdentityState::Disabled);
    assert!(!e.cloud.0.lock().unwrap().users["ada"].0.enabled);
    assert_ne!(
        e.mail.boxes.lock().unwrap()["ada@example.test"],
        mailbox_pw,
        "mailbox is locked"
    );
    assert_eq!(
        accounts(&e, "ada"),
        ["ada@example.test"],
        "shared accounts removed, own kept"
    );

    let again = e.engine.disable("agent-1").await.unwrap();
    assert!(
        again.steps.iter().all(|s| s.outcome == Outcome::Unchanged),
        "{:?}",
        outcomes(&again)
    );
    assert_eq!(
        e.engine
            .provision("agent-1", &req("ada", &[]))
            .await
            .unwrap_err()
            .code(),
        "identity_disabled"
    );
    assert_eq!(
        e.engine.rotate("agent-1").await.unwrap_err().code(),
        "not_active"
    );

    let r = e.engine.enable("agent-1").await.unwrap();
    assert_eq!(r.identity.unwrap().state, IdentityState::Active);
    assert!(
        r.credentials.is_empty(),
        "re-enable keeps credentials: {:?}",
        r.credentials
    );
    assert_eq!(e.mail.boxes.lock().unwrap()["ada@example.test"], mailbox_pw);
    assert_eq!(
        accounts(&e, "ada"),
        ["ada@example.test", "contact@example.test"]
    );

    assert_eq!(
        e.engine.purge("agent-1").await.unwrap_err().code(),
        "not_disabled"
    );
    e.engine.disable("agent-1").await.unwrap();
    let r = e.engine.purge("agent-1").await.unwrap();
    assert!(r.identity.is_none());
    assert!(e.reg.get("agent-1").unwrap().is_none());
    assert!(e.cloud.0.lock().unwrap().users.is_empty());
    assert_eq!(
        *e.mail.deleted.lock().unwrap(),
        [("ada@example.test".to_string(), true)]
    );
    assert_eq!(
        e.engine.purge("agent-1").await.unwrap_err().code(),
        "not_found"
    );

    // The name is free again.
    e.engine
        .provision("agent-2", &req("ada", &[]))
        .await
        .unwrap();
}

#[tokio::test]
async fn rotate_revokes_the_old_mcp_password() {
    let e = env();
    let first = e
        .engine
        .provision("agent-1", &req("ada", &[]))
        .await
        .unwrap()
        .credentials;
    let r = e.engine.rotate("agent-1").await.unwrap();
    let new = r.credentials.mcp_app_password.clone().unwrap();
    assert!(r.credentials.nextcloud_login_password.is_some());
    assert!(
        !e.engine
            .cloud
            .app_password_valid("ada", first.mcp_app_password.as_ref().unwrap())
            .await
            .unwrap()
    );
    assert!(
        e.engine
            .cloud
            .app_password_valid("ada", &new)
            .await
            .unwrap()
    );
    let again = e
        .engine
        .provision("agent-1", &req("ada", &[]))
        .await
        .unwrap();
    assert!(
        again.steps.iter().all(|s| s.outcome == Outcome::Unchanged),
        "{:?}",
        outcomes(&again)
    );
}

#[tokio::test]
async fn interrupted_run_converges_and_returns_fresh_credentials() {
    let e = env();
    // Nextcloud Mail keeps failing: the run errors after user, tokens and mailbox exist.
    e.cloud.0.lock().unwrap().fail_mail_calls = 3;
    assert!(
        e.engine
            .provision("agent-1", &req("ada", &[]))
            .await
            .is_err()
    );
    assert_eq!(
        e.reg.get("agent-1").unwrap().unwrap().state,
        IdentityState::Provisioning
    );

    let r = e
        .engine
        .provision("agent-1", &req("ada", &[]))
        .await
        .unwrap();
    assert!(outcomes(&r).contains(&("nextcloud_mail_account".into(), Outcome::Created)));
    assert_eq!(r.identity.unwrap().state, IdentityState::Active);
}

#[tokio::test]
async fn transient_mail_failures_are_retried() {
    let e = env();
    e.cloud.0.lock().unwrap().fail_mail_calls = 2;
    e.engine
        .provision("agent-1", &req("ada", &[]))
        .await
        .unwrap();
}

#[tokio::test]
async fn lost_mailbox_is_recreated_and_mail_account_updated() {
    let e = env();
    e.engine
        .provision("agent-1", &req("ada", &[]))
        .await
        .unwrap();
    e.mail.boxes.lock().unwrap().remove("ada@example.test");
    let r = e
        .engine
        .provision("agent-1", &req("ada", &[]))
        .await
        .unwrap();
    assert!(outcomes(&r).contains(&("mailbox".into(), Outcome::Created)));
    assert!(outcomes(&r).contains(&("nextcloud_mail_account".into(), Outcome::Updated)));
}

#[tokio::test]
async fn reshare_updates_granted_workers_only() {
    let e = env();
    e.engine
        .provision("a1", &req("ada", &["contact"]))
        .await
        .unwrap();
    e.engine.provision("a2", &req("bob", &[])).await.unwrap();
    let r = e.engine.reshare("contact").await.unwrap();
    assert_eq!(r, [("a1".to_string(), Outcome::Updated)]);
}

#[tokio::test]
async fn disable_and_purge_work_without_the_state_key() {
    let e = env();
    e.engine
        .provision("agent-1", &req("ada", &["contact"]))
        .await
        .unwrap();
    // The state key was replaced: every sealed value is now unreadable.
    let mut rec = e.reg.get("agent-1").unwrap().unwrap();
    let other =
        Sealer::from_base64(&base64::engine::general_purpose::STANDARD.encode([3u8; 32])).unwrap();
    rec.mgmt_token = Some(other.seal("agent-1/mgmt", "x").unwrap());
    rec.mailbox_password = Some(other.seal("agent-1/mailbox", "x").unwrap());
    e.reg.put(&rec).unwrap();
    let before = e.mail.boxes.lock().unwrap()["ada@example.test"].clone();

    let r = e.engine.disable("agent-1").await.unwrap();
    assert_eq!(r.identity.unwrap().state, IdentityState::Disabled);
    assert!(!e.cloud.0.lock().unwrap().users["ada"].0.enabled);
    assert_ne!(
        e.mail.boxes.lock().unwrap()["ada@example.test"],
        before,
        "mailbox locked anyway"
    );
    e.engine.purge("agent-1").await.unwrap();
    assert!(e.reg.get("agent-1").unwrap().is_none());
}
