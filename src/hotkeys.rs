//! Global hotkeys. Keys are polled (rather than registered) so they never
//! steal keystrokes from other apps, which also lets push-to-talk detect
//! key release.

use crate::config::{Hotkey, HotkeyAction};
use crate::engine::{Engine, MusicControl};
use anyhow::bail;
use std::collections::HashMap;
use std::sync::Arc;

/// Each element is satisfied if any of its virtual-key codes is down.
pub type Combo = Vec<Vec<u16>>;

pub fn parse(keys: &str) -> anyhow::Result<Combo> {
    let mut combo = Vec::new();
    for part in keys.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        match vk_for(part) {
            Some(vks) => combo.push(vks),
            None => bail!("unknown key {part:?}"),
        }
    }
    if combo.is_empty() {
        bail!("empty hotkey");
    }
    Ok(combo)
}

fn vk_for(name: &str) -> Option<Vec<u16>> {
    let n = name.to_ascii_lowercase();
    let single = |vk: u16| Some(vec![vk]);
    if n.len() == 1 {
        let c = n.as_bytes()[0];
        return match c {
            b'a'..=b'z' => single(c.to_ascii_uppercase() as u16),
            b'0'..=b'9' => single(c as u16),
            _ => None,
        };
    }
    if let Some(num) = n.strip_prefix('f').and_then(|d| d.parse::<u16>().ok()) {
        return (1..=24).contains(&num).then(|| vec![0x6F + num]);
    }
    if let Some(num) = n.strip_prefix("numpad").and_then(|d| d.parse::<u16>().ok()) {
        return (num <= 9).then(|| vec![0x60 + num]);
    }
    match n.as_str() {
        "ctrl" | "control" => single(0x11),
        "shift" => single(0x10),
        "alt" => single(0x12),
        "win" | "meta" => Some(vec![0x5B, 0x5C]),
        "space" => single(0x20),
        "enter" => single(0x0D),
        "tab" => single(0x09),
        "escape" | "esc" => single(0x1B),
        "backspace" => single(0x08),
        "insert" => single(0x2D),
        "delete" => single(0x2E),
        "home" => single(0x24),
        "end" => single(0x23),
        "pageup" => single(0x21),
        "pagedown" => single(0x22),
        "left" => single(0x25),
        "up" => single(0x26),
        "right" => single(0x27),
        "down" => single(0x28),
        "pause" => single(0x13),
        "scrolllock" => single(0x91),
        "capslock" => single(0x14),
        "numpadmultiply" => single(0x6A),
        "numpadadd" => single(0x6B),
        "numpadsubtract" => single(0x6D),
        "numpaddecimal" => single(0x6E),
        "numpaddivide" => single(0x6F),
        "backquote" => single(0xC0),
        "minus" => single(0xBD),
        "equal" => single(0xBB),
        "bracketleft" => single(0xDB),
        "bracketright" => single(0xDD),
        "backslash" => single(0xDC),
        "semicolon" => single(0xBA),
        "quote" => single(0xDE),
        "comma" => single(0xBC),
        "period" => single(0xBE),
        "slash" => single(0xBF),
        "mouse3" => single(0x04),
        "mouse4" => single(0x05),
        "mouse5" => single(0x06),
        _ => None,
    }
}

/// Decides whether each target's push-to-talk/mute gate is open, given which
/// hotkeys are currently held.
pub fn gate_states<'a>(hotkeys: impl Iterator<Item = (&'a Hotkey, bool)>) -> HashMap<String, bool> {
    // target -> (has push-to-talk binding, any ptt held, any ptm held)
    let mut acc: HashMap<String, (bool, bool, bool)> = HashMap::new();
    for (hk, held) in hotkeys {
        let entry = acc.entry(hk.target.clone()).or_default();
        match hk.action {
            HotkeyAction::PushToTalk => {
                entry.0 = true;
                entry.1 |= held;
            }
            HotkeyAction::PushToMute => entry.2 |= held,
            HotkeyAction::ToggleMute
            | HotkeyAction::PlaySound
            | HotkeyAction::StopSounds
            | HotkeyAction::MusicPlayPause
            | HotkeyAction::MusicNext
            | HotkeyAction::MusicPrevious => {}
        }
    }
    acc.into_iter()
        .map(|(t, (has_ptt, ptt, ptm))| (t, (!has_ptt || ptt) && !ptm))
        .collect()
}

/// Last seen (keys, held) per hotkey id.
type PrevHeld = HashMap<String, (String, bool)>;

/// Press-triggered hotkeys (everything but push-to-talk and push-to-mute)
/// that were just pressed. A binding that is new or whose keys changed starts from its
/// current state, so recording new keys (which are still held when saved)
/// doesn't count as a press.
fn presses<'a>(
    prev: &mut PrevHeld,
    hotkeys: impl Iterator<Item = &'a Hotkey>,
    held: &[bool],
) -> Vec<&'a Hotkey> {
    let mut pressed = Vec::new();
    for (hk, &is_held) in hotkeys.zip(held) {
        let was = match prev.insert(hk.id.clone(), (hk.keys.clone(), is_held)) {
            Some((keys, was)) if keys == hk.keys => was,
            _ => is_held,
        };
        let on_press = match hk.action {
            HotkeyAction::PushToTalk | HotkeyAction::PushToMute => false,
            HotkeyAction::ToggleMute
            | HotkeyAction::PlaySound
            | HotkeyAction::StopSounds
            | HotkeyAction::MusicPlayPause
            | HotkeyAction::MusicNext
            | HotkeyAction::MusicPrevious => true,
        };
        if on_press && is_held && !was {
            pressed.push(hk);
        }
    }
    pressed
}

#[cfg(windows)]
fn key_down(vk: u16) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 }
}

/// No global key state off Windows; bindings still apply their idle state
/// (e.g. a push-to-talk channel stays silent) so the demo behaves the same.
#[cfg(not(windows))]
fn key_down(_vk: u16) -> bool {
    false
}

pub fn spawn(engine: Arc<Engine>) {
    std::thread::Builder::new()
        .name("hotkeys".into())
        .spawn(move || poll_loop(engine))
        .expect("spawn hotkeys");
}

fn poll_loop(engine: Arc<Engine>) {
    let mut version = u64::MAX;
    let mut bindings: Vec<(Hotkey, Combo)> = Vec::new();
    let mut controls = HashMap::new();
    let mut prev_held = PrevHeld::new();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(10));
        let v = engine.version();
        if v != version {
            version = v;
            bindings = engine
                .hotkeys()
                .into_iter()
                .filter_map(|h| parse(&h.keys).ok().map(|c| (h, c)))
                .collect();
            controls = engine.controls();
        }

        let held: Vec<bool> = bindings
            .iter()
            .map(|(_, combo)| combo.iter().all(|any| any.iter().any(|&vk| key_down(vk))))
            .collect();

        for hk in presses(&mut prev_held, bindings.iter().map(|(h, _)| h), &held) {
            match hk.action {
                HotkeyAction::ToggleMute => engine.toggle_mute(&hk.target),
                HotkeyAction::PlaySound => {
                    if let Err(e) = engine.play_sound(&hk.target) {
                        tracing::warn!("hotkey {}: {e:#}", hk.keys);
                    }
                }
                HotkeyAction::StopSounds => engine.stop_sounds(),
                HotkeyAction::MusicPlayPause => {
                    let _ = engine.music_control(MusicControl::Toggle);
                }
                HotkeyAction::MusicNext => {
                    let _ = engine.music_control(MusicControl::Next);
                }
                HotkeyAction::MusicPrevious => {
                    let _ = engine.music_control(MusicControl::Previous);
                }
                HotkeyAction::PushToTalk | HotkeyAction::PushToMute => {}
            }
        }

        let gates = gate_states(bindings.iter().map(|(h, _)| h).zip(held.iter().copied()));
        for (target, ctrl) in &controls {
            let open = gates.get(target).copied().unwrap_or(true);
            if ctrl.gate_open() != open {
                ctrl.set_gate_open(open);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_combos() {
        assert_eq!(
            parse("Ctrl+Shift+M").unwrap(),
            vec![vec![0x11], vec![0x10], vec![0x4D]]
        );
        assert_eq!(parse("F13").unwrap(), vec![vec![0x7C]]);
        assert_eq!(parse("Mouse5").unwrap(), vec![vec![0x06]]);
        assert_eq!(
            parse("Win+Numpad1").unwrap(),
            vec![vec![0x5B, 0x5C], vec![0x61]]
        );
        assert!(parse("Ctrl+Nope").is_err());
        assert!(parse("").is_err());
        assert!(parse("F25").is_err());
    }

    fn hk(action: HotkeyAction, target: &str) -> Hotkey {
        Hotkey {
            id: String::new(),
            keys: "F1".into(),
            action,
            target: target.into(),
        }
    }

    #[test]
    fn press_actions_fire_on_press_but_not_when_keys_change() {
        let mut prev = PrevHeld::new();
        let mut hk = hk(HotkeyAction::ToggleMute, "mic");
        hk.id = "h1".into();
        let mut press = |hk: &Hotkey, held| {
            presses(&mut prev, std::iter::once(hk), &[held])
                .into_iter()
                .map(|h| h.target.clone())
                .collect::<Vec<_>>()
        };

        assert!(press(&hk, false).is_empty(), "first sight, not held");
        assert_eq!(press(&hk, true), vec!["mic"], "press");
        assert!(press(&hk, true).is_empty(), "still held");
        assert!(press(&hk, false).is_empty(), "release");

        // Re-recorded while the new keys are held: not a press.
        hk.keys = "F2".into();
        assert!(press(&hk, true).is_empty(), "keys changed while held");
        assert!(press(&hk, false).is_empty());
        assert_eq!(press(&hk, true), vec!["mic"], "next real press toggles");

        // Sound and music hotkeys fire on press too; push-to-talk never does.
        for action in [
            HotkeyAction::PlaySound,
            HotkeyAction::StopSounds,
            HotkeyAction::MusicPlayPause,
            HotkeyAction::MusicNext,
            HotkeyAction::MusicPrevious,
        ] {
            hk.action = action;
            assert!(press(&hk, false).is_empty());
            assert_eq!(press(&hk, true), vec!["mic"], "{action:?}");
        }
        hk.action = HotkeyAction::PushToTalk;
        assert!(press(&hk, false).is_empty());
        assert!(press(&hk, true).is_empty());
    }

    #[test]
    fn push_to_talk_and_mute_gates() {
        let ptt = hk(HotkeyAction::PushToTalk, "mic");
        let ptm = hk(HotkeyAction::PushToMute, "master");
        let toggle = hk(HotkeyAction::ToggleMute, "app");

        let g = gate_states([(&ptt, false), (&ptm, false), (&toggle, true)].into_iter());
        assert!(!g["mic"]);
        assert!(g["master"]);
        assert!(g["app"]);

        let g = gate_states([(&ptt, true), (&ptm, true)].into_iter());
        assert!(g["mic"]);
        assert!(!g["master"]);
    }
}
