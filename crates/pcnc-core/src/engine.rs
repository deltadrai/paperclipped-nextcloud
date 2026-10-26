//! The idempotent engine. Every operation is a sequence of check → apply steps, so running it
//! again converges instead of failing, and an interrupted run is finished by the next one.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::sync::Mutex;

use crate::backend::{
    Change, CloudBackend, CreateUser, MailAccount, MailAccountSpec, MailBackend, Registry, UserSpec,
};
use crate::error::EngineError;
use crate::model::*;
use crate::name;
use crate::secret::{Sealer, generate_password};

/// App password labels as they appear in the worker's Nextcloud security settings.
pub const MGMT_TOKEN_LABEL: &str = "paperclipped-nextcloud (management)";
pub const MCP_TOKEN_LABEL: &str = "paperclip-mcp";

/// A company mailbox that workers can be granted, e.g. `contact@`.
#[derive(Debug, Clone)]
pub struct SharedMailbox {
    /// Stable key used in requests and the registry, e.g. `contact`.
    pub key: String,
    pub address: String,
    /// Account name shown in the worker's Nextcloud Mail.
    pub label: String,
    pub password: String,
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Mail domain of the workers' addresses.
    pub domain: String,
    /// Extra names no worker may take, on top of [`name::DEFAULT_RESERVED`].
    pub reserved_names: Vec<String>,
    pub shared_mailboxes: Vec<SharedMailbox>,
    /// Purge also deletes stored mail when the backend can reach it.
    pub purge_mail: bool,
    /// A fresh mailbox needs a few seconds before the mail server accepts logins, so Nextcloud
    /// Mail account calls are retried this many times…
    pub mail_retry_attempts: u32,
    /// …with this delay between attempts.
    pub mail_retry_delay: Duration,
}

pub struct Engine {
    cfg: EngineConfig,
    mail: Arc<dyn MailBackend>,
    cloud: Arc<dyn CloudBackend>,
    reg: Arc<dyn Registry>,
    sealer: Sealer,
    /// Serialises all mutating operations. Provisioning volume is tiny; correctness wins.
    op: Mutex<()>,
}

type Result<T> = std::result::Result<T, EngineError>;

struct Run {
    steps: Vec<Step>,
    creds: Credentials,
    /// Login password set or created during this run, if any.
    login: Option<String>,
}

impl Run {
    fn new() -> Self {
        Self {
            steps: Vec::new(),
            creds: Credentials::default(),
            login: None,
        }
    }

    fn step(&mut self, step: impl Into<String>, outcome: Outcome) {
        self.steps.push(Step {
            step: step.into(),
            outcome,
        });
    }
}

fn outcome(c: Change) -> Outcome {
    match c {
        Change::Created => Outcome::Created,
        Change::Updated => Outcome::Updated,
        Change::Unchanged => Outcome::Unchanged,
        Change::Removed => Outcome::Removed,
    }
}

impl Engine {
    pub fn new(
        cfg: EngineConfig,
        mail: Arc<dyn MailBackend>,
        cloud: Arc<dyn CloudBackend>,
        reg: Arc<dyn Registry>,
        sealer: Sealer,
    ) -> Self {
        Self {
            cfg,
            mail,
            cloud,
            reg,
            sealer,
            op: Mutex::new(()),
        }
    }

    pub fn config(&self) -> &EngineConfig {
        &self.cfg
    }

    // ---------------------------------------------------------------- sealed fields

    fn open(&self, rec: &IdentityRecord, field: &str) -> anyhow::Result<Option<String>> {
        let sealed = match field {
            "mgmt" => &rec.mgmt_token,
            "mcp" => &rec.mcp_token,
            "mailbox" => &rec.mailbox_password,
            _ => unreachable!("unknown sealed field {field}"),
        };
        sealed
            .as_deref()
            .map(|s| self.sealer.open(&format!("{}/{field}", rec.external_id), s))
            .transpose()
    }

    fn seal(&self, rec: &mut IdentityRecord, field: &str, value: &str) -> anyhow::Result<()> {
        let sealed = Some(
            self.sealer
                .seal(&format!("{}/{field}", rec.external_id), value)?,
        );
        match field {
            "mgmt" => rec.mgmt_token = sealed,
            "mcp" => rec.mcp_token = sealed,
            "mailbox" => rec.mailbox_password = sealed,
            _ => unreachable!("unknown sealed field {field}"),
        }
        Ok(())
    }

    fn save(&self, rec: &mut IdentityRecord) -> anyhow::Result<()> {
        rec.updated_at = self.reg.now();
        self.reg.put(rec)
    }

    fn record(&self, external_id: &str) -> Result<IdentityRecord> {
        self.reg.get(external_id)?.ok_or(EngineError::NotFound)
    }

    // ---------------------------------------------------------------- queries

    pub fn list(&self) -> Result<Vec<IdentityView>> {
        Ok(self.reg.list()?.iter().map(IdentityView::from).collect())
    }

    pub fn get(&self, external_id: &str) -> Result<IdentityView> {
        Ok(IdentityView::from(&self.record(external_id)?))
    }

    pub fn shared_mailboxes(&self) -> Vec<(String, String, String)> {
        self.cfg
            .shared_mailboxes
            .iter()
            .map(|m| (m.key.clone(), m.address.clone(), m.label.clone()))
            .collect()
    }

    /// Compares the registry's intent with live state, without changing anything.
    pub async fn inspect(&self, external_id: &str) -> Result<Inspection> {
        let rec = self.record(external_id)?;
        let active = rec.state != IdentityState::Disabled;
        let mut checks = Vec::new();
        let mut check = |c: &str, ok: bool, detail: Option<String>| {
            checks.push(Check {
                check: c.into(),
                ok,
                detail,
            })
        };

        match self.cloud.get_user(&rec.name).await? {
            None => check("nextcloud_user", false, Some("missing".into())),
            Some(u) => check(
                "nextcloud_user",
                u.enabled == active,
                (u.enabled != active).then(|| format!("enabled={}", u.enabled)),
            ),
        }
        if let Some(mpw) = self.open(&rec, "mailbox")? {
            let v = self.mail.verify(&rec.address, &mpw).await?;
            let ok = if active {
                v == Some(true)
            } else {
                v == Some(false)
            };
            check("mailbox", ok, (!ok).then(|| format!("{v:?}")));
        } else {
            check(
                "mailbox",
                false,
                Some("no mailbox password in registry".into()),
            );
        }
        if !active {
            return Ok(Inspection {
                identity: IdentityView::from(&rec),
                checks,
            });
        }
        let mgmt = match self.open(&rec, "mgmt")? {
            Some(t) if self.cloud.app_password_valid(&rec.name, &t).await? => {
                check("management_app_password", true, None);
                Some(t)
            }
            _ => {
                check("management_app_password", false, None);
                None
            }
        };
        let mcp_ok = match self.open(&rec, "mcp")? {
            Some(t) => self.cloud.app_password_valid(&rec.name, &t).await?,
            None => false,
        };
        check("mcp_app_password", mcp_ok, None);
        if let Some(mgmt) = mgmt {
            let accounts = self.cloud.list_mail_accounts(&rec.name, &mgmt).await?;
            let has = |addr: &str| accounts.iter().any(|a| a.email.eq_ignore_ascii_case(addr));
            check("nextcloud_mail_account", has(&rec.address), None);
            for mb in self
                .cfg
                .shared_mailboxes
                .iter()
                .filter(|m| rec.shared.contains(&m.key))
            {
                check(&format!("shared:{}", mb.key), has(&mb.address), None);
            }
        }
        Ok(Inspection {
            identity: IdentityView::from(&rec),
            checks,
        })
    }

    // ---------------------------------------------------------------- operations

    /// Creates the identity, or converges an existing one to `req`.
    pub async fn provision(
        &self,
        external_id: &str,
        req: &ProvisionRequest,
    ) -> Result<OperationResult> {
        let _guard = self.op.lock().await;
        name::validate(&req.name, &self.cfg.reserved_names)?;
        let display_name = req.display_name.trim();
        if display_name.is_empty() || display_name.chars().count() > 64 {
            return Err(EngineError::bad_request(
                "invalid_display_name",
                "display name must be 1 to 64 characters",
            ));
        }
        let mut shared: Vec<String> = req.shared_mailboxes.clone();
        shared.sort();
        shared.dedup();
        if let Some(unknown) = shared
            .iter()
            .find(|k| !self.cfg.shared_mailboxes.iter().any(|m| &m.key == *k))
        {
            return Err(EngineError::bad_request(
                "unknown_shared_mailbox",
                format!("no shared mailbox {unknown:?}"),
            ));
        }
        let address = format!("{}@{}", req.name, self.cfg.domain);

        let mut rec = match self.reg.get(external_id)? {
            Some(r) if r.name != req.name => {
                return Err(EngineError::conflict(
                    "rename_not_supported",
                    format!(
                        "identity is named {:?}; purge it to choose another name",
                        r.name
                    ),
                ));
            }
            Some(r) if r.state == IdentityState::Disabled => {
                return Err(EngineError::conflict(
                    "identity_disabled",
                    "identity is disabled; enable it first",
                ));
            }
            Some(r) => r,
            None => {
                if let Some(other) = self.reg.get_by_name(&req.name)? {
                    return Err(EngineError::conflict(
                        "name_taken",
                        format!("{:?} belongs to identity {}", req.name, other.external_id),
                    ));
                }
                if self.cloud.get_user(&req.name).await?.is_some() {
                    return Err(EngineError::conflict(
                        "nextcloud_user_exists",
                        format!(
                            "a Nextcloud user {:?} exists that this server did not create",
                            req.name
                        ),
                    ));
                }
                if self.mail.address_in_use(&address).await? {
                    return Err(EngineError::conflict(
                        "address_in_use",
                        format!("{address} already exists on the mail server"),
                    ));
                }
                let now = self.reg.now();
                IdentityRecord {
                    external_id: external_id.into(),
                    name: req.name.clone(),
                    address,
                    display_name: display_name.into(),
                    state: IdentityState::Provisioning,
                    shared: Vec::new(),
                    mgmt_token: None,
                    mcp_token: None,
                    mailbox_password: None,
                    created_at: now.clone(),
                    updated_at: now,
                }
            }
        };
        rec.display_name = display_name.into();
        rec.shared = shared;
        self.save(&mut rec)?;

        let mut run = Run::new();
        self.converge(&mut rec, &mut run).await?;
        Ok(OperationResult {
            identity: Some(IdentityView::from(&rec)),
            steps: run.steps,
            credentials: run.creds,
        })
    }

    /// Re-enables a disabled identity and converges it.
    pub async fn enable(&self, external_id: &str) -> Result<OperationResult> {
        let _guard = self.op.lock().await;
        let mut rec = self.record(external_id)?;
        let mut run = Run::new();
        self.converge(&mut rec, &mut run).await?;
        Ok(OperationResult {
            identity: Some(IdentityView::from(&rec)),
            steps: run.steps,
            credentials: run.creds,
        })
    }

    async fn converge(&self, rec: &mut IdentityRecord, run: &mut Run) -> Result<()> {
        let spec = UserSpec {
            id: rec.name.clone(),
            display_name: rec.display_name.clone(),
            email: rec.address.clone(),
        };

        // 1. Nextcloud user in the managed group, enabled.
        match self.cloud.get_user(&rec.name).await? {
            None => {
                let pw = generate_password();
                match self.cloud.create_user(&spec, &pw).await? {
                    CreateUser::Created => {
                        run.login = Some(pw.clone());
                        run.creds.nextcloud_login_password = Some(pw);
                        run.step("nextcloud_user", Outcome::Created);
                    }
                    CreateUser::AlreadyExists => {
                        if rec.mgmt_token.is_none() && rec.mcp_token.is_none() {
                            self.reg.delete(&rec.external_id)?;
                        }
                        return Err(EngineError::conflict(
                            "nextcloud_user_exists",
                            format!(
                                "a Nextcloud user {:?} exists outside the managed group",
                                rec.name
                            ),
                        ));
                    }
                }
            }
            Some(user) => {
                let mut change = self.cloud.reconcile_user(&user, &spec).await?;
                if !user.enabled {
                    self.cloud.set_enabled(&rec.name, true).await?;
                    change = Change::Updated;
                }
                run.step("nextcloud_user", outcome(change));
            }
        }

        // 2. Management app password, used for the worker's Nextcloud Mail.
        let mgmt = match self.open(rec, "mgmt")? {
            Some(t) if self.cloud.app_password_valid(&rec.name, &t).await? => {
                run.step("management_app_password", Outcome::Unchanged);
                t
            }
            previous => {
                let login = self.login(rec, run).await?;
                let t = self
                    .cloud
                    .mint_app_password(&rec.name, &login, MGMT_TOKEN_LABEL)
                    .await?;
                self.seal(rec, "mgmt", &t)?;
                self.save(rec)?;
                run.step(
                    "management_app_password",
                    if previous.is_some() {
                        Outcome::Updated
                    } else {
                        Outcome::Created
                    },
                );
                t
            }
        };

        // 3. The agent's MCP app password.
        match self.open(rec, "mcp")? {
            Some(t) if self.cloud.app_password_valid(&rec.name, &t).await? => {
                run.step("mcp_app_password", Outcome::Unchanged);
            }
            previous => {
                let login = self.login(rec, run).await?;
                let t = self
                    .cloud
                    .mint_app_password(&rec.name, &login, MCP_TOKEN_LABEL)
                    .await?;
                self.seal(rec, "mcp", &t)?;
                self.save(rec)?;
                run.creds.mcp_app_password = Some(t);
                run.step(
                    "mcp_app_password",
                    if previous.is_some() {
                        Outcome::Updated
                    } else {
                        Outcome::Created
                    },
                );
            }
        }

        // 4. Mailbox on the mail server, with the machine-only password.
        let mailbox_pw = match self.open(rec, "mailbox")? {
            Some(p) => p,
            None => {
                let p = generate_password();
                self.seal(rec, "mailbox", &p)?;
                self.save(rec)?;
                p
            }
        };
        let mailbox_changed = match self.mail.verify(&rec.address, &mailbox_pw).await? {
            Some(true) => {
                run.step("mailbox", Outcome::Unchanged);
                false
            }
            current => {
                self.mail.set_password(&rec.address, &mailbox_pw).await?;
                run.step(
                    "mailbox",
                    if current.is_none() {
                        Outcome::Created
                    } else {
                        Outcome::Updated
                    },
                );
                true
            }
        };

        // 5. The worker's own account in Nextcloud Mail.
        let accounts = self.cloud.list_mail_accounts(&rec.name, &mgmt).await?;
        let find = |addr: &str| {
            accounts
                .iter()
                .find(|a| a.email.eq_ignore_ascii_case(addr))
                .cloned()
        };
        let own = MailAccountSpec {
            name: rec.display_name.clone(),
            email: rec.address.clone(),
            login: rec.address.clone(),
            password: mailbox_pw,
        };
        match find(&rec.address) {
            None => {
                self.with_retry(|| self.cloud.create_mail_account(&rec.name, &mgmt, &own))
                    .await?;
                run.step("nextcloud_mail_account", Outcome::Created);
            }
            Some(a) if mailbox_changed => {
                self.with_retry(|| self.cloud.update_mail_account(&rec.name, &mgmt, a.id, &own))
                    .await?;
                run.step("nextcloud_mail_account", Outcome::Updated);
            }
            Some(_) => run.step("nextcloud_mail_account", Outcome::Unchanged),
        }

        // 6. Shared mailboxes: add the granted ones, remove the revoked ones.
        for mb in &self.cfg.shared_mailboxes {
            let step = format!("shared:{}", mb.key);
            let granted = rec.shared.contains(&mb.key);
            match (granted, find(&mb.address)) {
                (true, None) => {
                    let spec = shared_spec(mb);
                    self.with_retry(|| self.cloud.create_mail_account(&rec.name, &mgmt, &spec))
                        .await?;
                    run.step(step, Outcome::Created);
                }
                (true, Some(_)) => run.step(step, Outcome::Unchanged),
                (false, Some(a)) => {
                    self.cloud
                        .delete_mail_account(&rec.name, &mgmt, a.id)
                        .await?;
                    run.step(step, Outcome::Removed);
                }
                (false, None) => {}
            }
        }

        rec.state = IdentityState::Active;
        self.save(rec)?;
        Ok(())
    }

    /// The worker's login password for this run: the one created or set earlier in the run, or
    /// a new one. The admin's copy is then outdated, so it is returned to the caller.
    async fn login(&self, rec: &IdentityRecord, run: &mut Run) -> Result<String> {
        if let Some(pw) = &run.login {
            return Ok(pw.clone());
        }
        let pw = generate_password();
        self.cloud.set_password(&rec.name, &pw).await?;
        run.login = Some(pw.clone());
        run.creds.nextcloud_login_password = Some(pw.clone());
        run.step("nextcloud_login_password", Outcome::Updated);
        Ok(pw)
    }

    async fn with_retry<'a, F, Fut>(&self, mut f: F) -> Result<()>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<()>> + 'a,
    {
        let mut attempt = 1;
        loop {
            match f().await {
                Ok(()) => return Ok(()),
                Err(e) if attempt < self.cfg.mail_retry_attempts => {
                    tracing::debug!(attempt, error = %e, "mail account call failed; retrying");
                    attempt += 1;
                    tokio::time::sleep(self.cfg.mail_retry_delay).await;
                }
                Err(e) => return Err(e.context("Nextcloud Mail account call failed").into()),
            }
        }
    }

    /// First teardown step: removes shared mailboxes, disables the Nextcloud user and locks the
    /// mailbox. Files, mail and Talk history stay; [`Engine::enable`] restores everything.
    pub async fn disable(&self, external_id: &str) -> Result<OperationResult> {
        let _guard = self.op.lock().await;
        let mut rec = self.record(external_id)?;
        let mut run = Run::new();

        match self.cloud.get_user(&rec.name).await? {
            Some(user) if user.enabled => {
                // Mail accounts can only be managed while the user can still log in.
                if let Some(mgmt) = self.open(&rec, "mgmt")? {
                    if self.cloud.app_password_valid(&rec.name, &mgmt).await? {
                        let accounts = self.cloud.list_mail_accounts(&rec.name, &mgmt).await?;
                        for mb in &self.cfg.shared_mailboxes {
                            if let Some(a) = accounts
                                .iter()
                                .find(|a| a.email.eq_ignore_ascii_case(&mb.address))
                            {
                                self.cloud
                                    .delete_mail_account(&rec.name, &mgmt, a.id)
                                    .await?;
                                run.step(format!("shared:{}", mb.key), Outcome::Removed);
                            }
                        }
                    }
                }
                self.cloud.set_enabled(&rec.name, false).await?;
                run.step("nextcloud_user", Outcome::Updated);
            }
            Some(_) => run.step("nextcloud_user", Outcome::Unchanged),
            None => run.step("nextcloud_user", Outcome::Unchanged),
        }

        if let Some(mailbox_pw) = self.open(&rec, "mailbox")? {
            if self.mail.verify(&rec.address, &mailbox_pw).await? == Some(true) {
                self.mail
                    .set_password(&rec.address, &generate_password())
                    .await?;
                run.step("mailbox", Outcome::Updated);
            } else {
                run.step("mailbox", Outcome::Unchanged);
            }
        }

        rec.state = IdentityState::Disabled;
        self.save(&mut rec)?;
        Ok(OperationResult {
            identity: Some(IdentityView::from(&rec)),
            steps: run.steps,
            credentials: run.creds,
        })
    }

    /// New login password and MCP app password; the old MCP app password is revoked.
    pub async fn rotate(&self, external_id: &str) -> Result<OperationResult> {
        let _guard = self.op.lock().await;
        let mut rec = self.record(external_id)?;
        if rec.state != IdentityState::Active {
            return Err(EngineError::conflict(
                "not_active",
                "only an active identity can be rotated",
            ));
        }
        let mut run = Run::new();
        let login = self.login(&rec, &mut run).await?;
        let old = self.open(&rec, "mcp")?;
        let new = self
            .cloud
            .mint_app_password(&rec.name, &login, MCP_TOKEN_LABEL)
            .await?;
        self.seal(&mut rec, "mcp", &new)?;
        self.save(&mut rec)?;
        if let Some(old) = old {
            self.cloud.revoke_app_password(&rec.name, &old).await?;
        }
        run.creds.mcp_app_password = Some(new);
        run.step("mcp_app_password", Outcome::Updated);

        let mgmt_valid = match self.open(&rec, "mgmt")? {
            Some(t) => self.cloud.app_password_valid(&rec.name, &t).await?,
            None => false,
        };
        if !mgmt_valid {
            let t = self
                .cloud
                .mint_app_password(&rec.name, &login, MGMT_TOKEN_LABEL)
                .await?;
            self.seal(&mut rec, "mgmt", &t)?;
            self.save(&mut rec)?;
            run.step("management_app_password", Outcome::Updated);
        }
        Ok(OperationResult {
            identity: Some(IdentityView::from(&rec)),
            steps: run.steps,
            credentials: run.creds,
        })
    }

    /// Second teardown step, only for a disabled identity: deletes the Nextcloud user with its
    /// data, the mailbox and the registry record.
    pub async fn purge(&self, external_id: &str) -> Result<OperationResult> {
        let _guard = self.op.lock().await;
        let rec = self.record(external_id)?;
        if rec.state != IdentityState::Disabled {
            return Err(EngineError::conflict(
                "not_disabled",
                "disable the identity before purging it",
            ));
        }
        let mut run = Run::new();
        run.step(
            "nextcloud_user",
            outcome(self.cloud.delete_user(&rec.name).await?),
        );
        run.step(
            "mailbox",
            outcome(self.mail.delete(&rec.address, self.cfg.purge_mail).await?),
        );
        self.reg.delete(&rec.external_id)?;
        run.step("registry", Outcome::Removed);
        Ok(OperationResult {
            identity: None,
            steps: run.steps,
            credentials: run.creds,
        })
    }

    /// Pushes a shared mailbox's current password to every active worker granted it.
    pub async fn reshare(&self, key: &str) -> Result<Vec<(String, Outcome)>> {
        let _guard = self.op.lock().await;
        let mb = self
            .cfg
            .shared_mailboxes
            .iter()
            .find(|m| m.key == key)
            .ok_or_else(|| {
                EngineError::bad_request(
                    "unknown_shared_mailbox",
                    format!("no shared mailbox {key:?}"),
                )
            })?;
        let spec = shared_spec(mb);
        let mut results = Vec::new();
        for rec in self.reg.list()? {
            if rec.state != IdentityState::Active || !rec.shared.contains(&mb.key) {
                continue;
            }
            let mgmt = self
                .open(&rec, "mgmt")?
                .context("identity has no management app password")?;
            let accounts: Vec<MailAccount> =
                self.cloud.list_mail_accounts(&rec.name, &mgmt).await?;
            let result = match accounts
                .iter()
                .find(|a| a.email.eq_ignore_ascii_case(&mb.address))
            {
                Some(a) => {
                    self.cloud
                        .update_mail_account(&rec.name, &mgmt, a.id, &spec)
                        .await?;
                    Outcome::Updated
                }
                None => {
                    self.with_retry(|| self.cloud.create_mail_account(&rec.name, &mgmt, &spec))
                        .await?;
                    Outcome::Created
                }
            };
            results.push((rec.external_id.clone(), result));
        }
        Ok(results)
    }
}

fn shared_spec(mb: &SharedMailbox) -> MailAccountSpec {
    MailAccountSpec {
        name: mb.label.clone(),
        email: mb.address.clone(),
        login: mb.address.clone(),
        password: mb.password.clone(),
    }
}

#[cfg(test)]
mod tests;
