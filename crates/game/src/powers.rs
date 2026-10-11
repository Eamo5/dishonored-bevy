//! Supernatural powers and the left-hand gadgets: Blink, Dark Vision, Bend Time, Windblast,
//! Possession, Devouring Swarm, the pistol and crossbow (bolts, sleep darts), elixirs, mana
//! and health regeneration. Levels, ranges, durations and costs come from the original
//! tweaks (`gamedata`); powers are owned (acquired with runes in the journal).

use crate::audio::PostEvent;
use crate::footsteps::{impact_surface, ray_surface, ColliderSurfaces};
use crate::gamedata::{Attrs, Data};
use crate::gameplay::{HitKind, HudMessages, Noise, NpcHit, NpcStagger, PlayerStats, TimeControl};
use crate::level::{GameAssets, GROUP_NPC, GROUP_PROP, GROUP_WORLD};
use crate::npc::Npc;
use crate::player::{Player, PlayerCamera, RADIUS, STAND_HALF};
use crate::worlddamage::{Reach, WorldDamage};
use crate::GameState;
use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::mesh::MeshTag;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions};
use bevy_rapier3d::prelude::*;

pub struct PowersPlugin;

impl Plugin for PowersPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Powers>()
            .init_resource::<ProjectileRestore>()
            .add_message::<PowerUsed>()
            .add_message::<PowerEquipped>()
            .add_systems(OnEnter(GameState::InGame), (reset_powers, spawn_blink_marker))
            .add_systems(Update, restore_projectiles.after(crate::save::restore_npcs).before(bolt_focus).run_if(in_state(GameState::InGame)))
            .add_systems(Update, bolt_focus.after(crate::interact::FocusSet).before(crate::gadgets::grenade_focus).before(crate::interact::use_focus).run_if(in_state(GameState::InGame)))
            .add_systems(
                Update,
                (select_power, use_power, cook_grenade, blink_travel, update_projectiles, regenerate, dark_vision_post, crate::darkvision::update_dark_vision, crate::darkvision::dark_vision_sounds)
                    .chain()
                    .after(crate::gadgets::grenade_focus)
                    .run_if(in_state(GameState::InGame)),
            );
    }
}

/// What the left hand holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Power {
    Empty,
    Blink,
    DarkVision,
    BendTime,
    Windblast,
    Possess,
    DevouringSwarm,
    Pistol,
    Crossbow,
    SleepDart,
    Heart,
    Grenade,
    StickyGrenade,
    SpringRazor,
    IncendiaryBolt,
    ExplosiveBullet,
}

/// The original keyboard shortcut order (`m_AutoAssign_KeyboardItemOrder`): the first ten
/// items owned go on keys 1-0.
pub const SHORTCUT_ORDER: [Power; 15] = [
    Power::Pistol,
    Power::Crossbow,
    Power::Grenade,
    Power::SpringRazor,
    Power::Heart,
    Power::Blink,
    Power::DarkVision,
    Power::Windblast,
    Power::DevouringSwarm,
    Power::BendTime,
    Power::Possess,
    Power::SleepDart,
    Power::IncendiaryBolt,
    Power::StickyGrenade,
    Power::ExplosiveBullet,
];

impl Power {
    pub fn name(self) -> &'static str {
        match self {
            Power::Empty => "",
            Power::Blink => "Blink",
            Power::DarkVision => "Dark Vision",
            Power::BendTime => "Bend Time",
            Power::Windblast => "Wind Blast",
            Power::Possess => "Possession",
            Power::DevouringSwarm => "Devouring Swarm",
            Power::Pistol => "Pistol",
            Power::Crossbow => "Crossbow",
            Power::SleepDart => "Sleep Darts",
            Power::Heart => "The Heart",
            Power::Grenade => "Grenade",
            Power::StickyGrenade => "Sticky Grenade",
            Power::SpringRazor => "Springrazor",
            Power::IncendiaryBolt => "Incendiary Bolts",
            Power::ExplosiveBullet => "Explosive Bullets",
        }
    }
    /// The power's name in the cooked game data (`None` for weapons).
    pub fn key(self) -> Option<&'static str> {
        Some(match self {
            Power::Blink => "Blink",
            Power::DarkVision => "DarkVision",
            Power::BendTime => "BendTime",
            Power::Windblast => "Windblast",
            Power::Possess => "Possess",
            Power::DevouringSwarm => "DevouringSwarm",
            _ => return None,
        })
    }
    pub fn from_key(k: &str) -> Option<Power> {
        SHORTCUT_ORDER.iter().copied().find(|p| p.key().is_some_and(|x| x.eq_ignore_ascii_case(k)))
    }
    /// The original equipment icon (`cache/ui/icons/<name>.png`).
    pub fn icon(self) -> &'static str {
        match self {
            Power::Empty => "",
            Power::Blink => "ic_pow_Blink",
            Power::DarkVision => "ic_pow_darkVision",
            Power::BendTime => "ic_pow_bendTime",
            Power::Windblast => "ic_pow_windBlast",
            Power::Possess => "ic_pow_possession",
            Power::DevouringSwarm => "ic_pow_ratSwarm",
            Power::Pistol => "ic_item_regularBullet",
            Power::Crossbow => "ic_item_regularBolt",
            Power::SleepDart => "ic_item_sleepDart",
            Power::Heart => "ic_item_heart",
            Power::Grenade => "ic_item_regularGrenade",
            Power::StickyGrenade => "ic_item_stickyGrenade",
            Power::SpringRazor => "ic_item_springRazor",
            Power::IncendiaryBolt => "ic_item_flareBolt",
            Power::ExplosiveBullet => "ic_item_explosiveBullet",
        }
    }
    /// Fired from the pistol or crossbow.
    pub fn is_ranged(self) -> bool {
        matches!(self, Power::Pistol | Power::Crossbow | Power::SleepDart | Power::IncendiaryBolt | Power::ExplosiveBullet)
    }
    /// Held in the left hand rather than cast (the gadget idle).
    pub fn is_gadget(self) -> bool {
        self.is_ranged() || matches!(self, Power::Heart | Power::Grenade | Power::StickyGrenade | Power::SpringRazor)
    }
    /// The inventory entry of a gadget's ammunition (`stats.items`).
    pub fn ammo_item(self) -> Option<&'static str> {
        use crate::gadgets::*;
        Some(match self {
            Power::Grenade => GRENADES,
            Power::StickyGrenade => STICKY,
            Power::SpringRazor => RAZORS,
            Power::IncendiaryBolt => FLARES,
            Power::ExplosiveBullet => EXPLOSIVE,
            _ => return None,
        })
    }
    /// Ammunition left (`None` for powers and the Heart).
    pub fn ammo(self, stats: &PlayerStats) -> Option<u32> {
        match self {
            Power::Pistol => Some(stats.bullets),
            Power::Crossbow => Some(stats.bolts),
            Power::SleepDart => Some(stats.sleep_darts),
            p => p.ammo_item().map(|k| stats.items.get(k).copied().unwrap_or(0)),
        }
    }
    pub fn owned(self, stats: &PlayerStats) -> bool {
        match (self.key(), self.ammo_item()) {
            (Some(k), _) => stats.power(k) > 0,
            // gadgets once found or bought
            (_, Some(item)) => stats.items.contains_key(item),
            _ => self != Power::Empty && stats.weapons && !(stats.no_crossbow && matches!(self, Power::Crossbow | Power::SleepDart)),
        }
    }
    pub fn level(self, stats: &PlayerStats) -> u8 {
        self.key().map(|k| stats.power(k)).unwrap_or(1)
    }
    /// Mana per use (Dark Vision and the rest from the tweaks, percent of a 100 bar).
    pub fn mana(self, data: &Data) -> f32 {
        self.key().and_then(|k| data.active(k)).map(|a| a.mana).unwrap_or(0.0)
    }
}

/// The shortcut keys' items.
pub fn shortcuts(stats: &PlayerStats) -> Vec<Power> {
    SHORTCUT_ORDER.iter().copied().filter(|p| p.owned(stats)).take(10).collect()
}

/// A power was used (level scripts listen: `DisSeqEvent_PowerUsed`).
#[derive(Message, Clone, Copy)]
pub struct PowerUsed(pub Power);

/// A power was put in the left hand (`DisSeqEvent_PowerEquipped`).
#[derive(Message, Clone, Copy)]
pub struct PowerEquipped(pub Power);

#[derive(Resource)]
pub struct Powers {
    pub selected: Power,
    pub dark_vision: bool,
    /// seconds of Dark Vision left
    pub dark_vision_left: f32,
    pub aiming: bool,
    pub blink_target: Option<Vec3>,
    pub blink: Option<(Vec3, Vec3, f32)>,
    pub cooldown: f32,
    pub fov_kick: f32,
    /// Bumped on every use of a power or weapon (`cast_power` is what was used).
    pub cast_seq: u32,
    pub cast_power: Power,
    /// the left hand's press that ended a possession, held: it does nothing more
    swallow: bool,
    /// a grenade cooking in the hand: which, and for how long (`GrenadeCooking`)
    pub cooking: Option<(Power, f32)>,
}

impl Default for Powers {
    fn default() -> Self {
        Self {
            selected: Power::Empty,
            dark_vision: false,
            dark_vision_left: 0.0,
            aiming: false,
            blink_target: None,
            blink: None,
            cooldown: 0.0,
            fov_kick: 0.0,
            cast_seq: 0,
            cast_power: Power::Blink,
            swallow: false,
            cooking: None,
        }
    }
}

#[derive(Component)]
struct BlinkMarker;

/// A hit this high above a character's middle is to its head.
const HEAD: f32 = 0.58;

#[derive(Component)]
pub struct Projectile {
    pub vel: Vec3,
    pub kind: HitKind,
    pub life: f32,
    pub stuck: bool,
    pub recoverable: bool,
    attachment: Option<BoltAttachment>,
}

/// Player-owned power state must travel with a save: a projectile saved in
/// stopped time must not resume moving merely because its map was reloaded.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct PowersSave {
    selected: Power,
    dark_vision_left: f32,
    cooldown: f32,
    cooking: Option<(Power, f32)>,
    blink: Option<([f32; 3], [f32; 3], f32)>,
    bend_remaining: f32,
    world_dilation: f32,
    scripted: Option<(f32, f32)>,
}

impl PowersSave {
    pub fn capture(powers: &Powers, time: &TimeControl) -> Self {
        Self {
            selected: powers.selected,
            dark_vision_left: if powers.dark_vision { powers.dark_vision_left } else { 0.0 },
            cooldown: powers.cooldown,
            cooking: powers.cooking,
            blink: powers.blink.map(|(a, b, t)| (a.to_array(), b.to_array(), t)),
            bend_remaining: time.bend_remaining,
            world_dilation: time.world_dilation,
            scripted: time.scripted,
        }
    }

    pub fn restore(self, powers: &mut Powers, time: &mut TimeControl) {
        *powers = Powers {
            selected: self.selected,
            dark_vision: self.dark_vision_left > 0.0,
            dark_vision_left: self.dark_vision_left,
            cooldown: self.cooldown,
            cooking: self.cooking,
            blink: self.blink.map(|(a, b, t)| (Vec3::from(a), Vec3::from(b), t)),
            ..default()
        };
        *time = TimeControl {
            bend_remaining: self.bend_remaining,
            world_dilation: self.world_dilation,
            scripted: self.scripted,
            ..default()
        };
    }
}

#[derive(Clone, Copy)]
struct BoltAttachment {
    npc: Entity,
    bone: Option<usize>,
    offset: Vec3,
    rotation: Quat,
}

impl Projectile {
    fn physics(data: &Data, kind: HitKind) -> (f32, f32) {
        let (prefix, speed, gravity) = match kind {
            HitKind::SleepDart => ("arrow_sleep", 0.7, 0.4),
            HitKind::Fire => ("arrow_flare", 0.2, 0.5),
            _ => ("arrow", 1.0, 1.3),
        };
        let speed = data.pawn("crossbow.m_fArrowFireSpeed", 20000.0) * UU * data.pawn(&format!("{prefix}.m_fSpeedMultiplier"), speed).max(0.0);
        let gravity = data.pawn("world.DefaultGravityZ", -1500.0) * UU * data.pawn(&format!("{prefix}.m_fGravityMultiplier"), gravity).max(0.0);
        (speed, gravity)
    }
    fn survives_body_hit(base_chance: f32, modifier: f32, roll: f32) -> bool {
        roll >= (base_chance + modifier).clamp(0.0, 1.0)
    }
}

fn recover_bolt(stats: &mut PlayerStats, attrs: &Attrs) -> bool {
    if stats.bolts >= attrs.bolt_capacity { return false; }
    stats.bolts += 1;
    true
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ProjectileSave {
    position: [f32; 3],
    rotation: [f32; 4],
    velocity: [f32; 3],
    kind: HitKind,
    life: f32,
    stuck: bool,
    recoverable: bool,
    /// Stable NPC spawner and bone index; the offset is local to that bone.
    attachment: Option<(u32, Option<usize>, [f32; 3], [f32; 4])>,
}

#[derive(Resource, Default)]
pub struct ProjectileRestore(pub Option<Vec<ProjectileSave>>);

pub(crate) fn save_projectiles<'a>(projectiles: impl IntoIterator<Item = (&'a Projectile, &'a Transform)>, npcs: impl IntoIterator<Item = (Entity, u32)>) -> Vec<ProjectileSave> {
    let npcs: std::collections::HashMap<_, _> = npcs.into_iter().collect();
    projectiles.into_iter().map(|(p, t)| ProjectileSave {
        position: t.translation.to_array(), rotation: t.rotation.to_array(), velocity: p.vel.to_array(),
        kind: p.kind, life: p.life, stuck: p.stuck, recoverable: p.recoverable,
        attachment: p.attachment.and_then(|a| Some((*npcs.get(&a.npc)?, a.bone, a.offset.to_array(), a.rotation.to_array()))),
    }).collect()
}

fn projectile_model(entity: &mut EntityCommands, assets: Option<&GameAssets>, kind: HitKind) {
    if kind == HitKind::Fire {
        entity.insert((
            PointLight { color: Color::srgb(1.0, 0.55, 0.2), intensity: 60_000.0, range: 5.0, ..default() },
            crate::fxlight::FxLight { color: Vec3::new(1.0, 0.55, 0.2), brightness: 3.0, radius: 5.0 },
        ));
    }
    let prop = match kind { HitKind::SleepDart => "bolt_sleep", HitKind::Fire => "bolt_flare", _ => "bolt" };
    if let Some(parts) = assets.and_then(|a| a.props.get(prop).or(a.props.get("bolt"))) {
        entity.with_children(|c| {
            for (mesh, mat) in &parts.parts {
                let mut m = c.spawn((Mesh3d(mesh.clone()), MeshTag(0), Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2))));
                mat.apply(&mut m);
            }
        });
    }
}

fn restore_projectiles(mut commands: Commands, mut pending: ResMut<ProjectileRestore>, old: Query<Entity, With<Projectile>>, npcs: Query<(Entity, &crate::npc::FromSpawner)>, assets: Option<Res<GameAssets>>) {
    let Some(saved) = pending.0.take() else { return };
    for e in &old { commands.entity(e).despawn(); }
    let npcs: std::collections::HashMap<_, _> = npcs.iter().map(|(e, s)| (s.0, e)).collect();
    for p in saved {
        let attachment = p.attachment.and_then(|(s, bone, offset, rotation)| Some(BoltAttachment { npc: *npcs.get(&s)?, bone, offset: Vec3::from(offset), rotation: Quat::from_array(rotation) }));
        let mut e = commands.spawn((
            Projectile { vel: Vec3::from(p.velocity), kind: p.kind, life: p.life, stuck: p.stuck, recoverable: p.recoverable, attachment },
            Transform::from_translation(Vec3::from(p.position)).with_rotation(Quat::from_array(p.rotation)),
            Visibility::default(), DespawnOnExit(GameState::InGame),
        ));
        projectile_model(&mut e, assets.as_deref(), p.kind);
    }
}

/// Ordinary bolts use their own interaction, rather than masquerading as a
/// level-authored pickup (which would fire the wrong Kismet pickup event).
#[allow(clippy::too_many_arguments)]
pub(crate) fn bolt_focus(
    mut commands: Commands,
    (keys, bind): (Res<ButtonInput<KeyCode>>, Res<crate::bindings::Bindings>),
    mut focus: ResMut<crate::interact::InteractFocus>,
    bolts: Query<(Entity, &Projectile, &Transform)>,
    positions: Query<&Transform>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    player: Query<(Entity, &Player)>,
    (mut stats, attrs, mut held, carry, possession, paused): (ResMut<PlayerStats>, Res<Attrs>, ResMut<crate::props::Held>, Res<crate::carry::Carry>, Res<crate::possession::Possession>, Res<crate::hud::Paused>),
    rapier: ReadRapierContext,
    mut sound: MessageWriter<PostEvent>,
    mut log: ResMut<crate::pickuplog::PickupLog>,
    mut messages: ResMut<HudMessages>,
) {
    let (Ok((pe, p)), Ok(camera), Ok(ctx)) = (player.single(), cam.single(), rapier.single()) else { return };
    if paused.0 || stats.dead || p.locked || held.busy() || carry.carrying() || possession.host.is_some() { return; }
    let eye = camera.translation();
    let direction = camera.forward().as_vec3();
    let nearest = bolts.iter().filter(|(_, b, _)| b.recoverable).filter_map(|(e, bolt, t)| {
        let to = t.translation - eye;
        let distance = to.length();
        if distance > 2.4 || distance < 0.05 || to.dot(direction) / distance < 0.94 { return None; }
        let not_host = |collider| bolt.attachment.is_none_or(|a| collider != a.npc);
        let filter = QueryFilter::default().exclude_collider(pe).predicate(&not_host).groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP | GROUP_NPC));
        if ctx.cast_ray(eye, to / distance, (distance - 0.08).max(0.0), true, filter).is_some() { return None; }
        Some((e, distance, bolt.attachment.map(|a| a.npc)))
    }).min_by(|a, b| a.1.total_cmp(&b.1));
    let Some((e, distance, host)) = nearest else { return };
    if focus.1.filter(|focused| Some(*focused) != host).and_then(|e| positions.get(e).ok()).is_some_and(|t| t.translation.distance(eye) + 0.1 < distance) { return; }
    focus.0 = true;
    focus.1 = Some(e);
    focus.2 = format!("{} Pick up", crate::bindings::hint(crate::bindings::Act::Use));
    focus.3 = "Bolt".into();
    if keys.just_pressed(bind.key(crate::bindings::Act::Use)) {
        if recover_bolt(&mut stats, &attrs) {
            // Consume the short pickup action, so the same Use press cannot
            // also grab a loose prop behind the disappearing bolt.
            held.1 = held.1.max(0.1);
            commands.entity(e).despawn();
            focus.1 = None;
            focus.2.clear();
            focus.3.clear();
            log.add("Crossbow Bolts +1", crate::pickuplog::ammo_icon(2));
            sound.write(PostEvent::named("Snd_UI_Ingame_Ammo_Pickup", None));
        } else {
            messages.push("You can't carry more ammo of this type");
            sound.write(PostEvent::named("Snd_UI_Ingame_Max_Ammo", None));
        }
    }
}

fn reset_powers(mut p: ResMut<Powers>, stats: Res<PlayerStats>) {
    *p = Powers::default();
    p.selected = shortcuts(&stats).into_iter().find(|p| p.key().is_some()).unwrap_or(Power::Empty);
}

fn spawn_blink_marker(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut mats: ResMut<Assets<StandardMaterial>>) {
    let mat = mats.add(StandardMaterial {
        base_color: Color::srgba(0.4, 0.8, 1.0, 0.35),
        emissive: LinearRgba::rgb(0.6, 2.5, 4.0),
        alpha_mode: AlphaMode::Add,
        unlit: true,
        ..default()
    });
    commands.spawn((
        BlinkMarker,
        Mesh3d(meshes.add(Capsule3d::new(0.22, 1.2))),
        MeshMaterial3d(mat),
        Transform::default(),
        Visibility::Hidden,
        bevy::light::NotShadowCaster,
        DespawnOnExit(GameState::InGame),
    ));
}

fn select_power(
    keys: Res<ButtonInput<KeyCode>>,
    scroll: Res<AccumulatedMouseScroll>,
    stats: Res<PlayerStats>,
    mut powers: ResMut<Powers>,
    mut equipped: MessageWriter<PowerEquipped>,
    mut sfx: MessageWriter<PostEvent>,
    (possession, choice, paused): (Res<crate::possession::Possession>, Res<crate::choice::Choice>, Res<crate::hud::Paused>),
) {
    // the number keys answer a choice on screen
    if paused.0 || possession.host.is_some() || choice.pending.is_some() {
        return;
    }
    let list = shortcuts(&stats);
    let mut sel = None;
    let digits = [
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
        KeyCode::Digit0,
    ];
    for (i, k) in digits.iter().enumerate() {
        if keys.just_pressed(*k) {
            if let Some(p) = list.get(i) {
                sel = Some(*p);
            }
        }
    }
    if scroll.delta.y.abs() > 0.0 && !list.is_empty() {
        let cur = list.iter().position(|p| *p == powers.selected).map(|i| i as i32).unwrap_or(-1);
        let n = list.len() as i32;
        let next = (cur + if scroll.delta.y > 0.0 { -1 } else { 1 }).rem_euclid(n);
        sel = Some(list[next as usize]);
    }
    // lost what was held (a fresh map, a script took it), or an empty hand and something to hold
    if sel.is_none() && ((powers.selected != Power::Empty && !powers.selected.owned(&stats)) || (powers.selected == Power::Empty && !list.is_empty())) {
        // a power first (the left hand's natural use), else any item
        sel = Some(list.iter().copied().find(|p| p.key().is_some()).or(list.iter().copied().find(|p| !p.is_ranged())).or(list.first().copied()).unwrap_or(Power::Empty));
    }
    if let Some(s) = sel {
        if s != powers.selected {
            powers.selected = s;
            powers.aiming = false;
            equipped.write(PowerEquipped(s));
            sfx.write(PostEvent::named(if s.key().is_some() { "Snd_Power_Switch" } else { "Snd_Power_Equip" }, None));
        }
    }
}

/// Compute where Blink would put the player (capsule centre), including ledge grabs.
fn blink_destination(ctx: &RapierContext, player: Entity, eye: Vec3, dir: Vec3, range: f32, vert: f32) -> Option<Vec3> {
    let filter = QueryFilter::default().exclude_collider(player).groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP | GROUP_NPC));
    // upward blinks reach less far (the original's vertical limit)
    let range = if dir.y > 0.0 { range.min(vert / dir.y.max(1e-3)) } else { range };
    let mut point = eye + dir * range;
    let mut ledge = None;
    if let Some((_, hit)) = ctx.cast_ray_and_get_normal(eye, dir, range, true, filter) {
        let hp = hit.point;
        point = hp - dir * (RADIUS + 0.15);
        if hit.normal.y.abs() < 0.5 {
            // wall: is there a ledge just above?
            let flat = dir.with_y(0.0).normalize_or_zero();
            let probe = hp + flat * 0.45 + Vec3::Y * 2.0;
            if let Some((_, g)) = ctx.cast_ray_and_get_normal(probe, Vec3::NEG_Y, 2.4, true, filter) {
                if g.normal.y > 0.7 && g.point.y > hp.y - 0.3 {
                    let center = g.point + Vec3::Y * (STAND_HALF + RADIUS + 0.06);
                    if ctx.cast_ray(g.point + Vec3::Y * 0.05, Vec3::Y, 1.75, true, filter).is_none() {
                        ledge = Some(center);
                    }
                }
            }
        } else if hit.normal.y > 0.5 {
            // floor: stand on it
            point = hp + Vec3::Y * (STAND_HALF + RADIUS + 0.06);
        }
    }
    let mut dest = ledge.unwrap_or(point);
    if ledge.is_none() {
        // settle onto the ground below if close
        if let Some((_, g)) = ctx.cast_ray_and_get_normal(dest, Vec3::NEG_Y, STAND_HALF + RADIUS + 1.2, true, filter) {
            if g.normal.y > 0.6 {
                dest = g.point + Vec3::Y * (STAND_HALF + RADIUS + 0.06);
            }
        }
    }
    // make sure the capsule fits
    let shape = Collider::capsule_y(STAND_HALF, RADIUS * 0.95);
    let mut blocked = false;
    ctx.intersect_shape(dest, Quat::IDENTITY, &*shape.raw, filter, |_| {
        blocked = true;
        false
    });
    if blocked {
        // try a little higher (stairs/edges)
        let up = dest + Vec3::Y * 0.4;
        let mut b2 = false;
        ctx.intersect_shape(up, Quat::IDENTITY, &*shape.raw, filter, |_| {
            b2 = true;
            false
        });
        if b2 {
            return None;
        }
        dest = up;
    }
    Some(dest)
}

/// Unreal units to metres.
const UU: f32 = 0.01;

#[allow(clippy::too_many_arguments)]
fn use_power(
    mut commands: Commands,
    (time, mouse, cursor, scripted, settings): (Res<Time>, Res<ButtonInput<MouseButton>>, Single<&CursorOptions>, Option<Res<crate::script::Scripted>>, Res<crate::settings::Settings>),
    rapier: ReadRapierContext,
    (assets, data, attrs, tune, level_info): (Option<Res<GameAssets>>, Res<Data>, Res<Attrs>, Res<crate::musicbox::Tune>, Option<Res<crate::level::LevelInfo>>),
    mut powers: ResMut<Powers>,
    mut stats: ResMut<PlayerStats>,
    mut tc: ResMut<TimeControl>,
    mut msgs: ResMut<HudMessages>,
    (mut hits, mut stag, mut noise, mut sfx, mut used, mut world_hits): (MessageWriter<NpcHit>, MessageWriter<NpcStagger>, MessageWriter<Noise>, MessageWriter<PostEvent>, MessageWriter<PowerUsed>, MessageWriter<WorldDamage>),
    mut player: Query<(Entity, &Transform, &mut Player)>,
    (cam, surfaces, strikeables, mut struck): (Query<&GlobalTransform, With<PlayerCamera>>, Query<&ColliderSurfaces>, Query<&crate::gameplay::Strikeable>, MessageWriter<crate::gameplay::Struck>),
    mut marker: Query<(&mut Transform, &mut Visibility), (With<BlinkMarker>, Without<Player>)>,
    mut npcs: Query<(Entity, &Transform, &mut Npc), (Without<Player>, Without<BlinkMarker>)>,
    (mut possession, mut start_possess, hosts, possess_overrides): (
        ResMut<crate::possession::Possession>,
        MessageWriter<crate::possession::PossessRequest>,
        Query<(Entity, &GlobalTransform), (With<crate::possession::Host>, Without<crate::possession::Possessed>)>,
        Res<crate::possession::PossessOverrides>,
    ),
    (mut swarm, mut gadget, mut blast, mut door_blast, mut kill_cams): (MessageWriter<crate::swarm::SummonSwarm>, MessageWriter<crate::gadgets::UseGadget>, MessageWriter<crate::gadgets::Explosion>, MessageWriter<crate::interact::DoorBlast>, MessageWriter<crate::killcam::StartKillCam>),
    (mut aim, held, paused): (ResMut<crate::aim::Aim>, Res<crate::props::Held>, Res<crate::hud::Paused>),
) {
    if paused.0 {
        // Menu mouse releases must not spend mana or queue a Blink on resume.
        powers.aiming = false;
        powers.blink_target = None;
        if let Ok((_, mut visibility)) = marker.single_mut() { *visibility = Visibility::Hidden; }
        return;
    }
    let dt = time.delta_secs();
    powers.cooldown = (powers.cooldown - dt).max(0.0);
    powers.fov_kick = (powers.fov_kick - dt * 3.0).max(0.0);
    let Ok((pe, pt, mut p)) = player.single_mut() else { return };
    let Ok(cg) = cam.single() else { return };
    let eye = cg.translation();
    let dir = cg.forward().as_vec3();
    let Ok(ctx) = rapier.single() else { return };
    // Dark Vision lasts its duration
    if powers.dark_vision {
        powers.dark_vision_left -= dt;
        if powers.dark_vision_left <= 0.0 {
            powers.dark_vision = false;
            sfx.write(PostEvent::named(data.power_sound("DarkVision", 1, "m_pDeactivationSoundEvent").unwrap_or("Darkvision_Stop"), None));
        }
    }
    let grabbed = cursor.grab_mode != CursorGrabMode::None || scripted.is_some();
    let can_act = grabbed && !stats.dead && !p.locked && !held.busy() && powers.blink.is_none() && tc.wheel >= 1.0;
    let sel = powers.selected;
    let level = sel.level(&stats);
    // possessing: the left hand only ends it
    if possession.host.is_some() {
        if can_act && mouse.just_pressed(MouseButton::Right) {
            possession.exit_requested = true;
            powers.swallow = true;
        }
        return;
    }
    if powers.swallow {
        if mouse.pressed(MouseButton::Right) {
            return;
        }
        powers.swallow = false;
    }
    // Blink aiming
    let blink_range = (data.power_f("Blink", level, "m_HorizDistance", 1100.0)).min(data.power_f("Blink", level, "m_Distance", 2000.0)) * UU;
    let blink_vert = data.power_f("Blink", level, "m_VertDistance", 500.0) * UU;
    if sel == Power::Blink && can_act && mouse.pressed(MouseButton::Right) {
        powers.aiming = true;
        powers.blink_target = blink_destination(&ctx, pe, eye, dir, blink_range, blink_vert);
    }
    if let Ok((mut mt, mut mv)) = marker.single_mut() {
        match (powers.aiming && sel == Power::Blink, powers.blink_target) {
            (true, Some(t)) => {
                *mv = Visibility::Inherited;
                mt.translation = t;
            }
            _ => *mv = Visibility::Hidden,
        }
    }
    let cost = sel.mana(&data);
    if powers.aiming && sel == Power::Blink && mouse.just_released(MouseButton::Right) {
        powers.aiming = false;
        if let Some(target) = powers.blink_target.take() {
            if tune.inhibited {
                // an Overseer's music box: the Mark fails
                msgs.push("Your powers are blocked");
                sfx.write(PostEvent::named("Snd_Power_Empty", None));
            } else if stats.mana >= cost {
                stats.spend_mana(cost, &attrs);
                powers.blink = Some((pt.translation, target, 0.0));
                sfx.write(PostEvent::named(data.power_sound("Blink", level, "m_pBlinkWarmupSoundEvent").unwrap_or("Snd_Power_P_Blink_Start"), None));
                powers.cast_seq += 1;
                powers.cast_power = Power::Blink;
                used.write(PowerUsed(Power::Blink));
                p.locked = true;
                p.velocity = Vec3::ZERO;
                powers.fov_kick = 1.0;
            } else {
                out_of_mana(&mut powers, &mut sfx, &mut msgs);
            }
        }
        return;
    }
    // the Heart's whispers are its own (heart.rs)
    if !can_act || !mouse.just_pressed(MouseButton::Right) || powers.cooldown > 0.0 || matches!(sel, Power::Empty | Power::Blink | Power::Heart) {
        return;
    }
    // toggles end for free
    if sel == Power::DarkVision && powers.dark_vision {
        powers.dark_vision = false;
        sfx.write(PostEvent::named(data.power_sound("DarkVision", level, "m_pDeactivationSoundEvent").unwrap_or("Darkvision_Stop"), None));
        powers.cooldown = 0.3;
        return;
    }
    if sel == Power::BendTime && tc.bend_remaining > 0.0 {
        tc.bend_remaining = 0.01;
        return;
    }
    if tune.inhibited && sel.key().is_some() {
        msgs.push("Your powers are blocked");
        sfx.write(PostEvent::named("Snd_Power_Empty", None));
        powers.cooldown = 0.5;
        return;
    }
    // (short of mana: a remedy drunk first, if the option says so)
    if stats.mana < cost && settings.auto_mana_elixir && stats.mana_elixirs > 0 {
        stats.mana_elixirs -= 1;
        stats.mana = (stats.mana + attrs.mana_elixir_amount(stats.max_mana, rand::random::<f32>())).min(stats.max_mana);
        stats.mana_cap = stats.mana_cap.max(stats.mana);
        sfx.write(PostEvent::named("Snd_UI_Ingame_Wheel_Mana", None));
    }
    if stats.mana < cost {
        out_of_mana(&mut powers, &mut sfx, &mut msgs);
        return;
    }
    let mut cast = true;
    match sel {
        Power::Empty | Power::Blink | Power::Heart => {}
        Power::DarkVision => {
            stats.spend_mana(cost, &attrs);
            powers.dark_vision = true;
            powers.dark_vision_left = data.power_f("DarkVision", level, "m_fDuration", 30.0);
            sfx.write(PostEvent::named(data.power_sound("DarkVision", level, "m_pActivationSoundEvent").unwrap_or("Darkvision_Start"), None));
            powers.cooldown = 0.3;
        }
        Power::BendTime => {
            stats.spend_mana(cost, &attrs);
            // level 2 stops time; its duration is counted in the stopped world's time in the
            // original, so both levels last the level-1 span here
            tc.bend_remaining = data.power_f("BendTime", 1, "m_TimeDuration", 12.0);
            tc.world_dilation = data.power_f("BendTime", level, "m_WorldDilation", 0.15);
            sfx.write(PostEvent::named(data.power_sound("BendTime", level, "m_pBendTimeSoundStartEvent").unwrap_or("Snd_Power_P_Bend_Time"), None));
            sfx.write(PostEvent::named("Snd_UI_Ingame_Slomo_Start", None));
            powers.cooldown = 0.5;
        }
        Power::Windblast => {
            stats.spend_mana(cost, &attrs);
            powers.cooldown = 1.0;
            let force = data.power_f("Windblast", level, "m_Force", 1800.0);
            let up = data.power_f("Windblast", level, "m_ExtraUpForce", 1000.0);
            let damage = data.power_f("Windblast", level, "m_Damage", 20.0);
            let reach = data.power_f("Windblast", level, "m_Distance", 3000.0) * UU * 0.3;
            let half_angle = (data.power_f("Windblast", level, "m_Angle", 135.0) * 0.5).to_radians().min(1.2);
            // (the scripts' candles and fires it blows out, the things it knocks over)
            world_hits.write(WorldDamage::player(Reach::Cone { at: eye, dir, len: reach, half: half_angle }, damage, "DisDamageType_WindBlast"));
            let flat = dir.with_y(0.0).normalize_or_zero();
            sfx.write(PostEvent::named(data.power_sound("Windblast", level, "m_pWindBlastSoundEvent").unwrap_or("Snd_Pwr_P_Windblast_Lvl_1"), None));
            // the push: level 1 throws people off their feet, level 2 against walls hard
            // enough to kill
            let push = (8.0 + (force / 1800.0).ln().max(0.0) * 4.0).min(18.0);
            let lift = (2.5 + (up / 1000.0).ln().max(0.0) * 1.5).min(6.0);
            let filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
            for (e, nt, mut npc) in &mut npcs {
                if npc.is_down() {
                    continue;
                }
                let to = nt.translation - pt.translation;
                let d = to.length();
                if d > reach || to.normalize_or_zero().dot(dir).acos() > half_angle {
                    continue;
                }
                let falloff = 1.0 - (d / reach) * 0.5;
                npc.velocity += flat * push * falloff + Vec3::Y * lift;
                stag.write(NpcStagger { npc: e, secs: 2.0, parried: false });
                // slammed into a wall?
                let wall = ctx.cast_ray(nt.translation, flat, push * 0.35, true, filter).is_some();
                let dmg = if wall && level >= 2 { 500.0 } else if wall { damage + 15.0 } else { damage };
                if dmg > 0.0 {
                    hits.write(NpcHit { npc: e, damage: dmg, kind: HitKind::Windblast, from: pt.translation });
                }
            }
            noise.write(Noise { pos: pt.translation, radius: 15.0, combat: true });
            // (and the doors before it: the wooden ones break)
            door_blast.write(crate::interact::DoorBlast { from: pt.translation, dir, reach, half_angle, damage });
        }
        Power::Possess => {
            let range = data.power_f("Possess", level, "m_fPossessRange", 1000.0) * UU;
            match crate::possession::find_target(&ctx, pe, eye, dir, range, level, npcs.iter().map(|(e, t, n)| (e, t, &*n)), hosts.iter().map(|(e, g)| (e, g.translation() + Vec3::Y * 0.1)), &possess_overrides) {
                Some(target) => {
                    stats.spend_mana(cost, &attrs);
                    start_possess.write(crate::possession::PossessRequest { host: target, level });
                    powers.cooldown = 1.0;
                }
                None => {
                    msgs.push(if level >= 2 { "No one to possess" } else { "No animal to possess" });
                    sfx.write(PostEvent::named("Snd_Power_Empty", None));
                    cast = false;
                }
            }
        }
        Power::DevouringSwarm => {
            let reach = data.power_f("DevouringSwarm", level, "m_fMaxDistance", 1500.0) * UU;
            let filter = QueryFilter::default().exclude_collider(pe).groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
            // where it lands: the floor in front, or below the aimed point
            let aim = ctx.cast_ray_and_get_normal(eye, dir, reach, true, filter).map(|(_, h)| (h.point, h.normal));
            let spot = match aim {
                Some((p, n)) if n.y > 0.5 => Some(p),
                Some((p, n)) => ctx.cast_ray(p + n * 0.3, Vec3::NEG_Y, 6.0, true, filter).map(|(_, toi)| p + n * 0.3 - Vec3::Y * toi),
                None => ctx.cast_ray(eye + dir * reach.min(6.0), Vec3::NEG_Y, 8.0, true, filter).map(|(_, toi)| eye + dir * reach.min(6.0) - Vec3::Y * toi),
            };
            // (not where rats are kept away)
            let spot = spot.filter(|at| level_info.as_ref().is_none_or(|l| crate::swarm::summon_allowed(l, *at)));
            match spot {
                Some(at) => {
                    stats.spend_mana(cost, &attrs);
                    swarm.write(crate::swarm::SummonSwarm { at, level });
                    sfx.write(PostEvent::named(data.power_sound("DevouringSwarm", level, "m_pUseSoundEvent").unwrap_or("Snd_Power_P_Devouring_Swarm_Spell"), None));
                    powers.cooldown = 1.0;
                }
                None => {
                    msgs.push("Cannot summon the Devouring Swarm here");
                    cast = false;
                }
            }
        }
        Power::Grenade | Power::StickyGrenade | Power::SpringRazor => {
            if sel.ammo(&stats) == Some(0) {
                msgs.push(format!("Out of {}s", sel.name().to_lowercase()));
                sfx.write(PostEvent::named("Snd_Power_Empty", None));
                return;
            }
            // a grenade: the pin pulled, it cooks while held (`cook_grenade`)
            if matches!(sel, Power::Grenade | Power::StickyGrenade) {
                powers.cooking = Some((sel, 0.0));
                sfx.write(PostEvent::named("Snd_Grenade_Pin", None));
                return;
            }
            gadget.write(crate::gadgets::UseGadget { gadget: crate::gadgets::Gadget::SpringRazor, eye, dir, cooked: 0.0 });
            powers.cooldown = 1.0;
        }
        Power::Crossbow | Power::SleepDart | Power::IncendiaryBolt => {
            let (kind, empty) = match sel {
                Power::SleepDart => (HitKind::SleepDart, "Out of sleep darts"),
                Power::IncendiaryBolt => (HitKind::Fire, "Out of incendiary bolts"),
                _ => (HitKind::Bolt, "Out of bolts"),
            };
            let ammo = match sel {
                Power::SleepDart => &mut stats.sleep_darts,
                Power::Crossbow => &mut stats.bolts,
                _ => stats.items.entry(crate::gadgets::FLARES.to_string()).or_default(),
            };
            if *ammo == 0 {
                msgs.push(empty);
                sfx.write(PostEvent::named("Snd_Power_Empty", None));
                return;
            }
            *ammo -= 1;
            stats.shots_fired += 1;
            powers.cooldown = 0.9;
            sfx.write(PostEvent::named("Snd_Crossbow_P_Fire", None));
            // (the bolt strays within the aim's cone; the view kicks)
            let dir = aim.scatter(dir);
            aim.fire(&data, &stats, sel);
            // (a regular bolt that will kill: the kill cam follows it, as the option has it)
            let lethal = (kind == HitKind::Bolt && data.pawn("arrow.m_bKillCamEnabled", 0.0) > 0.0)
                .then(|| ray_surface(&ctx, &surfaces, eye, dir, 80.0, QueryFilter::default().exclude_collider(pe).groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_NPC | GROUP_PROP))))
                .flatten()
                .and_then(|(hit, toi, _)| {
                    let (_, nt, n) = npcs.get(hit).ok()?;
                    let at = eye + dir * toi;
                    let damage = attrs.bolt.damage(0.0, at.y - nt.translation.y > HEAD, n.alert != crate::npc::Alert::Combat);
                    (!n.is_down() && n.min_health <= 0.0 && damage >= n.health).then_some(hit)
                })
                .filter(|&hit| {
                    let last = !npcs.iter().any(|(e, t, n)| e != hit && !n.is_down() && n.mode == crate::npc::Mode::Combat && t.translation.distance(pt.translation) < 25.0);
                    crate::combat::kill_cam(settings.kill_cam, last)
                });
            let mut e = commands.spawn((
                Projectile { vel: dir * Projectile::physics(&data, kind).0, kind, life: 6.0, stuck: false, recoverable: kind == HitKind::Bolt && data.pawn("arrow.m_bCanRecoverAmmoInAir", 1.0) > 0.0, attachment: None },
                Transform::from_translation(eye + dir * 0.4).looking_to(dir, Vec3::Y),
                Visibility::default(),
                DespawnOnExit(GameState::InGame),
            ));
            projectile_model(&mut e, assets.as_deref(), kind);
            if let Some(npc) = lethal {
                kill_cams.write(crate::killcam::StartKillCam { bolt: e.id(), npc });
            }
            noise.write(Noise { pos: pt.translation, radius: 3.0, combat: false });
        }
        Power::Pistol | Power::ExplosiveBullet => {
            let explosive = sel == Power::ExplosiveBullet;
            let ammo = if explosive { stats.items.entry(crate::gadgets::EXPLOSIVE.to_string()).or_default() } else { &mut stats.bullets };
            if *ammo == 0 {
                msgs.push(if explosive { "Out of explosive bullets" } else { "Out of bullets" });
                sfx.write(PostEvent::named("Snd_Power_Empty", None));
                return;
            }
            *ammo -= 1;
            stats.shots_fired += 1;
            powers.cooldown = 1.2;
            let dir = aim.scatter(dir);
            aim.fire(&data, &stats, sel);
            let filter = QueryFilter::default().exclude_collider(pe).groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_NPC | GROUP_PROP));
            if let Some((hit, toi, surface)) = ray_surface(&ctx, &surfaces, eye, dir, 80.0, filter) {
                let at = eye + dir * toi;
                if explosive {
                    // `Twk_Proj_Bullet_Explosive`: 500 uu reach, 200 uu full, 50 damage
                    blast.write(crate::gadgets::Explosion { at: at - dir * 0.2, radius: 5.0, full: 2.0, damage: 50.0, effect: "explosive_bullet", player: None, kind: HitKind::ExplosiveBullet });
                }
                if let Ok((_, nt, n)) = npcs.get(hit) {
                    // the shot's damage by range, to the head (a kill), on the unaware
                    let head = at.y - nt.translation.y > HEAD;
                    let damage = attrs.bullet.damage(toi, head, n.alert != crate::npc::Alert::Combat);
                    if head {
                        commands.entity(hit).try_insert(crate::npc::HeadHit);
                    }
                    hits.write(NpcHit { npc: hit, damage, kind: HitKind::Bullet, from: pt.translation });
                    sfx.write(PostEvent::named("Imp_Bullet_on_Body", Some(at)));
                } else if let Ok(s) = strikeables.get(hit) {
                    struck.write(crate::gameplay::Struck { target: s.0, damage: attrs.bullet.damage(toi, false, false), kind: HitKind::Bullet, at });
                    sfx.write(PostEvent::named("Imp_Bullet_on_Body", Some(at)));
                } else {
                    sfx.write(PostEvent::named(&format!("Imp_Bullet_on_{}", impact_surface(surface)), Some(at)));
                }
                if npcs.get(hit).is_err() {
                    world_hits.write(WorldDamage::player(Reach::Hit { collider: hit, at }, attrs.bullet.damage(toi, false, false), crate::worlddamage::bullet_type(toi, &attrs)));
                }
            }
            noise.write(Noise { pos: pt.translation, radius: 30.0, combat: true });
        }
    }
    if cast {
        powers.cast_seq += 1;
        powers.cast_power = sel;
        if sel.key().is_some() {
            used.write(PowerUsed(sel));
        }
    }
}

fn out_of_mana(powers: &mut Powers, sfx: &mut MessageWriter<PostEvent>, msgs: &mut HudMessages) {
    msgs.push("Not enough mana");
    sfx.write(PostEvent::named("Snd_Power_Empty", None));
    powers.cooldown = 0.4;
}

fn blink_travel(
    time: Res<Time>,
    data: Res<Data>,
    stats: Res<PlayerStats>,
    mut powers: ResMut<Powers>,
    mut player: Query<(&mut Transform, &mut Player)>,
    mut sfx: MessageWriter<PostEvent>,
) {
    let Some((from, to, t)) = powers.blink else { return };
    let Ok((mut pt, mut p)) = player.single_mut() else { return };
    let dur = (from.distance(to) / 55.0).clamp(0.08, 0.2);
    let nt = (t + time.delta_secs() / dur).min(1.0);
    pt.translation = from.lerp(to, nt * nt * (3.0 - 2.0 * nt));
    if nt >= 1.0 {
        let level = stats.power("Blink");
        sfx.write(PostEvent::named(data.power_sound("Blink", level, "m_pBlinkSoundEvent").unwrap_or("Snd_Power_P_Blink_Stop"), None));
        powers.blink = None;
        p.locked = false;
        p.velocity = Vec3::ZERO;
        p.grounded = false;
    } else {
        powers.blink = Some((from, to, nt));
    }
}

#[allow(clippy::too_many_arguments)]
fn update_projectiles(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<TimeControl>,
    rapier: ReadRapierContext,
    (mut hits, attrs, data): (MessageWriter<NpcHit>, Res<Attrs>, Res<Data>),
    player: Query<Entity, With<Player>>,
    npcs: Query<(&Npc, &Transform, Option<&crate::npc::NpcRig>), Without<Projectile>>,
    mut q: Query<(Entity, &mut Projectile, &mut Transform)>,
    (surfaces, mut sfx): (Query<&ColliderSurfaces>, MessageWriter<PostEvent>),
    (strikeables, mut struck, mut world_hits): (Query<&crate::gameplay::Strikeable>, MessageWriter<crate::gameplay::Struck>, MessageWriter<WorldDamage>),
    globals: Query<&GlobalTransform>,
) {
    let dt = time.delta_secs() * tc.world_scale();
    let Ok(ctx) = rapier.single() else { return };
    let pe = player.single().ok();
    for (e, mut pr, mut t) in &mut q {
        let mut embedded_in_living = false;
        if let Some(a) = pr.attachment {
            let Ok((npc, nt, rig)) = npcs.get(a.npc) else {
                commands.entity(e).despawn();
                continue;
            };
            let frame = a.bone.and_then(|i| rig?.joint_at(i)).and_then(|e| globals.get(e).ok()).copied().unwrap_or(GlobalTransform::from(*nt));
            t.translation = frame.affine().transform_point3(a.offset);
            t.rotation = frame.compute_transform().rotation * a.rotation;
            embedded_in_living = !npc.is_down();
        }
        // Recoverable bolts remain on surfaces and corpses. Only free flight and
        // bolts embedded in living enemies consume their lifespan.
        if !pr.stuck || !pr.recoverable || embedded_in_living {
            pr.life -= dt;
        }
        if pr.life <= 0.0 {
            commands.entity(e).despawn();
            continue;
        }
        if pr.stuck {
            continue;
        }
        let acceleration = Vec3::Y * Projectile::physics(&data, pr.kind).1;
        let step = pr.vel * dt + acceleration * (0.5 * dt * dt);
        pr.vel += acceleration * dt;
        let len = step.length();
        if len <= 0.0 {
            continue;
        }
        let mut filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_NPC | GROUP_PROP));
        if let Some(pe) = pe {
            filter = filter.exclude_collider(pe);
        }
        if let Some((hit, toi, surface)) = ray_surface(&ctx, &surfaces, t.translation, step / len, len, filter) {
            t.translation += step / len * toi;
            pr.stuck = true;
            pr.life = pr.life.min(5.0);
            let what = if npcs.get(hit).is_ok() { "Body" } else { impact_surface(surface) };
            if pr.kind == HitKind::Bolt {
                pr.recoverable = data.pawn("arrow.m_bCanRecoverAmmoInEnvironment", 1.0) > 0.0;
            }
            sfx.write(PostEvent::named(&format!("Imp_Arrow_on_{what}"), Some(t.translation)));
            if pr.kind == HitKind::Fire && npcs.get(hit).is_err() {
                sfx.write(PostEvent::named(&format!("Imp_Arrow_on_{what}_Flare"), Some(t.translation)));
            }
            if let Ok(s) = strikeables.get(hit) {
                let damage = match pr.kind {
                    HitKind::Bolt => attrs.bolt.damage,
                    HitKind::Fire => 999.0,
                    _ => 0.0,
                };
                struck.write(crate::gameplay::Struck { target: s.0, damage, kind: pr.kind, at: t.translation });
            }
            if npcs.get(hit).is_err() {
                let kind = crate::worlddamage::hit_type(pr.kind, 0.0, &attrs);
                let damage = if pr.kind == HitKind::SleepDart { 0.0 } else { attrs.bolt.damage };
                world_hits.write(WorldDamage::player(Reach::Hit { collider: hit, at: t.translation }, damage, kind));
            }
            if let Ok((npc, nt, rig)) = npcs.get(hit) {
                if !npc.is_down() {
                    // a bolt: doubled on the unaware (`m_fDamageMultiplier_Stealth`), more to the head
                    let damage = match pr.kind {
                        HitKind::Bolt => attrs.bolt.damage(0.0, t.translation.y - nt.translation.y > HEAD, npc.alert != crate::npc::Alert::Combat),
                        HitKind::Fire => 999.0,
                        _ => 0.0,
                    };
                    hits.write(NpcHit { npc: hit, damage, kind: pr.kind, from: t.translation });
                }
                if pr.kind == HitKind::Bolt && data.pawn("arrow.m_bCanRecoverAmmoInBodies", 1.0) > 0.0
                    && Projectile::survives_body_hit(data.pawn("arrow.m_fChanceToBreak", 0.7), attrs.bolt_break_modifier, rand::random::<f32>()) {
                    let nearest = rig.and_then(|r| (0..r.len()).filter_map(|i| Some((i, *globals.get(r.joint_at(i)?).ok()?)))
                        .min_by(|a, b| a.1.translation().distance_squared(t.translation).total_cmp(&b.1.translation().distance_squared(t.translation))));
                    let frame = nearest.map(|(_, g)| g).unwrap_or(GlobalTransform::from(*nt));
                    pr.attachment = Some(BoltAttachment { npc: hit, bone: nearest.map(|(i, _)| i),
                        offset: frame.affine().inverse().transform_point3(t.translation), rotation: frame.compute_transform().rotation.inverse() * t.rotation });
                    pr.recoverable = true;
                    pr.life = data.pawn("arrow.m_fStuckInAlivePawnVanishTime", 10.0).max(0.01);
                } else {
                    commands.entity(e).despawn();
                }
            }
        } else {
            t.translation += step;
            let v = pr.vel;
            t.look_to(v, Vec3::Y);
        }
    }
}

#[cfg(test)]
mod projectile_tests {
    use super::*;

    #[test]
    fn arrow_variants_use_original_gravity_and_speed_modifiers() {
        let mut data = Data::default();
        for (kind, speed, gravity) in [(HitKind::Bolt, 200.0, -19.5), (HitKind::SleepDart, 140.0, -6.0), (HitKind::Fire, 40.0, -7.5)] {
            let actual = Projectile::physics(&data, kind);
            assert!((actual.0 - speed).abs() < 1e-5);
            assert!((actual.1 - gravity).abs() < 1e-5);
        }
        data.0.pawn.insert("world.DefaultGravityZ".into(), -1000.0);
        data.0.pawn.insert("arrow_sleep.m_fSpeedMultiplier".into(), 0.5);
        data.0.pawn.insert("arrow_sleep.m_fGravityMultiplier".into(), 0.2);
        assert_eq!(Projectile::physics(&data, HitKind::SleepDart), (100.0, -2.0));
    }

    #[test]
    fn loading_preserves_active_powers_and_resets_transient_menu_slowdown() {
        let powers = Powers { selected: Power::Crossbow, dark_vision: true, dark_vision_left: 6.5,
            cooldown: 0.4, cooking: Some((Power::Grenade, 1.2)), blink: Some((Vec3::X, Vec3::Z, 0.05)), ..default() };
        let tc = TimeControl { bend_remaining: 4.5, world_dilation: 0.0, wheel: 0.1, finisher: 0.2, scripted: Some((0.3, 7.0)) };
        let bytes = serde_json::to_vec(&PowersSave::capture(&powers, &tc)).unwrap();
        let saved: PowersSave = serde_json::from_slice(&bytes).unwrap();
        let mut loaded = Powers::default();
        let mut time = TimeControl::default();
        saved.restore(&mut loaded, &mut time);
        assert_eq!(loaded.selected, Power::Crossbow);
        assert!(loaded.dark_vision);
        assert_eq!(loaded.dark_vision_left, 6.5);
        assert_eq!(loaded.cooldown, 0.4);
        assert_eq!(loaded.cooking, powers.cooking);
        assert_eq!(loaded.blink, powers.blink);
        assert_eq!(time.bend_remaining, 4.5);
        assert_eq!(time.world_scale(), 0.0);
        assert_eq!(time.scripted, Some((0.3, 7.0)));
        assert_eq!(time.wheel, 1.0);
        assert_eq!(time.finisher, 1.0);
        PowersSave::capture(&Powers::default(), &TimeControl::default()).restore(&mut loaded, &mut time);
        assert!(!loaded.dark_vision);
        assert!(loaded.cooking.is_none());
        assert!(loaded.blink.is_none());
        assert_eq!(time.world_scale(), 1.0);
    }

    #[test]
    fn reinforced_bolts_reduce_breakage_and_recovery_respects_capacity() {
        let ordinary = (0..100).filter(|i| Projectile::survives_body_hit(0.7, 0.0, (*i as f32 + 0.5) / 100.0)).count();
        let reinforced = (0..100).filter(|i| Projectile::survives_body_hit(0.7, -0.5, (*i as f32 + 0.5) / 100.0)).count();
        assert_eq!(ordinary, 30);
        assert_eq!(reinforced, 80);
        assert!(Projectile::survives_body_hit(0.7, -2.0, 0.0));
        assert!(!Projectile::survives_body_hit(0.7, 2.0, 0.99));
        let mut stats = PlayerStats::default();
        stats.bolts = 9;
        let mut attrs = Attrs::default();
        assert!(recover_bolt(&mut stats, &attrs));
        assert_eq!(stats.bolts, 10);
        assert!(!recover_bolt(&mut stats, &attrs));
        assert_eq!(stats.bolts, 10);
        attrs.bolt_capacity = 20;
        assert!(recover_bolt(&mut stats, &attrs));
        assert_eq!(stats.bolts, 11);
    }

    #[test]
    fn projectile_save_remaps_embedded_bolts_and_preserves_flight() {
        let mut old = World::new();
        let npc = old.spawn_empty().id();
        let stuck = Projectile { vel: Vec3::ZERO, kind: HitKind::Bolt, life: 7.5, stuck: true, recoverable: true,
            attachment: Some(BoltAttachment { npc, bone: Some(7), offset: Vec3::X * 0.1, rotation: Quat::from_rotation_z(0.3) }) };
        let flying = Projectile { vel: Vec3::X * 45.0, kind: HitKind::Bolt, life: 4.0, stuck: false, recoverable: true, attachment: None };
        let pose = Transform::from_xyz(1.0, 2.0, 3.0);
        let saved = save_projectiles([(&stuck, &pose), (&flying, &pose)], [(npc, 11)]);
        let saved = serde_json::from_slice::<Vec<ProjectileSave>>(&serde_json::to_vec(&saved).unwrap()).unwrap();
        let mut app = App::new();
        app.insert_resource(ProjectileRestore(Some(saved))).add_systems(Update, restore_projectiles);
        for _ in 0..10 { app.world_mut().spawn_empty(); }
        let restored_npc = app.world_mut().spawn(crate::npc::FromSpawner(11)).id();
        assert_ne!(npc, restored_npc);
        let stale = app.world_mut().spawn((flying, pose)).id();
        app.update();
        assert!(app.world().get_entity(stale).is_err());
        let mut q = app.world_mut().query::<(&Projectile, &Transform)>();
        assert_eq!(q.iter(app.world()).count(), 2);
        for (p, t) in q.iter(app.world()) {
            assert_eq!(t.translation, pose.translation);
            assert!(p.recoverable);
            if p.stuck {
                let a = p.attachment.unwrap();
                assert_eq!(a.npc, restored_npc);
                assert_eq!(a.bone, Some(7));
                assert_eq!(a.offset, Vec3::X * 0.1);
                assert_eq!(a.rotation, Quat::from_rotation_z(0.3));
                assert_eq!(p.life, 7.5);
            } else {
                assert_eq!(p.vel, Vec3::X * 45.0);
                assert_eq!(p.life, 4.0);
            }
        }
        app.world_mut().resource_mut::<ProjectileRestore>().0 = Some(Vec::new());
        app.update();
        assert_eq!(q.iter(app.world()).count(), 0);
    }

    #[test]
    fn bend_time_freezes_bolt_motion_gravity_and_lifetime() {
        let mut app = App::new();
        app.init_resource::<Time>().init_resource::<Attrs>().init_resource::<Data>()
            .insert_resource(TimeControl { bend_remaining: 10.0, world_dilation: 0.0, ..default() })
            .add_message::<NpcHit>().add_message::<PostEvent>().add_message::<crate::gameplay::Struck>().add_message::<WorldDamage>()
            .add_systems(Update, update_projectiles);
        app.world_mut().spawn((DefaultRapierContext, RapierContextSimulation::default()));
        let e = app.world_mut().spawn((Projectile { vel: Vec3::X * 10.0, kind: HitKind::Bolt, life: 10.0, stuck: false, recoverable: true, attachment: None }, Transform::IDENTITY)).id();
        app.world_mut().resource_mut::<Time>().advance_by(std::time::Duration::from_secs(1));
        app.update();
        assert_eq!(app.world().get::<Projectile>(e).unwrap().life, 10.0);
        assert_eq!(app.world().get::<Projectile>(e).unwrap().vel, Vec3::X * 10.0);
        assert_eq!(app.world().get::<Transform>(e).unwrap().translation, Vec3::ZERO);
        app.world_mut().resource_mut::<TimeControl>().world_dilation = 0.1;
        app.update();
        assert!((app.world().get::<Projectile>(e).unwrap().life - 9.9).abs() < 1e-5);
        assert!((app.world().get::<Transform>(e).unwrap().translation.x - 1.0).abs() < 1e-5);
        assert!((app.world().get::<Transform>(e).unwrap().translation.y + 0.0975).abs() < 1e-5);
        assert!((app.world().get::<Projectile>(e).unwrap().vel.y + 1.95).abs() < 1e-5);
        app.world_mut().resource_mut::<TimeControl>().bend_remaining = 0.0;
        app.update();
        assert!((app.world().get::<Projectile>(e).unwrap().life - 8.9).abs() < 1e-5);
        assert!((app.world().get::<Transform>(e).unwrap().translation.x - 11.0).abs() < 1e-5);
        assert!((app.world().get::<Transform>(e).unwrap().translation.y + 11.7975).abs() < 1e-4);
        // An intact bolt stuck in the world must not vanish before it is looted.
        {
            let mut projectile = app.world_mut().get_mut::<Projectile>(e).unwrap();
            projectile.stuck = true;
            projectile.life = 0.1;
        }
        app.update();
        assert_eq!(app.world().get::<Projectile>(e).unwrap().life, 0.1);
        assert!((app.world().get::<Transform>(e).unwrap().translation.x - 11.0).abs() < 1e-5);
    }
}

/// A grenade cooking in the hand (the HUD's `GrenadeCooking`): its fuse burns while the button
/// is held; let go, it's thrown with what's left of it; [Use] puts it back
/// (`DUI_Context_CancelGrenadeCooking`); held to the end, it goes off in the hand.
#[allow(clippy::too_many_arguments)]
fn cook_grenade(
    time: Res<Time>,
    paused: Res<crate::hud::Paused>,
    tc: Res<TimeControl>,
    mouse: Res<ButtonInput<MouseButton>>,
    (keys, bind): (Res<ButtonInput<KeyCode>>, Res<crate::bindings::Bindings>),
    mut powers: ResMut<Powers>,
    stats: Res<PlayerStats>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    mut gadget: MessageWriter<crate::gadgets::UseGadget>,
) {
    if paused.0 { return; }
    let Some((p, t)) = powers.cooking else { return };
    let Ok(cg) = cam.single() else { return };
    if stats.dead || powers.selected != p {
        powers.cooking = None;
        return;
    }
    if keys.just_pressed(bind.key(crate::bindings::Act::Use)) {
        powers.cooking = None;
        powers.swallow = true;
        return;
    }
    let t = t + time.delta_secs() * tc.world_scale();
    let g = if p == Power::StickyGrenade { crate::gadgets::Gadget::StickyGrenade } else { crate::gadgets::Gadget::Grenade };
    let (eye, dir) = (cg.translation(), cg.forward().as_vec3());
    if t >= crate::gadgets::FUSE || !mouse.pressed(MouseButton::Right) {
        powers.cooking = None;
        powers.cooldown = 1.0;
        gadget.write(crate::gadgets::UseGadget { gadget: g, eye, dir, cooked: t.min(crate::gadgets::FUSE) });
    } else {
        powers.cooking = Some((p, t));
    }
}

#[cfg(test)]
mod elixir_input_tests {
    use super::*;

    #[test]
    fn menu_number_keys_do_not_change_equipped_power() {
        let mut app = App::new();
        app.init_resource::<ButtonInput<KeyCode>>().init_resource::<AccumulatedMouseScroll>()
            .init_resource::<PlayerStats>().init_resource::<Powers>()
            .init_resource::<crate::possession::Possession>().init_resource::<crate::choice::Choice>()
            .insert_resource(crate::hud::Paused(true))
            .add_message::<PowerEquipped>().add_message::<PostEvent>().add_systems(Update, select_power);
        app.world_mut().resource_mut::<PlayerStats>().weapons = true;
        app.world_mut().resource_mut::<Powers>().selected = Power::Empty;
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::Digit1);
        app.update();
        assert_eq!(app.world().resource::<Powers>().selected, Power::Empty);
        app.world_mut().resource_mut::<crate::hud::Paused>().0 = false;
        app.update();
        assert_eq!(app.world().resource::<Powers>().selected, Power::Pistol);
    }

    #[test]
    fn paused_mouse_release_keeps_a_cooking_grenade_in_hand() {
        let mut app = App::new();
        let mut powers = Powers::default();
        powers.selected = Power::Grenade;
        powers.cooking = Some((Power::Grenade, 0.5));
        app.init_resource::<Time>().init_resource::<TimeControl>()
            .init_resource::<ButtonInput<KeyCode>>().init_resource::<ButtonInput<MouseButton>>()
            .init_resource::<crate::bindings::Bindings>().init_resource::<PlayerStats>()
            .insert_resource(crate::hud::Paused(true)).insert_resource(powers)
            .add_message::<crate::gadgets::UseGadget>().add_systems(Update, cook_grenade);
        app.world_mut().spawn((PlayerCamera, GlobalTransform::IDENTITY));
        app.update();
        assert_eq!(app.world().resource::<Powers>().cooking, Some((Power::Grenade, 0.5)));
        assert_eq!(app.world().resource::<Messages<crate::gadgets::UseGadget>>().len(), 0);
        app.world_mut().resource_mut::<crate::hud::Paused>().0 = false;
        app.update();
        assert!(app.world().resource::<Powers>().cooking.is_none());
        assert_eq!(app.world().resource::<Messages<crate::gadgets::UseGadget>>().len(), 1);
    }

    #[test]
    fn paused_menu_shortcuts_do_not_drink_elixirs() {
        let mut app = App::new();
        let mut stats = PlayerStats::default();
        stats.health = 10.0;
        stats.max_health = 100.0;
        stats.mana = 10.0;
        stats.max_mana = 100.0;
        stats.mana_cap = 10.0;
        stats.health_elixirs = 2;
        stats.mana_elixirs = 2;
        app.init_resource::<Time>().init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<crate::bindings::Bindings>().init_resource::<Attrs>()
            .init_resource::<HudMessages>().insert_resource(crate::hud::Paused(true))
            .insert_resource(stats)
            .add_message::<PostEvent>().add_systems(Update, regenerate);
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::KeyR);
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::KeyT);
        app.update();
        let stats = app.world().resource::<PlayerStats>();
        assert_eq!((stats.health_elixirs, stats.mana_elixirs), (2, 2));
        assert_eq!((stats.health, stats.mana), (10.0, 10.0));
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().clear();
        app.world_mut().resource_mut::<crate::hud::Paused>().0 = false;
        app.update();
        assert_eq!(app.world().resource::<PlayerStats>().health_elixirs, 2);
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().release_all();
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::KeyR);
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::KeyT);
        app.update();
        let stats = app.world().resource::<PlayerStats>();
        assert_eq!((stats.health_elixirs, stats.mana_elixirs), (1, 1));
        assert!(stats.health > 10.0 && stats.mana > 10.0);
    }
}

/// Mana and health regeneration (the original's step regeneration up to a cap) and elixirs
/// (`GBA_HealthElixir` R, `GBA_ManaElixir` T).
#[allow(clippy::too_many_arguments)]
fn regenerate(
    time: Res<Time>,
    paused: Res<crate::hud::Paused>,
    (keys, bind): (Res<ButtonInput<KeyCode>>, Res<crate::bindings::Bindings>),
    attrs: Res<Attrs>,
    mut stats: ResMut<PlayerStats>,
    mut msgs: ResMut<HudMessages>,
    mut sfx: MessageWriter<PostEvent>,
    mut last_health: Local<f32>,
) {
    if paused.0 { return; }
    let dt = time.delta_secs();
    stats.damage_flash = (stats.damage_flash - dt * 1.5).max(0.0);
    if stats.dead {
        *last_health = 0.0;
        return;
    }
    // mana: after a delay, back up to the cap left by the last expense
    stats.mana_delay = (stats.mana_delay - dt).max(0.0);
    if stats.mana_delay <= 0.0 && stats.mana < stats.mana_cap {
        stats.mana = (stats.mana + attrs.mana_regen_rate * dt).min(stats.mana_cap);
    }
    // health: hurt restarts the delay; then it regenerates up to its limit
    if stats.health < *last_health - 0.01 {
        stats.health_delay = attrs.health_regen_delay;
    }
    stats.health_delay = (stats.health_delay - dt).max(0.0);
    if stats.health_delay <= 0.0 && stats.health < attrs.health_regen_limit.min(stats.max_health) {
        stats.health = (stats.health + attrs.health_regen_rate * dt).min(attrs.health_regen_limit.min(stats.max_health));
    }
    *last_health = stats.health;
    if keys.just_pressed(bind.key(crate::bindings::Act::HealthElixir)) {
        if stats.health_elixirs > 0 && stats.health < stats.max_health {
            stats.health_elixirs -= 1;
            stats.health = (stats.health + attrs.health_elixir).min(stats.max_health);
            *last_health = stats.health;
            sfx.write(PostEvent::named("Snd_UI_Ingame_Wheel_Life", None));
        } else if stats.health_elixirs == 0 {
            msgs.push("No health elixirs");
        }
    }
    if keys.just_pressed(bind.key(crate::bindings::Act::ManaElixir)) {
        if stats.mana_elixirs > 0 && stats.mana < stats.max_mana {
            stats.mana_elixirs -= 1;
            stats.mana = (stats.mana + attrs.mana_elixir_amount(stats.max_mana, rand::random::<f32>())).min(stats.max_mana);
            stats.mana_cap = stats.mana_cap.max(stats.mana);
            sfx.write(PostEvent::named("Snd_UI_Ingame_Wheel_Mana", None));
        } else if stats.mana_elixirs == 0 {
            msgs.push("No mana elixirs");
        }
    }
}

/// Dark Vision's grading over the world's (`Twk_DarkVision`'s `m_StaticUberAdjustement`), fading
/// in and out over its level's durations.
fn dark_vision_post(powers: Res<Powers>, data: Res<Data>, stats: Res<PlayerStats>, mut post: ResMut<crate::postfx::PowerPost>) {
    use crate::postfx::PowerPost;
    if powers.dark_vision == post.layers.contains_key(&PowerPost::DARK_VISION) {
        return;
    }
    if !powers.dark_vision {
        post.set(PowerPost::DARK_VISION, None);
        return;
    }
    let level = stats.power("DarkVision").max(1);
    let layer = data.active("DarkVision").map(|a| crate::postfx::PostLayer {
        fields: crate::postfx::uber_fields(&a.params, "m_PpGlobalParameters.m_StaticUberAdjustement."),
        fade_in: data.power_f("DarkVision", level, "m_PpPerLevelParameters.m_PowerFadeInDuration", 0.1),
        fade_out: data.power_f("DarkVision", level, "m_PpPerLevelParameters.m_PowerFadeOutDuration", 0.2),
    });
    post.set(PowerPost::DARK_VISION, layer);
}

pub fn configure_xray_gizmos(mut store: ResMut<GizmoConfigStore>) {
    let (cfg, _) = store.config_mut::<DefaultGizmoConfigGroup>();
    cfg.depth_bias = -1.0;
    cfg.line.width = 2.0;
}
