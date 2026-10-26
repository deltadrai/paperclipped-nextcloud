use serde::{Deserialize, Serialize};

/// Lifecycle state of an identity in the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityState {
    /// Created in the registry, convergence not yet completed.
    Provisioning,
    Active,
    /// Nextcloud user disabled, mailbox locked, shared mailboxes removed. Data kept.
    Disabled,
}

impl IdentityState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Provisioning => "provisioning",
            Self::Active => "active",
            Self::Disabled => "disabled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "provisioning" => Some(Self::Provisioning),
            "active" => Some(Self::Active),
            "disabled" => Some(Self::Disabled),
            _ => None,
        }
    }
}

/// One registry row. Sealed fields are encrypted with the state key; see [`crate::secret`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityRecord {
    /// The caller's id for this identity (for Paperclip: the agent id).
    pub external_id: String,
    /// Nextcloud user id and local part of the address.
    pub name: String,
    pub address: String,
    pub display_name: String,
    pub state: IdentityState,
    /// Keys of the shared mailboxes granted to this identity.
    pub shared: Vec<String>,
    /// App password the server uses to manage the worker's Nextcloud Mail accounts.
    pub mgmt_token: Option<String>,
    /// App password the agent uses for the Nextcloud MCP server. Kept so it can be revoked.
    pub mcp_token: Option<String>,
    /// The worker's mailbox password. Machine-only: used by Nextcloud Mail and the mail server.
    pub mailbox_password: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// What a caller asks for when creating or re-syncing an identity.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionRequest {
    pub name: String,
    pub display_name: String,
    #[serde(default)]
    pub shared_mailboxes: Vec<String>,
}

/// Result of one convergence step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Created,
    Updated,
    Unchanged,
    Removed,
}

#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub step: String,
    pub outcome: Outcome,
}

/// Credentials returned to the caller when they were just created or changed. Never stored by
/// the caller in plain text, never returned again by the server.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Credentials {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nextcloud_login_password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_app_password: Option<String>,
}

impl Credentials {
    pub fn is_empty(&self) -> bool {
        self.nextcloud_login_password.is_none() && self.mcp_app_password.is_none()
    }
}

/// Public view of an identity. Contains no secrets.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityView {
    pub external_id: String,
    pub name: String,
    pub address: String,
    pub display_name: String,
    pub state: IdentityState,
    pub shared_mailboxes: Vec<String>,
    pub nextcloud_user: String,
    pub created_at: String,
    pub updated_at: String,
}

impl From<&IdentityRecord> for IdentityView {
    fn from(r: &IdentityRecord) -> Self {
        Self {
            external_id: r.external_id.clone(),
            name: r.name.clone(),
            address: r.address.clone(),
            display_name: r.display_name.clone(),
            state: r.state,
            shared_mailboxes: r.shared.clone(),
            nextcloud_user: r.name.clone(),
            created_at: r.created_at.clone(),
            updated_at: r.updated_at.clone(),
        }
    }
}

/// Response of every mutating operation.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationResult {
    pub identity: Option<IdentityView>,
    pub steps: Vec<Step>,
    #[serde(skip_serializing_if = "Credentials::is_empty")]
    pub credentials: Credentials,
}

/// A drift check result for one aspect of an identity.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub check: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Inspection {
    pub identity: IdentityView,
    pub checks: Vec<Check>,
}
