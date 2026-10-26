//! Append-only audit log (JSON lines). Records who asked for what and how it ended; never
//! credentials.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use pcnc_core::model::Step;
use serde_json::json;

pub struct Audit {
    path: PathBuf,
    lock: Mutex<()>,
}

impl Audit {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    pub fn record(
        &self,
        ts: &str,
        source: &str,
        op: &str,
        target: &str,
        result: Result<&[Step], &str>,
    ) {
        let line = match result {
            Ok(steps) => {
                json!({ "ts": ts, "source": source, "op": op, "target": target, "ok": true, "steps": steps })
            }
            Err(code) => {
                json!({ "ts": ts, "source": source, "op": op, "target": target, "ok": false, "error": code })
            }
        };
        tracing::info!(target: "audit", "{line}");
        let _guard = self.lock.lock().unwrap();
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .and_then(|mut f| writeln!(f, "{line}"));
        if let Err(e) = written {
            tracing::error!(path = %self.path.display(), error = %e, "cannot write the audit log");
        }
    }
}
