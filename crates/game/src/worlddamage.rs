//! Blows landing on what the level scripts listen to (`SeqEvent_TakeDamage`): Corvo's bullets,
//! bolts, blade and blasts on PA speakers, crates, lamps, trap triggers, and on people, each
//! as its original damage type (the scripts take some and ignore others: a speaker shrugs off
//! the pistol's close blast, a candle only goes out to Wind Blast).

use crate::gamedata::Attrs;
use crate::gameplay::{HitKind, NpcHit};
use crate::kismet::Vm;
use crate::level::{InstanceCollider, LevelInfo, LevelInstance, GROUP_PROP, GROUP_WORLD};
use crate::npc::FromSpawner;
use crate::player::Player;
use crate::GameState;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

pub struct WorldDamagePlugin;

impl Plugin for WorldDamagePlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<WorldDamage>().add_systems(Update, (npc_damage, world_damage).run_if(in_state(GameState::InGame)));
    }
}

/// Where a blow lands on the world (not on a person).
#[derive(Clone, Copy, Debug)]
pub enum Reach {
    /// a projectile's hit on a collider, there
    Hit { collider: Entity, at: Vec3 },
    /// a blade's sweep: whatever the line meets first
    Ray { from: Vec3, dir: Vec3, len: f32 },
    /// all about a point (a blast, a thrown thing's knock): whole within `full`, fading to
    /// nothing at `radius`
    Near { at: Vec3, radius: f32, full: f32 },
    /// a cone (Wind Blast): its apex, axis, length, half angle
    Cone { at: Vec3, dir: Vec3, len: f32, half: f32 },
}

/// Damage to what is there: how much, of which original type (`DamageType` class), by Corvo
/// or not.
#[derive(Message, Clone, Copy, Debug)]
pub struct WorldDamage {
    pub reach: Reach,
    pub damage: f32,
    pub kind: &'static str,
    pub by_player: bool,
}

impl WorldDamage {
    pub fn player(reach: Reach, damage: f32, kind: &'static str) -> Self {
        WorldDamage { reach, damage, kind, by_player: true }
    }
}

/// The original damage types (`DishonoredGame`'s classes, the crossbow's from
/// `DishonoredGameContent`) and their parents, as far as the scripts tell them apart.
const PARENTS: &[(&str, &str)] = &[
    ("DishonoredDamageType", "DamageType"),
    ("DishonoredDamageType_FastHit", "DishonoredDamageType"),
    ("DisDamageType_FastHit_Left", "DishonoredDamageType_FastHit"),
    ("DisDamageType_FastHit_Right", "DishonoredDamageType_FastHit"),
    ("DisDamageType_FastHit_Up", "DishonoredDamageType_FastHit"),
    ("DisDamageType_FastHit_Up_Left", "DisDamageType_FastHit_Left"),
    ("DisDamageType_FastHit_Up_Right", "DisDamageType_FastHit_Right"),
    ("DisDamageType_ImpactHit", "DishonoredDamageType_FastHit"),
    ("DisDamageType_ImpactHit_Adrenaline", "DishonoredDamageType_FastHit"),
    ("DishonoredDamageType_Projectile", "DishonoredDamageType"),
    ("DishonoredDamageType_Bullet", "DishonoredDamageType_Projectile"),
    ("DishonoredDamageType_BulletBlast", "DishonoredDamageType_Bullet"),
    ("DishonoredDamageType_BulletMedium", "DishonoredDamageType_Bullet"),
    ("DishonoredDamageType_BulletFar", "DishonoredDamageType_Bullet"),
    ("DisDamageType_ExplosiveBullet", "DishonoredDamageType_Bullet"),
    ("DisDamageType_Arrow", "DishonoredDamageType_Projectile"),
    ("DisDamageType_Arrow_Sleep", "DishonoredDamageType_Projectile"),
    ("DisDamageType_Arrow_Stealth", "DishonoredDamageType_Projectile"),
    ("DisDamageType_Arrow_Flare", "DishonoredDamageType_Projectile"),
    ("DisDamageType_StickyGrenade", "DishonoredDamageType_Projectile"),
    ("DishonoredDamageType_Explosion", "DishonoredDamageType"),
    ("DisDamageType_Grenade", "DishonoredDamageType_Explosion"),
    ("DisDamageType_StickyGrenadeExplosion", "DishonoredDamageType_Explosion"),
    ("DisDamageType_TallboyExplosion", "DishonoredDamageType_Explosion"),
    ("DisDamageType_WhiskeyExplosion", "DishonoredDamageType_Explosion"),
    ("DisDamageType_WindBlast", "DishonoredDamageType"),
    ("DisDamageType_Impact", "DishonoredDamageType"),
    ("DisDamageType_RagdollImpact", "DisDamageType_Impact"),
    ("DisDamageType_UnpossessSquash", "DisDamageType_Impact"),
    ("DishonoredDamageType_Bash", "DishonoredDamageType"),
    ("DisDamageType_Slide", "DishonoredDamageType_Bash"),
    ("DisDamageType_Choke", "DishonoredDamageType"),
    ("DisDamageType_Assassination", "DishonoredDamageType"),
    ("DishonoredDamageType_Stomped", "DishonoredDamageType"),
    ("DishonoredDamageType_Bite", "DishonoredDamageType"),
    ("DisDamageType_FishBite", "DishonoredDamageType"),
    ("DisDamageType_Immolation", "DishonoredDamageType"),
    ("DisDamageType_WhiskeyFire", "DishonoredDamageType"),
    ("DisDamageType_GenericSleep", "DishonoredDamageType"),
    ("DisDamageType_InstantSleep", "DisDamageType_GenericSleep"),
    ("DisDamageType_Sleep_Deferred", "DishonoredDamageType"),
    ("DisDamageType_SpringRazor", "DishonoredDamageType"),
    ("DisDamageType_SpringRazorPlaced", "DishonoredDamageType"),
    ("DisDamageType_Energy", "DishonoredDamageType"),
    ("DisDamageType_WallOfLight", "DisDamageType_Energy"),
    ("DishonoredDamageType_Electricity", "DishonoredDamageType"),
    ("DisDamageType_RiverKrustSpit", "DishonoredDamageType"),
];

/// Whether a damage type is (a kind of) another (UE3 `ClassIsChildOf`).
pub fn is_a(class: &str, of: &str) -> bool {
    let mut c = class;
    for _ in 0..8 {
        if c.eq_ignore_ascii_case(of) {
            return true;
        }
        match PARENTS.iter().find(|(k, _)| k.eq_ignore_ascii_case(c)) {
            Some((_, p)) => c = p,
            None => return false,
        }
    }
    false
}

/// A pistol ball's type by range (`DishonoredDamageType_BulletBlast` is the close shot's
/// spray, not the ball).
pub fn bullet_type(dist: f32, attrs: &Attrs) -> &'static str {
    if dist >= attrs.bullet.long[0] {
        "DishonoredDamageType_BulletFar"
    } else {
        "DishonoredDamageType_BulletMedium"
    }
}

/// A blow on a person: its original type.
pub fn hit_type(kind: HitKind, dist: f32, attrs: &Attrs) -> &'static str {
    match kind {
        // (a blow of the blade: the original tells its swings apart, left, right, up)
        HitKind::Sword => "DisDamageType_FastHit_Right",
        HitKind::Assassinate => "DisDamageType_Assassination",
        HitKind::Choke => "DisDamageType_Choke",
        HitKind::SleepDart => "DisDamageType_Arrow_Sleep",
        HitKind::Bolt => "DisDamageType_Arrow",
        HitKind::Bullet => bullet_type(dist, attrs),
        HitKind::Windblast => "DisDamageType_WindBlast",
        HitKind::Fatality => "DisDamageType_ImpactHit_Adrenaline",
        HitKind::Rats => "DishonoredDamageType_Bite",
        HitKind::Fire => "DisDamageType_Arrow_Flare",
        HitKind::Explosion | HitKind::EnemyExplosion | HitKind::GrenadeThrowback => "DisDamageType_Grenade",
        HitKind::StickyGrenade => "DisDamageType_StickyGrenadeExplosion",
        HitKind::ExplosiveBullet => "DisDamageType_ExplosiveBullet",
        HitKind::Impact => "DisDamageType_Impact",
        HitKind::SpringRazor => "DisDamageType_SpringRazor",
        HitKind::WallOfLight => "DisDamageType_WallOfLight",
        _ => "DishonoredDamageType",
    }
}

/// People's damage events: each blow as its type.
fn npc_damage(mut hits: MessageReader<NpcHit>, vm: Option<ResMut<Vm>>, npcs: Query<(&FromSpawner, &Transform)>, attrs: Res<Attrs>) {
    let Some(mut vm) = vm else {
        hits.clear();
        return;
    };
    for h in hits.read() {
        let Ok((from, t)) = npcs.get(h.npc) else { continue };
        let Some(actor) = vm.actor_of_spawner(from.0) else { continue };
        let now = vm.time;
        vm.recent_hits.insert(h.npc, now);
        let kind = hit_type(h.kind, h.from.distance(t.translation), &attrs);
        vm.take_damage(actor, kind, h.damage, !matches!(h.kind, HitKind::ByOthers | HitKind::EnemyExplosion));
    }
}

/// The world's: what each blow reaches among the things the scripts listen to - by the
/// collider it struck, else by their bounds (meshes where they stand now, trigger volumes).
#[allow(clippy::too_many_arguments)]
fn world_damage(
    mut hits: MessageReader<WorldDamage>,
    mut blasts: MessageReader<crate::gadgets::Explosion>,
    vm: Option<ResMut<Vm>>,
    level: Option<Res<LevelInfo>>,
    instances: Query<(&LevelInstance, &GlobalTransform, Option<&InstanceCollider>)>,
    player: Query<Entity, With<Player>>,
    rapier: ReadRapierContext,
) {
    let (Some(mut vm), Some(level)) = (vm, level) else {
        hits.clear();
        blasts.clear();
        return;
    };
    let mut all: Vec<WorldDamage> = hits.read().copied().collect();
    for b in blasts.read() {
        let kind = match b.effect {
            "explosive_bullet" => "DisDamageType_ExplosiveBullet",
            "grenade" => hit_type(b.kind, 0.0, &Attrs::default()),
            _ => "DishonoredDamageType_Explosion",
        };
        all.push(WorldDamage { reach: Reach::Near { at: b.at, radius: b.radius, full: b.full }, damage: b.damage.max(1.0), kind, by_player: !matches!(b.kind, HitKind::ByOthers | HitKind::EnemyExplosion) });
    }
    if all.is_empty() {
        return;
    }
    let listeners: Vec<u32> = vm.damage_listeners().into_iter().filter(|&a| vm.g.actors.get(a as usize).is_some_and(|ka| ka.spawner.is_none())).collect();
    if listeners.is_empty() {
        return;
    }
    let scene = &level.scene;
    // each listener's bounds (world boxes)
    let want: std::collections::HashSet<u32> = listeners.iter().flat_map(|&a| vm.g.actors[a as usize].instances.iter().copied()).collect();
    let mut placed: std::collections::HashMap<u32, (GlobalTransform, Option<Entity>)> = std::collections::HashMap::new();
    for (li, gt, col) in &instances {
        if want.contains(&li.index) {
            placed.insert(li.index, (*gt, col.map(|c| c.0)));
        }
    }
    let boxes: Vec<(u32, Vec<(Vec3, Vec3)>, Vec<Entity>)> = listeners
        .iter()
        .map(|&a| {
            let ka = &vm.g.actors[a as usize];
            let mut bx = Vec::new();
            let mut cols = Vec::new();
            for i in &ka.instances {
                let (Some((gt, col)), Some(inst)) = (placed.get(i), scene.instances.get(*i as usize)) else { continue };
                let Some(mesh) = scene.meshes.get(inst.mesh as usize) else { continue };
                bx.push(world_box(gt, Vec3::from(mesh.min), Vec3::from(mesh.max)));
                cols.extend(*col);
            }
            if let Some(v) = ka.volume.and_then(|v| scene.volumes.get(v as usize)) {
                for h in &v.hulls {
                    let (lo, hi) = h.iter().fold((Vec3::MAX, Vec3::MIN), |(lo, hi), p| (lo.min(Vec3::from(*p)), hi.max(Vec3::from(*p))));
                    if lo.x <= hi.x {
                        bx.push((lo, hi));
                    }
                }
            }
            if bx.is_empty() && ka.instances.is_empty() && ka.volume.is_none() {
                let p = Vec3::from(ka.position);
                bx.push((p - Vec3::splat(0.4), p + Vec3::splat(0.4)));
            }
            (a, bx, cols)
        })
        .collect();
    let ctx = rapier.single().ok();
    let pe = player.single().ok();
    for d in all {
        let mut struck: Vec<(u32, f32)> = Vec::new();
        let point = |at: Vec3, collider: Option<Entity>, struck: &mut Vec<(u32, f32)>| {
            for (a, bx, cols) in &boxes {
                // the collider it struck is theirs, or it landed in their trigger / on them
                let on = collider.is_some_and(|c| cols.contains(&c)) || bx.iter().any(|b| box_dist(b, at) < 0.05) && (cols.is_empty() || collider.is_none());
                if on {
                    struck.push((*a, d.damage));
                }
            }
        };
        match d.reach {
            Reach::Hit { collider, at } => point(at, Some(collider), &mut struck),
            Reach::Ray { from, dir, len } => {
                let Some(ctx) = &ctx else { continue };
                let mut filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
                if let Some(pe) = pe {
                    filter = filter.exclude_collider(pe);
                }
                if let Some((c, toi)) = ctx.cast_ray(from, dir, len, true, filter) {
                    point(from + dir * toi, Some(c), &mut struck);
                } else {
                    // (a trigger in the air)
                    point(from + dir * len * 0.7, None, &mut struck);
                }
            }
            Reach::Near { at, radius, full } => {
                for (a, bx, _) in &boxes {
                    if let Some(dist) = bx.iter().map(|b| box_dist(b, at)).min_by(f32::total_cmp) {
                        if dist <= radius {
                            let fade = if dist <= full { 1.0 } else { 1.0 - (dist - full) / (radius - full).max(0.01) };
                            struck.push((*a, d.damage * fade.clamp(0.0, 1.0)));
                        }
                    }
                }
            }
            Reach::Cone { at, dir, len, half } => {
                for (a, bx, _) in &boxes {
                    let hit = bx.iter().any(|b| {
                        let c = (b.0 + b.1) * 0.5;
                        let ext = ((b.1 - b.0) * 0.5).length();
                        let to = c - at;
                        let dist = to.length();
                        dist - ext <= len && (dist <= ext || to.normalize_or_zero().dot(dir).clamp(-1.0, 1.0).acos() <= half + (ext / dist).clamp(0.0, 1.0).asin())
                    });
                    if hit {
                        struck.push((*a, d.damage));
                    }
                }
            }
        }
        for (a, dmg) in struck {
            vm.take_damage(a, d.kind, dmg, d.by_player);
        }
    }
}

/// A mesh's local bounds placed in the world (the box around the turned box).
fn world_box(gt: &GlobalTransform, lo: Vec3, hi: Vec3) -> (Vec3, Vec3) {
    let m = gt.affine();
    let (mut a, mut b) = (Vec3::MAX, Vec3::MIN);
    for i in 0..8 {
        let p = Vec3::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z });
        let w = m.transform_point3(p);
        a = a.min(w);
        b = b.max(w);
    }
    (a, b)
}

/// How far a point is outside a box (0 within).
fn box_dist(b: &(Vec3, Vec3), p: Vec3) -> f32 {
    (p - p.clamp(b.0, b.1)).length()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lineage() {
        assert!(is_a("DishonoredDamageType_BulletMedium", "DishonoredDamageType_Bullet"));
        assert!(is_a("DishonoredDamageType_BulletMedium", "DishonoredDamageType"));
        assert!(is_a("DisDamageType_Grenade", "DishonoredDamageType_Explosion"));
        assert!(is_a("DisDamageType_Arrow", "DamageType"));
        assert!(!is_a("DishonoredDamageType_BulletMedium", "DishonoredDamageType_BulletBlast"));
        assert!(!is_a("DishonoredDamageType_FastHit", "DisDamageType_Impact"));
        // (a sword kill counts as "Kills with sword" and as the scripts' FastHit)
        assert!(is_a(hit_type(HitKind::Sword, 0.0, &Attrs::default()), "DisDamageType_FastHit_Right"));
        assert!(is_a(hit_type(HitKind::Sword, 0.0, &Attrs::default()), "DishonoredDamageType_FastHit"));
    }
}
