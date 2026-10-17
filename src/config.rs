//! Persistent configuration, stored as JSON next to the user's app data.

use serde::{Deserialize, Deserializer, Serialize};
use std::path::PathBuf;

pub const DEFAULT_PORT: u16 = 47800;

/// The version of the config format, saved in `config.json` as `version`.
/// The first change to the format after a release (a field added, removed,
/// renamed or retyped, an enum value, a default) raises it by one, with a
/// migration from the one before in [`MIGRATIONS`] and the new format's
/// schema saved as `docs/config/v<VERSION>.schema.json`; the tests hold each
/// change to that. Configs from before versions (0.5.0 and earlier) are
/// version 1.
pub const VERSION: u32 = 1;

/// `MIGRATIONS[n - 1]` turns a version `n` config into version `n + 1`, on
/// its JSON, before it's read. One that only adds a field with a default can
/// change nothing.
const MIGRATIONS: &[Migration] = &[];

type Migration = fn(&mut serde_json::Map<String, serde_json::Value>);

/// Runs the `migrations` a version `version` config needs, in order.
fn migrate(
    config: &mut serde_json::Map<String, serde_json::Value>,
    version: u32,
    migrations: &[Migration],
) {
    for migration in migrations.iter().skip(version as usize - 1) {
        migration(config);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(default)]
pub struct Config {
    /// The format's version ([`VERSION`] once read).
    pub version: u32,
    pub port: u16,
    /// Render endpoint the final mix is written to (e.g. "CABLE Input").
    pub output: Option<DeviceRef>,
    pub master: Channel,
    #[serde(deserialize_with = "known_sources")]
    pub sources: Vec<SourceConfig>,
    #[serde(deserialize_with = "known_hotkeys")]
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
    #[serde(deserialize_with = "known_or_default")]
    pub theme: Theme,
    /// How updates are looked for and fetched.
    #[serde(deserialize_with = "known_or_default")]
    pub updates: UpdateMode,
}

/// How updates are looked for (see [`crate::update`]).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
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

/// A value this build doesn't know (an enum value written by a newer one)
/// falls back to the default instead of making the whole file unreadable.
fn known_or_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let v = serde_json::Value::deserialize(d)?;
    Ok(T::deserialize(v).unwrap_or_default())
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

/// A hotkey for an action this build doesn't know (added by a newer one) is
/// left out, like a source of an unknown kind.
fn known_hotkeys<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Hotkey>, D::Error> {
    let values = Vec::<serde_json::Value>::deserialize(d)?;
    let mut hotkeys = Vec::new();
    for v in values {
        if let Some(action) = v.get("action")
            && HotkeyAction::deserialize(action).is_err()
        {
            tracing::warn!("leaving out a hotkey for {action}, which this version doesn't know");
            continue;
        }
        hotkeys.push(Hotkey::deserialize(v).map_err(serde::de::Error::custom)?);
    }
    Ok(hotkeys)
}

/// The web UI's colours.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(default)]
pub struct MusicConfig {
    /// Files and folders, as added.
    pub items: Vec<MusicItem>,
    pub shuffle: bool,
    #[serde(deserialize_with = "known_or_default")]
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct MusicItem {
    pub id: String,
    pub path: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
            version: VERSION,
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct DeviceRef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(default)]
pub struct Processing {
    pub noise_suppression: bool,
    pub noise_gate: bool,
    #[serde(deserialize_with = "known_or_default")]
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
            Ok(text) => {
                let (config, version) = Self::parse(&text)?;
                if version > VERSION {
                    keep_newer(&config_path(), &text, version);
                }
                Ok(Some(config))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Reads a config of any version, migrating an older one to this one;
    /// also returns the version it was saved as.
    fn parse(text: &str) -> Result<(Self, u32), String> {
        // Notepad and friends may add a UTF-8 byte order mark.
        let mut value: serde_json::Value =
            serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
        let config = value.as_object_mut().ok_or("it isn't a JSON object")?;
        let version = match config.get("version") {
            None => 1,
            Some(v) => v
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .filter(|&v| v >= 1)
                .ok_or("its version isn't a number from 1")?,
        };
        if version > VERSION {
            // Written by a newer build: what this one knows still reads (see
            // the fallbacks above), and what it doesn't is dropped on save.
            tracing::warn!("config version {version} is newer than this build's ({VERSION})");
        }
        migrate(config, version, MIGRATIONS);
        config.insert("version".into(), VERSION.into());
        let config = serde_json::from_value(value).map_err(|e| e.to_string())?;
        Ok((config, version))
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

/// Copies a config a newer build saved (`text`, read from `path`) to
/// `config.v<version>.json` next to it, once, before this build saves over it
/// without the settings it doesn't know.
fn keep_newer(path: &std::path::Path, text: &str, version: u32) {
    let copy = path.with_file_name(format!("config.v{version}.json"));
    if copy.exists() {
        return;
    }
    match std::fs::write(&copy, text) {
        Ok(()) => tracing::info!("kept the newer config as {}", copy.display()),
        Err(e) => tracing::warn!("couldn't keep the newer config as {}: {e}", copy.display()),
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

    /// This build's config format, as a JSON Schema: every field, its type,
    /// its enum values and its default. What isn't part of the format is
    /// left out or put in order: doc comments, the order of fields and of
    /// enum values, and how schemars groups the values (a doc comment on one
    /// gives it a `oneOf` branch of its own).
    fn schema() -> String {
        use serde_json::Value;
        fn sorted(values: &mut [Value]) {
            values.sort_by_key(|v| v.to_string());
        }
        /// The strings a `oneOf` allows, if it's only a choice of strings.
        fn strings(branches: &[Value]) -> Option<Vec<Value>> {
            let mut values = Vec::new();
            for branch in branches {
                let branch = branch.as_object()?;
                if branch.get("type")? != "string"
                    || branch
                        .keys()
                        .any(|k| !["type", "const", "enum"].contains(&k.as_str()))
                {
                    return None;
                }
                match (branch.get("const"), branch.get("enum")) {
                    (Some(value), None) => values.push(value.clone()),
                    (None, Some(Value::Array(enumerated))) => values.extend(enumerated.clone()),
                    _ => return None,
                }
            }
            Some(values)
        }
        fn normalise(v: &mut Value) {
            match v {
                Value::Object(fields) => {
                    fields.remove("description");
                    fields.values_mut().for_each(normalise);
                    if let Some(Value::Array(branches)) = fields.get("oneOf")
                        && let Some(values) = strings(branches)
                    {
                        fields.remove("oneOf");
                        fields.insert("type".into(), "string".into());
                        fields.insert("enum".into(), values.into());
                    }
                    for list in ["enum", "required"] {
                        if let Some(Value::Array(values)) = fields.get_mut(list) {
                            sorted(values);
                        }
                    }
                }
                Value::Array(items) => items.iter_mut().for_each(normalise),
                _ => {}
            }
        }
        let mut schema = serde_json::to_value(schemars::schema_for!(Config)).unwrap();
        normalise(&mut schema);
        serde_json::to_string_pretty(&schema).unwrap() + "\n"
    }

    fn saved_schema(version: u32) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("docs/config/v{version}.schema.json"))
    }

    /// The format must be the one saved for [`VERSION`]: changing it means a
    /// new version, with a migration, so configs saved before still load.
    #[test]
    fn this_config_format_is_saved() {
        let now = schema();
        let new = std::env::temp_dir().join("virtual-audio-mixer.schema.json");
        let Ok(saved) = std::fs::read_to_string(saved_schema(VERSION)) else {
            std::fs::write(&new, &now).unwrap();
            panic!(
                "Save {} as docs/config/v{VERSION}.schema.json.",
                new.display()
            );
        };
        if saved.replace("\r\n", "\n") == now {
            return;
        }
        std::fs::write(&new, &now).unwrap();
        let next = VERSION + 1;
        panic!(
            "The config format changed. If no release has had version {VERSION} yet, save {new} \
             over docs/config/v{VERSION}.schema.json (and change its migration to match, if it \
             has one). Otherwise raise config::VERSION to {next}, add a migration from version \
             {VERSION} to config::MIGRATIONS (one that changes nothing is fine for a new field \
             with a default), and save {new} as docs/config/v{next}.schema.json: CI refuses a \
             change to a released one.",
            new = new.display()
        );
    }

    #[test]
    fn every_version_has_its_schema_and_migration() {
        assert_eq!(
            MIGRATIONS.len(),
            VERSION as usize - 1,
            "one per version after 1"
        );
        for version in 1..=VERSION {
            assert!(saved_schema(version).exists(), "v{version}.schema.json");
        }
        assert!(
            !saved_schema(VERSION + 1).exists(),
            "a schema without its version"
        );
    }

    #[test]
    fn a_config_saved_before_versions_is_version_1() {
        // 0.5.0 and earlier saved no version.
        let (old, version) = Config::parse(r#"{"port":1,"theme":"dark"}"#).unwrap();
        assert_eq!(
            (version, old.version, old.port, old.theme),
            (1, VERSION, 1, Theme::Dark)
        );
        let saved = serde_json::to_value(&old).unwrap();
        assert_eq!(saved["version"], VERSION, "saved with this version");
    }

    #[test]
    fn migrations_run_from_the_configs_version_in_order() {
        let to_2: Migration = |c| {
            c.insert("steps".into(), "2".into());
        };
        let to_3: Migration = |c| {
            let steps = c
                .get("steps")
                .and_then(|s| s.as_str())
                .unwrap_or("-")
                .to_owned();
            c.insert("steps".into(), (steps + "3").into());
        };
        let run = |version| {
            let mut config = serde_json::Map::new();
            migrate(&mut config, version, &[to_2, to_3]);
            config.get("steps").cloned()
        };
        assert_eq!(run(1), Some("23".into()));
        assert_eq!(run(2), Some("-3".into()));
        assert_eq!(run(3), None, "already the newest");
        assert_eq!(run(9), None, "newer than this build");
    }

    #[test]
    fn a_newer_config_still_reads() {
        let (newer, version) =
            Config::parse(r#"{"version":999,"port":2,"hologram":true}"#).unwrap();
        assert_eq!((version, newer.version, newer.port), (999, VERSION, 2));
        for bad in [r#"{"version":"two"}"#, r#"{"version":0}"#, r#"[1]"#] {
            assert!(Config::parse(bad).is_err(), "{bad}");
        }
    }

    /// What a sample config is for.
    #[derive(Clone, Copy, PartialEq)]
    enum Sample {
        /// Every field, with values other than the defaults where it can.
        Full,
        /// Only the fields that have no default.
        Minimal,
        /// Every field, plus a field a newer build might add in every object,
        /// and a value it might add for every enum.
        Newer,
    }

    /// Sample configs in the format `schema` describes: between them they
    /// use every enum value, every kind of source and every hotkey action,
    /// and both a value and null where a field can be null.
    fn samples(schema: &serde_json::Value, kind: Sample) -> Vec<serde_json::Value> {
        use serde_json::{Value, json};
        let defs = &schema["$defs"];
        let resolve = |s: &'_ Value| -> Value {
            match s.get("$ref").and_then(Value::as_str) {
                Some(r) => defs[r.trim_start_matches("#/$defs/")].clone(),
                None => s.clone(),
            }
        };
        // The schemas a value can take one of: a `oneOf` or `anyOf`, an
        // `enum` or a list of types, each enum value as its own `const`.
        fn alternatives(s: &Value) -> Option<Vec<Value>> {
            let mut alts = Vec::new();
            if let Some(Value::Array(branches)) = s.get("oneOf").or_else(|| s.get("anyOf")) {
                for branch in branches {
                    alts.extend(alternatives(branch).unwrap_or_else(|| vec![branch.clone()]));
                }
            } else if let Some(Value::Array(values)) = s.get("enum") {
                alts.extend(values.iter().map(|v| json!({ "const": v })));
            } else if let Some(Value::Array(types)) = s.get("type") {
                alts.extend(types.iter().map(|t| {
                    let mut alt = s.clone();
                    alt["type"] = t.clone();
                    alt
                }));
            } else {
                return None;
            }
            Some(alts)
        }
        // The most alternatives anywhere, so `pick`s up to it use them all.
        fn most(s: &Value) -> usize {
            let here = alternatives(s).map_or(1, |a| a.len());
            let below = match s {
                Value::Object(fields) => fields.values().map(most).max().unwrap_or(1),
                Value::Array(items) => items.iter().map(most).max().unwrap_or(1),
                _ => 1,
            };
            here.max(below)
        }
        fn sample(
            s: &Value,
            pick: usize,
            kind: Sample,
            resolve: &dyn Fn(&Value) -> Value,
        ) -> Value {
            let s = resolve(s);
            let default = s.get("default");
            let mut v = if let Some(value) = s.get("const") {
                value.clone()
            } else if let Some(alts) = alternatives(&s) {
                let newer_value = kind == Sample::Newer
                    && alts
                        .iter()
                        .all(|a| a.get("const").is_some_and(Value::is_string));
                if newer_value {
                    "from_a_newer_build".into()
                } else {
                    sample(&alts[pick % alts.len()], pick, kind, resolve)
                }
            } else {
                match s.get("type").and_then(Value::as_str) {
                    Some("boolean") => (!default.and_then(Value::as_bool).unwrap_or(false)).into(),
                    Some("integer") => {
                        let max = s.get("maximum").and_then(Value::as_u64).unwrap_or(u64::MAX);
                        let n = (1000 + pick as u64).min(max);
                        (if Some(n) == default.and_then(Value::as_u64) {
                            n - 1
                        } else {
                            n
                        })
                        .into()
                    }
                    Some("number") => {
                        let n = -3.5 - pick as f64;
                        (if Some(n) == default.and_then(Value::as_f64) {
                            n - 0.25
                        } else {
                            n
                        })
                        .into()
                    }
                    Some("string") => format!("text {pick}").into(),
                    Some("array") => {
                        let items = s.get("items").cloned().unwrap_or_default();
                        let count = alternatives(&resolve(&items)).map_or(1, |a| a.len());
                        (0..count)
                            .map(|i| sample(&items, pick + i, kind, resolve))
                            .collect()
                    }
                    Some("null") => Value::Null,
                    _ => json!({}),
                }
            };
            if let (Some(Value::Object(properties)), Some(object)) =
                (s.get("properties"), v.as_object_mut())
            {
                let required = s.get("required").and_then(Value::as_array);
                for (name, property) in properties {
                    let needed = required.is_some_and(|r| r.contains(&name.as_str().into()));
                    if kind != Sample::Minimal || needed {
                        object.insert(name.clone(), sample(property, pick, kind, resolve));
                    }
                }
                if kind == Sample::Newer {
                    object.insert("from_a_newer_build".into(), true.into());
                }
            }
            v
        }
        (0..most(schema))
            .map(|pick| sample(schema, pick, kind, &resolve))
            .collect()
    }

    /// The first place in `want` that `got` lacks or has another value.
    fn lost(want: &serde_json::Value, got: &serde_json::Value, at: &str) -> Option<String> {
        use serde_json::Value;
        match (want, got) {
            (Value::Object(want), Value::Object(got)) => want.iter().find_map(|(name, w)| {
                lost(
                    w,
                    got.get(name).unwrap_or(&Value::Null),
                    &format!("{at}.{name}"),
                )
            }),
            (Value::Array(want), Value::Array(got)) if want.len() == got.len() => want
                .iter()
                .zip(got)
                .enumerate()
                .find_map(|(i, (w, g))| lost(w, g, &format!("{at}[{i}]"))),
            (Value::Number(w), Value::Number(g)) if w.as_f64() == g.as_f64() => None,
            _ if want == got => None,
            _ => Some(format!("{at}: {want} became {got}")),
        }
    }

    /// Every format ever saved still loads, with nothing in it lost: sample
    /// configs in each, after its migrations, keep every value they had. A
    /// rename with a migration that does nothing fails here, for example.
    #[test]
    fn every_saved_format_loads_with_nothing_lost() {
        for version in 1..=VERSION {
            let text = std::fs::read_to_string(saved_schema(version)).unwrap();
            let schema: serde_json::Value = serde_json::from_str(&text).unwrap();
            for kind in [Sample::Full, Sample::Minimal] {
                for mut sample in samples(&schema, kind) {
                    sample["version"] = version.into();
                    let mut want = sample.clone();
                    migrate(want.as_object_mut().unwrap(), version, MIGRATIONS);
                    want["version"] = VERSION.into();
                    let (config, _) = Config::parse(&sample.to_string()).unwrap_or_else(|e| {
                        panic!("a v{version} config doesn't load: {e}\n{sample:#}")
                    });
                    let got = serde_json::to_value(&config).unwrap();
                    if let Some(lost) = lost(&want, &got, "config") {
                        panic!(
                            "Loading a v{version} config lost {lost}. A migration must move or \
                             convert it, or remove it if it's meant to go.\n{sample:#}"
                        );
                    }
                    let (again, _) = Config::parse(&got.to_string()).unwrap();
                    assert_eq!(
                        serde_json::to_value(again).unwrap(),
                        got,
                        "saved and read back"
                    );
                }
            }
        }
    }

    /// A config from a newer build still loads: a field this build doesn't
    /// know is ignored, and a value it doesn't know falls back to the
    /// default, or leaves its source or hotkey out.
    #[test]
    fn what_a_newer_build_adds_doesnt_stop_a_config_loading() {
        let schema: serde_json::Value = serde_json::from_str(&schema()).unwrap();
        for mut sample in samples(&schema, Sample::Newer) {
            sample["version"] = (VERSION + 1).into();
            if let Err(e) = Config::parse(&sample.to_string()) {
                panic!("{e}\n{sample:#}");
            }
        }
    }

    #[test]
    fn a_newer_builds_hotkeys_and_values_dont_stop_a_config_loading() {
        let (config, _) = Config::parse(
            r#"{"hotkeys":[
                {"id":"a","keys":"F13","action":"toggle_mute","target":"master"},
                {"id":"b","keys":"F14","action":"summon_cat"}
            ],"music":{"repeat":"twice"},"sources":[
                {"id":"m","name":"Mic","kind":"device","device":null,"gate_mode":"psychic"}
            ]}"#,
        )
        .unwrap();
        let ids: Vec<&str> = config.hotkeys.iter().map(|h| h.id.as_str()).collect();
        assert_eq!((ids, config.music.repeat), (vec!["a"], Repeat::default()));
        assert_eq!(config.sources[0].processing.gate_mode, GateMode::default());
    }

    #[test]
    fn a_newer_config_is_kept_before_its_saved_over() {
        let dir = std::env::temp_dir().join(format!("vam-keep-newer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        keep_newer(&path, "first", 7);
        keep_newer(&path, "second", 7);
        let kept = std::fs::read_to_string(dir.join("config.v7.json"));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(kept.unwrap(), "first", "the newer build's own, not ours");
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
        let (cfg, _) = Config::parse("\u{feff}{\"port\": 1234}").unwrap();
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
