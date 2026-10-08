//! The HUD effects movie's damage feedback (`UI_HUDFX.HUDFX`, its `DirectionalDamages` class)
//! and Corvo's health effects.
//!
//! Hurt, the screen's edges redden (`_vignette_mc`: the movie's edge strip on all four sides,
//! the sides at a third, its alpha raised by `0.01·d² + 0.4·d − 1` per hit of `d`, held
//! `max(40·d ms, 2.5 s)` then faded over 3.5 s). A hit sprite (`dirHitSprite`, played as
//! authored by `flash`) lands on the screen's edge towards the blow, in from further out and
//! larger: the lighter splatter under 20 damage, the heavier above with the cracked glass coming
//! up over it, the glass shaking and blood flying off (`particles_mc`: more, and the glass
//! glinting, above 50, 70 and 90); held `max(35·d ms, 1.5 s)`, faded over 1.2 s. The arc
//! (`_indic_mc`) points the way at seven tenths of the way out, flashing in (`hit`) and
//! fading (`close`). Low on health, his combat tweak's `m_HealthEffects` put their looping
//! camera lens effects up (blood at the screen's edges below a third, more below a sixth)
//! with their sounds; his eyes coming out of the water, `m_pWaterExitEffectTweaks` streams it
//! down the lens.

use crate::flash::{concat, turn_scale, Clip, FlashClip, Mat, MovieTimelines};
use crate::gameplay::PlayerStats;
use crate::level::LevelInfo;
use crate::player::{Player, PlayerCamera};
use crate::GameState;
use bevy::prelude::*;
use bevy::ui::widget::NodeImageMode;
use bevy::window::PrimaryWindow;

pub struct HudFxPlugin;

impl Plugin for HudFxPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DamageFx>()
            .add_systems(OnEnter(GameState::InGame), spawn_damage_fx)
            // (after the frame's blows, before the clips play and the interface's layout)
            .add_systems(PostUpdate, (take_damage, animate_damage).chain().before(crate::flash::PlayClips).run_if(in_state(GameState::InGame)))
            .add_systems(Update, (health_effects, water_exit_lens, adrenaline_full).run_if(in_state(GameState::InGame)));
    }
}

/// The movie's stage (its effects are laid out on it and scaled to fit the screen).
const STAGE: Vec2 = Vec2::new(1280.0, 720.0);
const MOVIE: &str = "HUDFX";
/// the edge strip's bitmap
const STRIP: u32 = 31;
/// the vignette's sides (`R_mc`, `L_mc`: alpha 84/256)
const SIDE_ALPHA: f32 = 84.0 / 256.0;
/// the hit sprite and the arc
const HIT_SPRITE: &str = "dirHitSprite";
const ARC_SPRITE: u16 = 39;
/// the arc's `close` (frames 26-48 at 30 per second)
const ARC_CLOSE: f32 = 23.0 / 30.0;
/// hit sprites in the pool (`_maxHitSprites`)
const MAX_HITS: usize = 10;
/// The lens effects' ops (above any level script op): health effects, then the water's.
pub const GAMEPLAY_LENS: u32 = 0xF000_0000;

/// `mx.transitions.easing.Strong` (quintic).
#[derive(Clone, Copy, Default, PartialEq)]
enum Ease {
    #[default]
    Out,
    InOut,
}

/// A Flash tween of one value.
#[derive(Clone, Copy, Default)]
struct Tween {
    from: f32,
    to: f32,
    t: f32,
    secs: f32,
    ease: Ease,
}

impl Tween {
    fn value(&self) -> f32 {
        let x = if self.secs <= 0.0 { 1.0 } else { (self.t / self.secs).clamp(0.0, 1.0) };
        let k = match self.ease {
            Ease::Out => 1.0 - (1.0 - x).powi(5),
            Ease::InOut => {
                if x < 0.5 {
                    16.0 * x.powi(5)
                } else {
                    1.0 - (-2.0 * x + 2.0).powi(5) / 2.0
                }
            }
        };
        self.from + (self.to - self.from) * k
    }
    fn start(&mut self, to: f32, secs: f32, ease: Ease) {
        *self = Tween { from: self.value(), to, t: 0.0, secs, ease };
    }
    fn step(&mut self, dt: f32) {
        self.t += dt;
    }
}

/// `Strong.easeIn` over `secs`.
fn ease_in(t: f32, secs: f32) -> f32 {
    (t / secs).clamp(0.0, 1.0).powi(5)
}

/// One hit sprite: where on the stage (from its middle, y up), its angle, how hard.
#[derive(Clone, Copy, Default)]
struct Hit {
    on: bool,
    /// its clips set going (`PlayHitAnim`)
    started: bool,
    at: Vec2,
    angle: f32,
    damage: f32,
    age: f32,
    hold: f32,
    fade: Option<Tween>,
}

/// The arc: angle, where, age, how long it holds before closing, set going, closing.
#[derive(Clone, Copy)]
struct ArcState {
    angle: f32,
    at: Vec2,
    age: f32,
    hold: f32,
    started: bool,
    closing: bool,
}

#[derive(Resource, Default)]
struct DamageFx {
    last_health: Option<f32>,
    vignette: Tween,
    vignette_hold: Option<f32>,
    hits: Vec<Hit>,
    next: usize,
    arc: Option<ArcState>,
    /// the health effect in force
    health_fx: Option<usize>,
}

#[derive(Component)]
struct Strip(u8);
/// a hit sprite of the pool, and its big glass's own matrix
#[derive(Component)]
struct HitSprite(usize, Mat);
#[derive(Component)]
struct Arc;

/// The stage's scale and offset on the screen.
fn stage_fit(w: &Window) -> (f32, Vec2) {
    let size = Vec2::new(w.width(), w.height());
    let s = (size.x / STAGE.x).min(size.y / STAGE.y);
    (s, (size - STAGE * s) * 0.5)
}

/// `GetClippedPosition`: the point towards a hit's angle on the square from the middle
/// (`ratio` of the way out; x right, y up).
fn clipped(angle: f32, ratio: f32) -> Vec2 {
    let (s, c) = angle.to_radians().sin_cos();
    let t = if c.abs() > 0.0 { s / c.abs() } else { 1.0 };
    if t.abs() < 1.0 {
        Vec2::new(t * ratio, if c < 0.0 { -ratio } else { ratio })
    } else {
        let u = if s.abs() > 0.0 { c / s.abs() } else { 1.0 };
        Vec2::new(if s < 0.0 { -ratio } else { ratio }, u * ratio)
    }
}

fn spawn_damage_fx(mut commands: Commands, mut ui: ResMut<crate::ui_images::UiImages>, mut images: ResMut<Assets<Image>>, mut timelines: ResMut<MovieTimelines>, mut fx: ResMut<DamageFx>) {
    *fx = DamageFx { hits: vec![Hit::default(); MAX_HITS], ..default() };
    let Some((strip, _)) = ui.get(&mut images, MOVIE, STRIP) else { return };
    let base = (Node { position_type: PositionType::Absolute, ..default() }, GlobalZIndex(-1), Pickable::IGNORE, DespawnOnExit(GameState::InGame));
    // the vignette's four sides: top, bottom (turned half round), right (a quarter), left
    // (a quarter back, mirrored)
    for (i, (fx_, fy)) in [(false, false), (true, true), (false, false), (true, false)].into_iter().enumerate() {
        let mut im = ImageNode::new(strip.clone()).with_color(Color::srgba(1.0, 1.0, 1.0, 0.0)).with_mode(NodeImageMode::Stretch);
        im.flip_x = fx_;
        im.flip_y = fy;
        commands.spawn((Strip(i as u8), im, base.clone(), UiTransform::default()));
    }
    let Some(tl) = timelines.get(MOVIE) else { return };
    for k in 0..MAX_HITS {
        let Some(clip) = Clip::export(&tl, HIT_SPRITE) else { break };
        let big = clip.placed("big_glassBreak_mc").map(|p| p.0).unwrap_or(crate::flash::IDENTITY);
        commands.spawn((HitSprite(k, big), FlashClip::new(MOVIE, tl.clone(), clip), base.clone(), Visibility::Hidden));
    }
    if tl.sprites.contains_key(&ARC_SPRITE) {
        commands.spawn((Arc, FlashClip::new(MOVIE, tl.clone(), Clip::new(&tl, ARC_SPRITE)), base.clone(), Visibility::Hidden));
    }
}

/// `TakeDamage`: each drop of Corvo's health, from where the blow came.
fn take_damage(
    mut stats: ResMut<PlayerStats>,
    mut fx: ResMut<DamageFx>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    player: Query<&Transform, With<Player>>,
) {
    let health = stats.health;
    let last = fx.last_health.replace(health);
    let from = stats.hit_from.take();
    let Some(last) = last else { return };
    let damage = last - health;
    if damage <= 0.01 || fx.hits.is_empty() || std::env::var("DH_NO_HUDFX").is_ok() {
        return;
    }
    // the blow's angle on the screen: 0 ahead (up), clockwise; unknown, from below
    let angle = match (from, cam.single(), player.single()) {
        (Some(f), Ok(c), Ok(p)) => {
            let to = (f - p.translation).with_y(0.0);
            let fwd = c.forward().as_vec3().with_y(0.0).normalize_or_zero();
            let right = c.right().as_vec3().with_y(0.0).normalize_or_zero();
            if to.length() < 0.05 {
                180.0
            } else {
                to.dot(right).atan2(to.dot(fwd)).to_degrees()
            }
        }
        _ => 180.0,
    };
    // `ShowVignette`
    let add = (0.01 * damage * damage + 0.4 * damage - 1.0) / 100.0;
    let target = (fx.vignette.value() + add).clamp(0.0, 1.0);
    fx.vignette.start(target, 0.15, Ease::InOut);
    fx.vignette_hold = Some((damage * 0.04).max(2.5));
    // the hit sprite and the arc
    let hold = (damage * 0.035).max(1.5);
    let k = fx.next % MAX_HITS;
    fx.next += 1;
    fx.hits[k] = Hit { on: true, started: false, at: clipped(angle, 1.0), angle, damage, age: 0.0, hold, fade: None };
    fx.arc = Some(ArcState { angle, at: clipped(angle, 0.7), age: 0.0, hold: hold + 0.15, started: false, closing: false });
    stats.damage_flash = 0.0;
}

#[allow(clippy::type_complexity)]
fn animate_damage(
    time: Res<Time>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut fx: ResMut<DamageFx>,
    mut strips: Query<(&Strip, &mut Node, &mut ImageNode, &mut UiTransform), (Without<HitSprite>, Without<Arc>)>,
    mut sprites: Query<(&HitSprite, &mut FlashClip, &mut Node, &mut Visibility), (Without<Strip>, Without<Arc>)>,
    mut arc: Query<(&mut FlashClip, &mut Node, &mut Visibility), (With<Arc>, Without<Strip>, Without<HitSprite>)>,
) {
    let dt = time.delta_secs();
    let Ok(w) = window.single() else { return };
    let (s, off) = stage_fit(w);
    // stage point (from the middle, y up) on the screen
    let screen = |p: Vec2| off + (STAGE * 0.5 + Vec2::new(p.x, -p.y) * STAGE * 0.5) * s;
    // the vignette: held, then `HideVignette` over 3.5 s
    fx.vignette.step(dt);
    if let Some(h) = fx.vignette_hold.as_mut() {
        *h -= dt;
        if *h <= 0.0 {
            fx.vignette_hold = None;
            fx.vignette.start(0.0, 3.5, Ease::Out);
        }
    }
    let va = fx.vignette.value().clamp(0.0, 1.0);
    for (st, mut node, mut im, mut tf) in &mut strips {
        // the strips' rectangles on the stage before turning: middle, size, turn
        let (mid, size, turn) = match st.0 {
            0 => (Vec2::new(640.0, 81.0), Vec2::new(1280.0, 162.0), 0.0),
            1 => (Vec2::new(640.0, 639.0), Vec2::new(1280.0, 162.0), 0.0),
            2 => (Vec2::new(1201.5, 360.0), Vec2::new(1280.0, 162.0), 90.0),
            _ => (Vec2::new(67.15, 203.2), Vec2::new(1280.0, 134.3), -90.0),
        };
        let px = off + (mid - size * 0.5) * s;
        node.left = Val::Px(px.x);
        node.top = Val::Px(px.y);
        node.width = Val::Px(size.x * s);
        node.height = Val::Px(size.y * s);
        tf.rotation = Rot2::degrees(turn);
        let a = va * if st.0 >= 2 { SIDE_ALPHA } else { 1.0 };
        if (im.color.alpha() - a).abs() > 1e-4 {
            im.color.set_alpha(a);
        }
    }
    // the hit sprites: in from further out and larger (0.15 s), held, faded (1.2 s)
    for (hs, mut fc, mut node, mut vis) in &mut sprites {
        let hit = &mut fx.hits[hs.0];
        if !hit.on {
            if *vis != Visibility::Hidden {
                *vis = Visibility::Hidden;
            }
            continue;
        }
        let d = hit.damage;
        if !hit.started {
            // `PlayHitAnim`
            hit.started = true;
            for mc in ["big_glassBreak_mc", "hit0_mc", "hit1_mc"] {
                fc.set_visible(mc, false);
            }
            fc.goto("small_glassBreak_mc", "shake", true);
            if d < 20.0 {
                fc.set_visible("hit0_mc", true);
                fc.goto("particles_mc", "hit0", true);
            } else {
                fc.set_visible("hit1_mc", true);
            }
            if d > 20.0 {
                fc.set_visible("big_glassBreak_mc", true);
                fc.goto("particles_mc", "hit1", true);
            }
            if d > 50.0 {
                fc.goto("particles_mc", "hit2", true);
            }
            if d > 70.0 {
                fc.goto("big_glassBreak_mc", "shine", true);
                fc.goto("particles_mc", "hit3", true);
            }
            if d > 90.0 {
                fc.goto("big_glassBreak_mc", "shine2", true);
                fc.goto("particles_mc", "hit3", true);
            }
        }
        hit.age += dt;
        if hit.fade.is_none() && hit.age >= hit.hold {
            let mut t = Tween { from: 1.0, to: 1.0, ..default() };
            t.start(0.0, 1.2, Ease::Out);
            hit.fade = Some(t);
        }
        let mut alpha = 1.0;
        if let Some(f) = hit.fade.as_mut() {
            f.step(dt);
            alpha = f.value();
            if f.t >= f.secs {
                hit.on = false;
            }
        }
        let k = ease_in(hit.age, 0.15);
        let out = Vec2::new(hit.at.x.signum(), hit.at.y.signum()) * 100.0 * (1.0 - k);
        let p = screen(hit.at) + Vec2::new(out.x, -out.y) * s;
        node.left = Val::Px(p.x);
        node.top = Val::Px(p.y);
        let sc = Vec2::new(1.5, 1.4).lerp(Vec2::ONE, k);
        fc.m = turn_scale(hit.angle, sc.x, sc.y);
        fc.scale = s;
        fc.alpha = alpha * k;
        // the big glass up to 60% and 90% of its size
        if d > 20.0 {
            let base = hs.1;
            if let Some((m, cx)) = fc.clip.placed_mut("big_glassBreak_mc") {
                let g = 1.0 - 0.1 * k;
                *m = concat(&[g, 0.0, 0.0, g, 0.0, 0.0], &[base[0], base[1], base[2], base[3], 0.0, 0.0]);
                m[4] = base[4];
                m[5] = base[5];
                cx[3] = 0.6 * k;
            }
        }
        if *vis != Visibility::Inherited {
            *vis = Visibility::Inherited;
        }
    }
    // the arc: `hit`, held, `close`
    let Ok((mut fc, mut node, mut vis)) = arc.single_mut() else { return };
    let Some(a) = fx.arc.as_mut() else {
        if *vis != Visibility::Hidden {
            *vis = Visibility::Hidden;
        }
        return;
    };
    a.age += dt;
    if !a.started {
        a.started = true;
        a.closing = false;
        fc.goto("", "hit", true);
    }
    if !a.closing && a.age >= a.hold {
        a.closing = true;
        fc.goto("", "close", true);
    }
    if a.closing && a.age >= a.hold + ARC_CLOSE {
        fx.arc = None;
        *vis = Visibility::Hidden;
        return;
    }
    let p = screen(a.at);
    node.left = Val::Px(p.x);
    node.top = Val::Px(p.y);
    fc.m = turn_scale(a.angle, 1.0, 1.0);
    fc.scale = s;
    if *vis != Visibility::Inherited {
        *vis = Visibility::Inherited;
    }
}

/// `m_HealthEffects`: the one Corvo's health has fallen into puts its lens effect up and
/// sounds; leaving it, its lens effect runs out and its leaving sounds play.
fn health_effects(
    level: Option<Res<LevelInfo>>,
    stats: Res<PlayerStats>,
    mut fx: ResMut<DamageFx>,
    vm: Option<ResMut<crate::kismet::Vm>>,
    mut sounds: MessageWriter<crate::audio::PostEvent>,
) {
    let (Some(level), Some(mut vm)) = (level, vm) else { return };
    let list = &level.scene.health_fx;
    if list.is_empty() {
        return;
    }
    let frac = if stats.max_health > 0.0 { stats.health / stats.max_health } else { 1.0 };
    // the lowest threshold his health is under (death's is the screen's own)
    let now = (0..list.len()).filter(|&i| frac <= list[i].threshold && list[i].threshold > 0.0).min_by(|&a, &b| list[a].threshold.total_cmp(&list[b].threshold));
    if now == fx.health_fx {
        return;
    }
    if std::env::var("DH_VIS_LOG").is_ok() {
        info!("health effect {:?} -> {:?} at {:.0}/{:.0}", fx.health_fx.map(|i| &list[i].name), now.map(|i| &list[i].name), stats.health, stats.max_health);
    }
    if let Some(i) = fx.health_fx {
        vm.lens.push((GAMEPLAY_LENS + i as u32, None));
        if !stats.dead {
            for e in &list[i].off {
                sounds.write(crate::audio::PostEvent::named(e, None));
            }
        }
    }
    if let Some(i) = now {
        if let Some(sys) = list[i].lens {
            vm.lens.push((GAMEPLAY_LENS + i as u32, Some((sys, true, 0.0))));
        }
        for e in &list[i].on {
            sounds.write(crate::audio::PostEvent::named(e, None));
        }
    }
    fx.health_fx = now;
}

/// His eyes leaving the water: water runs down the lens (`m_pWaterExitEffectTweaks`).
fn water_exit_lens(level: Option<Res<LevelInfo>>, swim: Res<crate::swim::Swim>, vm: Option<ResMut<crate::kismet::Vm>>, mut was_under: Local<bool>) {
    let under = swim.under.is_some();
    let left = *was_under && !under;
    *was_under = under;
    if !left {
        return;
    }
    let (Some(level), Some(mut vm)) = (level, vm) else { return };
    if let Some((sys, life)) = level.scene.water_exit_lens {
        vm.lens.push((GAMEPLAY_LENS + 0x100, Some((sys, false, life))));
    }
}

/// Blood Thirst's adrenaline filling up: his `m_BarkCues` stinger for it
/// (`EDisPlayerBarkType_AdrenalineFull`).
fn adrenaline_full(stats: Res<PlayerStats>, attrs: Res<crate::gamedata::Attrs>, mut sounds: MessageWriter<crate::audio::PostEvent>, mut was: Local<bool>) {
    let full = attrs.adrenaline_max > 0.0 && stats.adrenaline >= attrs.adrenaline_max && stats.power("BloodThirsty") > 0;
    if full && !*was {
        sounds.write(crate::audio::PostEvent::named("Snd_Pwr_P_Adrenaline_Full", None));
    }
    *was = full;
}
