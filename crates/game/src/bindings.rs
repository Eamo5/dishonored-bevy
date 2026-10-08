//! Key bindings: the actions the keyboard drives, their keys (the original's defaults,
//! `DefaultInput.ini`), changed in Options > Controls and kept with the settings.

use crate::settings::Settings;
use bevy::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Act {
    Forward,
    Back,
    Left,
    Right,
    Jump,
    Crouch,
    Sprint,
    LeanLeft,
    LeanRight,
    Use,
    Block,
    HealthElixir,
    ManaElixir,
    Journal,
    QuickSave,
    QuickLoad,
    Zoom,
}

impl Act {
    pub const ALL: [Act; 17] = [
        Act::Forward,
        Act::Back,
        Act::Left,
        Act::Right,
        Act::Jump,
        Act::Crouch,
        Act::Sprint,
        Act::LeanLeft,
        Act::LeanRight,
        Act::Use,
        Act::Block,
        Act::HealthElixir,
        Act::ManaElixir,
        Act::Journal,
        Act::QuickSave,
        Act::QuickLoad,
        Act::Zoom,
    ];

    /// Its name in the original's settings (`PSI_GBA_<name>` in `Settings.int`).
    pub fn psi(self) -> &'static str {
        match self {
            Act::Forward => "MoveForward",
            Act::Back => "MoveBackward",
            Act::Left => "StrafeLeft",
            Act::Right => "StrafeRight",
            Act::Jump => "Jump",
            Act::Crouch => "Sneak",
            Act::Sprint => "Sprint",
            Act::LeanLeft => "LeanLeft",
            Act::LeanRight => "LeanRight",
            Act::Use => "Use",
            Act::Block => "Block",
            Act::HealthElixir => "HealthElixir",
            Act::ManaElixir => "ManaElixir",
            Act::Journal => "Journal",
            Act::QuickSave => "QuickSave",
            Act::QuickLoad => "QuickLoad",
            Act::Zoom => "Zoom",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Act::Forward => "Move Forward",
            Act::Back => "Move Backward",
            Act::Left => "Strafe Left",
            Act::Right => "Strafe Right",
            Act::Jump => "Jump / Climb",
            Act::Crouch => "Crouch",
            Act::Sprint => "Sprint",
            Act::LeanLeft => "Lean Left",
            Act::LeanRight => "Lean Right",
            Act::Use => "Use",
            Act::Block => "Block / Choke",
            Act::HealthElixir => "Health Elixir",
            Act::ManaElixir => "Mana Remedy",
            Act::Journal => "Journal",
            Act::QuickSave => "Quick Save",
            Act::QuickLoad => "Quick Load",
            Act::Zoom => "Zoom (Mask Optics)",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Act::Forward => "forward",
            Act::Back => "back",
            Act::Left => "left",
            Act::Right => "right",
            Act::Jump => "jump",
            Act::Crouch => "crouch",
            Act::Sprint => "sprint",
            Act::LeanLeft => "lean_left",
            Act::LeanRight => "lean_right",
            Act::Use => "use",
            Act::Block => "block",
            Act::HealthElixir => "health_elixir",
            Act::ManaElixir => "mana_elixir",
            Act::Journal => "journal",
            Act::QuickSave => "quick_save",
            Act::QuickLoad => "quick_load",
            Act::Zoom => "zoom",
        }
    }

    pub fn default_key(self) -> KeyCode {
        match self {
            Act::Forward => KeyCode::KeyW,
            Act::Back => KeyCode::KeyS,
            Act::Left => KeyCode::KeyA,
            Act::Right => KeyCode::KeyD,
            Act::Jump => KeyCode::Space,
            Act::Crouch => KeyCode::KeyC,
            Act::Sprint => KeyCode::ShiftLeft,
            Act::LeanLeft => KeyCode::KeyQ,
            Act::LeanRight => KeyCode::KeyE,
            Act::Use => KeyCode::KeyF,
            Act::Block => KeyCode::ControlLeft,
            Act::HealthElixir => KeyCode::KeyR,
            Act::ManaElixir => KeyCode::KeyT,
            Act::Journal => KeyCode::KeyJ,
            Act::QuickSave => KeyCode::F5,
            Act::QuickLoad => KeyCode::F9,
            Act::Zoom => KeyCode::AltLeft,
        }
    }
}

/// The keys bound to the actions.
#[derive(Resource, Clone)]
pub struct Bindings {
    keys: [KeyCode; 17],
}

impl Default for Bindings {
    fn default() -> Self {
        Bindings { keys: Act::ALL.map(Act::default_key) }
    }
}

impl Bindings {
    pub fn key(&self, a: Act) -> KeyCode {
        self.keys[Act::ALL.iter().position(|x| *x == a).unwrap_or(0)]
    }

    /// The defaults with the settings' changes.
    pub fn from_settings(s: &Settings) -> Self {
        let mut b = Bindings::default();
        for (id, key) in &s.bindings {
            if let (Some(i), Some(k)) = (Act::ALL.iter().position(|a| a.id() == id), parse_key(key)) {
                b.keys[i] = k;
            }
        }
        b
    }

    /// Bind `a` to `key` in the settings (a key bound elsewhere is swapped with it).
    pub fn rebind(s: &mut Settings, a: Act, key: KeyCode) {
        let cur = Bindings::from_settings(s);
        let old = cur.key(a);
        let mut set = |act: Act, k: KeyCode| {
            s.bindings.retain(|(id, _)| id != act.id());
            if k != act.default_key() {
                s.bindings.push((act.id().to_string(), key_name(k)));
            }
        };
        if let Some(other) = Act::ALL.iter().copied().find(|o| *o != a && cur.key(*o) == key) {
            set(other, old);
        }
        set(a, key);
    }
}

/// Keys that can be bound.
pub const SUPPORTED: &[KeyCode] = &[
    KeyCode::KeyA, KeyCode::KeyB, KeyCode::KeyC, KeyCode::KeyD, KeyCode::KeyE, KeyCode::KeyF, KeyCode::KeyG, KeyCode::KeyH, KeyCode::KeyI,
    KeyCode::KeyJ, KeyCode::KeyK, KeyCode::KeyL, KeyCode::KeyM, KeyCode::KeyN, KeyCode::KeyO, KeyCode::KeyP, KeyCode::KeyQ, KeyCode::KeyR,
    KeyCode::KeyS, KeyCode::KeyT, KeyCode::KeyU, KeyCode::KeyV, KeyCode::KeyW, KeyCode::KeyX, KeyCode::KeyY, KeyCode::KeyZ,
    KeyCode::Digit0, KeyCode::Digit1, KeyCode::Digit2, KeyCode::Digit3, KeyCode::Digit4, KeyCode::Digit5, KeyCode::Digit6, KeyCode::Digit7,
    KeyCode::Digit8, KeyCode::Digit9, KeyCode::F1, KeyCode::F2, KeyCode::F3, KeyCode::F4, KeyCode::F5, KeyCode::F6, KeyCode::F7, KeyCode::F8,
    KeyCode::F9, KeyCode::F10, KeyCode::F11, KeyCode::F12, KeyCode::Space, KeyCode::ShiftLeft, KeyCode::ShiftRight, KeyCode::ControlLeft,
    KeyCode::ControlRight, KeyCode::AltLeft, KeyCode::AltRight, KeyCode::Tab, KeyCode::CapsLock, KeyCode::Enter, KeyCode::Backspace,
    KeyCode::Backquote, KeyCode::Minus, KeyCode::Equal, KeyCode::BracketLeft, KeyCode::BracketRight, KeyCode::Semicolon, KeyCode::Quote,
    KeyCode::Comma, KeyCode::Period, KeyCode::Slash, KeyCode::Backslash, KeyCode::Insert, KeyCode::Delete, KeyCode::Home, KeyCode::End,
    KeyCode::PageUp, KeyCode::PageDown, KeyCode::ArrowUp, KeyCode::ArrowDown, KeyCode::ArrowLeft, KeyCode::ArrowRight, KeyCode::Numpad0,
    KeyCode::Numpad1, KeyCode::Numpad2, KeyCode::Numpad3, KeyCode::Numpad4, KeyCode::Numpad5, KeyCode::Numpad6, KeyCode::Numpad7,
    KeyCode::Numpad8, KeyCode::Numpad9,
];

pub fn key_name(k: KeyCode) -> String {
    format!("{k:?}")
}

pub fn parse_key(s: &str) -> Option<KeyCode> {
    SUPPORTED.iter().copied().find(|k| key_name(*k) == s)
}

/// How a key reads on screen ("W", "Left Shift", "F5").
pub fn display(k: KeyCode) -> String {
    let n = key_name(k);
    if let Some(l) = n.strip_prefix("Key") {
        return l.to_string();
    }
    if let Some(d) = n.strip_prefix("Digit") {
        return d.to_string();
    }
    match k {
        KeyCode::ShiftLeft => "Left Shift".into(),
        KeyCode::ShiftRight => "Right Shift".into(),
        KeyCode::ControlLeft => "Left Ctrl".into(),
        KeyCode::ControlRight => "Right Ctrl".into(),
        KeyCode::AltLeft => "Left Alt".into(),
        KeyCode::AltRight => "Right Alt".into(),
        _ => n,
    }
}

static CURRENT: std::sync::RwLock<Option<Bindings>> = std::sync::RwLock::new(None);

/// The bindings in force (for texts that name keys).
pub fn set_current(b: &Bindings) {
    if let Ok(mut c) = CURRENT.write() {
        *c = Some(b.clone());
    }
}

/// "[F]": the key bound to an action, as prompts and tutorials show it.
pub fn hint(a: Act) -> String {
    let k = CURRENT.read().ok().and_then(|c| c.as_ref().map(|b| b.key(a))).unwrap_or(a.default_key());
    format!("[{}]", display(k))
}
