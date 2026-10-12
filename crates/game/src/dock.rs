//! The shortcut dock (`PC_ShortcutsHandler`, the power wheel movie's `shPC_`), as its own
//! ActionScript lays it out and animates it. It sits at the bottom middle of the stage
//! (`shortcutsPC_mc` at (Stage.width / 2, Stage.height)), with ten `shPC_item` slots in a row
//! (`MovieClipCreator.CreateElements`: centred, `_offsetOriginY` -25, `_offsetX` -5; the row
//! centred on the dock, half its height and 22.5 above it). Each slot shows its key (1 .. 9, 0)
//! and, when it holds something, its disc (`ic` at 47.5 %: a power's `stop_pow`, else the
//! first frame), its art and its count. An empty one keeps only its key and frame.
//!
//! - With the wheel (`Open(true)`) it rises 40 into place with its backing (Back.easeOut 0.2
//!   s), the slots to 100 %; it goes as the wheel closes.
//! - Alone (a number key held, the mouse wheel stepping: `Open()`) it rises 10 with the backing
//!   at 30 %, the slot shown at 110 %. When the slot is taken the others fade (0.25 s); 200 ms
//!   on, the slot glows (`animGlow` "select") and rises 20 (Strong.easeOut 0.25 s); 350 ms
//!   later the dock fades and drops (0.2 s).
//!
//! The slots themselves are `powers::Shortcuts` (decoded by roshan's r614/blink research).

use crate::flash::{Clip, Ease, FlashClip, MovieTimelines};
use crate::gameplay::PlayerStats;
use crate::powers::Shortcuts;
use crate::ui_images::UiImages;
use crate::wheel::Wheel;
use crate::GameState;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

pub struct DockPlugin;

impl Plugin for DockPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Dock>().add_systems(Update, (dock, layout).chain().run_if(in_state(GameState::InGame)));
    }
}

const MOVIE: &str = "powerwheel";
/// The dock's place on the stage, and how far below it it comes from (with the wheel, alone).
const AT: Vec2 = Vec2::new(640.0, 720.0);
const RISE: [f32; 2] = [40.0, 10.0];
/// `MovieClipCreator`'s spacing and the row's place.
const OFFSET_X: f32 = -5.0;
const ORIGIN_Y: f32 = -25.0;
const ROW_DY: f32 = -22.5;
/// a slot shown (`_slotOverScaleValue`) and the others (`_slotOutScaleValue`)
const OVER: f32 = 1.1;
/// the disc on a slot (`_assignedSlotIcProps`) and the art within it
const IC_SCALE: f32 = 0.475;
const IC_IMAGE: f32 = 100.0;

/// A value eased from one to another over a time.
#[derive(Clone, Copy)]
struct Tw {
    from: f32,
    to: f32,
    t: f32,
    dur: f32,
    ease: Ease,
}

impl Tw {
    fn at(v: f32) -> Tw {
        Tw { from: v, to: v, t: 0.0, dur: 0.0, ease: Ease::None }
    }
    fn now(&self) -> f32 {
        if self.dur <= 0.0 {
            return self.to;
        }
        self.from + (self.to - self.from) * self.ease.at(self.t / self.dur)
    }
    fn to(&mut self, to: f32, dur: f32, ease: Ease) {
        *self = Tw { from: self.now(), to, t: 0.0, dur, ease };
    }
    fn step(&mut self, dt: f32) {
        self.t = (self.t + dt).min(self.dur);
    }
}

#[derive(Clone, Copy, PartialEq, Default)]
enum Phase {
    #[default]
    Hidden,
    Open,
    /// the slot taken: since (s)
    Taking(f32),
    /// fading with the wheel
    Leaving(f32),
}

#[derive(Resource, Default)]
struct Dock {
    phase: Phase,
    in_wheel: bool,
    sel: Option<usize>,
    alpha: Option<Tw>,
    rise: Option<Tw>,
    backing: f32,
    slots: Vec<SlotTw>,
    /// a slot's size as it's made (`_width`, `_height`) and its own bounds' corners
    slot_size: Option<(Vec2, Vec2, Vec2)>,
}

#[derive(Clone, Copy)]
struct SlotTw {
    alpha: Tw,
    scale: Tw,
    dy: Tw,
}

impl SlotTw {
    fn new() -> SlotTw {
        SlotTw { alpha: Tw::at(1.0), scale: Tw::at(1.0), dy: Tw::at(0.0) }
    }
}

#[derive(Component)]
struct DockRoot;
#[derive(Component)]
struct Backing;
#[derive(Component)]
struct Slot(usize);
#[derive(Component)]
struct SlotArt(usize);
/// a slot's key (`key_mc.txt`) and count (`ic.quantity.txt`), drawn as text
#[derive(Component)]
struct SlotKey(usize);
#[derive(Component)]
struct SlotCount(usize);

/// The key's middle and size in a slot (`key_mc` at (-26.85, -22.2), its field `$NormalFont`
/// 20 at 80 %), the count's (the disc's `quantity` field, `$NormalFont` 28 centred, at 47.5 %).
const KEY_AT: Vec2 = Vec2::new(-26.85, -22.2);
const KEY_SIZE: f32 = 20.0;
const KEY_COLOR: [f32; 4] = [0.890_196_1, 0.949_019_6, 0.839_215_7, 0.8];
const COUNT_AT: Vec2 = Vec2::new(-25.0, 14.5);
const COUNT_SIZE: f32 = 28.0 * IC_SCALE;

#[allow(clippy::too_many_arguments)]
fn dock(
    mut commands: Commands,
    time: Res<Time>,
    mut d: ResMut<Dock>,
    shortcuts: Res<Shortcuts>,
    wheel: Res<Wheel>,
    mut timelines: ResMut<MovieTimelines>,
    roots: Query<Entity, With<DockRoot>>,
    mut slots: Query<(&Slot, &mut FlashClip)>,
) {
    let dt = time.delta_secs();
    let shown = shortcuts.held.or(shortcuts.stepping.map(|(s, _)| s));
    let mut opened = false;
    let mut open = |d: &mut Dock, commands: &mut Commands, in_wheel: bool, sel: Option<usize>| {
        opened = true;
        if d.phase == Phase::Hidden {
            let Some(tl) = timelines.get(MOVIE) else { return };
            let node = || Node { position_type: PositionType::Absolute, ..default() };
            let root = commands.spawn((DockRoot, Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() }, GlobalZIndex(71), Pickable::IGNORE, DespawnOnExit(GameState::InGame))).id();
            if let Some(c) = Clip::export(&tl, "shPC_") {
                commands.spawn((Backing, FlashClip::new(MOVIE, tl.clone(), c).real(), node(), Pickable::IGNORE, ChildOf(root)));
            }
            for i in 0..10 {
                let Some(c) = Clip::export(&tl, "shPC_item") else { break };
                let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real();
                if d.slot_size.is_none() {
                    if let (Some(size), Some((a, b))) = (fc.size(""), fc.clip.bounds(&tl)) {
                        d.slot_size = Some((size, a, b));
                    }
                }
                // (its key's round backing is a vector shape: the key drawn as text instead)
                fc.set_visible("key_mc", false);
                commands.spawn((Slot(i), fc, node(), Pickable::IGNORE, ChildOf(root)));
                commands.spawn((SlotArt(i), ImageNode::default(), node(), Visibility::Hidden, Pickable::IGNORE, ChildOf(root)));
                commands.spawn((SlotKey(i), Text::new(((i + 1) % 10).to_string()), TextFont::default(), TextColor(Color::NONE), node(), Pickable::IGNORE, ChildOf(root)));
                commands.spawn((SlotCount(i), Text::new(""), TextFont::default(), TextColor(Color::NONE), node(), Visibility::Hidden, Pickable::IGNORE, ChildOf(root)));
            }
            d.slots = vec![SlotTw::new(); 10];
            d.alpha = Some(Tw::at(0.0));
            d.rise = Some(Tw::at(RISE[if in_wheel { 0 } else { 1 }]));
        }
        // (`Open`: the dock up into place, its backing, the slots to their sizes)
        d.phase = Phase::Open;
        d.in_wheel = in_wheel;
        d.sel = sel;
        d.backing = if in_wheel { 1.0 } else { 0.3 };
        if let Some(a) = d.alpha.as_mut() {
            a.to(1.0, 0.2, Ease::BackOut);
        }
        if let Some(r) = d.rise.as_mut() {
            *r = Tw::at(RISE[if in_wheel { 0 } else { 1 }]);
            r.to(0.0, 0.2, Ease::BackOut);
        }
        for (i, s) in d.slots.iter_mut().enumerate() {
            s.alpha.to(1.0, 0.25, Ease::BackOut);
            s.scale.to(if sel == Some(i) { OVER } else { 1.0 }, 0.25, Ease::BackOut);
            s.dy = Tw::at(0.0);
        }
    };
    let phase = d.phase;
    match phase {
        Phase::Hidden | Phase::Leaving(_) if wheel.open => open(&mut *d, &mut commands, true, None),
        Phase::Hidden => {
            if let Some(s) = shown {
                open(&mut *d, &mut commands, false, Some(s));
            }
        }
        Phase::Open if d.in_wheel && !wheel.open => {
            d.phase = Phase::Leaving(0.0);
            if let Some(a) = d.alpha.as_mut() {
                a.to(0.0, 0.2, Ease::StrongOut);
            }
        }
        Phase::Open if !d.in_wheel => match shown {
            // the slot shown moves: it grows, the one before goes back
            Some(s) if d.sel != Some(s) => {
                d.sel = Some(s);
                for (i, t) in d.slots.iter_mut().enumerate() {
                    t.scale.to(if i == s { OVER } else { 1.0 }, 0.25, Ease::BackOut);
                }
            }
            Some(_) => {}
            // taken (or let go): the others fade
            None => {
                d.phase = Phase::Taking(0.0);
                let sel = d.sel;
                for (i, t) in d.slots.iter_mut().enumerate() {
                    if Some(i) != sel {
                        t.alpha.to(0.0, 0.25, Ease::StrongOut);
                    }
                }
            }
        },
        Phase::Taking(t) => {
            if wheel.open {
                open(&mut *d, &mut commands, true, None);
            } else if let Some(s) = shown {
                open(&mut *d, &mut commands, false, Some(s));
            } else {
                let t1 = t + dt;
                // 200 ms on the slot glows and rises; 350 ms later the dock fades and drops
                if t < 0.2 && t1 >= 0.2 {
                    if let Some(i) = d.sel {
                        if let Some(st) = d.slots.get_mut(i) {
                            st.dy.to(-20.0, 0.25, Ease::StrongOut);
                        }
                        for (s, mut fc) in &mut slots {
                            if s.0 == i {
                                fc.goto("ic.animGlow", "select", true);
                            }
                        }
                    }
                }
                if t < 0.55 && t1 >= 0.55 {
                    if let Some(a) = d.alpha.as_mut() {
                        a.to(0.0, 0.2, Ease::StrongOut);
                    }
                    let r = RISE[1];
                    if let Some(rise) = d.rise.as_mut() {
                        rise.to(r, 0.2, Ease::StrongOut);
                    }
                }
                d.phase = Phase::Taking(t1);
                if t1 >= 0.75 {
                    d.phase = Phase::Hidden;
                }
            }
        }
        Phase::Leaving(t) => {
            d.phase = Phase::Leaving(t + dt);
            if t + dt >= 0.2 {
                d.phase = Phase::Hidden;
            }
        }
        _ => {}
    }
    if opened {
        // (the glows back to their first frame)
        for (_, mut fc) in &mut slots {
            fc.goto_frame("ic.animGlow", 1, false);
        }
    }
    if d.phase == Phase::Hidden {
        for e in &roots {
            commands.entity(e).despawn();
        }
        return;
    }
    if let Some(a) = d.alpha.as_mut() {
        a.step(dt);
    }
    if let Some(r) = d.rise.as_mut() {
        r.step(dt);
    }
    for s in d.slots.iter_mut() {
        s.alpha.step(dt);
        s.scale.step(dt);
        s.dy.step(dt);
    }
}

/// The dock on the screen: its backing and slots placed, what each holds shown.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn layout(
    d: Res<Dock>,
    shortcuts: Res<Shortcuts>,
    stats: Res<PlayerStats>,
    data: Res<crate::gamedata::Data>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut clips: Query<(&mut FlashClip, &mut Node, Option<&Slot>, Has<Backing>), Without<SlotArt>>,
    mut arts: Query<(&SlotArt, &mut Node, &mut ImageNode, &mut Visibility), Without<FlashClip>>,
    (mut ui, mut images): (ResMut<UiImages>, ResMut<Assets<Image>>),
    mut texts: Query<(Option<&SlotKey>, Option<&SlotCount>, &mut Text, &mut TextFont, &mut TextColor, &mut Node, &ComputedNode, &mut Visibility), (Without<FlashClip>, Without<SlotArt>)>,
) {
    if d.phase == Phase::Hidden {
        return;
    }
    let Ok(win) = window.single() else { return };
    let Some((size, a, b)) = d.slot_size else { return };
    let s = (win.width() / 1280.0).min(win.height() / 720.0).max(0.01);
    let off = (Vec2::new(win.width(), win.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    let root_alpha = d.alpha.map(|t| t.now()).unwrap_or(0.0);
    let root = AT + Vec2::new(0.0, d.rise.map(|t| t.now()).unwrap_or(0.0));
    // the row (`CreateElements` centred, then centred on the dock)
    let slot_x = |i: usize| (size.x * 0.5 + i as f32 * (size.x + OFFSET_X)).round();
    let slot_y = (ORIGIN_Y + size.y * 0.5).round();
    let width = (slot_x(9) + b.x) - (slot_x(0) + a.x);
    let row = Vec2::new(-0.5 * width, -0.5 * (b.y - a.y) + ROW_DY);
    let daud = data.is_daud();
    let place = |i: usize| {
        let t = d.slots.get(i).copied().unwrap_or(SlotTw::new());
        (root + row + Vec2::new(slot_x(i), slot_y + t.dy.now()), t)
    };
    for (mut fc, mut n, slot, backing) in &mut clips {
        fc.scale = s;
        let at = if backing {
            fc.alpha = root_alpha * d.backing;
            root
        } else if let Some(sl) = slot {
            let (at, t) = place(sl.0);
            let k = t.scale.now();
            fc.m = [k, 0.0, 0.0, k, 0.0, 0.0];
            fc.alpha = root_alpha * t.alpha.now();
            // what it holds (`SetIcon`): a power's disc, else the first frame; its count; or
            // empty (its disc hidden)
            match shortcuts.slots[sl.0] {
                Some(p) => {
                    if p.key().is_some() {
                        if fc.label("ic") != Some("stop_pow") {
                            fc.goto("ic", "stop_pow", false);
                        }
                    } else {
                        fc.goto_frame("ic", 1, false);
                    }
                    fc.set("ic", crate::flash::PropsTo::default().alpha(1.0).scale(IC_SCALE));
                    match p.ammo(&stats) {
                        Some(n) => {
                            fc.set_visible("ic.quantity", true);
                            fc.set_text("ic.quantity.txt", n.to_string());
                        }
                        None => fc.set_visible("ic.quantity", false),
                    }
                }
                None => {
                    fc.set("ic", crate::flash::PropsTo::default().alpha(0.0).scale(0.6));
                    fc.set_visible("ic.quantity", false);
                }
            }
            at
        } else {
            continue;
        };
        let p = off + at * s;
        n.left = Val::Px(p.x);
        n.top = Val::Px(p.y);
    }
    // the keys and counts, centred on their fields
    for (key, count, mut text, mut font, mut color, mut n, cn, mut vis) in &mut texts {
        let (i, at, size, rgba) = match (key, count) {
            (Some(k), _) => (k.0, KEY_AT, KEY_SIZE, KEY_COLOR),
            (_, Some(c)) => {
                match shortcuts.slots[c.0].and_then(|p| p.ammo(&stats)) {
                    Some(q) => {
                        if text.0 != q.to_string() {
                            text.0 = q.to_string();
                        }
                        *vis = Visibility::Inherited;
                    }
                    None => {
                        *vis = Visibility::Hidden;
                        continue;
                    }
                }
                (c.0, COUNT_AT, COUNT_SIZE, [1.0, 1.0, 1.0, 1.0])
            }
            _ => continue,
        };
        let (base, t) = place(i);
        let k = t.scale.now();
        let fs = bevy::text::FontSize::Px(size * k * s);
        if font.font_size != fs {
            font.font_size = fs;
        }
        color.0 = Color::srgba(rgba[0], rgba[1], rgba[2], rgba[3] * root_alpha * t.alpha.now());
        let wh = cn.size() * cn.inverse_scale_factor();
        let c = off + (base + at * k) * s;
        n.left = Val::Px(c.x - wh.x * 0.5);
        n.top = Val::Px(c.y - wh.y * 0.5);
    }
    for (art, mut n, mut img, mut vis) in &mut arts {
        let Some(p) = shortcuts.slots[art.0] else {
            *vis = Visibility::Hidden;
            continue;
        };
        let (at, t) = place(art.0);
        if let Some((h, _)) = ui.file(&mut images, "icons", p.icon_for(daud)) {
            if img.image != h {
                img.image = h;
            }
        }
        *vis = Visibility::Inherited;
        img.color = Color::srgba(1.0, 1.0, 1.0, root_alpha * t.alpha.now());
        let w = IC_IMAGE * IC_SCALE * t.scale.now() * s;
        let c = off + (at + Vec2::new(-0.1, 0.0) * t.scale.now()) * s;
        n.left = Val::Px(c.x - w * 0.5);
        n.top = Val::Px(c.y - w * 0.5);
        n.width = Val::Px(w);
        n.height = Val::Px(w);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tween_eases_to_its_end() {
        let mut t = Tw::at(0.0);
        t.to(1.0, 0.2, Ease::StrongOut);
        t.step(0.1);
        assert!(t.now() > 0.5 && t.now() < 1.0);
        t.step(0.2);
        assert!((t.now() - 1.0).abs() < 1e-6);
    }
}
