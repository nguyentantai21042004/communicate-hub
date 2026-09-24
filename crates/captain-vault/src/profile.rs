//! Named accounts, so one machine can hold several logged-in Telegram accounts.

use crate::{Error, Result};

/// Validated profile name, used as the prefix of every Keychain entry and as the state
/// directory name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Profile(String);

const PROFILE_MAX_LEN: usize = 32;
const DEFAULT_PROFILE: &str = "default";

impl Profile {
    /// Parses a profile name.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidProfile`] unless the name is 1 to 32 characters of `a-z`, `0-9`, `-`, `_`.
    /// The restriction keeps names safe as path segments and Keychain account names.
    pub fn new(name: &str) -> Result<Self> {
        let ok = !name.is_empty()
            && name.len() <= PROFILE_MAX_LEN
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
        if ok {
            Ok(Self(name.to_owned()))
        } else {
            Err(Error::InvalidProfile(name.to_owned()))
        }
    }

    /// The profile name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for Profile {
    fn default() -> Self {
        Self(DEFAULT_PROFILE.to_owned())
    }
}

impl std::fmt::Display for Profile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_like_names() {
        assert!(Profile::new("work").is_ok());
        assert!(Profile::new("work-2_b").is_ok());
        for bad in ["", "../x", "Work", "a b", &"x".repeat(33)] {
            assert!(Profile::new(bad).is_err(), "{bad:?} should be rejected");
        }
    }
}
