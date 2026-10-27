//! Key bindings: the actions the keyboard and mouse drive, as the original's keyboard mapping
//! lists them (`PSI_GBA_*`: the 36 of `DefaultInput.ini`'s `m_PCBindings`, the sword and the left
//! hand on the mouse's buttons, the shortcut items on its wheel, the journal's tabs unbound),
//! their keys or buttons (the original's defaults), changed in Options > Controls and kept with
//! the settings. What each does this frame (`Bindings::pressed`...) is read once from the keys,
//! the buttons, the wheel and the pad (`update_acts`).

use crate::settings::Settings;
use bevy::input::mouse::AccumulatedMouseScroll;
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
    Primary,
    Secondary,
    Block,
    Zoom,
    Wheel,
    NextItem,
    PrevItem,
    HealthElixir,
    ManaElixir,
    Journal,
    OpenNotes,
    OpenPowers,
    OpenBoneCharms,
    OpenInventory,
    QuickSave,
    QuickLoad,
    Shortcut0,
    Shortcut1,
    Shortcut2,
    Shortcut3,
    Shortcut4,
    Shortcut5,
    Shortcut6,
    Shortcut7,
    Shortcut8,
    Shortcut9,
}

/// How many actions there are.
const N: usize = 36;

impl Act {
    /// In the keyboard mapping's order (`Settings.int`'s `PSI_GBA_*`).
    pub const ALL: [Act; N] = [
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
        Act::Primary,
        Act::Secondary,
        Act::Block,
        Act::Zoom,
        Act::Wheel,
        Act::NextItem,
        Act::PrevItem,
        Act::HealthElixir,
        Act::ManaElixir,
        Act::Journal,
        Act::OpenNotes,
        Act::OpenPowers,
        Act::OpenBoneCharms,
        Act::OpenInventory,
        Act::QuickSave,
        Act::QuickLoad,
        Act::Shortcut0,
        Act::Shortcut1,
        Act::Shortcut2,
        Act::Shortcut3,
        Act::Shortcut4,
        Act::Shortcut5,
        Act::Shortcut6,
        Act::Shortcut7,
        Act::Shortcut8,
        Act::Shortcut9,
    ];

    /// The shortcuts' actions, the dock's slots 1 to 10 (`GBA_Shortcut_0`..`9`).
    pub const SHORTCUTS: [Act; 10] = [Act::Shortcut0, Act::Shortcut1, Act::Shortcut2, Act::Shortcut3, Act::Shortcut4, Act::Shortcut5, Act::Shortcut6, Act::Shortcut7, Act::Shortcut8, Act::Shortcut9];

    fn index(self) -> usize {
        Act::ALL.iter().position(|a| *a == self).unwrap_or(0)
    }

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
            Act::Primary => "Primary",
            Act::Secondary => "Secondary",
            Act::Block => "Block",
            Act::Zoom => "Zoom",
            Act::Wheel => "Wheel",
            Act::NextItem => "NextItem",
            Act::PrevItem => "PrevItem",
            Act::HealthElixir => "HealthElixir",
            Act::ManaElixir => "ManaElixir",
            Act::Journal => "Journal",
            Act::OpenNotes => "OpenNotes",
            Act::OpenPowers => "OpenPowers",
            Act::OpenBoneCharms => "OpenBoneCharms",
            Act::OpenInventory => "OpenInventory",
            Act::QuickSave => "QuickSave",
            Act::QuickLoad => "QuickLoad",
            Act::Shortcut0 => "Shortcut_0",
            Act::Shortcut1 => "Shortcut_1",
            Act::Shortcut2 => "Shortcut_2",
            Act::Shortcut3 => "Shortcut_3",
            Act::Shortcut4 => "Shortcut_4",
            Act::Shortcut5 => "Shortcut_5",
            Act::Shortcut6 => "Shortcut_6",
            Act::Shortcut7 => "Shortcut_7",
            Act::Shortcut8 => "Shortcut_8",
            Act::Shortcut9 => "Shortcut_9",
        }
    }

    /// Its name where the settings' texts are missing.
    pub fn label(self) -> String {
        match self {
            Act::Forward => "Move Forward".into(),
            Act::Back => "Move Backward".into(),
            Act::Left => "Strafe Left".into(),
            Act::Right => "Strafe Right".into(),
            Act::Jump => "Jump".into(),
            Act::Crouch => "Sneak".into(),
            Act::Sprint => "Sprint".into(),
            Act::LeanLeft => "Lean Left".into(),
            Act::LeanRight => "Lean Right".into(),
            Act::Use => "Use".into(),
            Act::Primary => "Sword Attack".into(),
            Act::Secondary => "Power/Gadget".into(),
            Act::Block => "Block/Choke".into(),
            Act::Zoom => "Zoom".into(),
            Act::Wheel => "Quick-access Wheel".into(),
            Act::NextItem => "Next Shortcut Item".into(),
            Act::PrevItem => "Previous Shortcut Item".into(),
            Act::HealthElixir => "Health Elixir".into(),
            Act::ManaElixir => "Mana Elixir".into(),
            Act::Journal => "Journal".into(),
            Act::OpenNotes => "Notes".into(),
            Act::OpenPowers => "Powers Menu".into(),
            Act::OpenBoneCharms => "Bone Charms Menu".into(),
            Act::OpenInventory => "Inventory".into(),
            Act::QuickSave => "Quick Save".into(),
            Act::QuickLoad => "Quick Load".into(),
            a => format!("Shortcut {}", a.shortcut().unwrap_or(0) + 1),
        }
    }

    /// The dock's slot a shortcut action takes (0-9).
    pub fn shortcut(self) -> Option<usize> {
        Act::SHORTCUTS.iter().position(|a| *a == self)
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
            Act::Primary => "primary",
            Act::Secondary => "secondary",
            Act::Block => "block",
            Act::Zoom => "zoom",
            Act::Wheel => "wheel",
            Act::NextItem => "next_item",
            Act::PrevItem => "prev_item",
            Act::HealthElixir => "health_elixir",
            Act::ManaElixir => "mana_elixir",
            Act::Journal => "journal",
            Act::OpenNotes => "open_notes",
            Act::OpenPowers => "open_powers",
            Act::OpenBoneCharms => "open_bone_charms",
            Act::OpenInventory => "open_inventory",
            Act::QuickSave => "quick_save",
            Act::QuickLoad => "quick_load",
            Act::Shortcut0 => "shortcut_0",
            Act::Shortcut1 => "shortcut_1",
            Act::Shortcut2 => "shortcut_2",
            Act::Shortcut3 => "shortcut_3",
            Act::Shortcut4 => "shortcut_4",
            Act::Shortcut5 => "shortcut_5",
            Act::Shortcut6 => "shortcut_6",
            Act::Shortcut7 => "shortcut_7",
            Act::Shortcut8 => "shortcut_8",
            Act::Shortcut9 => "shortcut_9",
        }
    }

    /// The original's binding (`DefaultInput.ini`'s `m_PCBindings`).
    pub fn default_bind(self) -> Bind {
        use Bind::{Key, Mouse};
        match self {
            Act::Forward => Key(KeyCode::KeyW),
            Act::Back => Key(KeyCode::KeyS),
            Act::Left => Key(KeyCode::KeyA),
            Act::Right => Key(KeyCode::KeyD),
            Act::Jump => Key(KeyCode::Space),
            Act::Crouch => Key(KeyCode::KeyC),
            Act::Sprint => Key(KeyCode::ShiftLeft),
            Act::LeanLeft => Key(KeyCode::KeyQ),
            Act::LeanRight => Key(KeyCode::KeyE),
            Act::Use => Key(KeyCode::KeyF),
            Act::Primary => Mouse(MouseButton::Left),
            Act::Secondary => Mouse(MouseButton::Right),
            Act::Block => Key(KeyCode::ControlLeft),
            Act::Zoom => Key(KeyCode::AltLeft),
            Act::Wheel => Mouse(MouseButton::Middle),
            Act::NextItem => Bind::WheelUp,
            Act::PrevItem => Bind::WheelDown,
            Act::HealthElixir => Key(KeyCode::KeyR),
            Act::ManaElixir => Key(KeyCode::KeyT),
            Act::Journal => Key(KeyCode::KeyJ),
            Act::OpenNotes | Act::OpenPowers | Act::OpenBoneCharms | Act::OpenInventory => Bind::None,
            Act::QuickSave => Key(KeyCode::F5),
            Act::QuickLoad => Key(KeyCode::F9),
            Act::Shortcut0 => Key(KeyCode::Digit1),
            Act::Shortcut1 => Key(KeyCode::Digit2),
            Act::Shortcut2 => Key(KeyCode::Digit3),
            Act::Shortcut3 => Key(KeyCode::Digit4),
            Act::Shortcut4 => Key(KeyCode::Digit5),
            Act::Shortcut5 => Key(KeyCode::Digit6),
            Act::Shortcut6 => Key(KeyCode::Digit7),
            Act::Shortcut7 => Key(KeyCode::Digit8),
            Act::Shortcut8 => Key(KeyCode::Digit9),
            Act::Shortcut9 => Key(KeyCode::Digit0),
        }
    }
}

/// What an action is bound to: a key, a mouse button, the wheel's turn either way, or nothing.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Bind {
    Key(KeyCode),
    Mouse(MouseButton),
    WheelUp,
    WheelDown,
    None,
}

/// An action's state this frame: held, pressed, let go.
const HELD: u8 = 1;
const PRESSED: u8 = 2;
const RELEASED: u8 = 4;

/// The actions' bindings, and what each does this frame.
#[derive(Resource, Clone)]
pub struct Bindings {
    binds: [Bind; N],
    state: [u8; N],
    /// the actions the pad holds this frame and the frame before (`pad::pad_input`)
    pad: [bool; N],
    pad_prev: [bool; N],
}

impl Default for Bindings {
    fn default() -> Self {
        Bindings { binds: Act::ALL.map(Act::default_bind), state: [0; N], pad: [false; N], pad_prev: [false; N] }
    }
}

impl Bindings {
    pub fn bind(&self, a: Act) -> Bind {
        self.binds[a.index()]
    }

    /// The key an action is on (none for a mouse button, the wheel or nothing).
    pub fn key(&self, a: Act) -> Option<KeyCode> {
        match self.bind(a) {
            Bind::Key(k) => Some(k),
            _ => None,
        }
    }

    /// Held this frame.
    pub fn pressed(&self, a: Act) -> bool {
        self.state[a.index()] & HELD != 0
    }

    /// Pressed this frame.
    pub fn just_pressed(&self, a: Act) -> bool {
        self.state[a.index()] & PRESSED != 0
    }

    /// Let go this frame.
    pub fn just_released(&self, a: Act) -> bool {
        self.state[a.index()] & RELEASED != 0
    }

    /// The pad holds an action down this frame (`pad::pad_input`, before `update_acts`).
    pub fn pad_hold(&mut self, a: Act) {
        self.pad[a.index()] = true;
    }

    /// An action's state set outright (tests).
    pub fn set_state(&mut self, a: Act, held: bool, pressed: bool, released: bool) {
        self.state[a.index()] = u8::from(held) * HELD | u8::from(pressed) * PRESSED | u8::from(released) * RELEASED;
    }

    /// The defaults with the settings' changes.
    pub fn from_settings(s: &Settings) -> Self {
        let mut b = Bindings::default();
        for (id, name) in &s.bindings {
            if let (Some(i), Some(k)) = (Act::ALL.iter().position(|a| a.id() == id), parse_bind(name)) {
                b.binds[i] = k;
            }
        }
        b
    }

    /// The bindings changed in the settings, what is held kept.
    pub fn reload(&mut self, s: &Settings) {
        self.binds = Bindings::from_settings(s).binds;
    }

    /// Bind `a` to `to` in the settings (what was bound there before takes `a`'s old binding).
    pub fn rebind(s: &mut Settings, a: Act, to: Bind) {
        let cur = Bindings::from_settings(s);
        let old = cur.bind(a);
        let mut set = |act: Act, b: Bind| {
            s.bindings.retain(|(id, _)| id != act.id());
            if b != act.default_bind() {
                s.bindings.push((act.id().to_string(), bind_name(b)));
            }
        };
        if to != Bind::None {
            if let Some(other) = Act::ALL.iter().copied().find(|o| *o != a && cur.bind(*o) == to) {
                set(other, old);
            }
        }
        set(a, to);
    }
}

/// What each action does this frame, from the keys, the mouse's buttons and wheel, and the pad
/// (its held actions), after the pad and the scripts have pressed theirs.
pub fn update_acts(mut bind: ResMut<Bindings>, keys: Option<Res<ButtonInput<KeyCode>>>, mouse: Option<Res<ButtonInput<MouseButton>>>, scroll: Option<Res<AccumulatedMouseScroll>>) {
    let wheel = scroll.map_or(0.0, |s| s.delta.y);
    let b = &mut *bind;
    for i in 0..N {
        let (held, pressed, released) = match b.binds[i] {
            Bind::Key(k) => keys.as_ref().map_or((false, false, false), |i| (i.pressed(k), i.just_pressed(k), i.just_released(k))),
            Bind::Mouse(m) => mouse.as_ref().map_or((false, false, false), |i| (i.pressed(m), i.just_pressed(m), i.just_released(m))),
            Bind::WheelUp => (wheel > 0.0, wheel > 0.0, false),
            Bind::WheelDown => (wheel < 0.0, wheel < 0.0, false),
            Bind::None => (false, false, false),
        };
        let (pad, was) = (b.pad[i], b.pad_prev[i]);
        let held = held || pad;
        let pressed = pressed || (pad && !was);
        let released = (released || (was && !pad)) && !held;
        b.state[i] = u8::from(held) * HELD | u8::from(pressed) * PRESSED | u8::from(released) * RELEASED;
    }
    b.pad_prev = b.pad;
    b.pad = [false; N];
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

/// Mouse buttons that can be bound (the original's left, right, middle and two thumb buttons).
pub const MOUSE: [MouseButton; 5] = [MouseButton::Left, MouseButton::Right, MouseButton::Middle, MouseButton::Back, MouseButton::Forward];

pub fn key_name(k: KeyCode) -> String {
    format!("{k:?}")
}

/// A binding's name in the settings: a key's, or the original's for the mouse
/// (`LeftMouseButton`, `MouseScrollUp`...).
fn bind_name(b: Bind) -> String {
    match b {
        Bind::Key(k) => key_name(k),
        Bind::Mouse(m) => mouse_name(m).0.to_string(),
        Bind::WheelUp => "MouseScrollUp".into(),
        Bind::WheelDown => "MouseScrollDown".into(),
        Bind::None => "None".into(),
    }
}

/// A mouse button's names: in the settings (UE3's) and on screen (`Settings.int` `[Keys]`).
fn mouse_name(m: MouseButton) -> (&'static str, &'static str) {
    match m {
        MouseButton::Left => ("LeftMouseButton", "Left Mouse Button"),
        MouseButton::Right => ("RightMouseButton", "Right Mouse Button"),
        MouseButton::Middle => ("MiddleMouseButton", "Middle Mouse Button"),
        MouseButton::Back => ("ThumbMouseButton", "Thumb Mouse Button"),
        _ => ("ThumbMouseButton2", "Thumb Mouse Button 2"),
    }
}

pub fn parse_key(s: &str) -> Option<KeyCode> {
    SUPPORTED.iter().copied().find(|k| key_name(*k) == s)
}

pub fn parse_bind(s: &str) -> Option<Bind> {
    match s {
        "MouseScrollUp" => Some(Bind::WheelUp),
        "MouseScrollDown" => Some(Bind::WheelDown),
        "None" => Some(Bind::None),
        _ => MOUSE.iter().copied().find(|m| mouse_name(*m).0 == s).map(Bind::Mouse).or_else(|| parse_key(s).map(Bind::Key)),
    }
}

/// How the original names a key (`Settings.int` `[Keys]`, by its UE3 name: `SpaceBar` "Space
/// Bar", `LeftControl` "Left Control", `One` "One").
fn original_name(k: KeyCode) -> Option<&'static str> {
    use KeyCode::*;
    Some(match k {
        Digit0 => "Zero",
        Digit1 => "One",
        Digit2 => "Two",
        Digit3 => "Three",
        Digit4 => "Four",
        Digit5 => "Five",
        Digit6 => "Six",
        Digit7 => "Seven",
        Digit8 => "Eight",
        Digit9 => "Nine",
        Space => "Space Bar",
        ShiftLeft => "Left Shift",
        ShiftRight => "Right Shift",
        ControlLeft => "Left Control",
        ControlRight => "Right Control",
        AltLeft => "Left Alt",
        AltRight => "Right Alt",
        CapsLock => "Caps Lock",
        Backspace => "Back Space",
        Backquote => "Tilde",
        Minus => "Underscore",
        Equal => "Equals",
        BracketLeft => "Left Bracket",
        BracketRight => "Right Bracket",
        PageUp => "Page Up",
        PageDown => "Page Down",
        ArrowUp => "Up",
        ArrowDown => "Down",
        ArrowLeft => "Left",
        ArrowRight => "Right",
        Numpad0 => "Num Pad Zero",
        Numpad1 => "Num Pad One",
        Numpad2 => "Num Pad Two",
        Numpad3 => "Num Pad Three",
        Numpad4 => "Num Pad Four",
        Numpad5 => "Num Pad Five",
        Numpad6 => "Num Pad Six",
        Numpad7 => "Num Pad Seven",
        Numpad8 => "Num Pad Eight",
        Numpad9 => "Num Pad Nine",
        _ => return None,
    })
}

/// The sword's, the left hand's and the wheel's bindings as hints ("[Left Mouse Button]"), or, a
/// pad in use, the pad's buttons for their commands (`pad::MARK`: drawn as the button's icon).
pub fn left_mouse() -> String {
    hint(Act::Primary)
}
pub fn right_mouse() -> String {
    hint(Act::Secondary)
}
pub fn middle_mouse() -> String {
    hint(Act::Wheel)
}

/// How a key reads on screen, as the original names it ("W", "Left Shift", "Space Bar", "F5").
pub fn display(k: KeyCode) -> String {
    if let Some(n) = original_name(k) {
        return n.to_string();
    }
    let n = key_name(k);
    if let Some(l) = n.strip_prefix("Key") {
        return l.to_string();
    }
    if let Some(d) = n.strip_prefix("Digit") {
        return d.to_string();
    }
    n
}

/// How a binding reads on screen: a key's name, the mouse's (`Settings.int` `[Keys]`: "Left
/// Mouse Button", "Mouse Scroll Up"), nothing for none.
pub fn display_bind(b: Bind) -> String {
    match b {
        Bind::Key(k) => display(k),
        Bind::Mouse(m) => mouse_name(m).1.to_string(),
        Bind::WheelUp => "Mouse Scroll Up".into(),
        Bind::WheelDown => "Mouse Scroll Down".into(),
        Bind::None => String::new(),
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
    // (a pad in use: its button, drawn as its icon: `keytext`)
    if let Some(b) = crate::pad::button_for(a).filter(|_| crate::pad::using()) {
        return format!("[{}{b}]", crate::pad::MARK);
    }
    let b = CURRENT.read().ok().and_then(|c| c.as_ref().map(|b| b.bind(a))).unwrap_or(a.default_bind());
    format!("[{}]", display_bind(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The original's 36 actions in its keyboard mapping's order, with its defaults.
    #[test]
    fn the_keyboard_mapping_lists_the_originals_actions() {
        assert_eq!(Act::ALL.len(), 36);
        assert_eq!(Act::ALL[10].psi(), "Primary");
        assert_eq!(Act::Primary.default_bind(), Bind::Mouse(MouseButton::Left));
        assert_eq!(Act::NextItem.default_bind(), Bind::WheelUp);
        assert_eq!(Act::OpenNotes.default_bind(), Bind::None);
        assert_eq!(Act::Shortcut9.default_bind(), Bind::Key(KeyCode::Digit0));
        assert_eq!(Act::Shortcut4.shortcut(), Some(4));
    }

    /// A mouse button or the wheel bound, kept by name in the settings; a binding taken from
    /// another action gives it the old one.
    #[test]
    fn mouse_bindings_round_trip_and_swap() {
        let mut s = Settings::default();
        Bindings::rebind(&mut s, Act::Use, Bind::Mouse(MouseButton::Right));
        let b = Bindings::from_settings(&s);
        assert_eq!(b.bind(Act::Use), Bind::Mouse(MouseButton::Right));
        assert_eq!(b.bind(Act::Secondary), Bind::Key(KeyCode::KeyF));
        Bindings::rebind(&mut s, Act::OpenNotes, Bind::WheelDown);
        let b = Bindings::from_settings(&s);
        assert_eq!(b.bind(Act::OpenNotes), Bind::WheelDown);
        assert_eq!(b.bind(Act::PrevItem), Bind::None);
        assert_eq!(display_bind(Bind::Mouse(MouseButton::Back)), "Thumb Mouse Button");
    }

    /// What an action does follows its binding, and the pad's held actions.
    #[test]
    fn actions_read_their_bindings_and_the_pad() {
        let mut app = App::new();
        app.init_resource::<ButtonInput<KeyCode>>().init_resource::<ButtonInput<MouseButton>>().init_resource::<Bindings>().add_systems(Update, update_acts);
        app.world_mut().resource_mut::<ButtonInput<MouseButton>>().press(MouseButton::Left);
        app.update();
        let b = app.world().resource::<Bindings>();
        assert!(b.pressed(Act::Primary) && b.just_pressed(Act::Primary) && !b.pressed(Act::Use));
        app.world_mut().resource_mut::<ButtonInput<MouseButton>>().clear();
        app.world_mut().resource_mut::<Bindings>().pad_hold(Act::Use);
        app.update();
        let b = app.world().resource::<Bindings>();
        assert!(b.pressed(Act::Use) && b.just_pressed(Act::Use) && !b.just_pressed(Act::Primary));
        app.update();
        let b = app.world().resource::<Bindings>();
        assert!(!b.pressed(Act::Use) && b.just_released(Act::Use));
    }
}
