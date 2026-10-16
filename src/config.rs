//! Persistent configuration, stored as JSON next to the user's app data.

use serde::{Deserialize, Deserializer, Serialize};
use std::path::PathBuf;

pub const DEFAULT_PORT: u16 = 47800;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub port: u16,
    /// Render endpoint the final mix is written to (e.g. "CABLE Input").
    pub output: Option<DeviceRef>,
    pub master: Channel,
    #[serde(deserialize_with = "known_sources")]
    pub sources: Vec<SourceConfig>,
    pub hotkeys: Vec<Hotkey>,
    /// Soundboard sounds, in display order.
    pub sounds: Vec<SoundConfig>,
    /// Play the soundboard on the Windows default output (your headphones)
    /// too, so you hear your own sounds.
    pub hear_sounds: bool,
    pub hear_sounds_db: f32,
    /// Turn each sound up or down to a common loudness (see
    /// [`crate::soundboard`]); each sound's own volume applies on top.
    pub even_sounds: bool,
    /// Start with Windows uses the elevated scheduled task instead of the
    /// `Run` registry entry.
    pub autostart_admin: bool,
    pub music: MusicConfig,
    /// The web UI's colours.
    #[serde(deserialize_with = "theme_or_system")]
    pub theme: Theme,
    /// How updates are looked for and fetched.
    #[serde(deserialize_with = "update_mode_or_auto")]
    pub updates: UpdateMode,
}

/// How updates are looked for (see [`crate::update`]).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpdateMode {
    /// Check, and download what's found (installing still waits for a
    /// restart).
    #[default]
    Auto,
    /// Check, and offer to download.
    Notify,
    /// Only "Check now" checks.
    Off,
}

/// A mode this build doesn't know (written by a newer one) falls back to the
/// default instead of making the whole file unreadable.
fn update_mode_or_auto<'de, D: Deserializer<'de>>(d: D) -> Result<UpdateMode, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(UpdateMode::deserialize(v).unwrap_or_default())
}

/// The kinds of source this build knows (`SourceKind`'s tags).
const SOURCE_KINDS: [&str; 4] = ["device", "app", "soundboard", "music"];

/// A source of a kind this build doesn't know (added by another version,
/// such as a test build's phones) is left out, instead of making the whole
/// file unreadable. A broken one of a known kind still is.
fn known_sources<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<SourceConfig>, D::Error> {
    let values = Vec::<serde_json::Value>::deserialize(d)?;
    let mut sources = Vec::new();
    for v in values {
        match v.get("kind").and_then(|k| k.as_str()) {
            Some(kind) if !SOURCE_KINDS.contains(&kind) => {
                tracing::warn!("leaving out a {kind} source, which this version doesn't know");
            }
            _ => sources.push(SourceConfig::deserialize(v).map_err(serde::de::Error::custom)?),
        }
    }
    Ok(sources)
}

/// A theme this build doesn't know (written by a newer one) falls back to
/// System instead of making the whole file unreadable.
fn theme_or_system<'de, D: Deserializer<'de>>(d: D) -> Result<Theme, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(Theme::deserialize(v).unwrap_or_default())
}

/// The web UI's colours.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    /// Follow the system's light or dark setting.
    #[default]
    System,
    Light,
    Dark,
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
            even_sounds: true,
            autostart_admin: false,
            music: MusicConfig::default(),
            theme: Theme::System,
            updates: UpdateMode::Auto,
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
    /// Play it to you on the Windows default output too (mics; off by
    /// default).
    #[serde(default)]
    pub hear: bool,
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
    /// Automatic mode: stricter about what opens the gate (it ignores knocks
    /// and clicks), looking 30 ms ahead, which delays the mic that much.
    /// Only takes effect with the gate on in automatic mode.
    pub extra_strength: bool,
    /// With extra strength: delay the other sources by the same 30 ms, so
    /// they stay in time with the mic (e.g. singing along to music). Noise
    /// suppression's own 10 ms is matched regardless.
    pub keep_in_time: bool,
    /// Keep speech at a steady level (like voice chat apps' automatic gain
    /// control, which their "raw" modes turn off).
    pub auto_level: bool,
    /// Automatic level's adjustment (dB) when last saved, so it starts there
    /// rather than learning your voice again.
    pub auto_level_db: f32,
    /// Remove what you hear (the Windows default output) from the mic, for
    /// speakers rather than headphones.
    pub echo_cancellation: bool,
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
            extra_strength: false,
            keep_in_time: false,
            auto_level: false,
            auto_level_db: 0.0,
            echo_cancellation: false,
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
    fn theme_loads_from_old_new_and_unknown_files() {
        let old: Config = serde_json::from_str(r#"{"port":1}"#).unwrap();
        assert_eq!(old.theme, Theme::System);
        let dark: Config = serde_json::from_str(r#"{"theme":"dark"}"#).unwrap();
        assert_eq!(dark.theme, Theme::Dark);
        assert_eq!(serde_json::to_value(&dark).unwrap()["theme"], "dark");
        let newer: Config = serde_json::from_str(r#"{"port":1,"theme":"sepia"}"#).unwrap();
        assert_eq!((newer.port, newer.theme), (1, Theme::System));
    }

    #[test]
    fn updates_download_unless_turned_off() {
        let old: Config = serde_json::from_str(r#"{"port":1}"#).unwrap();
        assert_eq!(
            old.updates,
            UpdateMode::Auto,
            "for configs from before updates"
        );
        let off: Config = serde_json::from_str(r#"{"updates":"off"}"#).unwrap();
        assert_eq!(off.updates, UpdateMode::Off);
        let newer: Config = serde_json::from_str(r#"{"updates":"nightly"}"#).unwrap();
        assert_eq!(newer.updates, UpdateMode::Auto);
    }

    #[test]
    fn a_source_of_an_unknown_kind_is_left_out_not_the_file() {
        let other: Config = serde_json::from_str(
            r#"{"port":1,"sources":[
                {"id":"a","name":"Chrome","kind":"app","exe":"chrome.exe"},
                {"id":"b","name":"Hologram","kind":"hologram","beam":3},
                {"id":"c","name":"Phone","kind":"bluetooth","device":{"id":"bt","name":"Pixel"}}
            ]}"#,
        )
        .unwrap();
        let ids: Vec<&str> = other.sources.iter().map(|s| s.id.as_str()).collect();
        assert_eq!((other.port, ids), (1, vec!["a"]));
        // Every kind this build has is known.
        for kind in [
            SourceKind::Device { device: None },
            SourceKind::App {
                exe: "a.exe".into(),
            },
            SourceKind::Soundboard,
            SourceKind::Music,
        ] {
            let tag = serde_json::to_value(&kind).unwrap()["kind"].clone();
            assert!(SOURCE_KINDS.contains(&tag.as_str().unwrap()), "{tag}");
        }
    }

    /// `config.json` as each release saved it, with every feature set up
    /// (`testdata/config/<version>.json`, from that release's demo build).
    /// Each must still load, with nothing lost: from 1.0, breaking this is
    /// for a major version only. The release skill adds each new release's.
    #[test]
    fn every_released_versions_config_still_loads() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/testdata/config");
        let mut versions = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let version = path.file_stem().unwrap().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&path).unwrap();
            let config = Config::parse(&text).unwrap_or_else(|e| panic!("{version}: {e}"));
            // What every version had, as it was set up.
            let names: Vec<&str> = config.sources.iter().map(|s| s.name.as_str()).collect();
            assert_eq!(
                names,
                [
                    "Default microphone",
                    "Second mic",
                    "Game",
                    "Soundboard",
                    "Music"
                ],
                "{version}"
            );
            let [mic, mic2, app, ..] = &config.sources[..] else {
                unreachable!()
            };
            assert_eq!(mic.kind, SourceKind::Device { device: None }, "{version}");
            assert_eq!(mic.channel.gain_db, 3.0, "{version}");
            assert!(mic.processing.noise_suppression && mic.processing.extra_strength);
            assert_eq!(mic2.processing.gate_mode, GateMode::Manual, "{version}");
            assert_eq!(mic2.processing.gate_db, -42.0, "{version}");
            assert!(matches!(&app.kind, SourceKind::App { exe } if !exe.is_empty()));
            assert!(app.channel.muted, "{version}");
            assert_eq!(config.master.gain_db, -1.5, "{version}");
            assert!(config.output.is_some(), "{version}");
            assert_eq!(config.hotkeys.len(), 3, "{version}");
            assert_eq!(config.hotkeys[1].target, mic.id, "{version}");
            let sound = &config.sounds[0];
            assert_eq!(config.hotkeys[2].target, sound.id, "{version}");
            assert_eq!(
                (sound.gain_db, sound.end_ms),
                (-3.0, Some(1500)),
                "{version}"
            );
            assert!(!config.hear_sounds && config.even_sounds, "{version}");
            assert_eq!(config.hear_sounds_db, -4.0, "{version}");
            let music = &config.music;
            assert_eq!(music.items.len(), 1, "{version}");
            assert!(music.shuffle && music.duck && music.hear, "{version}");
            assert_eq!(
                (music.repeat, music.duck_db),
                (Repeat::One, -9.0),
                "{version}"
            );
            assert_eq!(config.theme, Theme::Dark, "{version}");
            versions.push(version);
        }
        versions.sort();
        assert_eq!(versions[..5], ["0.1.0", "0.2.0", "0.2.1", "0.3.0", "0.4.0"]);
    }

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
            hear: false,
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
