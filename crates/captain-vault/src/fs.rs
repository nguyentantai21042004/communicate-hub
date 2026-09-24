//! Owner-only, crash-safe file writes for local state.

use std::io::Write;
use std::path::Path;

use crate::{Error, Result};

const PRIVATE_FILE_MODE: u32 = 0o600;
const PRIVATE_DIR_MODE: u32 = 0o700;
const TEMP_SUFFIX: &str = ".tmp";

/// Writes `bytes` to `path` with mode `0600`, replacing any previous file atomically.
///
/// The parent directory is created with mode `0700` if missing. Readers see either the old file
/// or the new one, never a partial write.
///
/// # Errors
///
/// [`Error::Io`] when the directory or file cannot be created, written, or renamed.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

    let io = |source| Error::Io {
        path: path.to_owned(),
        source,
    };
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(PRIVATE_DIR_MODE)
            .create(dir)
            .map_err(io)?;
    }
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(TEMP_SUFFIX);
    let tmp = Path::new(&tmp_name);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(PRIVATE_FILE_MODE)
        .open(tmp)
        .map_err(io)?;
    f.write_all(bytes).map_err(io)?;
    f.sync_all().map_err(io)?;
    std::fs::rename(tmp, path).map_err(io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn writes_owner_only_and_replaces() {
        let dir = std::env::temp_dir().join(format!("captain-vault-{}", std::process::id()));
        let path = dir.join("nested/state.json");
        write_private(&path, b"one").unwrap();
        write_private(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, PRIVATE_FILE_MODE);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
