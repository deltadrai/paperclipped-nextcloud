//! Nextcloud backend for paperclipped-nextcloud.
//!
//! - User management runs as a **sub-admin of the workers' group**. Nextcloud requires the
//!   sub-admin's login password for it (`#[PasswordConfirmationRequired]` rejects app
//!   passwords), and itself stops the sub-admin from touching anyone outside the group.
//! - Nextcloud Mail accounts are managed **as the worker**, with a management app password.
//! - [`bootstrap`] is the one-time admin setup: group, sub-admin, team folder, Talk lobby.

pub mod bootstrap;
mod client;

use anyhow::{Context, anyhow, bail};
use async_trait::async_trait;
use pcnc_core::backend::{
    Change, CloudBackend, CloudUser, CreateUser, MailAccount, MailAccountSpec, UserSpec,
};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

pub use client::seg;
use client::{Auth, Client};

/// How Nextcloud Mail connects to the mail server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Security {
    None,
    /// Implicit TLS (IMAPS 993, SMTPS 465).
    Ssl,
    /// STARTTLS (IMAP 143, submission 587).
    Starttls,
}

impl Security {
    fn mail_app_value(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Ssl => "ssl",
            Self::Starttls => "tls",
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub security: Security,
}

#[derive(Debug, Clone)]
pub struct NextcloudConfig {
    pub url: String,
    pub subadmin_user: String,
    pub subadmin_password: String,
    /// The workers' group, e.g. `digital workforce`.
    pub group: String,
    /// Quota for new workers in Nextcloud's notation, e.g. `5 GB`.
    pub quota: String,
    pub imap: Endpoint,
    pub smtp: Endpoint,
}

pub struct NextcloudBackend {
    cfg: NextcloudConfig,
    client: Client,
    quota_bytes: Option<i64>,
}

/// Parses Nextcloud's human sizes (`5 GB`, `500MB`, `none`) into bytes (1024-based).
pub fn parse_size(s: &str) -> anyhow::Result<Option<i64>> {
    let s = s.trim().to_ascii_lowercase();
    if s == "none" || s == "default" || s.is_empty() {
        return Ok(None);
    }
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let num: f64 = num.parse().with_context(|| format!("bad size {s:?}"))?;
    let mult: f64 = match unit.trim() {
        "" | "b" => 1.0,
        "k" | "kb" => 1024.0,
        "m" | "mb" => 1024f64.powi(2),
        "g" | "gb" => 1024f64.powi(3),
        "t" | "tb" => 1024f64.powi(4),
        u => bail!("unknown size unit {u:?}"),
    };
    Ok(Some((num * mult).round() as i64))
}

impl NextcloudBackend {
    pub fn new(cfg: NextcloudConfig) -> anyhow::Result<Self> {
        let client = Client::new(&cfg.url)?;
        let quota_bytes = parse_size(&cfg.quota)?;
        Ok(Self {
            cfg,
            client,
            quota_bytes,
        })
    }

    fn sub(&self) -> Auth<'_> {
        Auth {
            user: &self.cfg.subadmin_user,
            password: &self.cfg.subadmin_password,
        }
    }

    async fn edit(&self, id: &str, key: &str, value: &str) -> anyhow::Result<()> {
        self.client
            .ocs(
                Method::PUT,
                &format!("/ocs/v2.php/cloud/users/{}", seg(id)),
                &self.sub(),
                &[("key", key), ("value", value)],
                None,
            )
            .await?
            .into_result(&format!("setting {key} of {id}"))?;
        Ok(())
    }

    fn mail_body(&self, spec: &MailAccountSpec) -> Value {
        json!({
            "accountName": spec.name,
            "emailAddress": spec.email,
            "imapHost": self.cfg.imap.host,
            "imapPort": self.cfg.imap.port,
            "imapSslMode": self.cfg.imap.security.mail_app_value(),
            "imapUser": spec.login,
            "imapPassword": spec.password,
            "smtpHost": self.cfg.smtp.host,
            "smtpPort": self.cfg.smtp.port,
            "smtpSslMode": self.cfg.smtp.security.mail_app_value(),
            "smtpUser": spec.login,
            "smtpPassword": spec.password,
            "authMethod": "password",
        })
    }

    async fn mail_call(
        &self,
        method: Method,
        path: &str,
        id: &str,
        token: &str,
        body: Option<&Value>,
        what: &str,
    ) -> anyhow::Result<Value> {
        let auth = Auth {
            user: id,
            password: token,
        };
        let (status, value) = self.client.json(method, path, &auth, body).await?;
        if status.is_success() {
            Ok(value)
        } else {
            // The Mail app's error bodies never echo passwords.
            Err(anyhow!(
                "{what} for {id}: HTTP {} {}",
                status.as_u16(),
                value
            ))
        }
    }
}

const MAIL_ACCOUNTS: &str = "/index.php/apps/mail/api/accounts";
const MAIL_MAILBOXES: &str = "/index.php/apps/mail/api/mailboxes";

impl NextcloudBackend {
    /// Nextcloud Mail lists only mailboxes it has cached ("mailbox … is not cached"), and the
    /// MCP server does not trigger that first sync. Sync every mailbox of a new account once, so
    /// an agent can read its mail right away.
    async fn initial_sync(&self, id: &str, token: &str, email: &str) -> anyhow::Result<()> {
        let account = self
            .list_mail_accounts(id, token)
            .await?
            .into_iter()
            .find(|a| a.email.eq_ignore_ascii_case(email))
            .context("new mail account not listed")?;
        let v = self
            .mail_call(
                Method::GET,
                &format!("{MAIL_MAILBOXES}?accountId={}&forceSync=true", account.id),
                id,
                token,
                None,
                "listing mailboxes",
            )
            .await?;
        let body = json!({ "ids": [], "init": true });
        for mb in v["mailboxes"].as_array().into_iter().flatten() {
            if let Some(mailbox) = mb["databaseId"].as_i64() {
                self.mail_call(
                    Method::POST,
                    &format!("{MAIL_MAILBOXES}/{mailbox}/sync"),
                    id,
                    token,
                    Some(&body),
                    "syncing a mailbox",
                )
                .await?;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl CloudBackend for NextcloudBackend {
    async fn get_user(&self, id: &str) -> anyhow::Result<Option<CloudUser>> {
        let r = self
            .client
            .ocs(
                Method::GET,
                &format!("/ocs/v2.php/cloud/users/{}", seg(id)),
                &self.sub(),
                &[],
                None,
            )
            .await?;
        if r.http == StatusCode::NOT_FOUND || r.code == 404 || r.code == 998 {
            return Ok(None);
        }
        let d = r.into_result(&format!("reading user {id}"))?;
        let groups: Vec<String> = d["groups"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|g| g.as_str().map(String::from))
            .collect();
        if !groups.iter().any(|g| g == &self.cfg.group) {
            // Visible but not a worker: only possible if the configured account is an admin.
            // Treat it as someone else's, so the engine refuses to touch it.
            return Ok(None);
        }
        Ok(Some(CloudUser {
            id: d["id"].as_str().unwrap_or(id).to_string(),
            enabled: d["enabled"].as_bool().unwrap_or(true),
            email: d["email"].as_str().map(String::from),
            display_name: d["displayname"]
                .as_str()
                .or(d["display-name"].as_str())
                .unwrap_or_default()
                .to_string(),
            groups,
            quota: d["quota"]["quota"].as_i64(),
        }))
    }

    async fn create_user(&self, spec: &UserSpec, password: &str) -> anyhow::Result<CreateUser> {
        let mut form = vec![
            ("userid", spec.id.as_str()),
            ("password", password),
            ("displayName", spec.display_name.as_str()),
            ("email", spec.email.as_str()),
            ("groups[]", self.cfg.group.as_str()),
        ];
        if self.quota_bytes.is_some() {
            form.push(("quota", self.cfg.quota.as_str()));
        }
        let r = self
            .client
            .ocs(
                Method::POST,
                "/ocs/v2.php/cloud/users",
                &self.sub(),
                &form,
                None,
            )
            .await?;
        if r.code == 102 {
            return Ok(CreateUser::AlreadyExists);
        }
        r.into_result(&format!("creating user {}", spec.id))?;
        Ok(CreateUser::Created)
    }

    async fn reconcile_user(&self, current: &CloudUser, spec: &UserSpec) -> anyhow::Result<Change> {
        let mut changed = false;
        if current.email.as_deref() != Some(spec.email.as_str()) {
            self.edit(&current.id, "email", &spec.email).await?;
            changed = true;
        }
        if current.display_name != spec.display_name {
            self.edit(&current.id, "displayname", &spec.display_name)
                .await?;
            changed = true;
        }
        if self.quota_bytes.is_some() && current.quota != self.quota_bytes {
            self.edit(&current.id, "quota", &self.cfg.quota).await?;
            changed = true;
        }
        Ok(if changed {
            Change::Updated
        } else {
            Change::Unchanged
        })
    }

    async fn set_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<()> {
        let action = if enabled { "enable" } else { "disable" };
        self.client
            .ocs(
                Method::PUT,
                &format!("/ocs/v2.php/cloud/users/{}/{action}", seg(id)),
                &self.sub(),
                &[],
                None,
            )
            .await?
            .into_result(&format!("{action} user {id}"))?;
        Ok(())
    }

    async fn set_password(&self, id: &str, password: &str) -> anyhow::Result<()> {
        self.edit(id, "password", password).await
    }

    async fn delete_user(&self, id: &str) -> anyhow::Result<Change> {
        let r = self
            .client
            .ocs(
                Method::DELETE,
                &format!("/ocs/v2.php/cloud/users/{}", seg(id)),
                &self.sub(),
                &[],
                None,
            )
            .await?;
        if r.http == StatusCode::NOT_FOUND || r.code == 404 || r.code == 998 || r.code == 101 {
            return Ok(Change::Unchanged);
        }
        r.into_result(&format!("deleting user {id}"))?;
        Ok(Change::Removed)
    }

    async fn mint_app_password(
        &self,
        id: &str,
        login_password: &str,
        label: &str,
    ) -> anyhow::Result<String> {
        let auth = Auth {
            user: id,
            password: login_password,
        };
        let d = self
            .client
            .ocs(
                Method::GET,
                "/ocs/v2.php/core/getapppassword",
                &auth,
                &[],
                Some(label),
            )
            .await?
            .into_result(&format!("minting an app password for {id}"))?;
        d["apppassword"]
            .as_str()
            .map(String::from)
            .context("no apppassword in response")
    }

    async fn app_password_valid(&self, id: &str, app_password: &str) -> anyhow::Result<bool> {
        let auth = Auth {
            user: id,
            password: app_password,
        };
        let r = self
            .client
            .ocs(Method::GET, "/ocs/v2.php/cloud/user", &auth, &[], None)
            .await?;
        if r.http == StatusCode::UNAUTHORIZED || r.code == 997 {
            return Ok(false);
        }
        let d = r.into_result(&format!("checking an app password of {id}"))?;
        Ok(d["id"].as_str() == Some(id))
    }

    async fn revoke_app_password(&self, id: &str, app_password: &str) -> anyhow::Result<()> {
        let auth = Auth {
            user: id,
            password: app_password,
        };
        let r = self
            .client
            .ocs(
                Method::DELETE,
                "/ocs/v2.php/core/apppassword",
                &auth,
                &[],
                None,
            )
            .await?;
        if r.ok() || r.http == StatusCode::UNAUTHORIZED || r.http == StatusCode::FORBIDDEN {
            return Ok(());
        }
        r.into_result(&format!("revoking an app password of {id}"))
            .map(|_| ())
    }

    async fn list_mail_accounts(
        &self,
        id: &str,
        app_password: &str,
    ) -> anyhow::Result<Vec<MailAccount>> {
        let v = self
            .mail_call(
                Method::GET,
                MAIL_ACCOUNTS,
                id,
                app_password,
                None,
                "listing mail accounts",
            )
            .await?;
        Ok(v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| {
                Some(MailAccount {
                    id: a["id"].as_i64().or(a["accountId"].as_i64())?,
                    email: a["emailAddress"].as_str()?.to_string(),
                })
            })
            .collect())
    }

    async fn create_mail_account(
        &self,
        id: &str,
        app_password: &str,
        spec: &MailAccountSpec,
    ) -> anyhow::Result<()> {
        let body = self.mail_body(spec);
        self.mail_call(
            Method::POST,
            MAIL_ACCOUNTS,
            id,
            app_password,
            Some(&body),
            &format!("adding mail account {}", spec.email),
        )
        .await?;
        // The account exists now. The initial sync is best effort: an error here must not make
        // the engine retry the creation, which would add the account twice.
        if let Err(e) = self.initial_sync(id, app_password, &spec.email).await {
            tracing::warn!(user = id, account = %spec.email, error = format!("{e:#}"), "initial mail sync failed; Mail's background job will catch up");
        }
        Ok(())
    }

    async fn update_mail_account(
        &self,
        id: &str,
        app_password: &str,
        account: i64,
        spec: &MailAccountSpec,
    ) -> anyhow::Result<()> {
        let body = self.mail_body(spec);
        self.mail_call(
            Method::PUT,
            &format!("{MAIL_ACCOUNTS}/{account}"),
            id,
            app_password,
            Some(&body),
            &format!("updating mail account {}", spec.email),
        )
        .await
        .map(|_| ())
    }

    async fn delete_mail_account(
        &self,
        id: &str,
        app_password: &str,
        account: i64,
    ) -> anyhow::Result<()> {
        self.mail_call(
            Method::DELETE,
            &format!("{MAIL_ACCOUNTS}/{account}"),
            id,
            app_password,
            None,
            "deleting a mail account",
        )
        .await
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("5 GB").unwrap(), Some(5 * 1024 * 1024 * 1024));
        assert_eq!(parse_size("500MB").unwrap(), Some(500 * 1024 * 1024));
        assert_eq!(parse_size("1.5 GB").unwrap(), Some(1610612736));
        assert_eq!(parse_size("none").unwrap(), None);
        assert!(parse_size("5 parsecs").is_err());
    }

    #[test]
    fn segments() {
        assert_eq!(seg("digital workforce"), "digital%20workforce");
        assert_eq!(seg("ada.l-1_x"), "ada.l-1_x");
        assert_eq!(seg("a/b"), "a%2Fb");
    }
}
