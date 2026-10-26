//! docker-mailserver (DMS) backend. DMS has no API; its `setup` CLI edits files in its config
//! volume, and its change detector applies them within seconds. This backend edits the same
//! files the same way, so it only needs that volume mounted:
//!
//! - `postfix-accounts.cf`: `address|{SHA512-CRYPT}$6$…` per mailbox;
//! - `postfix-virtual.cf`: aliases (`source target[,target…]`), read only;
//! - `dovecot-quotas.cf`: `address:quota`, cleaned up on delete.
//!
//! Writes replace a file atomically (temp file + rename) while holding the lock files DMS's own
//! account scripts use, so a concurrent `setup email …` cannot interleave.

pub mod probe;

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, bail};
use async_trait::async_trait;
use pcnc_core::backend::{Change, MailBackend};
use sha_crypt::{PasswordHashRef, PasswordHasher, PasswordVerifier, ShaCrypt};

const ACCOUNTS: &str = "postfix-accounts.cf";
const VIRTUAL: &str = "postfix-virtual.cf";
const QUOTAS: &str = "dovecot-quotas.cf";
const SCHEME: &str = "{SHA512-CRYPT}";
/// The scripts behind `setup email add|update|del`. DMS locks per script name.
const LOCKS: &[&str] = &[
    "addmailuser.lock",
    "updatemailuser.lock",
    "delmailuser.lock",
];
/// DMS treats a lock older than this as left behind by a crash.
const STALE_LOCK: Duration = Duration::from_secs(60);

pub struct DmsBackend {
    /// Waits for DMS to apply a change before callers use the password; see [`probe`].
    probe: Option<probe::ImapProbe>,
    config_dir: PathBuf,
    /// DMS's `/var/mail`, if mounted; needed only to purge stored mail.
    mail_dir: Option<PathBuf>,
    lock_poll: Duration,
}

impl DmsBackend {
    pub fn new(config_dir: impl Into<PathBuf>, mail_dir: Option<PathBuf>) -> anyhow::Result<Self> {
        let config_dir = config_dir.into();
        if !config_dir.join(ACCOUNTS).is_file() {
            bail!(
                "{} not found; is the DMS config volume mounted?",
                config_dir.join(ACCOUNTS).display()
            );
        }
        Ok(Self {
            probe: None,
            config_dir,
            mail_dir,
            lock_poll: Duration::from_secs(1),
        })
    }

    /// Probe IMAP after each password change until DMS accepts it.
    pub fn with_probe(mut self, probe: probe::ImapProbe) -> Self {
        self.probe = Some(probe);
        self
    }

    fn read(&self, file: &str) -> anyhow::Result<String> {
        let path = self.config_dir.join(file);
        match fs::read_to_string(&path) {
            Ok(s) => Ok(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Replaces `file` atomically, keeping the original's permissions.
    fn write(&self, file: &str, content: &str) -> anyhow::Result<()> {
        let path = self.config_dir.join(file);
        let tmp = self
            .config_dir
            .join(format!(".{file}.pcnc-{}", uuid::Uuid::new_v4()));
        let mode = fs::metadata(&path)
            .map(|m| m.permissions().mode())
            .unwrap_or(0o644);
        let result = (|| {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(content.as_bytes())?;
            f.sync_all()?;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
            fs::rename(&tmp, &path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result.with_context(|| format!("writing {}", path.display()))
    }

    async fn lock(&self) -> anyhow::Result<LockGuard> {
        let id = uuid::Uuid::new_v4().to_string();
        let mut held = Vec::new();
        for name in LOCKS {
            let path = self.config_dir.join(name);
            let deadline = tokio::time::Instant::now() + STALE_LOCK * 2;
            loop {
                match fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                {
                    Ok(mut f) => {
                        f.write_all(id.as_bytes())?;
                        held.push(path);
                        break;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        if is_stale(&path) {
                            tracing::warn!(lock = %path.display(), "removing stale DMS lock");
                            let _ = fs::remove_file(&path);
                            continue;
                        }
                        if tokio::time::Instant::now() > deadline {
                            release(&held, &id);
                            bail!("timed out waiting for DMS lock {}", path.display());
                        }
                        tokio::time::sleep(self.lock_poll).await;
                    }
                    Err(e) => {
                        release(&held, &id);
                        return Err(e).with_context(|| format!("creating lock {}", path.display()));
                    }
                }
            }
        }
        Ok(LockGuard { held, id })
    }
}

fn is_stale(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age > STALE_LOCK)
}

/// Removes only locks that still carry our id, as DMS's own `_remove_lock` does.
fn release(held: &[PathBuf], id: &str) {
    for path in held {
        if fs::read_to_string(path).is_ok_and(|c| c.trim() == id) {
            let _ = fs::remove_file(path);
        }
    }
}

struct LockGuard {
    held: Vec<PathBuf>,
    id: String,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        release(&self.held, &self.id);
    }
}

/// The address an account or quota line is keyed by, lowercased.
fn account_key(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    line.split('|')
        .next()
        .map(|a| a.trim().to_ascii_lowercase())
}

fn find_hash(accounts: &str, address: &str) -> Option<String> {
    accounts
        .lines()
        .find(|l| account_key(l).as_deref() == Some(address))
        .and_then(|l| {
            let hash = l.trim().split('|').nth(1)?;
            Some(hash.strip_prefix(SCHEME).unwrap_or(hash).to_string())
        })
}

/// crypt(3) uses at most 16 salt characters. The crate's default salt is 16 random *bytes*,
/// which encode to 22 characters: such hashes verify in Rust but not in Dovecot. 12 bytes
/// encode to exactly 16 characters.
const SALT_BYTES: usize = 12;

fn hash(password: &str) -> anyhow::Result<String> {
    let mut salt = [0u8; SALT_BYTES];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut salt);
    let h = ShaCrypt::SHA512
        .hash_password_with_salt(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("hashing: {e}"))?;
    Ok(format!("{SCHEME}{}", h.as_str()))
}

fn verifies(hash: &str, password: &str) -> bool {
    PasswordHashRef::new(hash).ok().is_some_and(|h| {
        ShaCrypt::SHA512
            .verify_password(password.as_bytes(), h)
            .is_ok()
    })
}

fn with_trailing_newline(mut s: String) -> String {
    if !s.is_empty() && !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

#[async_trait]
impl MailBackend for DmsBackend {
    async fn address_in_use(&self, address: &str) -> anyhow::Result<bool> {
        let address = address.to_ascii_lowercase();
        if find_hash(&self.read(ACCOUNTS)?, &address).is_some() {
            return Ok(true);
        }
        let virt = self.read(VIRTUAL)?;
        Ok(virt.lines().any(|l| {
            let l = l.trim();
            !l.starts_with('#')
                && l.split_whitespace()
                    .next()
                    .is_some_and(|src| src.eq_ignore_ascii_case(&address))
        }))
    }

    async fn verify(&self, address: &str, password: &str) -> anyhow::Result<Option<bool>> {
        let address = address.to_ascii_lowercase();
        Ok(find_hash(&self.read(ACCOUNTS)?, &address).map(|h| verifies(&h, password)))
    }

    async fn set_password(&self, address: &str, password: &str) -> anyhow::Result<Change> {
        let address = address.to_ascii_lowercase();
        let line = format!("{address}|{}", hash(password)?);
        let _lock = self.lock().await?;
        let accounts = self.read(ACCOUNTS)?;
        let mut found = false;
        let mut out: Vec<String> = accounts
            .lines()
            .map(|l| {
                if account_key(l).as_deref() == Some(address.as_str()) {
                    found = true;
                    line.clone()
                } else {
                    l.to_string()
                }
            })
            .collect();
        if !found {
            out.push(line);
        }
        self.write(ACCOUNTS, &with_trailing_newline(out.join("\n")))?;
        Ok(if found {
            Change::Updated
        } else {
            Change::Created
        })
    }

    async fn delete(&self, address: &str, purge_mail: bool) -> anyhow::Result<Change> {
        let address = address.to_ascii_lowercase();
        let _lock = self.lock().await?;
        let accounts = self.read(ACCOUNTS)?;
        let keep: Vec<&str> = accounts
            .lines()
            .filter(|l| account_key(l).as_deref() != Some(address.as_str()))
            .collect();
        let existed = keep.len() != accounts.lines().count();
        if existed {
            self.write(ACCOUNTS, &with_trailing_newline(keep.join("\n")))?;
        }
        let quotas = self.read(QUOTAS)?;
        let quota_key = |l: &str| l.split(':').next().map(|a| a.trim().to_ascii_lowercase());
        if quotas
            .lines()
            .any(|l| quota_key(l).as_deref() == Some(address.as_str()))
        {
            let keep: Vec<&str> = quotas
                .lines()
                .filter(|l| quota_key(l).as_deref() != Some(address.as_str()))
                .collect();
            self.write(QUOTAS, &with_trailing_newline(keep.join("\n")))?;
        }
        let mut purged = false;
        if purge_mail {
            if let (Some(root), Some((local, domain))) = (&self.mail_dir, address.split_once('@')) {
                let dir = root.join(domain).join(local);
                if dir.is_dir() {
                    fs::remove_dir_all(&dir)
                        .with_context(|| format!("removing {}", dir.display()))?;
                    purged = true;
                }
            }
        }
        Ok(if existed || purged {
            Change::Removed
        } else {
            Change::Unchanged
        })
    }

    async fn wait_until_login(&self, address: &str, password: &str) -> anyhow::Result<()> {
        match &self.probe {
            Some(p) => p.wait(address, password).await,
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, DmsBackend) {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(ACCOUNTS),
            "postmaster@example.test|{SHA512-CRYPT}$6$x$y\n",
        )
        .unwrap();
        fs::write(
            dir.path().join(VIRTUAL),
            "# aliases\ninfo@example.test contact@example.test\n",
        )
        .unwrap();
        fs::write(dir.path().join(QUOTAS), "ada@example.test:5G\n").unwrap();
        fs::create_dir_all(dir.path().join("mail/example.test/ada/cur")).unwrap();
        let mut b = DmsBackend::new(dir.path(), Some(dir.path().join("mail"))).unwrap();
        b.lock_poll = Duration::from_millis(10);
        (dir, b)
    }

    #[tokio::test]
    async fn create_verify_update_delete() {
        let (dir, b) = setup();
        assert_eq!(b.verify("ada@example.test", "pw1").await.unwrap(), None);
        assert_eq!(
            b.set_password("Ada@example.test", "pw1").await.unwrap(),
            Change::Created
        );
        assert_eq!(
            b.verify("ada@example.test", "pw1").await.unwrap(),
            Some(true)
        );
        assert_eq!(
            b.verify("ada@example.test", "nope").await.unwrap(),
            Some(false)
        );
        assert_eq!(
            b.set_password("ada@example.test", "pw2").await.unwrap(),
            Change::Updated
        );
        assert_eq!(
            b.verify("ada@example.test", "pw2").await.unwrap(),
            Some(true)
        );

        let content = fs::read_to_string(dir.path().join(ACCOUNTS)).unwrap();
        assert_eq!(content.lines().count(), 2, "{content}");
        assert!(
            content
                .lines()
                .nth(1)
                .unwrap()
                .starts_with("ada@example.test|{SHA512-CRYPT}$6$")
        );
        assert!(
            content.starts_with("postmaster@example.test|"),
            "other lines untouched"
        );

        assert_eq!(
            b.delete("ada@example.test", true).await.unwrap(),
            Change::Removed
        );
        assert_eq!(b.verify("ada@example.test", "pw2").await.unwrap(), None);
        assert!(!dir.path().join("mail/example.test/ada").exists());
        assert!(
            fs::read_to_string(dir.path().join(QUOTAS))
                .unwrap()
                .trim()
                .is_empty()
        );
        assert_eq!(
            b.delete("ada@example.test", true).await.unwrap(),
            Change::Unchanged
        );
        for l in LOCKS {
            assert!(!dir.path().join(l).exists(), "lock {l} released");
        }
    }

    #[tokio::test]
    async fn aliases_and_accounts_are_in_use() {
        let (_dir, b) = setup();
        assert!(b.address_in_use("info@example.test").await.unwrap());
        assert!(b.address_in_use("POSTMASTER@example.test").await.unwrap());
        assert!(!b.address_in_use("ada@example.test").await.unwrap());
    }

    #[tokio::test]
    async fn waits_for_a_held_lock_and_clears_a_stale_one() {
        let (dir, b) = setup();
        let lock = dir.path().join("updatemailuser.lock");
        fs::write(&lock, "someone-else").unwrap();
        let path = lock.clone();
        let releaser = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            fs::remove_file(path).unwrap();
        });
        b.set_password("ada@example.test", "pw").await.unwrap();
        releaser.await.unwrap();

        fs::write(&lock, "crashed").unwrap();
        let old = SystemTime::now() - Duration::from_secs(120);
        fs::File::options()
            .write(true)
            .open(&lock)
            .unwrap()
            .set_modified(old)
            .unwrap();
        b.set_password("ada@example.test", "pw2").await.unwrap();
        assert!(!lock.exists());
    }

    #[test]
    fn hashes_are_crypt3_compatible() {
        // Produced by busybox `mkpasswd -m sha512 postmaster-spike-pw` and accepted by DMS's
        // Dovecot in the P0 spike.
        let busybox = "$6$753bG6DvpjDdOvrg$NTMJ7r1QC4nVvFQxf2qUzKNt3Yziaza2GcEBczoJuj4hsXsb9l.jlEp/v19/i547OkEGDYj3bYaapaPw/c9AT1";
        assert!(verifies(busybox, "postmaster-spike-pw"));
        assert!(!verifies(busybox, "wrong"));
        assert!(!verifies("not-a-hash", "pw"));
        // Verified by Dovecot 2.4.1 (`doveadm pw -t`) in the spike; 16-character salt.
        let ours = "$6$rounds=5000$zwtzmK24pcgGE0Lt$AURzQlYO8X32q96/fGvy0PXmJeBOdOI2fm5tH2kw9TWHaQVhfnYDpcrl5DVaMEEmqm3iL9P9ALfeziRtOZdS.0";
        assert!(verifies(ours, "testpw"));

        let h = hash("pw").unwrap();
        assert!(h.starts_with("{SHA512-CRYPT}$6$"));
        let salt = h
            .trim_start_matches("{SHA512-CRYPT}$6$")
            .trim_start_matches("rounds=5000$")
            .split('$')
            .next()
            .unwrap();
        assert_eq!(
            salt.len(),
            16,
            "crypt(3) reads at most 16 salt characters: {h}"
        );
        assert!(verifies(h.strip_prefix(SCHEME).unwrap(), "pw"));
    }
}
