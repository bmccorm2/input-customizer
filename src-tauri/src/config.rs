use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum LockTrigger {
    Key { keycode: u16, flags: u64 },
    System { subtype: i16, data1_keycode: u32 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub colemak: bool,
    pub caps_backspace: bool,
    pub lock_sleep: bool,
    pub lock_trigger: Option<LockTrigger>,
    /// Scroll wheel multiplier; 1.0 = system speed (no tap needed).
    pub scroll_speed: f64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            colemak: true,
            caps_backspace: true,
            lock_sleep: false,
            lock_trigger: None,
            scroll_speed: 1.0,
        }
    }
}

/// Missing file → defaults. A corrupt file also falls back to defaults.
pub fn load(path: &Path) -> Config {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Atomic write: temp file in the same dir, then rename.
pub fn save(path: &Path, cfg: &Config) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(cfg)?)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("kc-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("config.json")
    }

    #[test]
    fn missing_file_gives_defaults() {
        let cfg = load(&tmp_path("missing"));
        assert_eq!(cfg, Config::default());
        assert!(cfg.colemak && cfg.caps_backspace && !cfg.lock_sleep);
        assert!(cfg.lock_trigger.is_none());
    }

    #[test]
    fn round_trip_key_trigger() {
        let path = tmp_path("key");
        let cfg = Config {
            lock_sleep: true,
            lock_trigger: Some(LockTrigger::Key {
                keycode: 12,
                flags: 1310720,
            }),
            ..Config::default()
        };
        save(&path, &cfg).unwrap();
        assert_eq!(load(&path), cfg);
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            raw["lock_trigger"],
            serde_json::json!({"kind":"key","keycode":12,"flags":1310720})
        );
    }

    #[test]
    fn round_trip_system_trigger() {
        let path = tmp_path("system");
        let cfg = Config {
            colemak: false,
            caps_backspace: false,
            lock_sleep: false,
            lock_trigger: Some(LockTrigger::System {
                subtype: 8,
                data1_keycode: 6,
            }),
            scroll_speed: 2.5,
        };
        save(&path, &cfg).unwrap();
        assert_eq!(load(&path), cfg);
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            raw["lock_trigger"],
            serde_json::json!({"kind":"system","subtype":8,"data1_keycode":6})
        );
    }

    #[test]
    fn parses_schema_null_trigger() {
        let cfg: Config = serde_json::from_str(
            r#"{"colemak":true,"caps_backspace":true,"lock_sleep":false,"lock_trigger":null}"#,
        )
        .unwrap();
        assert_eq!(cfg, Config::default());
    }
}
