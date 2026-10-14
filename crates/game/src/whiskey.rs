//! A Bottle Street thug's thrown whiskey bottle (`DisTweaks_NPCThrowWhiskey`'s projectile,
//! `Twk_Proj_WhiskeyBottle`): out of its hand at `m_fThrowSpeed`, tumbling (`m_fSpinSpeed`) and
//! trailing its flame (`m_pTrailEffect`, `ps_bottle_fly`); it bursts where it first strikes
//! (`DisTweaks_WhiskeyBottle.m_pExplosionTweaks`: its fire, `ps_bottle_explosion`, and its
//! sound). Its fire breath is the thug's own blow (`npc.rs`).

use bevy::mesh::MeshTag;
use bevy::prelude::*;
use bevy_rapier3d::prelude::{CollisionGroups, Group, QueryFilter, ReadRapierContext};

use crate::audio::PostEvent;
use crate::gadgets::Explosion;
use crate::gameplay::{HitKind, TimeControl};
use crate::level::{GameAssets, GROUP_NPC, GROUP_PLAYER, GROUP_PROP, GROUP_WORLD};
use crate::particles::SpawnEffect;
use crate::GameState;

pub struct WhiskeyPlugin;

impl Plugin for WhiskeyPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<NpcBottle>().add_systems(Update, (npc_bottles, fly_bottles).chain().run_if(in_state(GameState::InGame)));
    }
}

/// A bottle a thug throws: from where, how fast, its burst.
#[derive(Message, Clone)]
pub struct NpcBottle {
    pub owner: Entity,
    pub from: Vec3,
    pub vel: Vec3,
    pub blast: dhcook::format::TrapBlast,
}

/// A whiskey bottle in flight.
#[derive(Component)]
pub struct Bottle {
    vel: Vec3,
    /// what it turns about as it tumbles
    axis: Vec3,
    owner: Entity,
    blast: dhcook::format::TrapBlast,
    /// seconds left before it is given up (fallen out of the world)
    life: f32,
}

/// How fast it tumbles (rad/s: `DisTweaks_NPCThrowWhiskey.m_fSpinSpeed`).
const SPIN: f32 = 10.0;
/// What pulls it down (m/s², as a grenade: 1500 uu/s²).
const GRAVITY: f32 = 15.0;

fn npc_bottles(mut commands: Commands, mut throws: MessageReader<NpcBottle>, assets: Option<Res<GameAssets>>, mut fx: MessageWriter<SpawnEffect>) {
    for b in throws.read() {
        let axis = b.vel.cross(Vec3::Y).normalize_or(Vec3::X);
        let mut ec = commands.spawn((
            Bottle { vel: b.vel, axis, owner: b.owner, blast: b.blast.clone(), life: 6.0 },
            Transform::from_translation(b.from),
            Visibility::default(),
            DespawnOnExit(GameState::InGame),
        ));
        if let Some(parts) = assets.as_ref().and_then(|a| a.props.get("thug_bottle")) {
            ec.with_children(|c| {
                for (mesh, mat) in &parts.parts {
                    let mut m = c.spawn((Mesh3d(mesh.clone()), MeshTag(0), Transform::IDENTITY));
                    mat.apply(&mut m);
                }
            });
        }
        fx.write(SpawnEffect { follow: Some(ec.id()), secs: 6.0, ..SpawnEffect::at("whiskey_trail", Vec3::ZERO) });
    }
}

fn fly_bottles(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<TimeControl>,
    rapier: ReadRapierContext,
    mut bottles: Query<(Entity, &mut Bottle, &mut Transform)>,
    (mut blasts, mut sfx): (MessageWriter<Explosion>, MessageWriter<PostEvent>),
    settings: Res<crate::settings::Settings>,
) {
    let dt = time.delta_secs() * tc.world_scale();
    if dt <= 0.0 {
        return;
    }
    let Ok(ctx) = rapier.single() else { return };
    let all = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP | GROUP_NPC | GROUP_PLAYER));
    for (e, mut b, mut t) in &mut bottles {
        b.life -= dt;
        b.vel.y -= GRAVITY * dt;
        let step = b.vel * dt;
        let len = step.length();
        t.rotate(Quat::from_axis_angle(b.axis, SPIN * dt));
        // (its thrower's own body doesn't stop it)
        let hit = (len > 1e-5).then(|| ctx.cast_ray(t.translation, step / len, len + 0.05, true, all.exclude_collider(b.owner))).flatten();
        match hit {
            Some((_, toi)) => {
                let at = t.translation + step / len * (toi - 0.05).max(0.0);
                let x = &b.blast;
                blasts.write(Explosion {
                    at,
                    radius: x.radius,
                    full: x.full,
                    damage: x.damage[settings.difficulty.min(3) as usize],
                    effect: "whiskey_burst",
                    player: Some([x.player_radius, x.player_full]),
                    kind: HitKind::EnemyExplosion,
                    silent: !x.sound.is_empty(),
                });
                if !x.sound.is_empty() {
                    sfx.write(PostEvent::named(&x.sound, Some(at)));
                }
                commands.entity(e).despawn();
            }
            None if b.life <= 0.0 => commands.entity(e).despawn(),
            None => t.translation += step,
        }
    }
}
