//! A string that never prints itself.

/// Secret value whose `Debug` and `Display` output is `<redacted>`.
///
/// SECURITY: this stops secrets leaking through logs, error chains and panic messages. Read the
/// value only at the call that needs it, through [`Secret::expose`].
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

const REDACTED: &str = "<redacted>";

impl Secret {
    /// Wraps a secret value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the underlying value.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REDACTED)
    }
}

impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REDACTED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_formats_its_value() {
        let s = Secret::new("hunter2");
        assert_eq!(
            format!("{s} {s:?} {:?}", Some(&s)),
            "<redacted> <redacted> Some(<redacted>)"
        );
        assert_eq!(s.expose(), "hunter2");
    }
}
