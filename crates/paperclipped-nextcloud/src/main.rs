//! paperclipped-nextcloud: provisions mail + Nextcloud identities for Paperclip agents.

mod api;
mod audit;
mod config;
mod registry;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Context;
use clap::{Parser, Subcommand};
use pcnc_core::model::ProvisionRequest;
use pcnc_core::secret::Sealer;
use pcnc_core::{Engine, EngineError};
use pcnc_nextcloud::bootstrap::{BootstrapConfig, BootstrapState, bootstrap};
use serde_json::json;

use crate::config::{Config, env};
use crate::registry::SqliteRegistry;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Configuration file.
    #[arg(
        long,
        short,
        env = "PCNC_CONFIG",
        default_value = "/etc/paperclipped-nextcloud/config.yaml"
    )]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the HTTP API.
    Serve,
    /// Exit 0 if the API on `server.listen` answers /healthz (for container healthchecks).
    Health,
    /// One-time admin setup: group, sub-admin, team folder, Talk lobby. Idempotent. Needs
    /// PCNC_BOOTSTRAP_ADMIN_USER and PCNC_BOOTSTRAP_ADMIN_PASSWORD (an admin's login password).
    Bootstrap {
        /// Give the existing sub-admin a new password (printed once).
        #[arg(long)]
        reset_subadmin_password: bool,
    },
    /// Create an identity, or converge an existing one.
    Provision {
        external_id: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        display_name: String,
        /// Shared mailbox key to grant; repeat for several.
        #[arg(long = "shared")]
        shared: Vec<String>,
    },
    /// Show one identity.
    Get { external_id: String },
    /// Compare an identity with live state.
    Inspect { external_id: String },
    /// List all identities.
    List,
    /// Disable an identity (keeps data).
    Disable { external_id: String },
    /// Re-enable a disabled identity.
    Enable { external_id: String },
    /// New login password and MCP app password.
    Rotate { external_id: String },
    /// Delete a disabled identity with its data.
    Purge { external_id: String },
    /// Push a shared mailbox's current password to every worker granted it.
    Reshare { key: String },
}

struct Runtime {
    engine: Engine,
    registry: Arc<SqliteRegistry>,
}

fn runtime(cfg: &Config) -> anyhow::Result<Runtime> {
    std::fs::create_dir_all(&cfg.server.state_dir)
        .with_context(|| format!("creating {}", cfg.server.state_dir.display()))?;
    let registry = Arc::new(SqliteRegistry::open(
        &cfg.server.state_dir.join("registry.db"),
    )?);
    let sealer = Sealer::from_base64(&env("PCNC_STATE_KEY")?)?;
    let mail: Arc<dyn pcnc_core::backend::MailBackend> = match cfg.mail.backend {
        config::MailBackendKind::DockerMailserver => {
            let probe = pcnc_dms::probe::ImapProbe {
                endpoint: cfg
                    .mail
                    .probe
                    .clone()
                    .unwrap_or_else(|| cfg.mail.imap.clone()),
                timeout: std::time::Duration::from_secs(cfg.mail.probe_timeout_secs),
                interval: std::time::Duration::from_secs(1),
            };
            Arc::new(
                pcnc_dms::DmsBackend::new(&cfg.mail.config_dir, cfg.mail.mail_dir.clone())?
                    .with_probe(probe),
            )
        }
    };
    let cloud = Arc::new(pcnc_nextcloud::NextcloudBackend::new(
        cfg.nextcloud_config()?,
    )?);
    let engine = Engine::new(cfg.engine_config()?, mail, cloud, registry.clone(), sealer);
    Ok(Runtime { engine, registry })
}

fn print(v: impl serde::Serialize) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(&v)?);
    Ok(())
}

fn audit_for(cfg: &Config) -> audit::Audit {
    audit::Audit::new(cfg.server.state_dir.join("audit.log"))
}

/// Runs a CLI operation, audits it, and turns engine errors into a non-zero exit.
fn finish<T: serde::Serialize>(
    rt: &Runtime,
    audit: &audit::Audit,
    op: &str,
    target: &str,
    steps: impl Fn(&T) -> Vec<pcnc_core::model::Step>,
    r: Result<T, EngineError>,
) -> anyhow::Result<()> {
    use pcnc_core::backend::Registry;
    let now = rt.registry.now();
    match r {
        Ok(v) => {
            audit.record(&now, "cli", op, target, Ok(&steps(&v)));
            print(v)
        }
        Err(e) => {
            audit.record(&now, "cli", op, target, Err(e.code()));
            Err(anyhow::anyhow!("{e:#}"))
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let cfg = Config::load(&cli.config)?;

    if let Command::Bootstrap {
        reset_subadmin_password,
    } = cli.command
    {
        return run_bootstrap(&cfg, reset_subadmin_password).await;
    }
    if let Command::Health = cli.command {
        return health(&cfg.server.listen).await;
    }

    let rt = runtime(&cfg)?;
    let audit = audit_for(&cfg);
    let steps = |r: &pcnc_core::model::OperationResult| r.steps.clone();
    match cli.command {
        Command::Serve => serve(cfg, rt).await,
        Command::Bootstrap { .. } | Command::Health => unreachable!(),
        Command::Provision {
            external_id,
            name,
            display_name,
            shared,
        } => {
            let req = ProvisionRequest {
                name,
                display_name,
                shared_mailboxes: shared,
            };
            let r = rt.engine.provision(&external_id, &req).await;
            finish(&rt, &audit, "provision", &external_id, steps, r)
        }
        Command::Get { external_id } => print(rt.engine.get(&external_id)?),
        Command::Inspect { external_id } => print(rt.engine.inspect(&external_id).await?),
        Command::List => print(rt.engine.list()?),
        Command::Disable { external_id } => {
            let r = rt.engine.disable(&external_id).await;
            finish(&rt, &audit, "disable", &external_id, steps, r)
        }
        Command::Enable { external_id } => {
            let r = rt.engine.enable(&external_id).await;
            finish(&rt, &audit, "enable", &external_id, steps, r)
        }
        Command::Rotate { external_id } => {
            let r = rt.engine.rotate(&external_id).await;
            finish(&rt, &audit, "rotate", &external_id, steps, r)
        }
        Command::Purge { external_id } => {
            let r = rt.engine.purge(&external_id).await;
            finish(&rt, &audit, "purge", &external_id, steps, r)
        }
        Command::Reshare { key } => {
            let r = rt.engine.reshare(&key).await.map(|v| {
                v.into_iter()
                    .map(|(id, outcome)| json!({ "externalId": id, "outcome": outcome }))
                    .collect::<Vec<_>>()
            });
            finish(&rt, &audit, "reshare", &key, |_| Vec::new(), r)
        }
    }
}

async fn run_bootstrap(cfg: &Config, reset_subadmin_password: bool) -> anyhow::Result<()> {
    std::fs::create_dir_all(&cfg.server.state_dir)?;
    let registry = SqliteRegistry::open(&cfg.server.state_dir.join("registry.db"))?;
    let previous: BootstrapState = registry
        .meta_get("bootstrap")?
        .map(|s| serde_json::from_str(&s))
        .transpose()?
        .unwrap_or_default();
    let bcfg = BootstrapConfig {
        url: cfg.nextcloud.url.clone(),
        admin_user: env("PCNC_BOOTSTRAP_ADMIN_USER")?,
        admin_password: env("PCNC_BOOTSTRAP_ADMIN_PASSWORD")?,
        group: cfg.nextcloud.group.clone(),
        subadmin_user: cfg.nextcloud.subadmin_user.clone(),
        team_folder: cfg.bootstrap.team_folder.clone(),
        lobby: cfg.bootstrap.lobby.clone(),
        reset_subadmin_password,
    };
    let report = bootstrap(&bcfg, &previous).await?;
    registry.meta_set("bootstrap", &serde_json::to_string(&report.state)?)?;
    use pcnc_core::backend::Registry;
    audit_for(cfg).record(
        &registry.now(),
        "cli",
        "bootstrap",
        &cfg.nextcloud.group,
        Ok(&report.steps),
    );
    if report.subadmin_password.is_some() {
        eprintln!(
            "Store subadmin_password in your vault and set it as NEXTCLOUD_SUBADMIN_PASSWORD. It is not shown again."
        );
    }
    print(report)
}

async fn serve(cfg: Config, rt: Runtime) -> anyhow::Result<()> {
    let shared: Vec<_> = rt
        .engine
        .shared_mailboxes()
        .into_iter()
        .map(|(key, address, label)| json!({ "key": key, "address": address, "label": label }))
        .collect();
    let bootstrap: serde_json::Value = rt
        .registry
        .meta_get("bootstrap")?
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(json!(null));
    let info = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "domain": cfg.domain,
        "nextcloudUrl": cfg.nextcloud.url,
        "group": cfg.nextcloud.group,
        "mcpUrl": cfg.mcp.url,
        "sharedMailboxes": shared,
        "bootstrap": bootstrap,
    });
    let registry = rt.registry.clone();
    let state = Arc::new(api::AppState {
        engine: rt.engine,
        audit: audit_for(&cfg),
        token: env("PCNC_API_TOKEN")?,
        info,
        now: Box::new(move || {
            use pcnc_core::backend::Registry;
            registry.now()
        }),
        rate: Mutex::new((Instant::now(), 0)),
        rate_limit: cfg.server.rate_limit_per_minute,
    });
    anyhow::ensure!(
        state.token.len() >= 32,
        "PCNC_API_TOKEN must be at least 32 characters"
    );
    let listener = tokio::net::TcpListener::bind(&cfg.server.listen)
        .await
        .with_context(|| format!("binding {}", cfg.server.listen))?;
    tracing::info!(listen = %cfg.server.listen, "paperclipped-nextcloud API listening");
    axum::serve(listener, api::router(state))
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

/// A minimal HTTP/1.0 GET of /healthz, so the image needs no curl.
async fn health(listen: &str) -> anyhow::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addr = listen.replace("0.0.0.0", "127.0.0.1");
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        tokio::net::TcpStream::connect(&addr),
    )
    .await
    .context("timed out connecting")??;
    stream
        .write_all(b"GET /healthz HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .await?;
    let mut buf = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        stream.read_to_end(&mut buf),
    )
    .await
    .context("timed out reading")??;
    anyhow::ensure!(
        buf.starts_with(b"HTTP/1.0 200") || buf.starts_with(b"HTTP/1.1 200"),
        "unhealthy"
    );
    Ok(())
}

async fn shutdown() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = term.recv() => {},
    }
}
