//! What a character throws at Corvo with its bare hand (`DisTweaks_WepThrowingHand`'s
//! `DisTweaks_NPCThrow`, its projectile `Twk_Proj_ThrownObject`): one of the throwables
//! (`Twk_Thrown_CommonExterior`: a piece of metal, a brick, a broken bottle; `thrown_0..2` in
//! the scene's props), out of its hand at `m_fThrowSpeed`, tumbling (`m_fSpinSpeed`). Striking
//! Corvo it harms him (the hand's ranged damage); striking the world it sounds as the contact
//! system's `ThrownRock` on what it strikes and drops, coming to rest.

use bevy::mesh::MeshTag;
use bevy::prelude::*;
use bevy_rapier3d::prelude::{CollisionGroups, Group, QueryFilter, ReadRapierContext};

use crate::audio::PostEvent;
use crate::gameplay::{PlayerHit, TimeControl};
use crate::level::{GameAssets, LevelInfo, GROUP_NPC, GROUP_PLAYER, GROUP_PROP, GROUP_WORLD};
use crate::player::Player;
use crate::GameState;

pub struct ThrownPlugin;

impl Plugin for ThrownPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<NpcThrow>().add_systems(Update, (npc_throws, fly_thrown).chain().run_if(in_state(GameState::InGame)));
    }
}

/// A thing a character throws: from where, how fast, its harm to Corvo.
#[derive(Message, Clone, Copy)]
pub struct NpcThrow {
    pub owner: Entity,
    pub from: Vec3,
    pub vel: Vec3,
    pub damage: f32,
}

/// A thrown thing in flight, then fallen.
#[derive(Component)]
pub struct Thrown {
    vel: Vec3,
    axis: Vec3,
    owner: Entity,
    damage: f32,
    /// it has struck something (no harm after)
    spent: bool,
    /// seconds left before it is cleared away
    life: f32,
}

/// How fast it tumbles (rad/s: `DisTweaks_NPCThrow.m_fSpinSpeed`), and what pulls it down (m/s²,
/// as a grenade's 1500 uu/s²).
const SPIN: f32 = 10.0;
const GRAVITY: f32 = 15.0;
/// What is left of its speed after it strikes the world (our reading: a dull thing's bounce).
const BOUNCE: f32 = 0.3;

fn npc_throws(mut commands: Commands, mut throws: MessageReader<NpcThrow>, assets: Option<Res<GameAssets>>) {
    for t in throws.read() {
        let axis = t.vel.cross(Vec3::Y).normalize_or(Vec3::X);
        let mut ec = commands.spawn((
            Thrown { vel: t.vel, axis, owner: t.owner, damage: t.damage, spent: false, life: 8.0 },
            Transform::from_translation(t.from),
            Visibility::default(),
            DespawnOnExit(GameState::InGame),
        ));
        // (one of the throwables at random)
        let looks: Vec<_> = (0..3).filter_map(|i| assets.as_ref().and_then(|a| a.props.get(&format!("thrown_{i}")))).collect();
        if !looks.is_empty() {
            let parts = looks[rand::random::<u32>() as usize % looks.len()];
            ec.with_children(|c| {
                for (mesh, mat) in &parts.parts {
                    let mut m = c.spawn((Mesh3d(mesh.clone()), MeshTag(0), Transform::IDENTITY));
                    mat.apply(&mut m);
                }
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn fly_thrown(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<TimeControl>,
    rapier: ReadRapierContext,
    mut thrown: Query<(Entity, &mut Thrown, &mut Transform), Without<Player>>,
    player: Query<Entity, With<Player>>,
    surfaces: Query<&crate::footsteps::ColliderSurfaces>,
    level: Option<Res<LevelInfo>>,
    (mut hits, mut sfx): (MessageWriter<PlayerHit>, MessageWriter<PostEvent>),
) {
    let dt = time.delta_secs() * tc.world_scale();
    if dt <= 0.0 {
        return;
    }
    let Ok(ctx) = rapier.single() else { return };
    let pe = player.single().ok();
    let all = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP | GROUP_NPC | GROUP_PLAYER));
    for (e, mut th, mut t) in &mut thrown {
        th.life -= dt;
        if th.life <= 0.0 {
            commands.entity(e).despawn();
            continue;
        }
        th.vel.y -= GRAVITY * dt;
        let step = th.vel * dt;
        let len = step.length();
        if len < 1e-4 {
            continue;
        }
        t.rotate(Quat::from_axis_angle(th.axis, SPIN * dt * (len / dt / 25.0).min(1.0)));
        let filter = all.exclude_collider(th.owner);
        let Some((hit, toi, surface, normal)) = crate::footsteps::ray_surface_normal(&ctx, &surfaces, t.translation, step / len, len + 0.05, filter) else {
            t.translation += step;
            continue;
        };
        let at = t.translation + step / len * (toi - 0.05).max(0.0);
        let speed = th.vel.length();
        let corvo = Some(hit) == pe;
        if corvo && !th.spent {
            hits.write(PlayerHit { npc: th.owner, from: t.translation, damage: th.damage, kick: false, big: false, push: false, unblockable: true, missile: true });
        }
        // (its sound on what it strikes: `ThrownRock` x the surface, or a body)
        if speed > 2.0 {
            let against = if corvo { "Body" } else { crate::impacts::struck(surface) };
            if let Some(im) = level.as_ref().and_then(|l| l.scene.thrown_impacts.iter().find(|i| i.against == against).or_else(|| l.scene.thrown_impacts.iter().find(|i| i.against == "Environment"))) {
                if !im.sound.is_empty() {
                    sfx.write(PostEvent::named(&im.sound, Some(at)));
                }
            }
        }
        th.spent = true;
        t.translation = at;
        th.vel = (th.vel - 2.0 * th.vel.dot(normal) * normal) * BOUNCE;
        // (it comes to rest on the ground)
        if normal.y > 0.6 && th.vel.length() < 1.5 {
            th.vel = Vec3::ZERO;
        }
    }
}
