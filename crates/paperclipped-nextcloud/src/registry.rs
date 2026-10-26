//! SQLite registry of the identities this server manages, plus bootstrap state.

use std::path::Path;
use std::sync::Mutex;

use anyhow::Context;
use pcnc_core::backend::Registry;
use pcnc_core::model::{IdentityRecord, IdentityState};
use rusqlite::{Connection, OptionalExtension, Row, params};

pub struct SqliteRegistry {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS identities (
    external_id      TEXT PRIMARY KEY,
    name             TEXT NOT NULL UNIQUE,
    address          TEXT NOT NULL UNIQUE,
    display_name     TEXT NOT NULL,
    state            TEXT NOT NULL,
    shared           TEXT NOT NULL DEFAULT '[]',
    mgmt_token       TEXT,
    mcp_token        TEXT,
    mailbox_password TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
PRAGMA user_version = 1;
";

const COLUMNS: &str = "external_id, name, address, display_name, state, shared, mgmt_token, mcp_token, mailbox_password, created_at, updated_at";

fn row(r: &Row<'_>) -> rusqlite::Result<IdentityRecord> {
    let state: String = r.get(4)?;
    let shared: String = r.get(5)?;
    Ok(IdentityRecord {
        external_id: r.get(0)?,
        name: r.get(1)?,
        address: r.get(2)?,
        display_name: r.get(3)?,
        state: IdentityState::parse(&state).unwrap_or(IdentityState::Provisioning),
        shared: serde_json::from_str(&shared).unwrap_or_default(),
        mgmt_token: r.get(6)?,
        mcp_token: r.get(7)?,
        mailbox_password: r.get(8)?,
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
    })
}

impl SqliteRegistry {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn meta_get(&self, key: &str) -> anyhow::Result<Option<String>> {
        let c = self.conn.lock().unwrap();
        Ok(
            c.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
                .optional()?,
        )
    }

    pub fn meta_set(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute("INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", [key, value])?;
        Ok(())
    }

    fn one(&self, sql_where: &str, arg: &str) -> anyhow::Result<Option<IdentityRecord>> {
        let c = self.conn.lock().unwrap();
        Ok(c.query_row(
            &format!("SELECT {COLUMNS} FROM identities WHERE {sql_where} = ?1"),
            [arg],
            row,
        )
        .optional()?)
    }
}

impl Registry for SqliteRegistry {
    fn get(&self, external_id: &str) -> anyhow::Result<Option<IdentityRecord>> {
        self.one("external_id", external_id)
    }

    fn get_by_name(&self, name: &str) -> anyhow::Result<Option<IdentityRecord>> {
        self.one("name", name)
    }

    fn put(&self, r: &IdentityRecord) -> anyhow::Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            &format!(
                "INSERT INTO identities ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(external_id) DO UPDATE SET
                   name = excluded.name, address = excluded.address, display_name = excluded.display_name,
                   state = excluded.state, shared = excluded.shared, mgmt_token = excluded.mgmt_token,
                   mcp_token = excluded.mcp_token, mailbox_password = excluded.mailbox_password,
                   updated_at = excluded.updated_at"
            ),
            params![
                r.external_id,
                r.name,
                r.address,
                r.display_name,
                r.state.as_str(),
                serde_json::to_string(&r.shared)?,
                r.mgmt_token,
                r.mcp_token,
                r.mailbox_password,
                r.created_at,
                r.updated_at,
            ],
        )?;
        Ok(())
    }

    fn delete(&self, external_id: &str) -> anyhow::Result<()> {
        self.conn.lock().unwrap().execute(
            "DELETE FROM identities WHERE external_id = ?1",
            [external_id],
        )?;
        Ok(())
    }

    fn list(&self) -> anyhow::Result<Vec<IdentityRecord>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(&format!("SELECT {COLUMNS} FROM identities ORDER BY name"))?;
        let rows = stmt
            .query_map([], row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn now(&self) -> String {
        let c = self.conn.lock().unwrap();
        c.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now')", [], |r| {
            r.get(0)
        })
        .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let reg = SqliteRegistry::open(&dir.path().join("r.db")).unwrap();
        let now = reg.now();
        assert_eq!(now.len(), 20);
        let mut rec = IdentityRecord {
            external_id: "a1".into(),
            name: "ada".into(),
            address: "ada@example.test".into(),
            display_name: "Ada".into(),
            state: IdentityState::Provisioning,
            shared: vec!["contact".into()],
            mgmt_token: Some("sealed".into()),
            mcp_token: None,
            mailbox_password: None,
            created_at: now.clone(),
            updated_at: now,
        };
        reg.put(&rec).unwrap();
        rec.state = IdentityState::Active;
        reg.put(&rec).unwrap();
        assert_eq!(reg.get("a1").unwrap().unwrap(), rec);
        assert_eq!(reg.get_by_name("ada").unwrap().unwrap().external_id, "a1");
        assert_eq!(reg.list().unwrap().len(), 1);
        reg.delete("a1").unwrap();
        assert!(reg.get("a1").unwrap().is_none());

        reg.meta_set("k", "v1").unwrap();
        reg.meta_set("k", "v2").unwrap();
        assert_eq!(reg.meta_get("k").unwrap().as_deref(), Some("v2"));
    }
}
