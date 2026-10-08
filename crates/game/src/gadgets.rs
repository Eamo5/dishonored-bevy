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
            .add_message::<Explosion>()
            .add_message::<NpcGrenade>()
            .add_systems(Update, (use_gadget, npc_grenades, fly_grenades, razors, explode).chain().run_if(in_state(GameState::InGame)));
    }
}

/// The gadgets' ammunition in the inventory (store items).
pub const GRENADES: &str = "Grenade_Ammo_WithItem_twk";
pub const STICKY: &str = "StickyGrenade_Ammo_WithItem_twk";
pub const RAZORS: &str = "SpringRazor_Ammo_WithItem_twk";
pub const FLARES: &str = "Flare_Ammo_twk";
pub const EXPLOSIVE: &str = "ExplosiveBullets_Ammo_twk";

/// Ammunition by the original ammo type (the index into `m_AmmoRanges`): adds it and names it.
pub fn give_ammo(stats: &mut PlayerStats, ty: u8, n: u32) -> &'static str {
    let item = match ty {
        0 => {
            stats.bullets += n;
            return "Bullets";
        }
        2 => {
            stats.bolts += n;
            return "Crossbow Bolts";
        }
        3 => {
            stats.sleep_darts += n;
            return "Sleep Darts";
        }
        1 => (EXPLOSIVE, "Explosive Bullets"),
        4 => (FLARES, "Incendiary Bolts"),
        5 => (RAZORS, "Springrazors"),
        6 => (GRENADES, "Grenades"),
        _ => (STICKY, "Sticky Grenades"),
    };
    *stats.items.entry(item.0.to_string()).or_default() += n;
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
}

/// Corvo's grenade's blast (`Twk_Proj_Grenade`'s `DisTweaks_Explosion`: 600 uu reach, 300 uu
/// full damage, 600 / 150 uu to him, 50 damage).
const BLAST: [f32; 5] = [6.0, 3.0, 6.0, 1.5, 50.0];

#[derive(Component)]
struct Razor {
    armed: f32,
    triggered: Option<f32>,
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
fn npc_grenades(mut commands: Commands, mut throws: MessageReader<NpcGrenade>, assets: Option<Res<GameAssets>>, mut sfx: MessageWriter<PostEvent>) {
    for g in throws.read() {
        sfx.write(PostEvent::named("Snd_Grenade_Throw", Some(g.from)));
        let b = &g.blast;
        let mut ec = commands.spawn((
            Grenade {
                vel: g.vel,
                fuse: g.fuse[0],
                fuse_total: g.fuse[0],
                after_hit: g.fuse[1],
                sticky: false,
                stuck: None,
                blast: [b.radius, b.full, b.player_radius, b.player_full, b.damage[1]],
                owner: Some(g.owner),
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
) {
    let dt = time.delta_secs() * tc.world_scale();
    let Ok(ctx) = rapier.single() else { return };
    let pe = player.single().ok();
    let all = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP | GROUP_NPC | crate::level::GROUP_PLAYER));
    for (e, mut g, mut t) in &mut q {
        g.fuse -= dt;
        if g.fuse <= 0.0 {
            let b = g.blast;
            blasts.write(Explosion { at: t.translation, radius: b[0], full: b[1], damage: b[4], effect: "grenade", player: Some([b[2], b[3]]) });
            commands.entity(e).despawn();
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
                    g.stuck = None;
                    g.sticky = false;
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
                    hits.write(NpcHit { npc: ne, damage, kind: HitKind::Bullet, from: p });
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
        rats.write(crate::swarm::KillRats { at: b.at, radius: b.full.max(2.0) });
        fx.write(SpawnEffect::at(b.effect, b.at));
        noise.write(Noise { pos: b.at, radius: 40.0, combat: true });
        let scale = |d: f32| if d <= b.full { 1.0 } else { (1.0 - (d - b.full) / (b.radius - b.full).max(0.1)).max(0.0) };
        for (e, n, t) in &npcs {
            let d = t.translation.distance(b.at);
            if !n.is_down() && d < b.radius && seen(b.at + Vec3::Y * 0.3, t.translation) {
                hits.write(NpcHit { npc: e, damage: b.damage * scale(d), kind: HitKind::Explosion, from: b.at });
            }
        }
        if let Ok(pt) = player.single() {
            // `m_fPlayerFullDamageRadius` 150 uu
            let d = pt.translation.distance(b.at);
            let [reach, full] = b.player.unwrap_or([b.radius, 1.5]);
            if d < reach && !stats.dead && seen(b.at + Vec3::Y * 0.3, pt.translation) {
                let k = if d <= full { 1.0 } else { (1.0 - (d - full) / (reach - full).max(0.01)).max(0.0) };
                stats.health = (stats.health - b.damage * k).max(0.0);
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
