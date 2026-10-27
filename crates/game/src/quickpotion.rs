//! The quick elixir prompt (`QuickPotionMenu`, the power wheel movie's `quickPotion_mc`), as
//! the HUD's tick puts it up (0x00B9F6E0, decoded for this rewrite): while Corvo is low on
//! health (`m_bPlayerLowOnHealth`: at most the HUD tweak's `m_LowHealthThreshold` points) and
//! carries a health elixir, the power wheel opens it (its mode 4) at the shortcuts' corner: the
//! D-pad with the elixir on its right slot, the arrow red (its sound, `H_quickPotion`, an
//! empty event in the bank). On PC the D-pad is the health elixir's key in a box instead
//! (`QuickPotion_DPAD.SetIcon`). The whole fades in (0.25 s), the elixir comes out to its place (Back.easeOut 0.25 s)
//! and pulses (its circle to 75 % and 20 %, its glow to 20 %, the arrow out, over 0.65 s, again
//! every 0.75 s). Drinking one meanwhile (the pad's D-pad right, or the elixir's key) plays it
//! out (`ConsumeAndClose`: the elixir's `consume`, its circle swelling to 120 % as it fades over
//! 0.35 s, the elixir back to the middle 0.25 s on, the whole fading 0.1 s after); no longer
//! low, or without elixirs, it fades (`Close`, 0.25 s). While it is up the interaction window is
//! held back (`m_bBlockInteractionWindow`).

use crate::dock::Tw;
use crate::flash::{Clip, Ease, FlashClip, MovieTimelines, PropsTo};
use crate::gameplay::PlayerStats;
use crate::GameState;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

pub struct QuickPotionPlugin;

impl Plugin for QuickPotionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<QuickPotion>()
            .add_systems(OnExit(GameState::InGame), |mut q: ResMut<QuickPotion>| *q = QuickPotion::default())
            .add_systems(Update, (prompt, layout).chain().run_if(in_state(GameState::InGame)));
    }
}

const MOVIE: &str = "powerwheel";
const SYMBOL: &str = "quickPotion_mc";
/// the potion, its circle and glow, and the arrow of its direction (`_selSlotIdx` 3: the right)
const POTION: &str = "_potion_mc";
const CIRCLE: &str = "_potion_mc._circle_mc";
const GLOW: &str = "_potion_mc._potion_mc.glow_mc";
const ARROW: &str = "dpad.dir_Right";
/// where the potion stands (its place in the clip: `_potion_props`)
const POTION_AT: Vec2 = Vec2::new(72.7, 0.0);
/// the pulse (`Anim`: 0.65 s, again 100 ms after)
const PULSE: f32 = 0.65;

#[derive(Clone, Copy, PartialEq, Default, Debug)]
enum Phase {
    #[default]
    Hidden,
    /// up: since its last pulse began (s)
    Open(f32),
    /// drunk from (`ConsumeAndClose`): since (s)
    Consuming(f32),
    /// fading (`Close`, `FadeOut`): since (s)
    Closing(f32),
}

/// The prompt: up or not (the HUD's `m_bBlockInteractionWindow` with it), and its clip.
#[derive(Resource, Default)]
pub struct QuickPotion {
    phase: Phase,
    alpha: Option<Tw>,
    root: Option<Entity>,
    /// the shortcuts' width (`shortcuts_mc`, at its 90 %): where it stands
    w: f32,
    /// health elixirs carried last frame (one fewer: drunk)
    elixirs: u32,
}

impl QuickPotion {
    /// Up (or still going): the interaction window held back.
    pub fn shown(&self) -> bool {
        self.phase != Phase::Hidden
    }
    /// Its right slot is the pad's way to drink while it is open.
    pub fn takes_dpad_right(&self) -> bool {
        matches!(self.phase, Phase::Open(_))
    }
}

#[derive(Component)]
struct PromptRoot;
#[derive(Component)]
struct PromptClip;

#[allow(clippy::too_many_arguments)]
fn prompt(
    mut commands: Commands,
    time: Res<Time<bevy::time::Real>>,
    mut q: ResMut<QuickPotion>,
    (mut stats, data, paused, possession): (ResMut<PlayerStats>, Res<crate::gamedata::Data>, Res<crate::hud::Paused>, Res<crate::possession::Possession>),
    (pad, wheel, script_ui): (Option<Res<crate::pad::Pad>>, Res<crate::wheel::Wheel>, Option<Res<crate::kismet::ScriptUi>>),
    mut timelines: ResMut<MovieTimelines>,
    mut clips: Query<&mut FlashClip, With<PromptClip>>,
    mut roots: Query<&mut Visibility, With<PromptRoot>>,
    (mut sfx, attrs, bind): (MessageWriter<crate::audio::PostEvent>, Res<crate::gamedata::Attrs>, Res<crate::bindings::Bindings>),
) {
    let dt = time.delta_secs();
    if paused.0 {
        return;
    }
    if q.root.is_some_and(|r| roots.get(r).is_err()) {
        *q = QuickPotion::default();
    }
    // the HUD's own rule: low on health, an elixir to hand, the items free (not as another
    // body, nor with the wheel up or the HUD put away by the scripts)
    let (low, _) = crate::gauges::low(&data, &stats);
    let hud_away = script_ui.as_ref().is_some_and(|u| u.hud_hidden.contains(crate::kismet::HUD_ALL));
    let want = low && stats.health_elixirs > 0 && !stats.dead && possession.host.is_none() && !wheel.open && !hud_away;
    let drunk = stats.health_elixirs < q.elixirs;
    q.elixirs = stats.health_elixirs;
    // (made once)
    if want && q.root.is_none() {
        let Some(tl) = timelines.get(MOVIE) else { return };
        let Some(c) = Clip::export(&tl, SYMBOL) else { return };
        let w = Clip::export(&tl, "shortcuts_mc").and_then(|s| FlashClip::new(MOVIE, tl.clone(), s).size("")).map(|s| s.x * 0.9).unwrap_or(218.0);
        let fc = FlashClip::new(MOVIE, tl.clone(), c).real().with_texts();
        let node = || Node { position_type: PositionType::Absolute, ..default() };
        let root = commands.spawn((PromptRoot, Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() }, GlobalZIndex(71), Visibility::Hidden, Pickable::IGNORE, DespawnOnExit(GameState::InGame))).id();
        commands.spawn((PromptClip, fc, node(), Pickable::IGNORE, ChildOf(root)));
        q.root = Some(root);
        q.w = w;
        q.alpha = Some(Tw::at(0.0));
        return;
    }
    let Some(mut fc) = clips.iter_mut().next() else { return };
    // the pad's right on the D-pad drinks from it (the input's own way while it is up)
    if q.takes_dpad_right() && pad.as_ref().is_some_and(|p| p.dpad_pressed[3]) {
        crate::powers::drink_elixir(&mut stats, &attrs, false, &mut sfx);
    }
    match q.phase {
        Phase::Hidden | Phase::Closing(_) if want => {
            // `Open`
            q.phase = Phase::Open(0.0);
            // (its `H_quickPotion` posts `UI_H_quickPotion`, an event of Bank_UI with no actions:
            // the original's prompt opens silently)
            if let Some(a) = q.alpha.as_mut() {
                a.to(1.0, 0.25, Ease::StrongOut);
            }
            fc.set("dpad", PropsTo::default().alpha(1.0));
            fc.set("dpad._keyPC_mc", PropsTo::default().scale(1.0));
            set_icon(&mut fc, (!crate::pad::using()).then(|| crate::bindings::display_bind(bind.bind(crate::bindings::Act::HealthElixir))));
            fc.tween_end(ARROW, false);
            fc.goto(ARROW, "red", false);
            fc.set(ARROW, PropsTo::default().alpha(1.0));
            fc.tween_end(POTION, false);
            fc.goto_frame(POTION, 1, false);
            fc.set(POTION, PropsTo::default().x(0.0).y(0.0).alpha(0.0));
            fc.tween(POTION, PropsTo::default().x(POTION_AT.x).y(POTION_AT.y).alpha(1.0), 0.25, Ease::BackOut);
            fc.tween_end(CIRCLE, false);
            fc.set(CIRCLE, PropsTo::default().scale(1.0).alpha(1.0));
            fc.goto_frame(CIRCLE, 1, false);
            pulse(&mut fc);
        }
        Phase::Open(_) if drunk => {
            // `ConsumeAndClose` / `PlayConsumeAnim`
            q.phase = Phase::Consuming(0.0);
            fc.goto(POTION, "consume", true);
            fc.tween_end(CIRCLE, false);
            fc.goto(CIRCLE, "consume", true);
            fc.set(CIRCLE, PropsTo::default().scale(1.0).alpha(1.0));
            fc.tween(CIRCLE, PropsTo::default().alpha(0.0).scale(1.2), 0.35, Ease::StrongOut);
        }
        Phase::Open(_) if !want => {
            // `Close` / `FadeOut`
            q.phase = Phase::Closing(0.0);
            if let Some(a) = q.alpha.as_mut() {
                a.to(0.0, 0.25, Ease::StrongOut);
            }
        }
        Phase::Open(t) => {
            // (`AnimLoop`: 100 ms after a pulse, all back and again)
            let t1 = t + dt;
            if t1 >= PULSE + 0.1 {
                fc.set(CIRCLE, PropsTo::default().scale(1.0).alpha(1.0));
                fc.set(ARROW, PropsTo::default().alpha(1.0));
                fc.set(GLOW, PropsTo::default().alpha(1.0));
                pulse(&mut fc);
                q.phase = Phase::Open(0.0);
            } else {
                q.phase = Phase::Open(t1);
            }
        }
        Phase::Consuming(t) => {
            let t1 = t + dt;
            // (`PlayConsumeAnimEnd`, 0.25 s on: the elixir back to the middle; `FadeOut` 0.1 s
            // after)
            if t < 0.25 && t1 >= 0.25 {
                fc.tween(POTION, PropsTo::default().alpha(0.0).x(0.0).y(0.0), 0.25, Ease::StrongOut);
            }
            if t < 0.35 && t1 >= 0.35 {
                if let Some(a) = q.alpha.as_mut() {
                    a.to(0.0, 0.25, Ease::StrongOut);
                }
            }
            q.phase = if t1 >= 0.6 { Phase::Hidden } else { Phase::Consuming(t1) };
        }
        Phase::Closing(t) => {
            q.phase = if t + dt >= 0.25 { Phase::Hidden } else { Phase::Closing(t + dt) };
        }
        Phase::Hidden => {}
    }
    if let Some(a) = q.alpha.as_mut() {
        a.step(dt);
    }
    if let Some(mut v) = q.root.and_then(|r| roots.get_mut(r).ok()) {
        let want = if q.phase == Phase::Hidden { Visibility::Hidden } else { Visibility::Inherited };
        if *v != want {
            *v = want;
        }
    }
}

/// The D-pad's own `SetIcon` (`QuickPotion_DPAD`): with a pad its `Xenon` frame, the arrows
/// shown; on PC (`key`: the health elixir's) its `PC` frame, the arrows hidden and the key in
/// its box: without brackets, in capitals, sized to it and centred, the box 5 wider and taller
/// than the text, the whole half its width left of 35 out.
fn set_icon(fc: &mut FlashClip, key: Option<String>) {
    const DIRS: [&str; 4] = ["dpad.dir_Up", "dpad.dir_Down", "dpad.dir_Left", "dpad.dir_Right"];
    const KEY: &str = "dpad._keyPC_mc";
    const TXT: &str = "dpad._keyPC_mc.txt";
    const BOX: &str = "dpad._keyPC_mc.bkgd_mc";
    fc.goto("dpad", if key.is_some() { "PC" } else { "Xenon" }, false);
    for d in DIRS {
        fc.set_visible(d, key.is_none());
    }
    fc.set_visible(KEY, key.is_some());
    let Some(key) = key else { return };
    fc.set_text(TXT, key.replace(['[', ']'], "").to_uppercase());
    fc.autosize.insert(TXT.into(), 0);
    let (w, h) = (fc.text_width(TXT).unwrap_or(0.0), fc.text_height(TXT).unwrap_or(0.0));
    fc.set(TXT, PropsTo::default().x(-0.5 * w));
    // (the box's own size: its `_width` and `_height` through its scale)
    if let (Some(size), Some(p)) = (fc.size(BOX), fc.props(BOX)) {
        let raw = size / Vec2::new(p.xscale.abs().max(1e-3), p.yscale.abs().max(1e-3));
        fc.set(BOX, PropsTo::default().xscale((w + 5.0) / raw.x.max(1e-3)).yscale((h + 5.0) / raw.y.max(1e-3)));
    }
    fc.set_visible(BOX, true);
    fc.set(KEY, PropsTo::default().x(-0.5 * (w + 5.0) + 35.0));
}

/// `Anim`: the circle to 75 % and 20 %, the glow to 20 %, the arrow out, over 0.65 s.
fn pulse(fc: &mut FlashClip) {
    fc.tween_end(CIRCLE, false);
    fc.tween(CIRCLE, PropsTo::default().alpha(0.2).scale(0.75), PULSE, Ease::StrongOut);
    fc.tween_end(GLOW, false);
    fc.tween(GLOW, PropsTo::default().alpha(0.2), PULSE, Ease::StrongOut);
    fc.tween_end(ARROW, false);
    fc.tween(ARROW, PropsTo::default().alpha(0.0), PULSE, Ease::StrongOut);
}

/// The prompt on the screen: where the shortcuts stand (`Open`: `shortcuts_mc`'s place, half
/// its width in from the safe area's bottom left).
fn layout(q: Res<QuickPotion>, window: Query<&Window, With<PrimaryWindow>>, mut clips: Query<(&mut FlashClip, &mut Node), With<PromptClip>>) {
    if q.phase == Phase::Hidden {
        return;
    }
    let (Ok(win), Some((mut fc, mut n))) = (window.single(), clips.iter_mut().next()) else { return };
    let ms = crate::hud::MovieSpace::of(win);
    let p = ms.px(Vec2::new(ms.min.x + 0.5 * q.w, ms.max.y - 0.5 * q.w));
    n.left = Val::Px(p.x);
    n.top = Val::Px(p.y);
    fc.scale = ms.s;
    fc.alpha = q.alpha.map(|t| t.now()).unwrap_or(0.0);
}
