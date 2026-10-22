//! Deletes Zalo downloads that have sat in the media directory past [`MEDIA_RETENTION`](crate::MEDIA_RETENTION).

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::error::{Error, Result};
use crate::limits::MEDIA_RETENTION;

/// Deletes regular files directly inside `media_dir` whose mtime is older than `retention`.
///
/// Symlinks are left alone. Subdirectories are not entered. A missing directory deletes nothing.
///
/// # Errors
///
/// Returns [`Error::Io`] when the directory cannot be read. A file that disappears during the
/// walk is skipped.
pub fn cleanup_expired_media(
    media_dir: &Path,
    now: SystemTime,
    retention: Duration,
) -> Result<u32> {
    let meta = match fs::symlink_metadata(media_dir) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(Error::Io(err)),
    };
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Ok(0);
    }
    let entries = fs::read_dir(media_dir).map_err(Error::Io)?;
    let mut removed = 0u32;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let file_meta = match fs::symlink_metadata(entry.path()) {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        if !file_meta.file_type().is_file() {
            continue;
        }
        let modified = match file_meta.modified() {
            Ok(modified) => modified,
            Err(_) => continue,
        };
        let Some(age) = now.duration_since(modified).ok() else {
            continue;
        };
        if age <= retention {
            continue;
        }
        if fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// Deletes media older than [`MEDIA_RETENTION`].
///
/// # Errors
///
/// See [`cleanup_expired_media`].
pub fn cleanup_media_dir(media_dir: &Path) -> Result<u32> {
    cleanup_expired_media(media_dir, SystemTime::now(), MEDIA_RETENTION)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, SystemTime};

    use super::*;

    #[test]
    fn removes_old_regular_files_only() {
        let dir = std::env::temp_dir().join(format!("hub-zalo-media-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let old = dir.join("old.bin");
        let fresh = dir.join("fresh.bin");
        let nested = dir.join("nested");
        fs::write(&old, b"old").unwrap();
        fs::write(&fresh, b"new").unwrap();
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("kept.bin"), b"x").unwrap();
        let past = SystemTime::now() - Duration::from_secs(60);
        fs::File::open(&old).unwrap().set_modified(past).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink("fresh.bin", dir.join("link")).unwrap();
        }

        let removed =
            cleanup_expired_media(&dir, SystemTime::now(), Duration::from_secs(30)).unwrap();
        assert_eq!(removed, 1);
        assert!(!old.exists());
        assert!(fresh.exists());
        assert!(nested.join("kept.bin").exists());
        #[cfg(unix)]
        assert!(dir.join("link").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
