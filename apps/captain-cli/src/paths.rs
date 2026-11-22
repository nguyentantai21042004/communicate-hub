//! Files in a profile's state directory, and the lock that makes one process own a session.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

const LOCK_FILE: &str = "session.lock";
const SOCKET_FILE: &str = "captain.sock";
const STORE_FILE: &str = "inbox.sqlite";
const SCOPE_FILE: &str = "scope.toml";
const POLICY_FILE: &str = "policy.toml";
const RUNNER_CONFIG_FILE: &str = "runner.toml";
const RUNNER_STORE_FILE: &str = "runner.sqlite";
const RUNNER_DIR: &str = "runner";
const UI_TOKEN_FILE: &str = "ui_token";
const RANDOM_SOURCE: &str = "/dev/urandom";
const TOKEN_BYTES: usize = 32;

/// Where one profile keeps its state.
#[derive(Debug, Clone)]
pub struct Paths {
    pub dir: PathBuf,
}

impl Paths {
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_owned(),
        }
    }

    pub fn socket(&self) -> PathBuf {
        self.dir.join(SOCKET_FILE)
    }

    pub fn store(&self) -> PathBuf {
        self.dir.join(STORE_FILE)
    }

    pub fn scope(&self) -> PathBuf {
        self.dir.join(SCOPE_FILE)
    }

    pub fn policy(&self) -> PathBuf {
        self.dir.join(POLICY_FILE)
    }

    pub fn runner_config(&self) -> PathBuf {
        self.dir.join(RUNNER_CONFIG_FILE)
    }

    pub fn runner_store(&self) -> PathBuf {
        self.dir.join(RUNNER_STORE_FILE)
    }

    pub fn runner_dir(&self) -> PathBuf {
        self.dir.join(RUNNER_DIR)
    }

    /// Takes the session lock without waiting. The lock lasts until the returned file drops,
    /// and the kernel releases it if the process dies, so a stale lock cannot exist.
    pub fn lock_session(&self) -> Result<SessionLock> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("cannot create {}", self.dir.display()))?;
        let path = self.dir.join(LOCK_FILE);
        let file =
            File::create(&path).with_context(|| format!("cannot open {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(SessionLock { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => bail!(
                "another captain process owns this session (the daemon?); use it, or stop it first"
            ),
            Err(std::fs::TryLockError::Error(e)) => {
                Err(e).with_context(|| format!("cannot lock {}", path.display()))
            }
        }
    }

    /// The token that opens the web UI, created on first use.
    pub fn ui_token(&self) -> Result<String> {
        let path = self.dir.join(UI_TOKEN_FILE);
        match std::fs::read_to_string(&path) {
            Ok(token) if !token.trim().is_empty() => return Ok(token.trim().to_owned()),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
        let mut bytes = [0u8; TOKEN_BYTES];
        File::open(RANDOM_SOURCE)
            .and_then(|mut f| f.read_exact(&mut bytes))
            .context("cannot read random bytes for the UI token")?;
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        captain_vault::write_private(&path, token.as_bytes())?;
        Ok(token)
    }
}

/// Held while this process owns the session.
#[derive(Debug)]
pub struct SessionLock {
    _file: File,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_lock_fails_until_the_first_drops() {
        let dir = std::env::temp_dir().join(format!("captain-lock-{}", std::process::id()));
        let paths = Paths::new(&dir);
        let held = paths.lock_session().unwrap();
        assert!(paths.lock_session().is_err());
        drop(held);
        assert!(paths.lock_session().is_ok());
        let token = paths.ui_token().unwrap();
        assert_eq!(token.len(), TOKEN_BYTES * 2);
        assert_eq!(paths.ui_token().unwrap(), token);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
