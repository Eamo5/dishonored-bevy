//! The plague a weeper carries (its pawn's `m_bHasPlague`: `NpcPlague`, cooked from its tweaks).
//! Flies hang about it, buzzing, until a while after it dies. Its cough (the clip's
//! `DishonoredNotify_StartPlague`; its retching on Corvo as it holds him too) starts a cloud. For
//! `m_fPlagueDmgLength` s the cloud deals `m_iPlagueDmgPerTick` every `m_fPlagueDmgePeriod` s to
//! everyone within `m_fPlagueRadius` before it (`m_fPlagueDmgAngle`) with a clear line, sparing
//! plague carriers, as roshan's r614/blink research decoded it. Corvo struck sees its lens
//! effect; his charms lessen it and turn it to mana (`PlagueDamageReduction`,
//! `ManaGainOnPlagueDamage`).
//!
//! The characters' clips also start their effects here (`AnimNotify_PlayParticleEffect`: the
//! weeper's vomit at its `Puke_Spit` socket as it coughs).

use crate::anim::{Animator, ClipId};
use crate::audio::PostEvent;
use crate::gameplay::{HitKind, HudMessages, NpcHit, PlayerStats};
use crate::level::{GameAssets, GROUP_WORLD};
use crate::npc::{Npc, NpcAnim, NpcRig};
use crate::particles::SpawnEffect;
use crate::player::Player;
use crate::GameState;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

pub struct PlaguePlugin;

impl Plugin for PlaguePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (clip_notifies, flies, clouds).chain().run_if(in_state(GameState::InGame)));
    }
}

/// A cough's cloud, on its carrier: how long it lasts yet, until it next harms, and whether it
/// has shown Corvo its lens effect.
#[derive(Component)]
struct Cloud {
    left: f32,
    next: f32,
    struck: bool,
}

/// The flies about a carrier: their anchor at its socket and the buzz's (while Corvo is within
/// hearing), and once it's down, how long they stay.
#[derive(Component)]
struct Flies {
    anchor: Option<Entity>,
    buzz: Option<Entity>,
    fading: Option<f32>,
}

/// The clip a character was playing when last looked at, and how far in.
#[derive(Component, Default)]
struct ClipSeen(Option<(ClipId, f32)>);

/// A socket of a character's skeleton (or a bone of that name): its joint and its place there.
fn socket(assets: &GameAssets, npc: &Npc, rig: &NpcRig, name: &str) -> Option<(Entity, Transform)> {
    let vis = npc.npc_type.and_then(|t| assets.npc_types.get(t as usize)).and_then(|v| v.as_ref());
    if let Some(s) = vis.and_then(|v| v.skeleton.sockets.iter().find(|s| s.name.eq_ignore_ascii_case(name))) {
        let j = rig.joint(&s.bone)?;
        return Some((j, Transform::from_translation(Vec3::from(s.translation)).with_rotation(Quat::from_array(s.rotation).normalize())));
    }
    rig.joint(name).map(|j| (j, Transform::IDENTITY))
}

/// The notifies a character's clip passed since last looked at: its effects, and a plague
/// carrier's cloud.
#[allow(clippy::type_complexity)]
fn clip_notifies(
    mut commands: Commands,
    assets: Option<Res<GameAssets>>,
    mut npcs: Query<(Entity, &Npc, &Animator, &NpcAnim, &NpcRig, Option<&mut ClipSeen>, Option<&mut Cloud>)>,
    globals: Query<&GlobalTransform>,
    mut fx: MessageWriter<SpawnEffect>,
) {
    let Some(assets) = assets else { return };
    for (e, npc, anim, clips, rig, seen, cloud) in &mut npcs {
        let Some(mut seen) = seen else {
            commands.entity(e).try_insert(ClipSeen::default());
            continue;
        };
        let Some(p) = anim.current() else { continue };
        let last = seen.0.replace((p.clip, p.t));
        // (forward, and round a loop's end)
        let passed = |at: f32| match last {
            Some((c, t0)) if c == p.clip && t0 <= p.t => at > t0 && at <= p.t,
            Some((c, t0)) if c == p.clip && p.looping => at > t0 || at <= p.t,
            _ => at <= p.t,
        };
        for f in clips.clip_fx(p.clip).iter().filter(|f| passed(f.time)) {
            let Some((joint, local)) = socket(&assets, npc, rig, &f.socket) else { continue };
            if f.attach {
                let at = commands.spawn((local, ChildOf(joint), Expire(FX_SECS))).id();
                fx.write(SpawnEffect { follow: Some(at), turn: true, system: Some(f.system), secs: FX_SECS, ..SpawnEffect::at("", Vec3::ZERO) });
            } else if let Ok(g) = globals.get(joint) {
                let w = g.mul_transform(local);
                fx.write(SpawnEffect { rot: w.rotation(), system: Some(f.system), secs: FX_SECS, ..SpawnEffect::at("", w.translation()) });
            }
        }
        let Some(pl) = npc.arms.plague.as_ref() else { continue };
        if npc.is_down() || !clips.plague_marks(p.clip).iter().any(|t| passed(*t)) {
            continue;
        }
        let left = pl.length[0] + rand::random::<f32>() * (pl.length[1] - pl.length[0]).max(0.0);
        match cloud {
            // (still going: it lasts on, harming at its own pace)
            Some(mut c) => c.left = c.left.max(left),
            None => {
                commands.entity(e).try_insert(Cloud { left, next: 0.0, struck: false });
            }
        }
        if std::env::var("DH_PLAGUE_LOG").is_ok() {
            info!("plague: {} coughs a cloud for {left:.1} s", npc.name);
        }
    }
}

/// How long a clip's effect plays (s).
const FX_SECS: f32 = 3.0;

/// Despawned once its time is up (a clip effect's anchor).
#[derive(Component)]
struct Expire(f32);

/// The flies come with a carrier and go a while after it dies; their buzz plays while Corvo is
/// within its hearing.
#[allow(clippy::type_complexity)]
fn flies(
    mut commands: Commands,
    time: Res<Time>,
    assets: Option<Res<GameAssets>>,
    mut carriers: Query<(Entity, &Npc, &NpcRig, &GlobalTransform, Option<&mut Flies>)>,
    mut expiring: Query<(Entity, &mut Expire)>,
    player: Query<&GlobalTransform, With<Player>>,
    mut fx: MessageWriter<SpawnEffect>,
    mut sfx: MessageWriter<PostEvent>,
) {
    let dt = time.delta_secs();
    for (e, mut x) in &mut expiring {
        x.0 -= dt;
        if x.0 <= 0.0 {
            commands.entity(e).try_despawn();
        }
    }
    let Some(assets) = assets else { return };
    let ppos = player.single().map(|g| g.translation()).ok();
    for (e, npc, rig, g, flies) in &mut carriers {
        let Some(pl) = npc.arms.plague.as_ref() else { continue };
        let Some(mut f) = flies else {
            // (a body already down when it comes has none)
            let anchor = socket(&assets, npc, rig, &pl.socket).filter(|_| !npc.is_down()).map(|(joint, local)| commands.spawn((local, ChildOf(joint))).id());
            if let (Some(a), Some(sys)) = (anchor, pl.flies) {
                fx.write(SpawnEffect { follow: Some(a), system: Some(sys), secs: 1.0e9, ..SpawnEffect::at("", Vec3::ZERO) });
            }
            commands.entity(e).try_insert(Flies { anchor, buzz: None, fading: None });
            continue;
        };
        let Some(anchor) = f.anchor else { continue };
        if npc.is_down() && f.fading.is_none() {
            f.fading = Some(pl.fade);
            if f.buzz.is_some() && !pl.buzz[1].is_empty() {
                sfx.write(PostEvent::named(&pl.buzz[1], Some(g.translation())));
            }
        }
        if let Some(left) = f.fading.as_mut() {
            *left -= dt;
            if *left <= 0.0 {
                // (their effect and buzz end with their anchor)
                commands.entity(anchor).try_despawn();
                f.anchor = None;
                f.buzz = None;
                continue;
            }
        }
        // the buzz: heard within its reach (a little more, not to flicker at its edge)
        let near = ppos.map(|p| p.distance(g.translation()));
        match f.buzz {
            None if f.fading.is_none() && near.is_some_and(|d| d < pl.buzz_reach) && !pl.buzz[0].is_empty() => {
                let b = commands.spawn((Transform::IDENTITY, ChildOf(anchor))).id();
                sfx.write(PostEvent::named(&pl.buzz[0], Some(g.translation())).following(b));
                f.buzz = Some(b);
            }
            Some(b) if near.is_none_or(|d| d > pl.buzz_reach + 2.0) => {
                commands.entity(b).try_despawn();
                f.buzz = None;
            }
            _ => {}
        }
    }
}

/// Whether `at` is in a cloud from `mouth`: nearer than `radius`, and within `angle` degrees
/// of `fwd` across the ground (90: anywhere before it).
fn within(mouth: Vec3, fwd: Vec3, radius: f32, angle: f32, at: Vec3) -> bool {
    let to = at - mouth;
    to.length() < radius && to.with_y(0.0).normalize_or_zero().dot(fwd.with_y(0.0).normalize_or_zero()) >= angle.to_radians().cos() - 1e-3
}

/// The clouds harm whoever is before their carrier.
#[allow(clippy::too_many_arguments)]
fn clouds(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<crate::gameplay::TimeControl>,
    rapier: ReadRapierContext,
    mut carriers: Query<(Entity, &Npc, &Transform, &mut Cloud)>,
    others: Query<(Entity, &Npc, &Transform), Without<Cloud>>,
    player: Query<&Transform, With<Player>>,
    (mut stats, mut msgs, attrs): (ResMut<PlayerStats>, ResMut<HudMessages>, Res<crate::gamedata::Attrs>),
    (mut hits, mut sfx): (MessageWriter<NpcHit>, MessageWriter<PostEvent>),
    (mut vm, mut lens_n): (Option<ResMut<crate::kismet::Vm>>, Local<u32>),
) {
    let dt = time.delta_secs() * tc.world_scale();
    let ctx = rapier.single().ok();
    let walls = QueryFilter::default().exclude_sensors().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
    let ppos = player.single().map(|t| t.translation).ok();
    let log = std::env::var("DH_PLAGUE_LOG").is_ok();
    for (e, npc, t, mut c) in &mut carriers {
        let Some(pl) = npc.arms.plague.as_ref() else {
            commands.entity(e).try_remove::<Cloud>();
            continue;
        };
        c.left -= dt;
        if c.left <= 0.0 || npc.is_down() {
            commands.entity(e).try_remove::<Cloud>();
            continue;
        }
        c.next -= dt;
        if c.next > 0.0 {
            continue;
        }
        c.next += pl.period.max(0.05);
        // (from its mouth: before it, within its reach, in a clear line)
        let mouth = t.translation + Vec3::Y * 0.6;
        let fwd = npc.forward();
        let reaches = |at: Vec3| {
            let to = at - mouth;
            let d = to.length();
            within(mouth, fwd, pl.radius, pl.angle, at) && ctx.as_ref().is_none_or(|ctx| ctx.cast_ray(mouth, to / d.max(1e-3), (d - 0.3).max(0.0), true, walls).is_none())
        };
        if let Some(p) = ppos.filter(|p| !stats.dead && reaches(*p)) {
            let damage = (pl.per_tick - attrs.plague_damage_reduction).max(0.0);
            if damage > 0.0 {
                stats.mana = (stats.mana + attrs.plague_mana).min(stats.max_mana);
                stats.mana_cap = stats.mana_cap.max(stats.mana);
                crate::gameplay::hurt_player(&mut stats, &mut msgs, &mut sfx, damage);
                stats.hit_from = Some(t.translation);
            }
            if !pl.hurt.is_empty() {
                sfx.write(PostEvent::named(&pl.hurt, Some(p)));
            }
            if let (false, Some(lens), Some(vm)) = (c.struck, pl.lens, vm.as_mut()) {
                *lens_n = (*lens_n + 1) % 0x100;
                vm.lens.push((crate::hudfx::GAMEPLAY_LENS + 0x400 + *lens_n, Some((lens, false, 0.0))));
            }
            c.struck = true;
            if log {
                info!("plague: {}'s cloud strikes Corvo for {damage}", npc.name);
            }
        }
        for (oe, o, ot) in &others {
            if oe == e || o.is_down() || o.arms.plague.is_some() || !reaches(ot.translation) {
                continue;
            }
            hits.write(NpcHit { npc: oe, damage: pl.per_tick, kind: HitKind::ByOthers, from: t.translation });
            if log {
                info!("plague: {}'s cloud strikes {}", npc.name, o.name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cloud_reaches_before_its_carrier_only() {
        let (mouth, fwd) = (Vec3::new(0.0, 1.5, 0.0), Vec3::NEG_Z);
        // (3.5 m, the front half: `m_fPlagueRadius` 350, `m_fPlagueDmgAngle` 90)
        assert!(within(mouth, fwd, 3.5, 90.0, Vec3::new(0.0, 0.9, -2.0)));
        assert!(within(mouth, fwd, 3.5, 90.0, Vec3::new(2.5, 1.0, -0.2)));
        assert!(!within(mouth, fwd, 3.5, 90.0, Vec3::new(0.0, 0.9, 1.0)));
        assert!(!within(mouth, fwd, 3.5, 90.0, Vec3::new(0.0, 0.9, -4.0)));
        assert!(!within(mouth, fwd, 3.5, 45.0, Vec3::new(2.5, 1.0, -0.5)));
    }
}
