//! One-time, idempotent admin setup. Runs with an admin's **login password** that is passed
//! for the run only and never stored. Creates what every worker shares through group
//! membership, so nothing about it is done per worker:
//!
//! 1. the workers' group;
//! 2. the sub-admin user the server runs as, and its sub-admin right on the group;
//! 3. a team folder for the group (Team folders app), or else a folder shared to the group;
//! 4. a Talk lobby owned by the admin, with the group as participant.
//!
//! Folder and lobby are tracked by id, so renaming them in Nextcloud breaks nothing.

use anyhow::Context;
use pcnc_core::model::{Outcome, Step};
use pcnc_core::secret::generate_password;
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};

use crate::client::{Auth, Client, seg};

/// Read, write, create, delete, share.
const ALL_PERMISSIONS: &str = "31";
const ROOM_TYPE_GROUP: &str = "2";

#[derive(Debug, Clone)]
pub struct BootstrapConfig {
    pub url: String,
    pub admin_user: String,
    pub admin_password: String,
    pub group: String,
    pub subadmin_user: String,
    /// Name of the team folder (or shared folder) created for the group.
    pub team_folder: String,
    /// Name of the Talk room created for the group.
    pub lobby: String,
    /// Set a new sub-admin password even if the user already exists.
    pub reset_subadmin_password: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TeamFolder {
    #[default]
    None,
    /// A Team folders (groupfolders) folder.
    Groupfolder { id: i64 },
    /// A folder in the admin's files, shared to the group.
    Share { id: String, path: String },
}

/// What bootstrap created, persisted by the caller between runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BootstrapState {
    #[serde(default)]
    pub team_folder: TeamFolder,
    #[serde(default)]
    pub lobby_token: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BootstrapReport {
    pub steps: Vec<Step>,
    /// Set only when the sub-admin was created or its password reset. Shown once.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subadmin_password: Option<String>,
    pub state: BootstrapState,
}

struct Ctx<'a> {
    client: Client,
    cfg: &'a BootstrapConfig,
    steps: Vec<Step>,
}

impl Ctx<'_> {
    fn admin(&self) -> Auth<'_> {
        Auth {
            user: &self.cfg.admin_user,
            password: &self.cfg.admin_password,
        }
    }

    fn step(&mut self, step: &str, outcome: Outcome) {
        self.steps.push(Step {
            step: step.into(),
            outcome,
        });
    }
}

pub async fn bootstrap(
    cfg: &BootstrapConfig,
    previous: &BootstrapState,
) -> anyhow::Result<BootstrapReport> {
    let mut c = Ctx {
        client: Client::new(&cfg.url)?,
        cfg,
        steps: Vec::new(),
    };
    let mut state = previous.clone();

    // 1. Group.
    let d = c
        .client
        .ocs(
            Method::GET,
            &format!("/ocs/v2.php/cloud/groups?search={}", seg(&cfg.group)),
            &c.admin(),
            &[],
            None,
        )
        .await?
        .into_result("listing groups")?;
    let exists = d["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|g| g.as_str() == Some(cfg.group.as_str()));
    if exists {
        c.step("group", Outcome::Unchanged);
    } else {
        c.client
            .ocs(
                Method::POST,
                "/ocs/v2.php/cloud/groups",
                &c.admin(),
                &[("groupid", &cfg.group)],
                None,
            )
            .await?
            .into_result("creating the group")?;
        c.step("group", Outcome::Created);
    }

    // 2. Sub-admin user and right. It is deliberately not a member of the group.
    let mut subadmin_password = None;
    let user_path = format!("/ocs/v2.php/cloud/users/{}", seg(&cfg.subadmin_user));
    let r = c
        .client
        .ocs(Method::GET, &user_path, &c.admin(), &[], None)
        .await?;
    if r.ok() {
        if cfg.reset_subadmin_password {
            let pw = generate_password();
            c.client
                .ocs(
                    Method::PUT,
                    &user_path,
                    &c.admin(),
                    &[("key", "password"), ("value", &pw)],
                    None,
                )
                .await?
                .into_result("resetting the sub-admin password")?;
            subadmin_password = Some(pw);
            c.step("subadmin_user", Outcome::Updated);
        } else {
            c.step("subadmin_user", Outcome::Unchanged);
        }
    } else {
        let pw = generate_password();
        c.client
            .ocs(
                Method::POST,
                "/ocs/v2.php/cloud/users",
                &c.admin(),
                &[
                    ("userid", &cfg.subadmin_user),
                    ("password", &pw),
                    ("displayName", "paperclipped-nextcloud provisioner"),
                ],
                None,
            )
            .await?
            .into_result("creating the sub-admin user")?;
        subadmin_password = Some(pw);
        c.step("subadmin_user", Outcome::Created);
    }
    let d = c
        .client
        .ocs(
            Method::GET,
            &format!("{user_path}/subadmins"),
            &c.admin(),
            &[],
            None,
        )
        .await?
        .into_result("listing sub-admin groups")?;
    if d.as_array()
        .into_iter()
        .flatten()
        .any(|g| g.as_str() == Some(cfg.group.as_str()))
    {
        c.step("subadmin_right", Outcome::Unchanged);
    } else {
        c.client
            .ocs(
                Method::POST,
                &format!("{user_path}/subadmins"),
                &c.admin(),
                &[("groupid", &cfg.group)],
                None,
            )
            .await?
            .into_result("granting the sub-admin right")?;
        c.step("subadmin_right", Outcome::Created);
    }

    // 3. Team folder, or a shared folder.
    state.team_folder = team_folder(&mut c, &previous.team_folder).await?;

    // 4. Talk lobby.
    state.lobby_token = Some(lobby(&mut c, previous.lobby_token.as_deref()).await?);

    Ok(BootstrapReport {
        steps: c.steps,
        subadmin_password,
        state,
    })
}

async fn app_enabled(c: &Ctx<'_>, app: &str) -> anyhow::Result<bool> {
    let d = c
        .client
        .ocs(
            Method::GET,
            "/ocs/v2.php/cloud/apps?filter=enabled",
            &c.admin(),
            &[],
            None,
        )
        .await?
        .into_result("listing apps")?;
    Ok(d["apps"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|a| a.as_str() == Some(app)))
}

const GF: &str = "/index.php/apps/groupfolders/folders";

async fn team_folder(c: &mut Ctx<'_>, previous: &TeamFolder) -> anyhow::Result<TeamFolder> {
    let group = c.cfg.group.clone();
    if app_enabled(c, "groupfolders").await? {
        // Known folder, or one with the configured name, or a new one.
        let mut id = match previous {
            TeamFolder::Groupfolder { id } => {
                let r = c
                    .client
                    .ocs(Method::GET, &format!("{GF}/{id}"), &c.admin(), &[], None)
                    .await?;
                r.ok().then_some(*id)
            }
            _ => None,
        };
        let mut created = false;
        if id.is_none() {
            let d = c
                .client
                .ocs(Method::GET, GF, &c.admin(), &[], None)
                .await?
                .into_result("listing team folders")?;
            let folders: Vec<&serde_json::Value> = match &d {
                serde_json::Value::Object(m) => m.values().collect(),
                serde_json::Value::Array(a) => a.iter().collect(),
                _ => Vec::new(),
            };
            id = folders
                .iter()
                .find(|f| f["mount_point"].as_str() == Some(c.cfg.team_folder.as_str()))
                .and_then(|f| f["id"].as_i64());
        }
        let id = match id {
            Some(id) => id,
            None => {
                let d = c
                    .client
                    .ocs(
                        Method::POST,
                        GF,
                        &c.admin(),
                        &[("mountpoint", &c.cfg.team_folder)],
                        None,
                    )
                    .await?
                    .into_result("creating the team folder")?;
                created = true;
                d["id"].as_i64().context("no id for the new team folder")?
            }
        };
        let d = c
            .client
            .ocs(Method::GET, &format!("{GF}/{id}"), &c.admin(), &[], None)
            .await?
            .into_result("reading the team folder")?;
        let perms = d["groups"][&group]["permissions"]
            .as_i64()
            .or_else(|| d["groups"][&group].as_i64());
        let mut changed = false;
        if perms.is_none() {
            c.client
                .ocs(
                    Method::POST,
                    &format!("{GF}/{id}/groups"),
                    &c.admin(),
                    &[("group", &group)],
                    None,
                )
                .await?
                .into_result("assigning the team folder to the group")?;
            changed = true;
        }
        if perms != Some(31) {
            c.client
                .ocs(
                    Method::POST,
                    &format!("{GF}/{id}/groups/{}", seg(&group)),
                    &c.admin(),
                    &[("permissions", ALL_PERMISSIONS)],
                    None,
                )
                .await?
                .into_result("setting team folder permissions")?;
            changed = true;
        }
        c.step(
            "team_folder",
            if created {
                Outcome::Created
            } else if changed {
                Outcome::Updated
            } else {
                Outcome::Unchanged
            },
        );
        return Ok(TeamFolder::Groupfolder { id });
    }

    // No Team folders app: a folder in the admin's files, shared to the group.
    const SHARES: &str = "/ocs/v2.php/apps/files_sharing/api/v1/shares";
    if let TeamFolder::Share { id, path } = previous {
        let r = c
            .client
            .ocs(
                Method::GET,
                &format!("{SHARES}/{}", seg(id)),
                &c.admin(),
                &[],
                None,
            )
            .await?;
        if r.ok() {
            c.step("team_folder", Outcome::Unchanged);
            return Ok(TeamFolder::Share {
                id: id.clone(),
                path: path.clone(),
            });
        }
    }
    let path = format!("/{}", c.cfg.team_folder);
    let dav = format!(
        "/remote.php/dav/files/{}/{}",
        seg(&c.cfg.admin_user),
        seg(&c.cfg.team_folder)
    );
    let status = c.client.dav("PROPFIND", &dav, &c.admin()).await?;
    if status == StatusCode::NOT_FOUND {
        let s = c.client.dav("MKCOL", &dav, &c.admin()).await?;
        anyhow::ensure!(s.is_success(), "creating folder {path}: HTTP {s}");
    }
    let d = c
        .client
        .ocs(
            Method::GET,
            &format!("{SHARES}?path={}", seg(&path)),
            &c.admin(),
            &[],
            None,
        )
        .await?
        .into_result("listing shares")?;
    let existing = d.as_array().into_iter().flatten().find(|s| {
        s["share_type"].as_i64() == Some(1) && s["share_with"].as_str() == Some(group.as_str())
    });
    if let Some(s) = existing {
        c.step("team_folder", Outcome::Unchanged);
        return Ok(TeamFolder::Share {
            id: s["id"].to_string().trim_matches('"').to_string(),
            path,
        });
    }
    let d = c
        .client
        .ocs(
            Method::POST,
            SHARES,
            &c.admin(),
            &[
                ("path", &path),
                ("shareType", "1"),
                ("shareWith", &group),
                ("permissions", ALL_PERMISSIONS),
            ],
            None,
        )
        .await?
        .into_result("sharing the folder with the group")?;
    c.step("team_folder", Outcome::Created);
    Ok(TeamFolder::Share {
        id: d["id"].to_string().trim_matches('"').to_string(),
        path,
    })
}

const ROOMS: &str = "/ocs/v2.php/apps/spreed/api/v4/room";

async fn lobby(c: &mut Ctx<'_>, previous: Option<&str>) -> anyhow::Result<String> {
    if let Some(token) = previous {
        let r = c
            .client
            .ocs(
                Method::GET,
                &format!("{ROOMS}/{}", seg(token)),
                &c.admin(),
                &[],
                None,
            )
            .await?;
        if r.ok() {
            c.step("lobby", Outcome::Unchanged);
            return Ok(token.to_string());
        }
    }
    let d = c
        .client
        .ocs(Method::GET, ROOMS, &c.admin(), &[], None)
        .await?
        .into_result("listing Talk rooms")?;
    let adopt = d.as_array().into_iter().flatten().find(|r| {
        r["type"].as_i64() == Some(2) && r["name"].as_str() == Some(c.cfg.lobby.as_str())
    });
    if let Some(r) = adopt {
        c.step("lobby", Outcome::Unchanged);
        return r["token"]
            .as_str()
            .map(String::from)
            .context("room without token");
    }
    let d = c
        .client
        .ocs(
            Method::POST,
            ROOMS,
            &c.admin(),
            &[
                ("roomType", ROOM_TYPE_GROUP),
                ("invite", &c.cfg.group),
                ("source", "groups"),
                ("roomName", &c.cfg.lobby),
            ],
            None,
        )
        .await?
        .into_result("creating the Talk lobby")?;
    c.step("lobby", Outcome::Created);
    d["token"]
        .as_str()
        .map(String::from)
        .context("no token for the new room")
}
