//! Health of a hub module, and the login state of a source account.

use serde::{Deserialize, Serialize};

/// Snapshot a dashboard can show for one module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleHealth {
    /// Module name, for example `zalo`, `tgbot`, or `bridge`.
    pub module: String,
    /// Whether the module can do its job right now.
    pub state: ModuleState,
    /// Log-safe detail. Empty when there is nothing to add.
    pub detail: String,
    /// When this snapshot was taken, as Unix seconds.
    pub updated_at_unix_secs: i64,
}

/// Coarse health of one module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleState {
    /// The module is serving requests.
    Ok,
    /// The module is serving some requests and failing others.
    Degraded,
    /// The module is not serving requests.
    Down,
    /// The module has not reported yet.
    Unknown,
}

/// Login state of a source account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// The source accepts calls for this account.
    Valid,
    /// The source rejected the stored credentials.
    Expired,
    /// A person logged the account out.
    LoggedOut,
    /// The adapter is signing in and cannot send yet.
    Connecting,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_health_and_session_state_round_trip() -> serde_json::Result<()> {
        let health = ModuleHealth {
            module: "zalo".into(),
            state: ModuleState::Degraded,
            detail: "sidecar restarted".into(),
            updated_at_unix_secs: 1_700_000_000,
        };
        let json = serde_json::to_string(&health)?;
        assert_eq!(
            json,
            concat!(
                r#"{"module":"zalo","state":"degraded","detail":"sidecar restarted","#,
                r#""updated_at_unix_secs":1700000000}"#
            )
        );
        assert_eq!(serde_json::from_str::<ModuleHealth>(&json)?, health);

        let states = [
            (SessionState::Valid, "\"valid\""),
            (SessionState::Expired, "\"expired\""),
            (SessionState::LoggedOut, "\"logged_out\""),
            (SessionState::Connecting, "\"connecting\""),
        ];
        for (state, wire) in states {
            assert_eq!(serde_json::to_string(&state)?, wire);
            assert_eq!(serde_json::from_str::<SessionState>(wire)?, state);
        }

        let module_states = [
            (ModuleState::Ok, "\"ok\""),
            (ModuleState::Degraded, "\"degraded\""),
            (ModuleState::Down, "\"down\""),
            (ModuleState::Unknown, "\"unknown\""),
        ];
        for (state, wire) in module_states {
            assert_eq!(serde_json::to_string(&state)?, wire);
        }
        Ok(())
    }
}
