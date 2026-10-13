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
    /// Soundboard sounds, in display order.
    pub sounds: Vec<SoundConfig>,
    /// Play the soundboard on the Windows default output (your headphones)
    /// too, so you hear your own sounds.
    pub hear_sounds: bool,
    pub hear_sounds_db: f32,
    /// Start with Windows uses the elevated scheduled task instead of the
    /// `Run` registry entry.
    pub autostart_admin: bool,
    pub music: MusicConfig,
}

/// The music playlist and its settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MusicConfig {
    /// Files and folders, as added.
    pub items: Vec<MusicItem>,
    pub shuffle: bool,
    pub repeat: Repeat,
    /// Turn the music down while you talk, by `duck_db`.
    pub duck: bool,
    pub duck_db: f32,
    /// Play the music on your own default output too, at `hear_db`.
    pub hear: bool,
    pub hear_db: f32,
    /// Tracks taken out of the playlist (paths) that its folders still have.
    pub removed: Vec<String>,
}

impl Default for MusicConfig {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            shuffle: false,
            repeat: Repeat::All,
            duck: false,
            duck_db: -12.0,
            hear: true,
            hear_db: 0.0,
            removed: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MusicItem {
    pub id: String,
    pub path: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            output: None,
            master: Channel::default(),
            sources: Vec::new(),
            hotkeys: Vec::new(),
            sounds: Vec::new(),
            hear_sounds: true,
            hear_sounds_db: 0.0,
            autostart_admin: false,
            music: MusicConfig::default(),
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
    /// Only used by device sources.
    #[serde(flatten)]
    pub processing: Processing,
}

/// Microphone clean-up (see [`crate::dsp`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Processing {
    pub noise_suppression: bool,
    pub noise_gate: bool,
    pub gate_mode: GateMode,
    /// Manual mode: the gate opens when the level reaches this (dBFS).
    pub gate_db: f32,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GateMode {
    /// Opens when it hears your voice, like Discord's automatic sensitivity.
    #[default]
    Auto,
    /// Opens above `gate_db`.
    Manual,
}

impl Default for Processing {
    fn default() -> Self {
        Self {
            noise_suppression: false,
            noise_gate: false,
            gate_mode: GateMode::Auto,
            gate_db: -45.0,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SoundConfig {
    pub id: String,
    pub name: String,
    /// File name inside the `sounds` folder of the data directory.
    pub file: String,
    #[serde(default)]
    pub gain_db: f32,
    /// Trim (non-destructive): the part of the file that plays, in ms.
    #[serde(default)]
    pub start_ms: u32,
    /// `None` plays to the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_ms: Option<u32>,
    #[serde(default)]
    pub fade_in_ms: u32,
    #[serde(default)]
    pub fade_out_ms: u32,
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
    /// The built-in soundboard (at most one).
    Soundboard,
    /// The music player (at most one).
    Music,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Hotkey {
    pub id: String,
    /// e.g. "Ctrl+Shift+M", "Mouse5", "F13".
    pub keys: String,
    pub action: HotkeyAction,
    /// "master" or a source id; a sound id for `PlaySound`; unused by
    /// `StopSounds`.
    #[serde(default)]
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
    PlaySound,
    StopSounds,
    MusicPlayPause,
    MusicNext,
    MusicPrevious,
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
            processing: Processing::default(),
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["kind"], "app");
        assert_eq!(json["exe"], "chrome.exe");
        assert_eq!(json["gain_db"], -3.0);
        let back: SourceConfig = serde_json::from_value(json).unwrap();
        assert_eq!(back.kind, s.kind);

        // Configs from before processing settings existed still load.
        let old =
            r#"{"id":"m","name":"Mic","kind":"device","device":null,"gain_db":0,"muted":false}"#;
        let old: SourceConfig = serde_json::from_str(old).unwrap();
        assert_eq!(old.processing, Processing::default());
        let board: SourceConfig =
            serde_json::from_str(r#"{"id":"b","name":"Board","kind":"soundboard"}"#).unwrap();
        assert_eq!(board.kind, SourceKind::Soundboard);
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
