//! Configuration: a YAML file for everything that is not secret, environment variables for
//! everything that is.
//!
//! | Variable | Purpose |
//! |---|---|
//! | `PCNC_API_TOKEN` | Bearer token API clients (the Paperclip plugin) must send |
//! | `PCNC_STATE_KEY` | 32 random bytes, base64: seals credentials in the registry |
//! | `NEXTCLOUD_SUBADMIN_PASSWORD` | Login password of the group sub-admin |
//! | `SHARED_MAILBOX_<KEY>_PASSWORD` | Password of each shared mailbox, key upper-cased |
//! | `PCNC_BOOTSTRAP_ADMIN_USER`, `PCNC_BOOTSTRAP_ADMIN_PASSWORD` | `bootstrap` only; never stored |

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use pcnc_core::{EngineConfig, SharedMailbox};
use pcnc_nextcloud::{Endpoint, NextcloudConfig};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Mail domain of the workers, e.g. `example.com`.
    pub domain: String,
    #[serde(default)]
    pub reserved_names: Vec<String>,
    pub nextcloud: NextcloudSection,
    pub mail: MailSection,
    #[serde(default)]
    pub shared_mailboxes: Vec<SharedMailboxSection>,
    #[serde(default)]
    pub bootstrap: BootstrapSection,
    #[serde(default)]
    pub mcp: McpSection,
    #[serde(default)]
    pub server: ServerSection,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NextcloudSection {
    pub url: String,
    pub subadmin_user: String,
    #[serde(default = "default_group")]
    pub group: String,
    #[serde(default = "default_quota")]
    pub quota: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub enum MailBackendKind {
    DockerMailserver,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MailSection {
    pub backend: MailBackendKind,
    /// DMS config volume (`/tmp/docker-mailserver` inside DMS).
    pub config_dir: PathBuf,
    /// DMS mail volume (`/var/mail` inside DMS); optional, needed only to purge mail.
    #[serde(default)]
    pub mail_dir: Option<PathBuf>,
    #[serde(default = "yes")]
    pub purge_mail: bool,
    /// How Nextcloud Mail reaches the mail server.
    pub imap: Endpoint,
    pub smtp: Endpoint,
    /// Where this server checks, by IMAP login, that a changed mailbox password is live before
    /// Nextcloud Mail uses it. Defaults to `imap`; set it when this server reaches the mail
    /// server by another address than Nextcloud does.
    #[serde(default)]
    pub probe: Option<Endpoint>,
    #[serde(default = "default_probe_timeout")]
    pub probe_timeout_secs: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedMailboxSection {
    pub key: String,
    pub address: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapSection {
    #[serde(default = "default_group")]
    pub team_folder: String,
    #[serde(default = "default_lobby")]
    pub lobby: String,
}

impl Default for BootstrapSection {
    fn default() -> Self {
        Self {
            team_folder: default_group(),
            lobby: default_lobby(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpSection {
    /// URL of the nextcloud-mcp-server instance (multi_user_basic) agents connect to. Shown to
    /// API clients so they can tell the admin how to connect an agent.
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSection {
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default = "default_state_dir")]
    pub state_dir: PathBuf,
    /// Mutating API calls allowed per minute, across all clients.
    #[serde(default = "default_rate")]
    pub rate_limit_per_minute: u32,
}

impl Default for ServerSection {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            state_dir: default_state_dir(),
            rate_limit_per_minute: default_rate(),
        }
    }
}

fn default_group() -> String {
    "digital workforce".into()
}
fn default_quota() -> String {
    "5 GB".into()
}
fn default_lobby() -> String {
    "digital workforce lobby".into()
}
fn default_listen() -> String {
    "127.0.0.1:8091".into()
}
fn default_state_dir() -> PathBuf {
    "/var/lib/paperclipped-nextcloud".into()
}
fn default_rate() -> u32 {
    30
}
fn default_probe_timeout() -> u64 {
    90
}
fn yes() -> bool {
    true
}

pub fn env(name: &str) -> anyhow::Result<String> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Ok(v.trim().to_string()),
        _ => bail!("environment variable {name} is not set"),
    }
}

/// `contact` → `SHARED_MAILBOX_CONTACT_PASSWORD`, `sales-de` → `SHARED_MAILBOX_SALES_DE_PASSWORD`.
pub fn shared_mailbox_env(key: &str) -> String {
    let k: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("SHARED_MAILBOX_{k}_PASSWORD")
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let cfg: Self =
            serde_yaml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> anyhow::Result<()> {
        let mut keys = std::collections::HashSet::new();
        for m in &self.shared_mailboxes {
            if m.key.is_empty()
                || !m
                    .key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                bail!(
                    "shared mailbox key {:?} must be letters, digits, '-' or '_'",
                    m.key
                );
            }
            if !keys.insert(&m.key) {
                bail!("shared mailbox key {:?} is used twice", m.key);
            }
            if !m.address.contains('@') {
                bail!("shared mailbox {:?} has no valid address", m.key);
            }
        }
        Ok(())
    }

    /// Local parts no worker may take: the configured ones plus every shared mailbox in the domain.
    fn reserved(&self) -> Vec<String> {
        let mut r = self.reserved_names.clone();
        for m in &self.shared_mailboxes {
            if let Some((local, domain)) = m.address.split_once('@') {
                if domain.eq_ignore_ascii_case(&self.domain) {
                    r.push(local.to_ascii_lowercase());
                }
            }
        }
        r
    }

    pub fn engine_config(&self) -> anyhow::Result<EngineConfig> {
        let shared_mailboxes = self
            .shared_mailboxes
            .iter()
            .map(|m| {
                Ok(SharedMailbox {
                    key: m.key.clone(),
                    address: m.address.to_ascii_lowercase(),
                    label: m.label.clone().unwrap_or_else(|| m.address.clone()),
                    password: env(&shared_mailbox_env(&m.key))?,
                })
            })
            .collect::<anyhow::Result<_>>()?;
        Ok(EngineConfig {
            domain: self.domain.to_ascii_lowercase(),
            reserved_names: self.reserved(),
            shared_mailboxes,
            purge_mail: self.mail.purge_mail,
            mail_retry_attempts: 10,
            mail_retry_delay: Duration::from_secs(3),
        })
    }

    pub fn nextcloud_config(&self) -> anyhow::Result<NextcloudConfig> {
        Ok(NextcloudConfig {
            url: self.nextcloud.url.clone(),
            subadmin_user: self.nextcloud.subadmin_user.clone(),
            subadmin_password: env("NEXTCLOUD_SUBADMIN_PASSWORD")?,
            group: self.nextcloud.group.clone(),
            quota: self.nextcloud.quota.clone(),
            imap: self.mail.imap.clone(),
            smtp: self.mail.smtp.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../../../deploy/config.example.yaml");

    #[test]
    fn example_config_parses() {
        let cfg: Config = serde_yaml::from_str(EXAMPLE).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.nextcloud.group, "digital workforce");
        assert!(cfg.reserved().contains(&"contact".to_string()));
    }

    #[test]
    fn env_names() {
        assert_eq!(
            shared_mailbox_env("contact"),
            "SHARED_MAILBOX_CONTACT_PASSWORD"
        );
        assert_eq!(
            shared_mailbox_env("sales-de"),
            "SHARED_MAILBOX_SALES_DE_PASSWORD"
        );
    }
}
