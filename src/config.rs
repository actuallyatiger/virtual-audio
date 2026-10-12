//! Persistent configuration, stored as JSON next to the user's app data.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT_PORT: u16 = 47800;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub port: u16,
    /// Render endpoint the final mix is written to (e.g. "CABLE Input").
    pub output: Option<DeviceRef>,
    pub master: Channel,
    pub sources: Vec<SourceConfig>,
    pub hotkeys: Vec<Hotkey>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            output: None,
            master: Channel::default(),
            sources: Vec::new(),
            hotkeys: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceRef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Channel {
    pub gain_db: f32,
    pub muted: bool,
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            muted: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceConfig {
    pub id: String,
    pub name: String,
    #[serde(flatten)]
    pub kind: SourceKind,
    #[serde(flatten)]
    pub channel: Channel,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceKind {
    /// A capture endpoint (microphone, line-in, ...). `None` follows the
    /// Windows default microphone (the console role, as set in Settings →
    /// Sound → Input), and switches when that default changes.
    Device { device: Option<DeviceRef> },
    /// Audio rendered by a process (and its child processes), matched by
    /// executable name so it survives app restarts.
    App { exe: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Hotkey {
    pub id: String,
    /// e.g. "Ctrl+Shift+M", "Mouse5", "F13".
    pub keys: String,
    pub action: HotkeyAction,
    /// "master" or a source id.
    pub target: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyAction {
    ToggleMute,
    /// Target is silent unless the keys are held.
    PushToTalk,
    /// Target is silent while the keys are held.
    PushToMute,
}

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("VirtualAudioMixer")
}

fn config_path() -> PathBuf {
    data_dir().join("config.json")
}

impl Config {
    /// Reads the saved config: `Ok(None)` if there is none yet, `Err` if it
    /// exists but can't be read or parsed.
    pub fn read() -> Result<Option<Self>, String> {
        match std::fs::read_to_string(config_path()) {
            Ok(text) => Self::parse(&text).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn parse(text: &str) -> Result<Self, String> {
        // Notepad and friends may add a UTF-8 byte order mark.
        serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())
    }

    /// Moves an unreadable config aside (to `config.<timestamp>.json.bad`,
    /// never replacing an earlier backup) so starting over with defaults
    /// doesn't overwrite what the user may want back.
    pub fn back_up_unreadable() -> std::io::Result<PathBuf> {
        let path = config_path();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let backup = path.with_file_name(format!("config.{stamp}.json.bad"));
        if backup.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("{} already exists", backup.display()),
            ));
        }
        std::fs::rename(&path, &backup)?;
        Ok(backup)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = config_path();
        std::fs::create_dir_all(path.parent().unwrap())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        // Replaces the old file atomically; on Windows this is MoveFileExW
        // with MOVEFILE_REPLACE_EXISTING.
        std::fs::rename(tmp, path)?;
        Ok(())
    }
}

pub fn new_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}{:x}{n:x}", nanos & 0xffff_ffff_ffff)
}

pub fn db_to_gain(db: f32) -> f32 {
    if db <= -60.0 {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_roundtrip() {
        let s = SourceConfig {
            id: "a".into(),
            name: "Chrome".into(),
            kind: SourceKind::App {
                exe: "chrome.exe".into(),
            },
            channel: Channel {
                gain_db: -3.0,
                muted: true,
            },
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["kind"], "app");
        assert_eq!(json["exe"], "chrome.exe");
        assert_eq!(json["gain_db"], -3.0);
        let back: SourceConfig = serde_json::from_value(json).unwrap();
        assert_eq!(back.kind, s.kind);
    }

    #[test]
    fn parses_with_bom_and_rejects_garbage() {
        let cfg = Config::parse("\u{feff}{\"port\": 1234}").unwrap();
        assert_eq!(cfg.port, 1234);
        assert!(cfg.sources.is_empty(), "missing fields default");
        assert!(Config::parse("{\"sources\": [{\"name\": 1}]}").is_err());
    }

    #[test]
    fn gain_conversion() {
        assert_eq!(db_to_gain(0.0), 1.0);
        assert_eq!(db_to_gain(-60.0), 0.0);
        assert!((db_to_gain(-6.0) - 0.501).abs() < 0.01);
    }
}
