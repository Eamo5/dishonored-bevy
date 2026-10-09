//! Gadgets bought from Piero (and found): grenades and sticky grenades, springrazors,
//! incendiary bolts and explosive bullets, with the original tweaks' numbers
//! (`DisTweaks_Explosion`: 6 m reach, full damage within 3 m; `DisTweaks_SpringRazorPlaced`:
//! a 2.5 m trigger cylinder, 0.5 s arming, shrapnel to 9 m).

use crate::audio::PostEvent;
use crate::gameplay::{HitKind, HudMessages, Noise, NpcHit, PlayerStats, TimeControl};
use crate::level::{GameAssets, GROUP_NPC, GROUP_PROP, GROUP_WORLD};
use crate::npc::Npc;
use crate::particles::SpawnEffect;
use crate::player::Player;
use crate::GameState;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

pub struct GadgetsPlugin;

impl Plugin for GadgetsPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<UseGadget>()
            .init_resource::<GadgetRestore>()
            .add_message::<Explosion>()
            .add_message::<NpcGrenade>()
            .add_systems(Update, grenade_focus.after(crate::interact::FocusSet).before(crate::props::prop_focus).before(crate::interact::use_focus).run_if(in_state(GameState::InGame)))
            .add_systems(Update, restore_gadgets.after(crate::save::restore_npcs).before(grenade_focus).run_if(in_state(GameState::InGame)))
            .add_systems(Update, (use_gadget, npc_grenades, fly_grenades, razors, explode).chain().after(grenade_focus).run_if(in_state(GameState::InGame)));
    }
}

/// The gadgets' ammunition in the inventory (store items).
pub const GRENADES: &str = "Grenade_Ammo_WithItem_twk";
pub const STICKY: &str = "StickyGrenade_Ammo_WithItem_twk";
pub const RAZORS: &str = "SpringRazor_Ammo_WithItem_twk";
pub const FLARES: &str = "Flare_Ammo_twk";
pub const EXPLOSIVE: &str = "ExplosiveBullets_Ammo_twk";

/// Ammunition by the original ammo type (the index into `m_AmmoRanges`): adds it and names it.
pub fn give_ammo(stats: &mut PlayerStats, attrs: &crate::gamedata::Attrs, ty: u8, n: u32) -> (&'static str, u32) {
    let Some(&capacity) = attrs.ammo_capacity.get(ty as usize) else { return ("Ammo", 0) };
    let count = ammo_mut(stats, ty, true).unwrap();
    let added = n.min(capacity.saturating_sub(*count));
    *count += added;
    (ammo_name(ty), added)
}

pub fn ammo_count(stats: &PlayerStats, ty: u8) -> u32 {
    match ty {
        0 => stats.bullets,
        2 => stats.bolts,
        3 => stats.sleep_darts,
        _ => ammo_item(ty).and_then(|id| stats.items.get(id)).copied().unwrap_or(0),
    }
}

fn ammo_item(ty: u8) -> Option<&'static str> {
    Some(match ty { 1 => EXPLOSIVE, 4 => FLARES, 5 => RAZORS, 6 => GRENADES, 7 => STICKY, _ => return None })
}

pub fn ammo_type(item: &str) -> Option<u8> {
    Some(match item {
        "Bullets_Store_Ammo_twk" => 0,
        "Bolt_Ammo_twk" | "Bolts_Ammo_twk" => 2,
        "SleepDart_Ammo_twk" => 3,
        _ => return (0..8).find(|ty| ammo_item(*ty) == Some(item)),
    })
}

pub fn ammo_name(ty: u8) -> &'static str {
    let item = match ty {
        0 => {
            return "Bullets";
        }
        2 => {
            return "Crossbow Bolts";
        }
        3 => {
            return "Sleep Darts";
        }
        1 => (EXPLOSIVE, "Explosive Bullets"),
        4 => (FLARES, "Incendiary Bolts"),
        5 => (RAZORS, "Springrazors"),
        6 => (GRENADES, "Grenades"),
        _ => (STICKY, "Sticky Grenades"),
    };
    item.1
}

/// The count of an ammunition type (the original's `eDisAmmoType` order, as `give_ammo`);
/// a gadget's only when had, or `add` to have it.
pub fn ammo_mut(stats: &mut PlayerStats, ty: u8, add: bool) -> Option<&mut u32> {
    let key = match ty {
        0 => return Some(&mut stats.bullets),
        2 => return Some(&mut stats.bolts),
        3 => return Some(&mut stats.sleep_darts),
        1 => EXPLOSIVE,
        4 => FLARES,
        5 => RAZORS,
        6 => GRENADES,
        _ => STICKY,
    };
    if add {
        Some(stats.items.entry(key.to_string()).or_default())
    } else {
        stats.items.get_mut(key)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Gadget {
    Grenade,
    StickyGrenade,
    SpringRazor,
}

/// Throw or place a gadget (the left hand, aimed along `dir`).
#[derive(Message, Clone, Copy)]
pub struct UseGadget {
    pub gadget: Gadget,
    pub eye: Vec3,
    pub dir: Vec3,
    /// a grenade's fuse already burnt in the hand (cooking): all of it, it goes off there
    pub cooked: f32,
}

/// A blast: everyone within `radius` is hurt, fully within `full`.
#[derive(Message, Clone, Copy)]
pub struct Explosion {
    pub at: Vec3,
    pub radius: f32,
    pub full: f32,
    pub damage: f32,
    /// the original effect (`scene.effects`)
    pub effect: &'static str,
    /// its reach to Corvo, all and full (`m_fPlayerDamageRadius`, `m_fPlayerFullDamageRadius`;
    /// else `radius` and 1.5 m)
    pub player: Option<[f32; 2]>,
    /// Damage attribution survives the projectile, including a returned enemy grenade.
    pub kind: HitKind,
}

/// A grenade a character throws (an overseer's): from where, how fast, its fuse (and the most
/// left once it hits someone), its blast.
#[derive(Message, Clone)]
pub struct NpcGrenade {
    pub owner: Entity,
    pub from: Vec3,
    pub vel: Vec3,
    pub fuse: [f32; 2],
    pub blast: dhcook::format::TrapBlast,
}

/// What pulls a thrown grenade down (m/s², the original's 1500 uu/s²).
const GRAVITY: f32 = 15.0;

/// The velocity that throws from `from` to land at `to` at `speed` (m/s): the low arc (at 45
/// degrees, as far as it goes, when it can't reach).
pub fn aim_throw(from: Vec3, to: Vec3, speed: f32) -> Vec3 {
    let d = to - from;
    let flat = d.with_y(0.0);
    let x = flat.length().max(0.01);
    let dir = flat / x;
    let v2 = speed * speed;
    let disc = v2 * v2 - GRAVITY * (GRAVITY * x * x + 2.0 * d.y * v2);
    let ang = if disc < 0.0 { std::f32::consts::FRAC_PI_4 } else { ((v2 - disc.sqrt()) / (GRAVITY * x)).atan() };
    dir * ang.cos() * speed + Vec3::Y * ang.sin() * speed
}

/// The grenade's throw and fuse (`Twk_Inv_GrenadeCorvo`'s `DisTweaks_ThrowGrenade`,
/// `Twk_Proj_Grenade`'s `DisTweaks_GrenadeComponent`): its speed (m/s), the angle above the
/// view, the fuse, and the fuse left at most once it hits someone.
const THROW_SPEED: f32 = 25.0;
const THROW_ANGLE: f32 = 5.0;
pub const FUSE: f32 = 3.0;
const FUSE_AFTER_HIT: f32 = 0.5;

/// The view's direction pitched up by `deg`.
fn throw_dir(dir: Vec3, deg: f32) -> Vec3 {
    let flat = dir.with_y(0.0);
    let right = flat.cross(Vec3::Y).normalize_or(Vec3::X);
    (Quat::from_axis_angle(right, deg.to_radians()) * dir).normalize_or(dir)
}

#[derive(Component)]
pub struct Grenade {
    vel: Vec3,
    pub fuse: f32,
    /// the whole fuse, and the most left once it hits someone
    pub fuse_total: f32,
    after_hit: f32,
    sticky: bool,
    stuck: Option<(Entity, Vec3)>,
    /// its blast: reach to characters (all, full), to Corvo (all, full), damage
    pub blast: [f32; 5],
    /// who threw it (a character: its own body doesn't stop it, Corvo's does)
    owner: Option<Entity>,
    throwback: bool,
    /// Kept separately from adhesion, which changes after a sticky grenade lands.
    kind: HitKind,
}

impl Grenade {
    fn take(&mut self) {
        self.throwback |= self.owner.take().is_some();
        if self.kind == HitKind::EnemyExplosion {
            self.kind = HitKind::Explosion;
        }
        self.stuck = None;
        self.vel = Vec3::ZERO;
        // Picking one up never restarts its fuse.
    }

    fn release(&mut self, direction: Vec3, velocity: Vec3, strength: f32, thrown: bool) {
        self.vel = velocity.with_y(0.0) + if thrown { throw_dir(direction, THROW_ANGLE) * strength.max(5.0) } else { direction * 1.0 };
        if thrown && self.throwback {
            self.kind = HitKind::GrenadeThrowback;
        }
    }
}

/// The armed projectile's original interaction is "Tap `k to carry". It stays
/// live in the hand, uses the ordinary movable drop/throw prompts, and consumes
/// no inventory ammunition when thrown back.
#[allow(clippy::too_many_arguments)]
pub(crate) fn grenade_focus(
    (keys, mouse, bind): (Res<ButtonInput<KeyCode>>, Res<ButtonInput<MouseButton>>, Res<crate::bindings::Bindings>),
    mut focus: ResMut<crate::interact::InteractFocus>,
    mut held: ResMut<crate::props::Held>,
    mut grenades: Query<(Entity, &mut Grenade, &Transform)>,
    player: Query<(Entity, &Player)>,
    cam: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    rapier: ReadRapierContext,
    (stats, attrs, carry, possession, peek): (Res<PlayerStats>, Res<crate::gamedata::Attrs>, Res<crate::carry::Carry>, Res<crate::possession::Possession>, Res<crate::keyhole::Peek>),
    mut sfx: MessageWriter<PostEvent>,
) {
    let (Ok((pe, p)), Ok(camera)) = (player.single(), cam.single()) else { return };
    let use_key = bind.key(crate::bindings::Act::Use);
    if let Some(e) = held.0 {
        let Ok((_, mut grenade, _)) = grenades.get_mut(e) else { return };
        focus.0 = true;
        focus.1 = None;
        focus.2.clear();
        focus.3.clear();
        let thrown = mouse.just_pressed(MouseButton::Left) && !stats.dead && possession.host.is_none();
        if thrown || keys.just_pressed(use_key) || stats.dead || possession.host.is_some() {
            grenade.release(camera.forward().as_vec3(), p.velocity, attrs.throw, thrown);
            held.0 = None;
            held.1 = 0.3;
            if thrown {
                sfx.write(PostEvent::named("Snd_Grenade_Throw", None));
            }
        }
        return;
    }
    if held.busy() || stats.dead || p.locked || carry.carrying() || possession.host.is_some() || peek.at.is_some() {
        return;
    }
    let Ok(ctx) = rapier.single() else { return };
    let eye = camera.translation();
    let dir = camera.forward().as_vec3();
    let filter = QueryFilter::default().exclude_collider(pe).groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP | GROUP_NPC));
    let nearest = grenades.iter().filter_map(|(e, grenade, t)| {
        let delta = t.translation - eye;
        let along = delta.dot(dir);
        if grenade.fuse <= 0.0 || !(0.0..=2.2).contains(&along) || (delta - dir * along).length_squared() > 0.18 * 0.18 {
            return None;
        }
        let distance = delta.length();
        if distance > 0.12 && ctx.cast_ray(eye, delta / distance, distance - 0.12, true, filter).is_some() {
            return None;
        }
        Some((e, along))
    }).min_by(|a, b| a.1.total_cmp(&b.1));
    let Some((e, _)) = nearest else { return };
    focus.0 = true;
    focus.1 = Some(e);
    focus.2 = format!("Tap {} Carry", crate::bindings::hint(crate::bindings::Act::Use));
    focus.3 = "Armed Grenade".into();
    if keys.just_pressed(use_key) {
        if let Ok((_, mut grenade, _)) = grenades.get_mut(e) {
            grenade.take();
            held.0 = Some(e);
        }
    }
}

/// Corvo's grenade's blast (`Twk_Proj_Grenade`'s `DisTweaks_Explosion`: 600 uu reach, 300 uu
/// full damage, 600 / 150 uu to him, 50 damage).
const BLAST: [f32; 5] = [6.0, 3.0, 6.0, 1.5, 50.0];

#[derive(Component)]
pub(crate) struct Razor {
    armed: f32,
    triggered: Option<f32>,
}

/// Entity references use saved NPC spawners, never runtime entity IDs.
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct GadgetsSave {
    grenades: Vec<GrenadeSave>,
    razors: Vec<RazorSave>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct GrenadeSave {
    position: [f32; 3],
    rotation: [f32; 4],
    velocity: [f32; 3],
    fuse: f32,
    fuse_total: f32,
    after_hit: f32,
    sticky: bool,
    /// None: free flight; Some(None, ..): stuck to the world.
    stuck: Option<(Option<u32>, [f32; 3])>,
    blast: [f32; 5],
    owner: Option<u32>,
    throwback: bool,
    kind: HitKind,
    held: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct RazorSave {
    position: [f32; 3],
    rotation: [f32; 4],
    armed: f32,
    triggered: Option<f32>,
}

#[derive(Resource, Default)]
pub struct GadgetRestore(pub Option<GadgetsSave>);

impl GadgetsSave {
    pub(crate) fn capture<'a>(
        grenades: impl IntoIterator<Item = (Entity, &'a Grenade, &'a Transform)>,
        razors: impl IntoIterator<Item = (&'a Razor, &'a Transform)>,
        npcs: impl IntoIterator<Item = (Entity, u32)>,
        held: Option<Entity>,
    ) -> Self {
        let npcs: std::collections::HashMap<_, _> = npcs.into_iter().collect();
        Self {
            grenades: grenades.into_iter().map(|(e, g, t)| GrenadeSave {
                position: t.translation.to_array(), rotation: t.rotation.to_array(), velocity: g.vel.to_array(),
                fuse: g.fuse, fuse_total: g.fuse_total, after_hit: g.after_hit, sticky: g.sticky,
                stuck: g.stuck.map(|(e, offset)| (npcs.get(&e).copied(), offset.to_array())),
                blast: g.blast, owner: g.owner.and_then(|e| npcs.get(&e).copied()),
                throwback: g.throwback, kind: g.kind, held: held == Some(e),
            }).collect(),
            razors: razors.into_iter().map(|(r, t)| RazorSave {
                position: t.translation.to_array(), rotation: t.rotation.to_array(), armed: r.armed, triggered: r.triggered,
            }).collect(),
        }
    }
}

/// Run after saved characters have spawned so adhesive grenades and their owners
/// point at the new entities. Restoring never replays a throw or restarts a fuse.
fn restore_gadgets(
    mut commands: Commands,
    mut pending: ResMut<GadgetRestore>,
    npcs: Query<(Entity, &crate::npc::FromSpawner)>,
    old: Query<Entity, Or<(With<Grenade>, With<Razor>)>>,
    mut held: ResMut<crate::props::Held>,
    assets: Option<Res<GameAssets>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let Some(saved) = pending.0.take() else { return };
    for e in &old {
        if held.0 == Some(e) {
            held.0 = None;
        }
        commands.entity(e).despawn();
    }
    let npcs: std::collections::HashMap<_, _> = npcs.iter().map(|(e, s)| (s.0, e)).collect();
    let mut model = |entity: &mut EntityCommands, name: &str, fallback: Mesh| {
        if let Some(parts) = assets.as_ref().and_then(|a| a.props.get(name)) {
            entity.with_children(|c| {
                for (mesh, mat) in &parts.parts {
                    let mut m = c.spawn((Mesh3d(mesh.clone()), bevy::mesh::MeshTag(0), Transform::IDENTITY));
                    mat.apply(&mut m);
                }
            });
        } else {
            entity.insert((Mesh3d(meshes.add(fallback)), MeshMaterial3d(materials.add(StandardMaterial {
                base_color: Color::srgb(0.12, 0.11, 0.1), perceptual_roughness: 0.6, metallic: 0.6, ..default()
            }))));
        }
    };
    for g in saved.grenades {
        let mut e = commands.spawn((
            Grenade {
                vel: Vec3::from(g.velocity), fuse: g.fuse, fuse_total: g.fuse_total, after_hit: g.after_hit,
                sticky: g.sticky, stuck: g.stuck.map(|(s, off)| (s.and_then(|s| npcs.get(&s).copied()).unwrap_or(Entity::PLACEHOLDER), Vec3::from(off))),
                blast: g.blast, owner: g.owner.and_then(|s| npcs.get(&s).copied()), throwback: g.throwback, kind: g.kind,
            },
            Transform::from_translation(Vec3::from(g.position)).with_rotation(Quat::from_array(g.rotation)),
            Visibility::default(), DespawnOnExit(GameState::InGame),
        ));
        if g.held {
            held.0 = Some(e.id());
            held.1 = 0.0;
        }
        model(&mut e, "grenade", Sphere::new(0.06).into());
    }
    for r in saved.razors {
        let mut e = commands.spawn((
            Razor { armed: r.armed, triggered: r.triggered },
            Transform::from_translation(Vec3::from(r.position)).with_rotation(Quat::from_array(r.rotation)),
            Visibility::default(), DespawnOnExit(GameState::InGame),
        ));
        model(&mut e, "springrazor", Cylinder::new(0.09, 0.06).into());
    }
}

fn use_gadget(
    mut commands: Commands,
    mut uses: MessageReader<UseGadget>,
    mut stats: ResMut<PlayerStats>,
    mut msgs: ResMut<HudMessages>,
    mut sfx: MessageWriter<PostEvent>,
    rapier: ReadRapierContext,
    player: Query<Entity, With<Player>>,
    assets: Option<Res<GameAssets>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    for u in uses.read() {
        let key = match u.gadget {
            Gadget::Grenade => GRENADES,
            Gadget::StickyGrenade => STICKY,
            Gadget::SpringRazor => RAZORS,
        };
        let n = stats.items.get(key).copied().unwrap_or(0);
        if n == 0 {
            msgs.push("Out of ammunition");
            sfx.write(PostEvent::named("Snd_Power_Empty", None));
            continue;
        }
        // the original model, else a stand-in
        let mut model = |ec: &mut EntityCommands, prop: &str, fallback: Mesh| match assets.as_ref().and_then(|a| a.props.get(prop)) {
            Some(parts) => {
                ec.with_children(|c| {
                    for (mesh, mat) in &parts.parts {
                        let mut m = c.spawn((Mesh3d(mesh.clone()), bevy::mesh::MeshTag(0), Transform::IDENTITY));
                        mat.apply(&mut m);
                    }
                });
            }
            None => {
                ec.insert((
                    Mesh3d(meshes.add(fallback)),
                    MeshMaterial3d(mats.add(StandardMaterial { base_color: Color::srgb(0.12, 0.11, 0.1), perceptual_roughness: 0.6, metallic: 0.6, ..default() })),
                ));
            }
        };
        match u.gadget {
            Gadget::Grenade | Gadget::StickyGrenade => {
                stats.items.insert(key.into(), n - 1);
                // (the pin was pulled as it began to cook)
                let fuse = (FUSE - u.cooked).max(0.0);
                if fuse > 0.0 {
                    sfx.write(PostEvent::named("Snd_Grenade_Throw", None));
                }
                let mut ec = commands.spawn((
                    // (`DisTweaks_ThrowGrenade`: 2500 uu/s, 5 degrees above the view;
                    // `DisTweaks_GrenadeComponent.m_fDetonationDelay` 3 s)
                    Grenade {
                        vel: if fuse > 0.0 { throw_dir(u.dir, THROW_ANGLE) * THROW_SPEED } else { Vec3::ZERO },
                        fuse,
                        fuse_total: FUSE,
                        after_hit: FUSE_AFTER_HIT,
                        sticky: u.gadget == Gadget::StickyGrenade,
                        stuck: None,
                        blast: BLAST,
                        owner: None,
                        throwback: false,
                        kind: if u.gadget == Gadget::StickyGrenade { HitKind::StickyGrenade } else { HitKind::Explosion },
                    },
                    Transform::from_translation(u.eye + u.dir * 0.5),
                    Visibility::default(),
                    DespawnOnExit(GameState::InGame),
                ));
                model(&mut ec, "grenade", Sphere::new(0.06).into());
            }
            Gadget::SpringRazor => {
                // placed on the surface aimed at, within reach
                let Ok(ctx) = rapier.single() else { continue };
                let mut filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
                if let Ok(pe) = player.single() {
                    filter = filter.exclude_collider(pe);
                }
                // on the surface aimed at, else the floor in front
                let ahead = u.eye + u.dir.with_y(0.0).normalize_or_zero() * 1.2;
                let Some((_, hit)) = ctx.cast_ray_and_get_normal(u.eye, u.dir, 3.0, true, filter).or_else(|| ctx.cast_ray_and_get_normal(ahead, Vec3::NEG_Y, 3.0, true, filter)) else {
                    msgs.push("Cannot place the springrazor here");
                    continue;
                };
                stats.items.insert(key.into(), n - 1);
                sfx.write(PostEvent::named("Snd_Spring_Razor_Install", Some(hit.point)));
                let mut ec = commands.spawn((
                    Razor { armed: 0.0, triggered: None },
                    Transform::from_translation(hit.point + hit.normal * 0.03).with_rotation(Quat::from_rotation_arc(Vec3::Y, hit.normal)),
                    Visibility::default(),
                    DespawnOnExit(GameState::InGame),
                ));
                model(&mut ec, "springrazor", Cylinder::new(0.09, 0.06).into());
            }
        }
    }
}

/// The characters' grenades: out of the hand, the grenade's own model.
fn npc_grenades(mut commands: Commands, mut throws: MessageReader<NpcGrenade>, assets: Option<Res<GameAssets>>, mut sfx: MessageWriter<PostEvent>, data: Res<crate::gamedata::Data>, stats: Res<PlayerStats>, settings: Res<crate::settings::Settings>) {
    for g in throws.read() {
        sfx.write(PostEvent::named("Snd_Grenade_Throw", Some(g.from)));
        let b = &g.blast;
        let fuse = g.fuse[0] + data.attribute("GrenadeExplosionDelay", settings.difficulty, &stats.powers, &stats.charms).max(0.0);
        let mut ec = commands.spawn((
            Grenade {
                vel: g.vel,
                fuse,
                fuse_total: fuse,
                after_hit: g.fuse[1],
                sticky: false,
                stuck: None,
                blast: [b.radius, b.full, b.player_radius, b.player_full, b.damage[settings.difficulty.min(3) as usize]],
                owner: Some(g.owner),
                throwback: false,
                kind: HitKind::EnemyExplosion,
            },
            Transform::from_translation(g.from),
            Visibility::default(),
            DespawnOnExit(GameState::InGame),
        ));
        if let Some(parts) = assets.as_ref().and_then(|a| a.props.get("grenade")) {
            ec.with_children(|c| {
                for (mesh, mat) in &parts.parts {
                    let mut m = c.spawn((Mesh3d(mesh.clone()), bevy::mesh::MeshTag(0), Transform::IDENTITY));
                    mat.apply(&mut m);
                }
            });
        }
    }
}

fn fly_grenades(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<TimeControl>,
    rapier: ReadRapierContext,
    mut q: Query<(Entity, &mut Grenade, &mut Transform)>,
    npcs: Query<(Entity, &Transform), (With<Npc>, Without<Grenade>)>,
    player: Query<Entity, With<Player>>,
    mut blasts: MessageWriter<Explosion>,
    mut sfx: MessageWriter<PostEvent>,
    mut held: ResMut<crate::props::Held>,
    cam: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
) {
    let dt = time.delta_secs() * tc.world_scale();
    let Ok(ctx) = rapier.single() else { return };
    let pe = player.single().ok();
    let all = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP | GROUP_NPC | crate::level::GROUP_PLAYER));
    for (e, mut g, mut t) in &mut q {
        let in_hand = held.0 == Some(e);
        if in_hand {
            if let Ok(camera) = cam.single() {
                let eye = camera.translation();
                let dir = camera.forward().as_vec3();
                let filter = pe.map_or(all, |p| all.exclude_collider(p));
                let reach = ctx.cast_ray(eye, dir, 0.8, true, filter).map_or(0.8, |(_, distance)| (distance - 0.1).max(0.1));
                t.translation = eye + dir * reach - Vec3::Y * 0.15;
            }
        }
        g.fuse -= dt;
        if g.fuse <= 0.0 {
            let b = g.blast;
            blasts.write(Explosion { at: t.translation, radius: b[0], full: b[1], damage: b[4], effect: "grenade", player: Some([b[2], b[3]]), kind: g.kind });
            if in_hand {
                held.0 = None;
                held.1 = 0.3;
            }
            commands.entity(e).despawn();
            continue;
        }
        if in_hand {
            continue;
        }
        // (Corvo's own pass through him; a character's through its thrower)
        let filter = match (g.owner, pe) {
            (Some(o), _) => all.exclude_collider(o),
            (None, Some(p)) => all.exclude_collider(p),
            (None, None) => all,
        };
        if let Some((who, off)) = g.stuck {
            if let Ok((_, nt)) = npcs.get(who) {
                t.translation = nt.translation + off;
            }
            continue;
        }
        g.vel.y -= 15.0 * dt;
        let step = g.vel * dt;
        let len = step.length();
        if len < 1e-5 {
            continue;
        }
        if let Some((hit, h)) = ctx.cast_ray_and_get_normal(t.translation, step / len, len + 0.06, true, filter) {
            t.translation = h.point + h.normal * 0.06;
            if g.sticky {
                // sticks to what it hits: a person carries it
                g.stuck = Some(npcs.get(hit).map(|(_, nt)| (hit, t.translation - nt.translation)).unwrap_or((hit, Vec3::ZERO)));
                if npcs.get(hit).is_err() {
                    g.vel = Vec3::ZERO;
                }
            } else {
                // someone hit: it goes off soon (`m_fMinDetonationDelayAfterHit`)
                if npcs.get(hit).is_ok() || Some(hit) == pe {
                    g.fuse = g.fuse.min(g.after_hit);
                }
                // bounce, losing most of its speed
                g.vel = (g.vel - 2.0 * g.vel.dot(h.normal) * h.normal) * 0.35;
                if g.vel.length() > 1.0 {
                    sfx.write(PostEvent::named("Snd_Phys_Metal_Grenade", Some(t.translation)));
                }
            }
        } else {
            t.translation += step;
        }
    }
}

fn razors(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<TimeControl>,
    rapier: ReadRapierContext,
    mut q: Query<(Entity, &mut Razor, &Transform)>,
    npcs: Query<(Entity, &Npc, &Transform), Without<Razor>>,
    mut hits: MessageWriter<NpcHit>,
    mut sfx: MessageWriter<PostEvent>,
    mut noise: MessageWriter<Noise>,
    mut fx: MessageWriter<SpawnEffect>,
) {
    let dt = time.delta_secs() * tc.world_scale();
    let Ok(ctx) = rapier.single() else { return };
    let walls = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
    for (e, mut r, t) in &mut q {
        let p = t.translation;
        if r.armed < 0.5 {
            r.armed += dt;
            if r.armed >= 0.5 {
                sfx.write(PostEvent::named("Snd_Spring_Razor_Arming", Some(p)));
            }
            continue;
        }
        match r.triggered {
            None => {
                // someone steps into the 2.5 m cylinder
                if npcs.iter().any(|(_, n, nt)| !n.is_down() && (nt.translation - p).with_y(0.0).length() < 2.5 && (nt.translation.y - p.y).abs() < 2.5) {
                    r.triggered = Some(0.0);
                }
            }
            Some(tt) if tt + dt >= 0.35 => {
                // the shrapnel: everyone in sight within 9 m
                sfx.write(PostEvent::named("Snd_Spring_Razor_Shot", Some(p)));
                fx.write(SpawnEffect { rot: t.rotation, ..SpawnEffect::at("springrazor", p) });
                noise.write(Noise { pos: p, radius: 20.0, combat: true });
                for (ne, n, nt) in &npcs {
                    let to = nt.translation - p;
                    let d = to.length();
                    if n.is_down() || d > 9.0 || ctx.cast_ray(p + Vec3::Y * 0.1, to / d.max(1e-3), d, true, walls).is_some() {
                        continue;
                    }
                    let damage = if d < 4.0 { 999.0 } else { 999.0 * (1.0 - (d - 4.0) / 5.0) };
                    hits.write(NpcHit { npc: ne, damage, kind: HitKind::SpringRazor, from: p });
                    sfx.write(PostEvent::named("Imp_SpringRazor_on_Body", Some(nt.translation)));
                    fx.write(SpawnEffect { follow: Some(ne), secs: 2.0, ..SpawnEffect::at("springrazor_blood", Vec3::Y * 0.3) });
                }
                commands.entity(e).despawn();
            }
            Some(tt) => r.triggered = Some(tt + dt),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn explode(
    mut commands: Commands,
    mut blasts: MessageReader<Explosion>,
    mut hits: MessageWriter<NpcHit>,
    mut stats: ResMut<PlayerStats>,
    mut sfx: MessageWriter<PostEvent>,
    mut noise: MessageWriter<Noise>,
    npcs: Query<(Entity, &Npc, &Transform)>,
    player: Query<&Transform, With<Player>>,
    mut flashes: Query<(Entity, &mut PointLight, &mut crate::fxlight::FxLight, &mut ExplosionFlash)>,
    time: Res<Time>,
    mut fx: MessageWriter<SpawnEffect>,
    mut rats: MessageWriter<crate::swarm::KillRats>,
    rapier: bevy_rapier3d::prelude::ReadRapierContext,
) {
    // a blast reaches what it can see (the original's `VisibleCollidingActors`)
    let ctx = rapier.single().ok();
    let walls = bevy_rapier3d::prelude::QueryFilter::default().groups(bevy_rapier3d::prelude::CollisionGroups::new(bevy_rapier3d::prelude::Group::ALL, crate::level::GROUP_WORLD));
    let seen = |from: Vec3, to: Vec3| {
        let d = to - from;
        let l = d.length();
        l < 0.3 || ctx.as_ref().is_none_or(|c| c.cast_ray(from, d / l, l - 0.2, true, walls).is_none())
    };
    for b in blasts.read() {
        sfx.write(PostEvent::named("Snd_Grenade_Explosion", Some(b.at)));
        rats.write(crate::swarm::KillRats { at: b.at, radius: b.full.max(2.0), by_player: !matches!(b.kind, HitKind::EnemyExplosion | HitKind::ByOthers), source: b.at });
        fx.write(SpawnEffect::at(b.effect, b.at));
        noise.write(Noise { pos: b.at, radius: 40.0, combat: true });
        let scale = |d: f32| if d <= b.full { 1.0 } else { (1.0 - (d - b.full) / (b.radius - b.full).max(0.1)).max(0.0) };
        for (e, n, t) in &npcs {
            let d = t.translation.distance(b.at);
            if !n.is_down() && d < b.radius && seen(b.at + Vec3::Y * 0.3, t.translation) {
                hits.write(NpcHit { npc: e, damage: b.damage * scale(d), kind: b.kind, from: b.at });
            }
        }
        if let Ok(pt) = player.single() {
            // `m_fPlayerFullDamageRadius` 150 uu
            let d = pt.translation.distance(b.at);
            let [reach, full] = b.player.unwrap_or([b.radius, 1.5]);
            if d < reach && !stats.dead && seen(b.at + Vec3::Y * 0.3, pt.translation) {
                let k = if d <= full { 1.0 } else { (1.0 - (d - full) / (reach - full).max(0.01)).max(0.0) };
                stats.take_damage(b.damage * k);
                stats.damage_flash = 1.0;
                stats.hit_from = Some(b.at);
                if stats.health <= 0.0 {
                    stats.dead = true;
                }
            }
        }
        // the flash
        commands.spawn((
            ExplosionFlash(0.0),
            PointLight { color: Color::srgb(1.0, 0.6, 0.25), intensity: 4.0e6, range: 12.0, ..default() },
            crate::fxlight::FxLight { color: Vec3::new(1.0, 0.6, 0.25), brightness: FLASH_BRIGHTNESS, radius: 12.0 },
            Transform::from_translation(b.at + Vec3::Y * 0.3),
            DespawnOnExit(GameState::InGame),
        ));
    }
    for (e, mut l, mut fx, mut f) in &mut flashes {
        f.0 += time.delta_secs();
        let k = (1.0 - f.0 / 0.4).max(0.0);
        l.intensity = 4.0e6 * k;
        fx.brightness = FLASH_BRIGHTNESS * k;
        if f.0 > 0.4 {
            commands.entity(e).despawn();
        }
    }
}

#[derive(Component)]
struct ExplosionFlash(f32);

/// An explosion's flash on the original-shader surfaces (UE3 brightness).
const FLASH_BRIGHTNESS: f32 = 6.0;

#[cfg(test)]
mod tests {
    #[test]
    fn ammunition_grants_clamp_all_types_and_preserve_existing_excess() {
        let attrs = crate::gamedata::Attrs::default();
        let mut stats = crate::gameplay::PlayerStats::default();
        for ty in 0..8 {
            *super::ammo_mut(&mut stats, ty, true).unwrap() = 0;
            let capacity = attrs.ammo_capacity[ty as usize];
            assert_eq!(super::give_ammo(&mut stats, &attrs, ty, capacity - 1).1, capacity - 1);
            assert_eq!(super::give_ammo(&mut stats, &attrs, ty, u32::MAX).1, 1);
            assert_eq!(super::ammo_count(&stats, ty), capacity);
            assert_eq!(super::give_ammo(&mut stats, &attrs, ty, 1).1, 0);
            *super::ammo_mut(&mut stats, ty, true).unwrap() = capacity + 4;
            assert_eq!(super::give_ammo(&mut stats, &attrs, ty, 1).1, 0);
            assert_eq!(super::ammo_count(&stats, ty), capacity + 4);
        }
        assert_eq!(super::give_ammo(&mut stats, &attrs, 255, 10).1, 0);
        for id in [super::GRENADES, super::STICKY, super::RAZORS, super::FLARES, super::EXPLOSIVE, "Bolt_Ammo_twk", "Bolts_Ammo_twk", "SleepDart_Ammo_twk", "Bullets_Store_Ammo_twk"] {
            assert!(super::ammo_type(id).is_some(), "{id}");
        }
        assert!(super::ammo_type("Twk_Upgrade_AmmoPouch2").is_none());
    }
    use super::*;

    fn grenade(owner: Option<Entity>) -> Grenade {
        Grenade { vel: Vec3::X, fuse: 0.7, fuse_total: 3.0, after_hit: 0.5, sticky: false, stuck: None, blast: BLAST, owner, throwback: false, kind: if owner.is_some() { HitKind::EnemyExplosion } else { HitKind::Explosion } }
    }

    #[test]
    fn taking_and_returning_a_grenade_keeps_its_remaining_fuse() {
        let mut world = World::new();
        let owner = world.spawn_empty().id();
        let mut g = grenade(Some(owner));
        g.take();
        assert_eq!(g.fuse, 0.7);
        assert_eq!(g.owner, None);
        assert_eq!(g.kind, HitKind::Explosion);
        g.release(Vec3::NEG_Z, Vec3::new(2.0, -20.0, 0.0), 25.0, true);
        assert_eq!(g.kind, HitKind::GrenadeThrowback);
        assert_eq!(g.fuse, 0.7);
        assert!(g.vel.z < -20.0 && g.vel.y > 0.0);
        assert_eq!(g.vel.x, 2.0);
    }

    #[test]
    fn own_grenades_and_dropped_enemy_grenades_are_not_throwbacks() {
        let mut own = grenade(None);
        own.take();
        own.release(Vec3::NEG_Z, Vec3::ZERO, 25.0, true);
        assert_eq!(own.kind, HitKind::Explosion);
        let mut world = World::new();
        let mut enemy = grenade(Some(world.spawn_empty().id()));
        enemy.take();
        enemy.release(Vec3::NEG_Z, Vec3::ZERO, 25.0, false);
        assert_eq!(enemy.kind, HitKind::Explosion);
        assert_eq!(enemy.fuse, 0.7);
    }

    #[test]
    fn enemy_grenades_use_difficulty_and_clockwork_malfunction() {
        let mut app = App::new();
        app.add_message::<NpcGrenade>().add_message::<PostEvent>()
            .init_resource::<crate::gamedata::Data>()
            .init_resource::<PlayerStats>()
            .insert_resource(crate::settings::Settings { difficulty: 3, ..default() })
            .add_systems(Update, npc_grenades);
        app.world_mut().resource_mut::<crate::gamedata::Data>().0.charms.push(dhcook::format::CharmDef { attribute: "GrenadeExplosionDelay".into(), levels: vec![("Clockwork Malfunction".into(), String::new(), 1.0)] });
        app.world_mut().resource_mut::<PlayerStats>().charms.push("Clockwork Malfunction".into());
        let owner = app.world_mut().spawn_empty().id();
        app.world_mut().write_message(NpcGrenade { owner, from: Vec3::ZERO, vel: Vec3::X, fuse: [3.0, 0.5], blast: dhcook::format::TrapBlast { damage: [10.0, 20.0, 30.0, 40.0], ..default() } });
        app.update();
        let mut q = app.world_mut().query::<&Grenade>();
        let g = q.single(app.world()).unwrap();
        assert_eq!(g.blast[4], 40.0);
        assert_eq!(g.fuse, 4.0);
        assert_eq!(g.fuse_total, 4.0);
        assert_eq!(g.kind, HitKind::EnemyExplosion);
    }

    #[test]
    fn saved_explosives_restore_fuses_adhesion_holding_and_npc_references() {
        let mut original = World::new();
        let owner = original.spawn_empty().id();
        let wall = original.spawn_empty().id();
        let attached = original.spawn_empty().id();
        let carried = original.spawn_empty().id();
        let fixed = original.spawn_empty().id();
        let mut enemy = grenade(Some(owner));
        enemy.sticky = true;
        enemy.stuck = Some((owner, Vec3::Y));
        let mut returned = grenade(Some(owner));
        returned.take();
        let mut sticky = grenade(None);
        sticky.kind = HitKind::StickyGrenade;
        sticky.sticky = true;
        sticky.stuck = Some((wall, Vec3::ZERO));
        let pose = Transform::from_translation(Vec3::new(2.0, 3.0, 4.0)).with_rotation(Quat::from_rotation_y(0.7));
        let razor = Razor { armed: 0.3, triggered: Some(0.2) };
        let saved = GadgetsSave::capture([(attached, &enemy, &pose), (carried, &returned, &pose), (fixed, &sticky, &pose)], [(&razor, &pose)], [(owner, 17)], Some(carried));
        let bytes = serde_json::to_vec(&saved).unwrap();
        let saved: GadgetsSave = serde_json::from_slice(&bytes).unwrap();

        let mut app = App::new();
        app.init_resource::<crate::props::Held>().init_resource::<Assets<Mesh>>().init_resource::<Assets<StandardMaterial>>()
            .insert_resource(GadgetRestore(Some(saved))).add_systems(Update, restore_gadgets);
        for _ in 0..10 { app.world_mut().spawn_empty(); }
        let new_owner = app.world_mut().spawn(crate::npc::FromSpawner(17)).id();
        assert_ne!(owner, new_owner);
        let stale = app.world_mut().spawn((grenade(None), Transform::IDENTITY)).id();
        app.world_mut().resource_mut::<crate::props::Held>().0 = Some(stale);
        app.update();
        assert!(app.world().get_entity(stale).is_err());
        let held = app.world().resource::<crate::props::Held>().0.unwrap();
        let mut q = app.world_mut().query::<(Entity, &Grenade, &Transform)>();
        assert_eq!(q.iter(app.world()).count(), 3);
        for (e, g, t) in q.iter(app.world()) {
            assert_eq!(g.fuse, 0.7);
            assert_eq!(g.fuse_total, 3.0);
            assert_eq!(t.translation, pose.translation);
            assert_eq!(t.rotation, pose.rotation);
            match g.kind {
                HitKind::EnemyExplosion => {
                    assert_eq!(g.owner, Some(new_owner));
                    assert_eq!(g.stuck, Some((new_owner, Vec3::Y)));
                }
                HitKind::StickyGrenade => assert_eq!(g.stuck, Some((Entity::PLACEHOLDER, Vec3::ZERO))),
                HitKind::Explosion => {
                    assert_eq!(e, held);
                    assert!(g.throwback);
                    assert!(g.owner.is_none());
                }
                _ => panic!("unexpected saved damage type"),
            }
        }
        let mut razors = app.world_mut().query::<(&Razor, &Transform)>();
        let (razor, t) = razors.single(app.world()).unwrap();
        assert_eq!(razor.armed, 0.3);
        assert_eq!(razor.triggered, Some(0.2));
        assert_eq!(t.translation, pose.translation);

        app.world_mut().resource_mut::<GadgetRestore>().0 = Some(GadgetsSave::default());
        app.update();
        assert_eq!(q.iter(app.world()).count(), 0);
        assert_eq!(razors.iter(app.world()).count(), 0);
        assert!(app.world().resource::<crate::props::Held>().0.is_none());
    }
}
