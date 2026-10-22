//! `hub.toml` in the profile state directory.
//!
//! The bot token is not a field here. It lives in the Keychain.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use hub_bridge::{BACKFILL_RENAME_CAP, BACKFILL_RENAME_GAP_SECS};

/// File name inside the state directory.
pub const HUB_FILE: &str = "hub.toml";

const DEFAULT_PROGRAM: &str = "node";
const DEFAULT_EVENT_RETENTION_DAYS: i64 = 30;
const DEFAULT_MEDIA_RETENTION_DAYS: i64 = 7;
const SIDECAR_SCRIPT: &str = "sidecars/zalo-node/dist/main.js";
const FORBIDDEN_TOKEN_FIELD: &str = "bot_token";

/// Bridge settings loaded from [`HUB_FILE`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubConfig {
    /// Forum supergroup the bot posts into. Negative.
    pub group_id: i64,
    /// Topic for session lines. Absent means those lines are only logged.
    pub ops_topic_id: Option<i32>,
    /// Telegram user ids allowed to run `/login` and `/recall`.
    pub admin_user_ids: Vec<i64>,
    /// Drop events for conversations whose `muted` flag is set.
    pub skip_muted_zalo_groups: bool,
    /// Sidecar executable, usually `node`.
    pub sidecar_program: String,
    /// Arguments. The default is the repo's `dist/main.js` when that file is found.
    pub sidecar_args: Vec<String>,
    /// Days of `module_events` and `bridge_events` to keep.
    pub event_retention_days: i64,
    /// Days of downloaded Zalo media to keep.
    pub media_retention_days: i64,
    /// Pin one avatar photo the first time a topic is created.
    pub avatar_card: bool,
    /// Placeholder topics one directory pass renames. The rest wait for the next pass.
    pub backfill_rename_cap: usize,
    /// Seconds between those renames. Zero waits nothing. The wait ends with the pass.
    pub backfill_rename_gap_secs: i64,
}

impl HubConfig {
    /// Reads `hub.toml` from `state_dir`.
    ///
    /// # Errors
    ///
    /// Returns an error whose text starts with `hub.toml:` and names the field.
    pub fn load(state_dir: &Path) -> Result<Self> {
        let path = state_dir.join(HUB_FILE);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("hub.toml: cannot read {}", path.display()))?;
        Self::parse(&text)
    }

    /// Parses the file body.
    ///
    /// # Errors
    ///
    /// Same as [`Self::load`]. A `bot_token` key is rejected without copying its value
    /// into the error.
    pub fn parse(text: &str) -> Result<Self> {
        let value: toml::Value =
            toml::from_str(text).map_err(|_| anyhow::anyhow!("hub.toml: cannot parse the file"))?;
        let Some(table) = value.as_table() else {
            bail!("hub.toml: the file must be a table");
        };
        if table.contains_key(FORBIDDEN_TOKEN_FIELD) {
            bail!("hub.toml: bot_token is not a config field");
        }
        let group_id = required_i64(table, "group_id")?;
        if group_id >= 0 {
            bail!("hub.toml: group_id must be a negative supergroup id");
        }
        let ops_topic_id = match optional_i64(table, "ops_topic_id")? {
            Some(id) => {
                let id = i32::try_from(id)
                    .map_err(|_| anyhow::anyhow!("hub.toml: ops_topic_id is out of range"))?;
                if id <= 0 {
                    bail!("hub.toml: ops_topic_id must be positive");
                }
                Some(id)
            }
            None => None,
        };
        let admin_user_ids = int_array(table, "admin_user_ids")?;
        let skip_muted_zalo_groups = bool_field(table, "skip_muted_zalo_groups", false)?;
        let avatar_card = bool_field(table, "avatar_card", true)?;
        let backfill_rename_cap = rename_cap(table)?;
        let backfill_rename_gap_secs = rename_gap(table)?;
        let sidecar_program = match table.get("sidecar_program") {
            None => DEFAULT_PROGRAM.to_owned(),
            Some(toml::Value::String(value)) if !value.is_empty() => value.clone(),
            Some(_) => bail!("hub.toml: sidecar_program must be a program name"),
        };
        let sidecar_args = match table.get("sidecar_args") {
            None => {
                let script = discover_sidecar().ok_or_else(|| {
                    anyhow::anyhow!(
                        "hub.toml: sidecar_args is required when {SIDECAR_SCRIPT} is not found"
                    )
                })?;
                vec![script.to_string_lossy().into_owned()]
            }
            Some(value) => string_array(value, "sidecar_args")?,
        };
        if sidecar_args.is_empty() {
            bail!("hub.toml: sidecar_args is empty");
        }
        Ok(Self {
            group_id,
            ops_topic_id,
            admin_user_ids,
            skip_muted_zalo_groups,
            sidecar_program,
            sidecar_args,
            event_retention_days: days(
                table,
                "event_retention_days",
                DEFAULT_EVENT_RETENTION_DAYS,
            )?,
            media_retention_days: days(
                table,
                "media_retention_days",
                DEFAULT_MEDIA_RETENTION_DAYS,
            )?,
            avatar_card,
            backfill_rename_cap,
            backfill_rename_gap_secs,
        })
    }
}

fn rename_cap(table: &toml::map::Map<String, toml::Value>) -> Result<usize> {
    match optional_i64(table, "backfill_rename_cap")? {
        None => Ok(BACKFILL_RENAME_CAP),
        Some(value) if value > 0 => usize::try_from(value)
            .map_err(|_| anyhow::anyhow!("hub.toml: backfill_rename_cap is out of range")),
        Some(_) => bail!("hub.toml: backfill_rename_cap must be positive"),
    }
}

fn rename_gap(table: &toml::map::Map<String, toml::Value>) -> Result<i64> {
    match optional_i64(table, "backfill_rename_gap_secs")? {
        None => Ok(BACKFILL_RENAME_GAP_SECS),
        Some(value) if value >= 0 => Ok(value),
        Some(_) => bail!("hub.toml: backfill_rename_gap_secs must be zero or positive"),
    }
}

fn bool_field(
    table: &toml::map::Map<String, toml::Value>,
    field: &str,
    default: bool,
) -> Result<bool> {
    match table.get(field) {
        None => Ok(default),
        Some(toml::Value::Boolean(value)) => Ok(*value),
        Some(_) => bail!("hub.toml: {field} must be true or false"),
    }
}

fn required_i64(table: &toml::map::Map<String, toml::Value>, field: &str) -> Result<i64> {
    match table.get(field) {
        Some(toml::Value::Integer(value)) => Ok(*value),
        Some(_) => bail!("hub.toml: {field} must be an integer"),
        None => bail!("hub.toml: {field} is required"),
    }
}

fn optional_i64(table: &toml::map::Map<String, toml::Value>, field: &str) -> Result<Option<i64>> {
    match table.get(field) {
        None => Ok(None),
        Some(toml::Value::Integer(value)) => Ok(Some(*value)),
        Some(_) => bail!("hub.toml: {field} must be an integer"),
    }
}

fn int_array(table: &toml::map::Map<String, toml::Value>, field: &str) -> Result<Vec<i64>> {
    match table.get(field) {
        None => Ok(Vec::new()),
        Some(toml::Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let Some(value) = item.as_integer() else {
                    bail!("hub.toml: {field} must be integers");
                };
                out.push(value);
            }
            Ok(out)
        }
        Some(_) => bail!("hub.toml: {field} must be an array"),
    }
}

fn string_array(value: &toml::Value, field: &str) -> Result<Vec<String>> {
    let Some(items) = value.as_array() else {
        bail!("hub.toml: {field} must be an array");
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(text) = item.as_str() else {
            bail!("hub.toml: {field} must be strings");
        };
        out.push(text.to_owned());
    }
    Ok(out)
}

fn days(table: &toml::map::Map<String, toml::Value>, field: &str, default: i64) -> Result<i64> {
    match optional_i64(table, field)? {
        None => Ok(default),
        Some(value) if value > 0 => Ok(value),
        Some(_) => bail!("hub.toml: {field} must be a positive number of days"),
    }
}

fn discover_sidecar() -> Option<PathBuf> {
    let mut starts = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        starts.push(dir.to_path_buf());
    }
    for start in starts {
        let mut dir = Some(start.as_path());
        while let Some(current) = dir {
            let candidate = current.join(SIDECAR_SCRIPT);
            if candidate.is_file() {
                return Some(candidate);
            }
            dir = current.parent();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_group_and_forbidden_token_name_the_field() {
        let missing = HubConfig::parse("").unwrap_err();
        let text = missing.to_string();
        assert!(text.starts_with("hub.toml:"), "{text}");
        assert!(text.contains("group_id"), "{text}");

        let secret = "super-secret-token";
        let rejected =
            HubConfig::parse(&format!("bot_token = \"{secret}\"\ngroup_id = -1001\n")).unwrap_err();
        let text = rejected.to_string();
        assert!(text.contains("bot_token"), "{text}");
        assert!(!text.contains(secret), "{text}");
        assert!(!format!("{rejected:?}").contains(secret));
    }

    #[test]
    fn positive_group_id_is_rejected() {
        let err = HubConfig::parse("group_id = 5\n").unwrap_err();
        assert!(err.to_string().contains("group_id"));
        let topic = HubConfig::parse(
            "group_id = -1001\nops_topic_id = 0\nsidecar_args = [\"fixture.js\"]\n",
        )
        .unwrap_err();
        let text = topic.to_string();
        assert!(text.starts_with("hub.toml:"), "{text}");
        assert!(text.contains("ops_topic_id"), "{text}");
    }

    #[test]
    fn defaults_apply_when_the_script_is_passed() {
        let config = HubConfig::parse(
            "group_id = -100123\nsidecar_args = [\"fixture.js\"]\nadmin_user_ids = [7]\n",
        )
        .unwrap();
        assert_eq!(config.group_id, -100123);
        assert!(!config.skip_muted_zalo_groups);
        assert_eq!(config.sidecar_program, "node");
        assert_eq!(config.sidecar_args, vec!["fixture.js".to_owned()]);
        assert_eq!(config.event_retention_days, 30);
        assert_eq!(config.media_retention_days, 7);
        assert_eq!(config.admin_user_ids, vec![7]);
        assert_eq!(config.ops_topic_id, None);
        assert!(config.avatar_card);
        assert_eq!(config.backfill_rename_cap, BACKFILL_RENAME_CAP);
        assert_eq!(config.backfill_rename_gap_secs, BACKFILL_RENAME_GAP_SECS);
        let tuned = HubConfig::parse(
            "group_id = -100123\nsidecar_args = [\"fixture.js\"]\nbackfill_rename_cap = 2\nbackfill_rename_gap_secs = 0\n",
        )
        .unwrap();
        assert_eq!(tuned.backfill_rename_cap, 2);
        assert_eq!(tuned.backfill_rename_gap_secs, 0);
        let bad_cap = HubConfig::parse(
            "group_id = -100123\nsidecar_args = [\"fixture.js\"]\nbackfill_rename_cap = 0\n",
        )
        .unwrap_err();
        assert!(bad_cap.to_string().contains("backfill_rename_cap"));
        let off = HubConfig::parse(
            "group_id = -100123\nsidecar_args = [\"fixture.js\"]\navatar_card = false\n",
        )
        .unwrap();
        assert!(!off.avatar_card);
    }
}
