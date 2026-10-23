//! The pad's shortcuts on the D-pad (`ShortcutsHandler`, the power wheel movie's
//! `shortcuts_mc`), as its ActionScript lays them out and animates them: the D-pad
//! (`lib_quickPotionDPAD_Multi` at 80 %, its arrows `dir_Up`..) with the four slots round it
//! (`slot_up`, `_down`, `_left`, `_right`, 80 out), the whole at 90 % in the safe area's
//! bottom left corner (`SetPosition`: `GetMovieSpaceCoord(0.9)`, its middle half its width in
//! from the left and up from the bottom). Each slot's disc (`ic`) shows what it holds as the
//! wheel's do (a power's `stop_pow`, the art, the count; an empty one its bare disc); the one
//! chosen grows from 75 % to 90 % (`_slotOutScaleValue`, `_slotOverScaleValue`, Strong.easeOut
//! 0.2 s), glows (`animGlow` "over"), turns its disc (`over`, `over_pow`) and lights its arrow.
//!
//! - With the wheel, a pad in use (`Open(true)`): the D-pad fades in (0.25 s) and the slots
//!   come out to their places (Back.easeOut 0.25 s); a direction puts what the wheel points at
//!   on its slot (`powers::Shortcuts::pad`). D-pad and slots fade as the wheel closes (0.25 s).
//! - Alone (a direction held: `Open(false)`): the whole fades in (0.2 s) with the direction's
//!   slot chosen. Let go (`Close`): the D-pad and the other slots fade (0.25 s); 200 ms on the
//!   slot glows ("select") and slides to the middle (Strong.easeOut 0.25 s), its arrow going
//!   out; 350 ms later the whole fades (0.25 s). What it holds is taken as it's let go.
//!
//! The clips stay made between showings, as the movie's do: a slot taken last comes back out
//! from the middle.

use crate::dock::Tw;
use crate::flash::{Clip, Ease, FlashClip, MovieTimelines, PropsTo};
use crate::gameplay::PlayerStats;
use crate::powers::Shortcuts;
use crate::ui_fonts::UiFonts;
use crate::ui_images::UiImages;
use crate::wheel::Wheel;
use crate::GameState;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

pub struct DpadPlugin;

impl Plugin for DpadPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Display>().add_systems(Update, (display, layout).chain().run_if(in_state(GameState::InGame)));
    }
}

const MOVIE: &str = "powerwheel";
const MOVIE_DLC06: &str = "powerwheel_DLC06";
const SYMBOL: &str = "shortcuts_mc";
/// The slots (up, down, left, right) and where each stands from the D-pad's middle.
const SLOTS: [&str; 4] = ["slot_up", "slot_down", "slot_left", "slot_right"];
const SLOT_AT: [Vec2; 4] = [Vec2::new(0.0, -80.0), Vec2::new(0.0, 80.0), Vec2::new(-80.0, 0.0), Vec2::new(80.0, 0.0)];
const ARROWS: [&str; 4] = ["dpad.dir_Up", "dpad.dir_Down", "dpad.dir_Left", "dpad.dir_Right"];
/// The whole's scale (the class's 90), the slots' out and over (75, 90) and the D-pad's (80).
const SCALE: f32 = 0.9;
const OUT: f32 = 0.75;
const OVER: f32 = 0.9;
const DPAD: f32 = 0.8;
/// the art's box within a disc, and the count's field (`quantity` at (-52.4, 31.55), 24 high)
const IC_IMAGE: f32 = 100.0;
const COUNT_AT: Vec2 = Vec2::new(-52.4, 31.55);
const COUNT_SIZE: f32 = 24.0;
const COUNT_COLOR: Color = Color::srgb(0xe1 as f32 / 255.0, 0xf0 as f32 / 255.0, 0xd4 as f32 / 255.0);
/// an art on a disc turned over (`over`): dark on the light disc
const ART_OVER: Color = Color::srgb(23.0 / 255.0, 25.0 / 255.0, 28.0 / 255.0);

#[derive(Clone, Copy, PartialEq, Default, Debug)]
enum Phase {
    #[default]
    Hidden,
    Open,
    /// let go (`Close`): since (s)
    Closing(f32),
    /// fading with the wheel: since (s)
    Leaving(f32),
}

#[derive(Resource, Default)]
struct Display {
    phase: Phase,
    in_wheel: bool,
    /// the slot chosen (`_selSlotIdx`)
    sel: Option<usize>,
    /// the whole's alpha
    alpha: Option<Tw>,
    /// the clip (made once) and its width as made (`_w`)
    root: Option<Entity>,
    w: f32,
}

#[derive(Component)]
struct DpadRoot;
#[derive(Component)]
struct DpadClip;
#[derive(Component)]
struct SlotArt(usize);
#[derive(Component)]
struct SlotCount(usize);

/// A slot's disc turned to what it holds (`SetIcon`): a power's `stop_pow`, else its first
/// frame.
fn rest(fc: &mut FlashClip, i: usize, power: Option<bool>) {
    let path = format!("{}.ic", SLOTS[i]);
    match power {
        Some(true) => {
            if fc.label(&path) != Some("stop_pow") {
                fc.goto(&path, "stop_pow", false);
            }
        }
        _ => {
            if fc.label(&path) != Some("default") {
                fc.goto(&path, "default", false);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn display(
    mut commands: Commands,
    time: Res<Time<bevy::time::Real>>,
    mut d: ResMut<Display>,
    shortcuts: Res<Shortcuts>,
    wheel: Res<Wheel>,
    data: Res<crate::gamedata::Data>,
    fonts: Res<UiFonts>,
    mut timelines: ResMut<MovieTimelines>,
    mut clips: Query<&mut FlashClip, With<DpadClip>>,
    mut roots: Query<&mut Visibility, With<DpadRoot>>,
) {
    let dt = time.delta_secs();
    let held = shortcuts.pad_held;
    let power = |i: usize| shortcuts.pad[i].map(|p| p.key().is_some());
    // (made once, kept between showings)
    if d.root.is_some_and(|r| roots.get(r).is_err()) {
        *d = Display::default();
    }
    let opening = match d.phase {
        Phase::Hidden | Phase::Leaving(_) | Phase::Closing(_) if wheel.open && crate::pad::using() => Some((true, None)),
        Phase::Hidden | Phase::Closing(_) if !wheel.open && held.is_some() => Some((false, held)),
        _ => None,
    };
    if let Some((in_wheel, sel)) = opening {
        if d.root.is_none() {
            let movie = data.wheel_movies().iter().copied().find(|m| timelines.get(m).is_some_and(|tl| Clip::export(&tl, SYMBOL).is_some())).unwrap_or(MOVIE);
            let Some(tl) = timelines.get(movie) else { return };
            let Some(c) = Clip::export(&tl, SYMBOL) else { return };
            let mut fc = FlashClip::new(movie, tl.clone(), c).real();
            d.w = fc.size("").map(|s| s.x * SCALE).unwrap_or(218.0);
            fc.m = [SCALE, 0.0, 0.0, SCALE, 0.0, 0.0];
            // (`Init`: the slots and the D-pad unseen, the D-pad at 80 %)
            for s in SLOTS {
                fc.set(s, PropsTo::default().alpha(0.0));
                fc.set_visible(&format!("{s}.slot_glow"), false);
            }
            fc.set("dpad", PropsTo::default().alpha(0.0).scale(DPAD));
            fc.goto("dpad", "Xenon", false);
            let node = || Node { position_type: PositionType::Absolute, ..default() };
            let root = commands.spawn((DpadRoot, Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() }, GlobalZIndex(71), Pickable::IGNORE, DespawnOnExit(GameState::InGame))).id();
            commands.spawn((DpadClip, fc, node(), Pickable::IGNORE, ChildOf(root)));
            let title: bevy::text::FontSource = fonts.title.clone().into();
            for i in 0..4 {
                commands.spawn((SlotArt(i), ImageNode::default(), node(), Visibility::Hidden, Pickable::IGNORE, ChildOf(root)));
                commands.spawn((SlotCount(i), Text::new(""), TextFont { font: title.clone(), ..default() }, TextColor(COUNT_COLOR), node(), Visibility::Hidden, Pickable::IGNORE, ChildOf(root)));
            }
            d.root = Some(root);
            d.alpha = Some(Tw::at(0.0));
            // (the clip is there next frame)
            return;
        }
        let Some(mut fc) = clips.iter_mut().next() else { return };
        // `Open`: the D-pad in, each slot out to its place at its size, the whole in
        d.phase = Phase::Open;
        d.in_wheel = in_wheel;
        fc.tween_end("dpad", true);
        fc.tween("dpad", PropsTo::default().alpha(1.0), 0.25, Ease::StrongOut);
        for (i, s) in SLOTS.iter().enumerate() {
            fc.tween_end(s, true);
            fc.goto_frame(&format!("{s}.ic.animGlow"), 1, false);
            fc.set_visible(&format!("{s}.ic.bkgd"), true);
            let k = if d.sel == Some(i) { OVER } else { OUT };
            fc.tween(s, PropsTo::default().x(SLOT_AT[i].x).y(SLOT_AT[i].y).scale(k).alpha(1.0), 0.25, Ease::BackOut);
            rest(&mut fc, i, power(i));
        }
        if let Some(a) = d.alpha.as_mut() {
            if !in_wheel {
                *a = Tw::at(0.0);
            }
            a.to(1.0, 0.2, Ease::StrongOut);
        }
        // (alone: the direction's slot chosen)
        if let Some(i) = sel {
            select(&mut d, &mut fc, i, &power);
        }
    } else if let Some(mut fc) = clips.iter_mut().next() {
        let phase = d.phase;
        match phase {
            // (with the wheel: each disc kept to what its slot holds, `SetIcons` after an
            // `AssignShortcut`)
            Phase::Open if d.in_wheel && wheel.open => {
                for i in 0..4 {
                    rest(&mut fc, i, power(i));
                }
            }
            Phase::Open if d.in_wheel && !wheel.open => {
                // (`Close` with the wheel: the D-pad and every slot fade)
                d.phase = Phase::Leaving(0.0);
                fc.tween("dpad", PropsTo::default().alpha(0.0), 0.25, Ease::StrongOut);
                for s in SLOTS {
                    fc.tween(s, PropsTo::default().alpha(0.0), 0.25, Ease::StrongOut);
                }
            }
            Phase::Open if !d.in_wheel => match held {
                Some(i) if d.sel != Some(i) => select(&mut d, &mut fc, i, &power),
                Some(_) => {}
                // let go: the D-pad and the other slots fade
                None => {
                    d.phase = Phase::Closing(0.0);
                    fc.tween_end("dpad", false);
                    fc.tween("dpad", PropsTo::default().alpha(0.0), 0.25, Ease::StrongOut);
                    for (i, s) in SLOTS.iter().enumerate() {
                        if d.sel != Some(i) {
                            fc.tween(s, PropsTo::default().alpha(0.0), 0.25, Ease::StrongOut);
                        }
                    }
                }
            },
            Phase::Closing(t) => {
                let t1 = t + dt;
                // 200 ms on (`Close_SelSlot`): the slot glows and slides to the middle, its
                // arrow out
                if t < 0.2 && t1 >= 0.2 {
                    if let Some(i) = d.sel.take() {
                        fc.goto(&format!("{}.ic.animGlow", SLOTS[i]), "select", true);
                        fc.tween(SLOTS[i], PropsTo::default().x(0.0).y(0.0), 0.25, Ease::StrongOut);
                        fc.goto(ARROWS[i], "out", true);
                    }
                }
                // 350 ms later (`Close_Mc`): the whole fades
                if t < 0.55 && t1 >= 0.55 {
                    if let Some(a) = d.alpha.as_mut() {
                        a.to(0.0, 0.25, Ease::StrongOut);
                    }
                }
                d.phase = if t1 >= 0.8 { Phase::Hidden } else { Phase::Closing(t1) };
            }
            Phase::Leaving(t) => {
                d.phase = if t + dt >= 0.25 { Phase::Hidden } else { Phase::Leaving(t + dt) };
                if d.phase == Phase::Hidden {
                    d.sel = None;
                }
            }
            _ => {}
        }
    }
    if let Some(a) = d.alpha.as_mut() {
        a.step(dt);
    }
    if let Some(mut v) = d.root.and_then(|r| roots.get_mut(r).ok()) {
        let want = if d.phase == Phase::Hidden { Visibility::Hidden } else { Visibility::Inherited };
        if *v != want {
            *v = want;
        }
    }
}

/// `SetShortcutSelection`: the slot chosen before goes back (75 %, its glow and disc out, its
/// arrow out), the new one grows (90 %), glows, turns its disc and lights its arrow.
fn select(d: &mut Display, fc: &mut FlashClip, i: usize, power: &impl Fn(usize) -> Option<bool>) {
    if let Some(was) = d.sel {
        let s = SLOTS[was];
        fc.tween_end(s, true);
        fc.tween(s, PropsTo::default().scale(OUT), 0.2, Ease::StrongOut);
        fc.goto(&format!("{s}.ic.animGlow"), "out", true);
        if let Some(pow) = power(was) {
            fc.goto(&format!("{s}.ic"), if pow { "out_pow" } else { "out" }, true);
        }
        if was != i {
            fc.goto(ARROWS[was], "out", true);
        }
    }
    d.sel = Some(i);
    let s = SLOTS[i];
    fc.goto(ARROWS[i], "on", true);
    fc.tween_end(s, true);
    fc.tween(s, PropsTo::default().scale(OVER), 0.2, Ease::StrongOut);
    fc.goto(&format!("{s}.ic.animGlow"), "over", true);
    if let Some(pow) = power(i) {
        fc.goto(&format!("{s}.ic"), if pow { "over_pow" } else { "over" }, true);
    }
}

/// The display on the screen: the clip at its corner, each slot's art and count over its disc.
#[allow(clippy::type_complexity)]
fn layout(
    d: Res<Display>,
    shortcuts: Res<Shortcuts>,
    stats: Res<PlayerStats>,
    data: Res<crate::gamedata::Data>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut clips: Query<(&mut FlashClip, &mut Node), (With<DpadClip>, Without<SlotArt>, Without<SlotCount>)>,
    mut arts: Query<(&SlotArt, &mut Node, &mut ImageNode, &mut Visibility), (Without<FlashClip>, Without<SlotCount>)>,
    mut counts: Query<(&SlotCount, &mut Text, &mut TextFont, &mut TextColor, &mut Node, &mut Visibility), (Without<FlashClip>, Without<SlotArt>)>,
    (mut ui, mut images): (ResMut<UiImages>, ResMut<Assets<Image>>),
) {
    if d.phase == Phase::Hidden {
        return;
    }
    let Ok(win) = window.single() else { return };
    let Some((mut fc, mut n)) = clips.iter_mut().next() else { return };
    let ms = crate::hud::MovieSpace::of(win);
    // (`SetPosition`: half its width in from the safe area's left and up from its bottom)
    let at = Vec2::new(ms.min.x + 0.5 * d.w, ms.max.y - 0.5 * d.w);
    let p = ms.px(at);
    n.left = Val::Px(p.x);
    n.top = Val::Px(p.y);
    fc.scale = ms.s;
    let alpha = d.alpha.map(|t| t.now()).unwrap_or(0.0);
    fc.alpha = alpha;
    // what each slot holds (`SetIcon`): its count on the disc, or none
    for i in 0..4 {
        match shortcuts.pad[i].and_then(|p| p.ammo(&stats)) {
            Some(q) => {
                fc.set_visible(&format!("{}.ic.quantity", SLOTS[i]), true);
                fc.set_text(&format!("{}.ic.quantity.txt", SLOTS[i]), q.to_string());
            }
            None => fc.set_visible(&format!("{}.ic.quantity", SLOTS[i]), false),
        }
    }
    // a slot's middle on the screen, its scale and alpha
    let slot = |i: usize| {
        let pr = fc.props(SLOTS[i]);
        let (xy, k, a) = pr.map(|q| (Vec2::new(q.x, q.y), q.xscale, q.alpha)).unwrap_or((SLOT_AT[i], OUT, 0.0));
        (p + xy * SCALE * ms.s, k * SCALE, a * alpha)
    };
    let daud = data.is_daud();
    for (art, mut an, mut img, mut vis) in &mut arts {
        let Some(item) = shortcuts.pad[art.0] else {
            *vis = Visibility::Hidden;
            continue;
        };
        let (c, k, a) = slot(art.0);
        if let Some((h, _)) = ui.file(&mut images, "icons", item.icon_for(daud)) {
            if img.image != h {
                img.image = h;
            }
        }
        *vis = Visibility::Inherited;
        let base = if d.sel == Some(art.0) { ART_OVER } else { Color::WHITE };
        img.color = base.with_alpha(a);
        let w = IC_IMAGE * k * ms.s;
        an.left = Val::Px(c.x - w * 0.5);
        an.top = Val::Px(c.y - w * 0.5);
        an.width = Val::Px(w);
        an.height = Val::Px(w);
    }
    for (count, mut t, mut f, mut col, mut cn, mut vis) in &mut counts {
        let Some(q) = shortcuts.pad[count.0].and_then(|p| p.ammo(&stats)) else {
            *vis = Visibility::Hidden;
            continue;
        };
        if t.0 != q.to_string() {
            t.0 = q.to_string();
        }
        *vis = Visibility::Inherited;
        let (c, k, a) = slot(count.0);
        let fs = bevy::text::FontSize::Px(COUNT_SIZE * k * ms.s);
        if f.font_size != fs {
            f.font_size = fs;
        }
        col.0 = COUNT_COLOR.with_alpha(a);
        let q = c + COUNT_AT * k * ms.s;
        cn.left = Val::Px(q.x);
        cn.top = Val::Px(q.y - COUNT_SIZE * k * ms.s);
    }
}
