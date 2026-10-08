//! The crosshair as the HUD movie draws it, played from its own symbols (`flash`): the dot
//! (`crosshair_dot`, the `_dot_mc` at the stage's middle); over something to use, the HUD
//! tweak's `m_InteractionCrosshairSettings` symbol for it by `eCrossHairStatus` (a hand to
//! take, `crosshair_pickUp`; a note, `crosshair_readNote`; a prop to carry, `crosshair_carry`;
//! a locked door, `crosshair_lockedDoor`; anything else to use, `crosshair_actionnable`), with
//! the dot where the tweak keeps it (`m_bShowDot`); otherwise the left hand's reticle
//! (`crosshair_gun`, `_crossbow`, `_grenade`, `_stickyGrenade`, `_springRazor`, `_power`) in
//! its `EDisCrosshairState`: ready, empty (no ammunition, too little mana) or over an enemy.

use crate::flash::{Clip, FlashClip, MovieTimelines};
use crate::gameplay::PlayerStats;
use crate::powers::{Power, Powers};
use crate::GameState;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

pub struct CrosshairPlugin;

impl Plugin for CrosshairPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Shown>()
            .add_systems(OnEnter(GameState::InGame), |mut s: ResMut<Shown>| *s = Shown::default())
            .add_systems(Update, (update_crosshair, info_line).run_if(in_state(GameState::InGame)));
    }
}

const MOVIE: &str = "HUD";
/// `EDisCrosshairState`: the frame shown
const DEFAULT: usize = 0;
const EMPTY: usize = 1;
const OVER: usize = 2;
/// how far an enemy under the crosshair counts
const ENEMY_RANGE: f32 = 40.0;

/// The crosshair's parts: the dot, the interaction symbol, the reticle (entity, symbol).
#[derive(Resource, Default)]
struct Shown {
    root: Option<Entity>,
    parts: [Option<(Entity, &'static str)>; 3],
}

#[derive(Component)]
struct CrosshairRoot;

/// `m_InteractionCrosshairSettings` by what Corvo looks at: the symbol, whether the dot stays.
fn interaction_symbol(kind: Option<Focus>) -> Option<(&'static str, bool)> {
    Some(match kind? {
        Focus::Pickup => ("crosshair_pickUp", true),
        Focus::Note => ("crosshair_readNote", true),
        Focus::Carry => ("crosshair_carry", true),
        Focus::LockedDoor => ("crosshair_lockedDoor", false),
        Focus::UnbreakableDoor => ("crosshair_unbreakableLockedDoor", false),
        Focus::TravelDoor => ("crosshair_travelDoor", false),
        Focus::Usable => ("crosshair_actionnable", false),
        Focus::Body => return None,
    })
}

/// The crosshair's info line (`crosshairInfosTxt_mc` at (640, 409.1): the body font at 23,
/// `#e1f0d4`, centred): over someone asleep, their type's `m_AsleepCrosshairText`
/// ("(Unconscious)").
#[derive(Component)]
struct InfoText;

const INFO_AT: Vec2 = Vec2::new(640.0 - 323.45 + 2.0, 409.1 + 2.0);
const INFO_W: f32 = 647.0;
const INFO_SIZE: f32 = 23.0;
const INFO_COLOR: Color = Color::srgb(225.0 / 255.0, 240.0 / 255.0, 212.0 / 255.0);

#[allow(clippy::type_complexity)]
fn info_line(
    mut commands: Commands,
    window: Query<&Window, With<PrimaryWindow>>,
    hud: Query<Entity, With<crate::hud::HudRoot>>,
    focus: Res<crate::interact::InteractFocus>,
    level: Option<Res<crate::level::LevelInfo>>,
    npcs: Query<&crate::npc::Npc>,
    mut line: Query<(&mut Text, &mut TextFont, &mut Node, &mut Visibility), With<InfoText>>,
) {
    let Ok((mut t, mut f, mut n, mut v)) = line.single_mut() else {
        if let Ok(h) = hud.single() {
            commands.spawn((
                InfoText,
                Text::new(""),
                TextFont::default(),
                TextColor(INFO_COLOR),
                TextShadow { offset: Vec2::splat(1.5), color: Color::srgba(23.0 / 255.0, 25.0 / 255.0, 28.0 / 255.0, 0.85) },
                TextLayout::new(Justify::Center, bevy::text::LineBreak::NoWrap),
                Node { position_type: PositionType::Absolute, ..default() },
                Visibility::Hidden,
                Pickable::IGNORE,
                ChildOf(h),
            ));
        }
        return;
    };
    let Ok(w) = window.single() else { return };
    let asleep = focus
        .1
        .and_then(|e| npcs.get(e).ok())
        .filter(|n| n.mode == crate::npc::Mode::Unconscious)
        .map(|n| n.npc_type.and_then(|ty| level.as_ref()?.scene.npc_types.get(ty as usize)?.stats.as_ref()).map(|s| s.asleep_text.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| "(Unconscious)".into()));
    let want = if asleep.is_some() { Visibility::Inherited } else { Visibility::Hidden };
    if *v != want {
        *v = want;
    }
    let Some(text) = asleep else { return };
    if t.0 != text {
        t.0 = text;
    }
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    let p = off + INFO_AT * s;
    n.left = Val::Px(p.x);
    n.top = Val::Px(p.y);
    n.width = Val::Px(INFO_W * s);
    let fs = bevy::text::FontSize::Px(INFO_SIZE * s);
    if f.font_size != fs {
        f.font_size = fs;
    }
}

/// `Crosshair_Gun.SetCrosshairState`: each bracket to the state's frame.
fn parts_state(fc: &mut FlashClip, state: usize) {
    let tl = fc.tl.clone();
    for set in ["_parts_ext_mc", "_parts_int_mc"] {
        for i in 0..6 {
            if let Some(c) = fc.clip.child_mut(&format!("{set}.part{i}")) {
                if c.frame != state {
                    c.goto(&tl, state, false);
                }
            }
        }
    }
}

/// `Crosshair_Gun.SetDispersion` / `Crosshair_Crossbow.SetDispersion` (the dispersion, and how
/// far it is from the least to the most, 0-100): the brackets moved out that far, the pistol's
/// turned and the outer ones dimmed as it opens.
fn set_dispersion(fc: &mut FlashClip, crossbow: bool, v: f32, pct: f32) {
    let place = |c: &mut Clip, name: &str, x: Option<f32>, y: Option<f32>| {
        if let Some((m, _)) = c.placed_mut(name) {
            if let Some(x) = x {
                m[4] = x;
            }
            if let Some(y) = y {
                m[5] = y;
            }
        }
    };
    if crossbow {
        let off = -15.0 * pct / 100.0;
        if let Some((_, cx)) = fc.clip.placed_mut("_parts_ext_mc") {
            cx[3] = (100.0 - (pct - 40.0).max(0.0) * 1.5) / 100.0;
        }
        if let Some(c) = fc.clip.child_mut("_parts_ext_mc") {
            place(c, "part0", None, Some(-v - off));
            place(c, "part1", Some(v + off), None);
            place(c, "part2", None, Some(v + off));
            place(c, "part3", Some(-v - off), None);
            place(c, "part4", Some(v + off + 5.0), None);
            place(c, "part5", Some(-v - off - 5.0), None);
        }
        if let Some(c) = fc.clip.child_mut("_parts_int_mc") {
            place(c, "part0", None, Some(-v));
            place(c, "part1", Some(v), Some(-v));
            place(c, "part2", None, Some(v));
            place(c, "part3", Some(-v), Some(-v));
        }
        return;
    }
    for (set, turn) in [("_parts_int_mc", -5.0), ("_parts_ext_mc", -20.0)] {
        if let Some((m, cx)) = fc.clip.placed_mut(set) {
            let r = crate::flash::turn_scale(pct * turn / 100.0, 1.0, 1.0);
            m[..4].copy_from_slice(&r[..4]);
            if set == "_parts_ext_mc" {
                cx[3] = (100.0 - (pct - 50.0).max(0.0) * 1.5) / 100.0;
            }
        }
        if let Some(c) = fc.clip.child_mut(set) {
            place(c, "part0", Some(v), Some(-v));
            place(c, "part1", Some(v), Some(v));
            place(c, "part2", Some(-v), Some(v));
            place(c, "part3", Some(-v), Some(-v));
        }
    }
}

/// The left hand's reticle.
fn reticle(p: Power) -> Option<&'static str> {
    Some(match p {
        Power::Pistol | Power::ExplosiveBullet => "crosshair_gun",
        Power::Crossbow | Power::SleepDart | Power::IncendiaryBolt => "crosshair_crossbow",
        Power::Grenade => "crosshair_grenade",
        Power::StickyGrenade => "crosshair_stickyGrenade",
        Power::SpringRazor => "crosshair_springRazor",
        Power::Blink | Power::Windblast | Power::Possess | Power::DevouringSwarm | Power::BendTime | Power::DarkVision => "crosshair_power",
        Power::Empty | Power::Heart => return None,
    })
}

#[derive(Clone, Copy, PartialEq)]
enum Focus {
    Pickup,
    /// a locked door that can't be broken down; a door to another map
    UnbreakableDoor,
    TravelDoor,
    Note,
    Carry,
    LockedDoor,
    Usable,
    Body,
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn update_crosshair(
    mut commands: Commands,
    mut shown: ResMut<Shown>,
    mut timelines: ResMut<MovieTimelines>,
    window: Query<&Window, With<PrimaryWindow>>,
    hud: Query<Entity, With<crate::hud::HudRoot>>,
    mut clips: Query<(&mut FlashClip, &mut Node, &mut Visibility)>,
    focus: Res<crate::interact::InteractFocus>,
    what: (Query<&crate::interact::Pickup>, Query<&crate::interact::Door>, Query<&crate::npc::Npc>, Query<&crate::interact::Usable>, Option<Res<crate::interact::TravelActors>>),
    (stats, powers, data, settings, paused, script_ui, carry): (
        Res<PlayerStats>,
        Res<Powers>,
        Res<crate::gamedata::Data>,
        Res<crate::settings::Settings>,
        Res<crate::hud::Paused>,
        Option<Res<crate::kismet::ScriptUi>>,
        Res<crate::carry::Carry>,
    ),
    cam: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    npcs: Query<(&crate::npc::Npc, &GlobalTransform)>,
    rapier: bevy_rapier3d::prelude::ReadRapierContext,
    aim_state: Res<crate::aim::Aim>,
) {
    let Ok(w) = window.single() else { return };
    let Some(tl) = timelines.get(MOVIE) else { return };
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    // under the HUD (hidden with it)
    let root = match shown.root.filter(|r| commands.get_entity(*r).is_ok()) {
        Some(r) => r,
        None => {
            let Ok(h) = hud.single() else { return };
            let r = commands.spawn((CrosshairRoot, Node { position_type: PositionType::Absolute, ..default() }, Pickable::IGNORE, ChildOf(h))).id();
            *shown = Shown { root: Some(r), parts: [None; 3] };
            r
        }
    };
    // (`PSI_HUD_CrosshairStyle`: off; simple, the dot alone; normal)
    let hidden = stats.dead || paused.0 || settings.crosshair_style == 0 || script_ui.is_some_and(|u| u.hud_hidden.contains("DHE_Crosshair"));
    let simple = settings.crosshair_style == 1;
    // what Corvo looks at
    let (pickups, doors, bodies, usables, travel) = what;
    // (a prop to pick up is focused by name, without an entity: `props`)
    let prop = focus.0 && focus.1.is_none() && !focus.3.is_empty() && carry.body.is_none();
    let kind = if prop { Some(Focus::Carry) } else { focus.1.filter(|_| focus.0 && carry.body.is_none()).map(|e| {
        if let Ok(p) = pickups.get(e) {
            if matches!(p.kind, crate::interact::PickupKind::Note) { Focus::Note } else { Focus::Pickup }
        } else if let Ok(d) = doors.get(e) {
            if d.locked && d.key_in(&stats.keys).is_none() {
                if d.breakable { Focus::LockedDoor } else { Focus::UnbreakableDoor }
            } else if d.travel {
                Focus::TravelDoor
            } else {
                Focus::Usable
            }
        } else if usables.get(e).is_ok_and(|u| travel.as_ref().is_some_and(|t| t.0.contains(&u.actor))) {
            Focus::TravelDoor
        } else if bodies.get(e).is_ok_and(|n| n.is_down()) {
            Focus::Body
        } else {
            Focus::Usable
        }
    }) };
    let interaction = if simple { None } else { interaction_symbol(kind) };
    let aim = if interaction.is_some() || simple { None } else { reticle(powers.selected) };
    let dot = interaction.is_none_or(|i| i.1);
    // the reticle's state: empty, over an enemy, ready
    let over_enemy = || -> bool {
        let Ok(c) = cam.single() else { return false };
        let (eye, fwd) = (c.translation(), c.forward().as_vec3());
        let ctx = rapier.single().ok();
        npcs.iter().any(|(n, g)| {
            if !n.hostile() || n.is_down() {
                return false;
            }
            let mid = g.translation() + Vec3::Y * 0.1;
            let to = mid - eye;
            let along = to.dot(fwd);
            if along <= 0.5 || along > ENEMY_RANGE {
                return false;
            }
            // the view's ray through the body (a 0.4 m wide, 1.9 m tall capsule)
            let off = to - fwd * along;
            if off.with_y(0.0).length() > 0.4 || off.y.abs() > 0.95 {
                return false;
            }
            let world = bevy_rapier3d::prelude::QueryFilter::default().groups(bevy_rapier3d::prelude::CollisionGroups::new(bevy_rapier3d::prelude::Group::ALL, crate::level::GROUP_WORLD));
            ctx.as_ref().is_none_or(|ctx| ctx.cast_ray(eye, to.normalize(), to.length() - 0.5, true, world).is_none())
        })
    };
    let state = match aim {
        Some(_) if powers.selected.ammo(&stats) == Some(0) || powers.selected.mana(&data) > stats.mana => EMPTY,
        Some(_) if over_enemy() => OVER,
        _ => DEFAULT,
    };
    let want: [Option<&'static str>; 3] = [dot.then_some("crosshair_dot"), interaction.map(|i| i.0), aim];
    for (k, name) in want.iter().enumerate() {
        // a new symbol: its clip from the start
        if shown.parts[k].map(|p| p.1) != *name {
            if let Some((e, _)) = shown.parts[k].take() {
                commands.entity(e).try_despawn();
            }
            if let Some(n) = name {
                if let Some(clip) = Clip::export(&tl, n) {
                    let fc = FlashClip::new(MOVIE, tl.clone(), clip);
                    let e = commands.spawn((fc, Node { position_type: PositionType::Absolute, ..default() }, Visibility::Hidden, Pickable::IGNORE, ChildOf(root))).id();
                    shown.parts[k] = Some((e, n));
                }
            }
        }
        let Some((e, _)) = shown.parts[k] else { continue };
        let Ok((mut fc, mut node, mut vis)) = clips.get_mut(e) else { continue };
        let p = Vec2::new(w.width(), w.height()) * 0.5;
        node.left = Val::Px(p.x);
        node.top = Val::Px(p.y);
        fc.scale = s;
        // the dot and the reticles show their state (the interaction symbols play): a reticle's
        // brackets each (`Crosshair_Gun.SetCrosshairState`)
        if k != 1 && fc.clip.frame != state {
            let tl = fc.tl.clone();
            fc.clip.goto(&tl, state, false);
        }
        if k == 2 {
            parts_state(&mut fc, state);
            // its brackets opened with the dispersion (`SetDispersion`; still, at the least,
            // without `PSI_HUD_bCrosshairMovement`)
            if let Some(w) = aim_state.weapon {
                let v = if settings.crosshair_movement { aim_state.dispersion } else { aim_state.min };
                let pct = if aim_state.max > aim_state.min { ((v - aim_state.min) / (aim_state.max - aim_state.min) * 100.0).clamp(0.0, 100.0) } else { 0.0 };
                set_dispersion(&mut fc, w == crate::aim::Weapon::Crossbow, v, pct);
            }
        }
        // (`PSI_HUD_CrosshairOpacity`)
        fc.alpha = (settings.crosshair_opacity / 100.0).clamp(0.0, 1.0);
        let v = if hidden { Visibility::Hidden } else { Visibility::Inherited };
        if *vis != v {
            *vis = v;
        }
    }
}
