//! The gamepad as the original binds it (`DefaultInput.ini`): the control scheme's buttons
//! (`m_PadBindingSet1`..`4`, by `PSI_Gamepad_BindingSet`) press what the keyboard's bindings
//! would (the actions' keys, the mouse's buttons), Back opens the journal and Start the pause
//! menu (the base bindings), the D-pad picks the shortcuts (`Dis_WheelShortcuts_GamepadSelect`
//! 0-3); the left stick moves Corvo (analogue: slower below full, as `m_fWalk_Trigger_Threshold`
//! has it), the right stick turns him (`DisTweaks_PlayerInput`: the turn's scale from
//! `m_fLookTurnScale_Min` 230 up to `_Max` 700 while the stick is held full, 500 a second; the
//! look up's 150, each times UE3's 100 a second, in its rotation units), the left stick's click
//! sprints until he stops (`m_bUseSprintToggle_GamePad`), and the lean button with the stick
//! leans (`m_bLeanWithAnalogStick`, past `m_fLean_Trigger_Threshold`). In the menus the pad steers
//! as the arrows, Enter, Escape, Shift and Alt do (the D-pad or the left stick, A, B, X, Y), the
//! shoulders change tabs (Tab, Shift+Tab) and the triggers sub tabs (Q, E). A pad used last marks
//! the interface's icons for the pad (`m_bUsingGamepad`).

use crate::bindings::{Act, Bindings};
use bevy::input::gamepad::{Gamepad, GamepadAxis, GamepadButton};
use bevy::prelude::*;

pub struct PadPlugin;

impl Plugin for PadPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Pad>().init_resource::<VirtualPad>().add_systems(PreUpdate, pad_input.after(bevy::input::InputSystems).after(crate::helpbar::taps));
    }
}

/// Whether a pad gave the last input (for those drawing icons without the resource at hand).
static USING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn using() -> bool {
    USING.load(std::sync::atomic::Ordering::Relaxed)
}

/// What the pad holds down (the scripts' runs leave it be when they drop real input).
static HELD: std::sync::Mutex<(Vec<KeyCode>, Vec<MouseButton>)> = std::sync::Mutex::new((Vec::new(), Vec::new()));

pub fn holds_key(k: KeyCode) -> bool {
    HELD.lock().is_ok_and(|h| h.0.contains(&k))
}

pub fn holds_mouse(m: MouseButton) -> bool {
    HELD.lock().is_ok_and(|h| h.1.contains(&m))
}

/// The pad's state as the game reads it.
#[derive(Resource, Default)]
pub struct Pad {
    /// a pad gave the last input (the interface shows its icons: `m_bUsingGamepad`)
    pub using: bool,
    /// the left stick past its dead zone (x right, y forward; 0..1), for walking
    pub walk: Vec2,
    /// the turn and look this frame (radians: yaw left, pitch up)
    pub look: Vec2,
    /// the sticks past their dead zones (left, right: x right, y up), for the wheel
    pub sticks: [Vec2; 2],
    /// what the pad holds down for the keyboard and mouse
    held: Vec<Emu>,
    /// the left stick's click sprints until he slows (`DisToggleSprint`)
    sprint: bool,
    /// the turn's scale, rising while the stick is held full (`m_fLookTurnAcceleration`)
    turn_scale: f32,
    /// the menus' steering: the direction held, and when it repeats
    steer: Option<(KeyCode, f32)>,
    /// the buttons down the frame before
    prev: Vec<GamepadButton>,
}

/// Something the pad holds down.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Emu {
    Key(KeyCode),
    Mouse(MouseButton),
}

/// A pad the scripts hold (tests): buttons held for a while, the sticks pushed for a while.
#[derive(Resource, Default)]
pub struct VirtualPad {
    pub buttons: Vec<(GamepadButton, f32)>,
    pub left: Option<(Vec2, f32)>,
    pub right: Option<(Vec2, f32)>,
}

/// The pad's commands (`GBA_*_Gamepad`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cmd {
    Jump,
    Sneak,
    Use,
    Lean,
    Block,
    Wheel,
    Primary,
    Secondary,
    Sprint,
    Zoom,
}

/// The control schemes (`m_PadBindingSet1`..`4`): A, B, X, Y, the bumpers, the triggers, the
/// sticks' clicks.
fn scheme(set: u8) -> [(GamepadButton, Cmd); 10] {
    use Cmd::*;
    use GamepadButton as B;
    match set {
        1 => [(B::South, Jump), (B::East, Sneak), (B::West, Use), (B::North, Lean), (B::RightTrigger, Primary), (B::LeftTrigger, Secondary), (B::LeftTrigger2, Wheel), (B::RightTrigger2, Block), (B::LeftThumb, Sprint), (B::RightThumb, Zoom)],
        2 => [(B::South, Jump), (B::East, Sneak), (B::West, Use), (B::North, Lean), (B::LeftTrigger, Block), (B::RightTrigger, Wheel), (B::LeftTrigger2, Secondary), (B::RightTrigger2, Primary), (B::LeftThumb, Sprint), (B::RightThumb, Zoom)],
        3 => [(B::South, Use), (B::East, Lean), (B::West, Jump), (B::North, Wheel), (B::LeftTrigger, Sprint), (B::RightTrigger, Secondary), (B::LeftTrigger2, Block), (B::RightTrigger2, Primary), (B::LeftThumb, Sneak), (B::RightThumb, Zoom)],
        _ => [(B::South, Jump), (B::East, Sneak), (B::West, Use), (B::North, Lean), (B::RightTrigger, Block), (B::LeftTrigger, Wheel), (B::LeftTrigger2, Secondary), (B::RightTrigger2, Primary), (B::LeftThumb, Sprint), (B::RightThumb, Zoom)],
    }
}

/// The sticks' dead zone, the menus' steering threshold and repeat.
const DEAD: f32 = 0.2;
const STEER: f32 = 0.5;
const STEER_FIRST: f32 = 0.4;
const STEER_EVERY: f32 = 0.12;
/// UE3's rotation units a radian (65536 a turn), and its axis scale (`TimeScale`: 100 a second).
const UNITS: f32 = 65536.0 / std::f32::consts::TAU;

fn dead(v: Vec2) -> Vec2 {
    let m = v.length();
    if m < DEAD {
        Vec2::ZERO
    } else {
        v / m * ((m - DEAD) / (1.0 - DEAD)).min(1.0)
    }
}

#[allow(clippy::too_many_arguments)]
fn pad_input(
    mut pad: ResMut<Pad>,
    mut vpad: ResMut<VirtualPad>,
    pads: Query<&Gamepad>,
    (mut keys, mut mouse, mut taps): (ResMut<ButtonInput<KeyCode>>, ResMut<ButtonInput<MouseButton>>, ResMut<crate::helpbar::Taps>),
    motion: Res<bevy::input::mouse::AccumulatedMouseMotion>,
    (bind, settings, time): (Res<Bindings>, Res<crate::settings::Settings>, Res<Time<Real>>),
    (menu, journal, store, note, end): (Res<crate::menu::Menu>, Res<crate::journal::Journal>, Res<crate::store::Store>, Res<crate::notescreen::NoteScreen>, Option<Res<crate::mission::MissionEnd>>),
    wheel: Res<crate::wheel::Wheel>,
) {
    let dt = time.delta_secs();
    // the scripts' pad (tests) over the real one
    vpad.buttons.retain_mut(|(_, t)| {
        *t -= dt;
        *t > 0.0
    });
    let tick = |s: &mut Option<(Vec2, f32)>| {
        if let Some((_, t)) = s {
            *t -= dt;
            if *t <= 0.0 {
                *s = None;
            }
        }
    };
    tick(&mut vpad.left);
    tick(&mut vpad.right);
    let real = pads.iter().next();
    let pressed = |b: GamepadButton| real.is_some_and(|g| g.pressed(b)) || vpad.buttons.iter().any(|(x, _)| *x == b);
    let left = dead(vpad.left.map(|s| s.0).unwrap_or_else(|| real.map_or(Vec2::ZERO, |g| Vec2::new(g.get(GamepadAxis::LeftStickX).unwrap_or(0.0), g.get(GamepadAxis::LeftStickY).unwrap_or(0.0)))));
    let right = dead(vpad.right.map(|s| s.0).unwrap_or_else(|| real.map_or(Vec2::ZERO, |g| Vec2::new(g.get(GamepadAxis::RightStickX).unwrap_or(0.0), g.get(GamepadAxis::RightStickY).unwrap_or(0.0)))));
    use GamepadButton as B;
    const ALL: [GamepadButton; 16] = [B::South, B::East, B::West, B::North, B::LeftTrigger, B::RightTrigger, B::LeftTrigger2, B::RightTrigger2, B::LeftThumb, B::RightThumb, B::Select, B::Start, B::DPadUp, B::DPadDown, B::DPadLeft, B::DPadRight];
    let down: Vec<GamepadButton> = ALL.iter().copied().filter(|b| pressed(*b)).collect();
    let prev = std::mem::take(&mut pad.prev);
    let just = |b: GamepadButton| down.contains(&b) && !prev.contains(&b);
    let any_pad = !down.is_empty() || left != Vec2::ZERO || right != Vec2::ZERO;
    // (who drove last: the pad, or the keyboard and mouse)
    let kbm = (keys.get_just_pressed().any(|k| !pad.held.contains(&Emu::Key(*k)))) || motion.delta.length() > 2.0 || mouse.get_just_pressed().any(|m| !pad.held.contains(&Emu::Mouse(*m)));
    if any_pad {
        pad.using = true;
    } else if kbm {
        pad.using = false;
    }
    USING.store(pad.using, std::sync::atomic::Ordering::Relaxed);
    let ui = menu.open.is_some() || journal.open || store.open.is_some() || note.open.is_some() || end.is_some();
    let mut want: Vec<Emu> = Vec::new();
    let mut pulse: Option<KeyCode> = None;
    pad.walk = Vec2::ZERO;
    pad.look = Vec2::ZERO;
    pad.sticks = [left, right];
    if ui {
        pad.sprint = false;
        // the menus: A Enter, B (and Start) Escape, X and Y the Shift and Alt taps, the
        // shoulders the tabs, the triggers the sub tabs
        if pressed(B::South) {
            want.push(Emu::Key(KeyCode::Enter));
        }
        if pressed(B::East) || pressed(B::Start) {
            want.push(Emu::Key(KeyCode::Escape));
        }
        if journal.open && pressed(B::Select) {
            want.push(Emu::Key(bind.key(Act::Journal)));
        }
        if pressed(B::RightTrigger) {
            want.push(Emu::Key(KeyCode::Tab));
        }
        if pressed(B::LeftTrigger) {
            want.extend([Emu::Key(KeyCode::ShiftLeft), Emu::Key(KeyCode::Tab)]);
        }
        if pressed(B::LeftTrigger2) {
            want.push(Emu::Key(KeyCode::KeyQ));
        }
        if pressed(B::RightTrigger2) {
            want.push(Emu::Key(KeyCode::KeyE));
        }
        // the steering, repeating while held
        let dir = if pressed(B::DPadUp) || left.y > STEER {
            Some(KeyCode::ArrowUp)
        } else if pressed(B::DPadDown) || left.y < -STEER {
            Some(KeyCode::ArrowDown)
        } else if pressed(B::DPadLeft) || left.x < -STEER {
            Some(KeyCode::ArrowLeft)
        } else if pressed(B::DPadRight) || left.x > STEER {
            Some(KeyCode::ArrowRight)
        } else {
            None
        };
        pad.steer = match (dir, pad.steer) {
            (Some(d), Some((was, t))) if d == was => {
                let t = t - dt;
                if t <= 0.0 {
                    pulse = Some(d);
                    Some((d, STEER_EVERY))
                } else {
                    Some((d, t))
                }
            }
            (Some(d), _) => {
                pulse = Some(d);
                Some((d, STEER_FIRST))
            }
            (None, _) => None,
        };
    } else {
        pad.steer = None;
        // the game: the scheme's commands
        let set = settings.pad_binding_set.min(3);
        let mut lean = false;
        for (b, c) in scheme(set) {
            if !pressed(b) {
                continue;
            }
            match c {
                Cmd::Jump => want.push(Emu::Key(bind.key(Act::Jump))),
                Cmd::Sneak => want.push(Emu::Key(bind.key(Act::Crouch))),
                Cmd::Use => want.push(Emu::Key(bind.key(Act::Use))),
                Cmd::Block => want.push(Emu::Key(bind.key(Act::Block))),
                Cmd::Zoom => want.push(Emu::Key(bind.key(Act::Zoom))),
                Cmd::Wheel => want.push(Emu::Mouse(MouseButton::Middle)),
                Cmd::Primary => want.push(Emu::Mouse(MouseButton::Left)),
                Cmd::Secondary => want.push(Emu::Mouse(MouseButton::Right)),
                Cmd::Sprint => pad.sprint = true,
                Cmd::Lean => lean = true,
            }
        }
        // the lean button with the stick leans; else the stick walks (the wheel open: it
        // points at the wheel's items instead)
        if wheel.open {
        } else if lean {
            if left.x < -0.5 {
                want.push(Emu::Key(bind.key(Act::LeanLeft)));
            } else if left.x > 0.5 {
                want.push(Emu::Key(bind.key(Act::LeanRight)));
            }
        } else {
            pad.walk = left;
        }
        // (the sprint holds until he slows: `m_fStopSprint_Threshold` 0.8)
        if pad.walk.length() < 0.8 {
            pad.sprint = false;
        }
        if pad.sprint {
            want.push(Emu::Key(bind.key(Act::Sprint)));
        }
        // Back the journal, Start the pause menu, the D-pad the shortcuts
        if pressed(B::Select) {
            want.push(Emu::Key(bind.key(Act::Journal)));
        }
        if pressed(B::Start) {
            want.push(Emu::Key(KeyCode::Escape));
        }
        for (b, k) in [(B::DPadUp, KeyCode::Digit1), (B::DPadDown, KeyCode::Digit2), (B::DPadLeft, KeyCode::Digit3), (B::DPadRight, KeyCode::Digit4)] {
            if pressed(b) {
                want.push(Emu::Key(k));
            }
        }
        // the look: the turn's scale rising while the stick is held full
        let full = right.x.abs() > 0.95;
        pad.turn_scale = if full { (pad.turn_scale.max(230.0) + 500.0 * dt).min(700.0) } else { 230.0 };
        let sens_x = settings.pad_look_x.clamp(0.25, 3.0);
        let sens_y = settings.pad_look_y.clamp(0.25, 3.0);
        let invert = if settings.pad_invert_y { -1.0 } else { 1.0 };
        if !wheel.open {
            pad.look = Vec2::new(-right.x * pad.turn_scale * 100.0 * sens_x, right.y * invert * 150.0 * 100.0 * sens_y) * dt / UNITS;
        }
    }
    // press what is newly wanted, let go what is no more
    let held = std::mem::take(&mut pad.held);
    for e in held.iter().filter(|e| !want.contains(e)) {
        match *e {
            Emu::Key(k) => keys.release(k),
            Emu::Mouse(m) => mouse.release(m),
        }
    }
    for e in want.iter().filter(|e| !held.contains(e)) {
        match *e {
            Emu::Key(k) => keys.press(k),
            Emu::Mouse(m) => mouse.press(m),
        }
    }
    pad.held = want;
    // a steering pulse: pressed this frame only
    if let Some(k) = pulse {
        keys.release(k);
        keys.press(k);
        pad.held.push(Emu::Key(k));
    }
    // X and Y in the menus: the Shift and Alt taps (the help bar's X, Y)
    if ui {
        if just(B::West) {
            taps.tapped[0] = true;
        }
        if just(B::North) {
            taps.tapped[1] = true;
        }
    }
    pad.prev = down;
    if let Ok(mut h) = HELD.lock() {
        h.0 = pad.held.iter().filter_map(|e| if let Emu::Key(k) = e { Some(*k) } else { None }).collect();
        h.1 = pad.held.iter().filter_map(|e| if let Emu::Mouse(m) = e { Some(*m) } else { None }).collect();
    }
}
