//! The seams between the engine and the outside world. A mail server, a Nextcloud and a
//! registry each implement one trait, so other mail servers can be added as backends.

use async_trait::async_trait;

use crate::model::IdentityRecord;

/// What a backend call changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Created,
    Updated,
    Unchanged,
    Removed,
}

/// Transport security of a mail endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Security {
    None,
    /// Implicit TLS (IMAPS 993, SMTPS 465).
    Ssl,
    /// STARTTLS (IMAP 143, submission 587).
    Starttls,
}

/// An IMAP or SMTP endpoint.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub security: Security,
}

/// A mail server that hosts the workers' mailboxes.
#[async_trait]
pub trait MailBackend: Send + Sync {
    /// True if `address` is taken in any form: a mailbox or an alias.
    async fn address_in_use(&self, address: &str) -> anyhow::Result<bool>;
    /// `Some(true)` if the mailbox exists and accepts `password`, `Some(false)` if it exists
    /// with another password, `None` if it does not exist.
    async fn verify(&self, address: &str, password: &str) -> anyhow::Result<Option<bool>>;
    /// Creates the mailbox or sets its password.
    async fn set_password(&self, address: &str, password: &str) -> anyhow::Result<Change>;
    /// Deletes the mailbox, and its stored mail when `purge_mail` is set.
    async fn delete(&self, address: &str, purge_mail: bool) -> anyhow::Result<Change>;
    /// Waits until the mail server accepts `password` for `address`. A backend that applies
    /// changes asynchronously must implement this: Nextcloud Mail logs in right after, and
    /// failed logins there trip Nextcloud's brute-force protection.
    async fn wait_until_login(&self, _address: &str, _password: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

/// A Nextcloud user as the sub-admin sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudUser {
    pub id: String,
    pub enabled: bool,
    pub email: Option<String>,
    pub display_name: String,
    pub groups: Vec<String>,
    pub quota: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSpec {
    pub id: String,
    pub display_name: String,
    pub email: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateUser {
    Created,
    /// A user with that id exists but is not visible to the sub-admin: someone else's.
    AlreadyExists,
}

/// A mail account in a user's Nextcloud Mail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailAccount {
    pub id: i64,
    pub email: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailAccountSpec {
    pub name: String,
    pub email: String,
    pub login: String,
    pub password: String,
}

/// Nextcloud, driven as the group's sub-admin and, for Mail, as the worker.
#[async_trait]
pub trait CloudBackend: Send + Sync {
    /// The user, if it exists in the managed group.
    async fn get_user(&self, id: &str) -> anyhow::Result<Option<CloudUser>>;
    /// Creates the user in the managed group with the configured quota.
    async fn create_user(&self, spec: &UserSpec, password: &str) -> anyhow::Result<CreateUser>;
    /// Brings email, display name, quota and group membership to `spec`.
    async fn reconcile_user(&self, current: &CloudUser, spec: &UserSpec) -> anyhow::Result<Change>;
    async fn set_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<()>;
    async fn set_password(&self, id: &str, password: &str) -> anyhow::Result<()>;
    async fn delete_user(&self, id: &str) -> anyhow::Result<Change>;

    /// Mints an app password named `label`, authenticating with the login password.
    async fn mint_app_password(
        &self,
        id: &str,
        login_password: &str,
        label: &str,
    ) -> anyhow::Result<String>;
    async fn app_password_valid(&self, id: &str, app_password: &str) -> anyhow::Result<bool>;
    /// Revokes the given app password. A token that is already invalid is not an error.
    async fn revoke_app_password(&self, id: &str, app_password: &str) -> anyhow::Result<()>;

    async fn list_mail_accounts(
        &self,
        id: &str,
        app_password: &str,
    ) -> anyhow::Result<Vec<MailAccount>>;
    async fn create_mail_account(
        &self,
        id: &str,
        app_password: &str,
        spec: &MailAccountSpec,
    ) -> anyhow::Result<()>;
    async fn update_mail_account(
        &self,
        id: &str,
        app_password: &str,
        account: i64,
        spec: &MailAccountSpec,
    ) -> anyhow::Result<()>;
    async fn delete_mail_account(
        &self,
        id: &str,
        app_password: &str,
        account: i64,
    ) -> anyhow::Result<()>;
}

/// Durable store of the identities this server manages. Sealed fields are stored as given.
pub trait Registry: Send + Sync {
    fn get(&self, external_id: &str) -> anyhow::Result<Option<IdentityRecord>>;
    fn get_by_name(&self, name: &str) -> anyhow::Result<Option<IdentityRecord>>;
    fn put(&self, record: &IdentityRecord) -> anyhow::Result<()>;
    fn delete(&self, external_id: &str) -> anyhow::Result<()>;
    fn list(&self) -> anyhow::Result<Vec<IdentityRecord>>;
    fn now(&self) -> String;
}
