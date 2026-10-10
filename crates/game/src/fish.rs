//! Hagfish (`DisFish` + `DisTweaks_Fish`): the flooded streets' fish. Each roams the water around
//! where it was put (`m_fRoamingRadius`, `m_fSpeed`); whoever swims near is set upon
//! (`m_fAttackSpeed`) and bitten (`m_DamagePerHit` by difficulty, `m_fMinTimeBetweenBites`
//! apart, the `m_BiteAnimNames` snaps); bodies in the water are eaten (`m_fConsumeCorpseTime`).
//! A blade, bolt or bullet kills one (`m_DeathAnimName`).

use crate::anim::{Animator, ClipId};
use crate::audio::PostEvent;
use crate::gadgets::Explosion;
use crate::gameplay::{HudMessages, PlayerStats, Strikeable, Struck, TimeControl};
use crate::level::{GameAssets, LevelInfo, GROUP_PROP, GROUP_WORLD};
use crate::npc::{Mode, Npc};
use crate::player::{Player, PlayerCamera};
use crate::swim::{Swim, Waters};
use crate::world_light::{LitActor, WorldLighting};
use crate::GameState;
use bevy::camera::visibility::DynamicSkinnedMeshBounds;
use bevy::mesh::skinning::SkinnedMesh;
use bevy::mesh::MeshTag;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;
use std::collections::HashMap;
use std::f32::consts::{FRAC_PI_2, TAU};

pub struct FishPlugin;

impl Plugin for FishPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(GameState::InGame), spawn_fish.after(crate::level::LevelSpawnSet))
            .add_systems(Update, (fish_hits, fish_brain).chain().run_if(in_state(GameState::InGame)));
    }
}

/// How far a fish notices a swimmer in its water.
const NOTICE: f32 = 12.0;
/// The bite's reach beyond `m_fBiteDistanceFromCam`.
const REACH: f32 = 0.45;
/// Kept under the surface by this much.
const DEPTH: f32 = 0.35;
/// Seconds a dead fish lingers.
const DEAD_FOR: f32 = 8.0;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Doing {
    Roam,
    Attack,
    Eat(Entity),
    Dead(f32),
}

#[derive(Component)]
pub struct Fish {
    index: u32,
    home: Vec3,
    goal: Vec3,
    doing: Doing,
    bite_t: f32,
    vel: Vec3,
    clips: FishClips,
    /// what blades, bolts and bullets strike
    collider: Entity,
}

impl Fish {
    pub fn index(&self) -> u32 { self.index }
    pub fn describe(&self) -> String {
        format!("#{} {:?} speed {:.1}", self.index, self.doing, self.vel.length())
    }
}

#[derive(Clone, Default)]
struct FishClips {
    swim: Option<ClipId>,
    bites: Vec<ClipId>,
    eat: Option<ClipId>,
    death: Option<ClipId>,
}

fn spawn_fish(mut commands: Commands, assets: Option<Res<GameAssets>>, level: Option<Res<LevelInfo>>, mut wl: Option<ResMut<WorldLighting>>) {
    let (Some(assets), Some(level)) = (assets, level) else { return };
    let mut n = 0;
    for (i, def) in level.scene.fish.iter().enumerate() {
        let Some(vis) = def.npc_type.and_then(|t| assets.npc_types.get(t as usize)).and_then(|v| v.as_ref()) else { continue };
        let at = Vec3::from(def.position);
        let slot = wl.as_mut().map(|w| w.alloc_slot()).unwrap_or(0);
        let bones = &vis.skeleton.bones;
        let mut joints = Vec::with_capacity(bones.len());
        for b in bones {
            joints.push(commands.spawn(Transform::from_translation(Vec3::from(b.translation)).with_rotation(Quat::from_array(b.rotation).normalize())).id());
        }
        let visual = commands.spawn((Transform::from_rotation(Quat::from_rotation_y(FRAC_PI_2)), Visibility::default())).id();
        for (k, b) in bones.iter().enumerate() {
            let parent = if b.parent >= 0 && (b.parent as usize) < k { joints[b.parent as usize] } else { visual };
            commands.entity(parent).add_child(joints[k]);
        }
        for (mesh, mat) in &vis.parts.parts {
            let mut ec = commands.spawn((
                Mesh3d(mesh.clone()),
                MeshTag(slot),
                SkinnedMesh { inverse_bindposes: vis.inverse_bindposes.clone(), joints: joints.clone() },
                DynamicSkinnedMeshBounds,
                bevy::light::NotShadowCaster,
            ));
            mat.apply(&mut ec);
            let m = ec.id();
            commands.entity(visual).add_child(m);
        }
        let collider = commands.spawn((Transform::default(), Collider::ball(0.22), CollisionGroups::new(GROUP_PROP, Group::ALL), Sensor)).id();
        let root = commands
            .spawn((
                Transform::from_translation(at).with_rotation(Quat::from_rotation_y(def.yaw)),
                Visibility::default(),
                LitActor { slot, probe_height: 0.0, brightness: 0.3, color: Vec3::splat(0.05), sun: 0.0, dominant: None },
                DespawnOnExit(GameState::InGame),
            ))
            .add_children(&[visual, collider])
            .id();
        commands.entity(collider).insert(Strikeable(root));
        commands.entity(root).insert(crate::possession::Host { npc_type: def.npc_type, rooted: false, fish: true, seat: Vec3::ZERO, facing: Vec3::NEG_Z });
        let mut clips = FishClips::default();
        if let Some(lib) = vis.anims.clone() {
            clips = FishClips {
                swim: lib.find(&def.swim),
                bites: def.bites.iter().filter_map(|b| lib.find(b)).collect(),
                eat: lib.find(&def.eat),
                death: lib.find(&def.death),
            };
            let mut a = Animator::new(lib, &vis.skeleton, joints);
            if let Some(s) = clips.swim {
                a.play(s, true, 0.9 + rand::random::<f32>() * 0.2, 0.0);
                a.seek(rand::random::<f32>());
            }
            commands.entity(root).insert(a);
        }
        commands.entity(root).insert(Fish { index: i as u32, home: at, goal: at, doing: Doing::Roam, bite_t: 0.0, vel: Vec3::ZERO, clips, collider });
        n += 1;
    }
    if n > 0 {
        info!("{n} fish");
    }
}

/// Struck by a blade, a bolt, a bullet or a blast: dead.
fn fish_hits(
    mut commands: Commands,
    mut struck: MessageReader<Struck>,
    mut blasts: MessageReader<Explosion>,
    mut fish: Query<(Entity, &mut Fish, &mut Animator, &Transform)>,
    mut sfx: MessageWriter<PostEvent>,
) {
    let mut killed: Vec<Entity> = struck.read().map(|s| s.target).collect();
    for b in blasts.read() {
        killed.extend(fish.iter().filter(|(_, _, _, t)| t.translation.distance(b.at) < b.radius).map(|(e, ..)| e));
    }
    for e in killed {
        let Ok((_, mut f, mut a, t)) = fish.get_mut(e) else { continue };
        if matches!(f.doing, Doing::Dead(_)) {
            continue;
        }
        f.doing = Doing::Dead(0.0);
        if let Some(d) = f.clips.death {
            a.restart(d, false, 1.0, 0.1);
        }
        commands.entity(f.collider).remove::<Strikeable>();
        sfx.write(PostEvent::named("Imp_Sword_on_Body", Some(t.translation)));
        if std::env::var("DH_FISH_LOG").is_ok() {
            info!("fish #{} killed", f.index);
        }
    }
}

/// Physical cover blocks a bite or feeding just as it blocks swimming.
fn fish_target_visible(ctx: &RapierContext, from: Vec3, target: Vec3) -> bool {
    let to = target - from;
    let distance = to.length();
    distance < 0.001 || ctx.cast_ray(from, to / distance, distance, true,
        QueryFilter::default().exclude_sensors()
            .groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP))).is_none()
}

#[cfg(test)]
mod cover_tests {
    use super::*;

    #[derive(Resource, Default)]
    struct Reachable(bool);

    #[test]
    fn fish_animation_clock_stops_resumes_and_follows_possession() {
        let mut app = App::new();
        app.init_resource::<Time>().init_resource::<TimeControl>().init_resource::<crate::settings::Settings>()
            .init_resource::<Swim>().init_resource::<PlayerStats>().init_resource::<HudMessages>()
            .init_resource::<crate::possession::Possession>().add_message::<PostEvent>()
            .add_systems(Update, fish_brain);
        let skeleton = dhcook::format::SkeletonDef::default();
        let lib = std::sync::Arc::new(crate::anim::CharAnims::new(&skeleton, vec![]));
        let entity = app.world_mut().spawn((Fish { index: 0, home: Vec3::ZERO, goal: Vec3::ZERO,
            doing: Doing::Roam, bite_t: 0.0, vel: Vec3::ZERO, clips: FishClips::default(), collider: Entity::PLACEHOLDER },
            Transform::default(), Animator::new(lib, &skeleton, vec![]))).id();
        // No level/water is needed to update clocks, including a zero-delta load frame.
        app.world_mut().resource_mut::<TimeControl>().bend_remaining = 10.0;
        app.update();
        assert_eq!(app.world().get::<Animator>(entity).unwrap().time_scale, 0.0);
        app.world_mut().resource_mut::<TimeControl>().world_dilation = 0.25;
        app.update();
        assert_eq!(app.world().get::<Animator>(entity).unwrap().time_scale, 0.25);
        app.world_mut().get_mut::<Animator>(entity).unwrap().frozen = true;
        app.world_mut().entity_mut(entity).insert(crate::possession::Possessed);
        app.update();
        let anim = app.world().get::<Animator>(entity).unwrap();
        assert_eq!(anim.time_scale, 1.0);
        assert!(!anim.frozen);
        app.world_mut().entity_mut(entity).remove::<crate::possession::Possessed>();
        app.world_mut().resource_mut::<TimeControl>().world_dilation = 0.0;
        app.update();
        assert_eq!(app.world().get::<Animator>(entity).unwrap().time_scale, 0.0);
        app.world_mut().resource_mut::<TimeControl>().bend_remaining = 0.0;
        app.update();
        assert_eq!(app.world().get::<Animator>(entity).unwrap().time_scale, 1.0);
    }

    #[test]
    fn nearby_fish_targets_require_clear_water_through_world_and_prop_cover() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), bevy::scene::ScenePlugin, TransformPlugin, RapierPhysicsPlugin::<NoUserData>::default()))
            .init_resource::<Assets<Mesh>>().init_resource::<Reachable>()
            .add_systems(Last, |ctx: ReadRapierContext, mut reachable: ResMut<Reachable>| {
                reachable.0 = fish_target_visible(&ctx.single().unwrap(), Vec3::ZERO, Vec3::X * 0.4);
            });
        app.update();
        assert!(app.world().resource::<Reachable>().0);
        for group in [GROUP_WORLD, GROUP_PROP] {
            let cover = app.world_mut().spawn((Collider::cuboid(0.01, 1.0, 1.0), Transform::from_xyz(0.2, 0.0, 0.0), CollisionGroups::new(group, Group::ALL))).id();
            app.update();
            assert!(!app.world().resource::<Reachable>().0, "even a target within bite reach is blocked");
            app.world_mut().entity_mut(cover).insert(Sensor);
            app.update();
            assert!(app.world().resource::<Reachable>().0, "water/trigger sensors do not provide cover");
            app.world_mut().entity_mut(cover).remove::<Sensor>();
            app.update();
            assert!(!app.world().resource::<Reachable>().0);
            app.world_mut().despawn(cover);
            app.update();
            assert!(app.world().resource::<Reachable>().0);
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn fish_brain(
    mut commands: Commands,
    (time, tc, settings): (Res<Time>, Res<TimeControl>, Res<crate::settings::Settings>),
    level: Option<Res<LevelInfo>>,
    waters: Option<Res<Waters>>,
    swim: Res<Swim>,
    rapier: ReadRapierContext,
    (mut stats, mut msgs): (ResMut<PlayerStats>, ResMut<HudMessages>),
    possession: Res<crate::possession::Possession>,
    player: Query<&Transform, (With<Player>, Without<Fish>)>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    bodies: Query<(Entity, &Npc, &Transform, Option<&Visibility>), Without<Fish>>,
    mut fish: Query<(Entity, &mut Fish, &mut Animator, &mut Transform, Has<crate::possession::Possessed>), Without<Npc>>,
    mut sfx: MessageWriter<PostEvent>,
    mut eaten: Local<HashMap<Entity, f32>>,
) {
    // Update animation clocks even when the brain cannot advance. Possessed
    // creatures follow Corvo's clock, including when entered during Bend Time.
    for (_, _, mut anim, _, possessed) in &mut fish {
        anim.time_scale = if possessed { 1.0 } else { tc.world_scale().max(0.0) };
        if possessed { anim.frozen = false; }
    }
    let (Some(level), Some(waters)) = (level, waters) else { return };
    let dt = time.delta_secs() * tc.world_scale();
    if dt <= 0.0 {
        return;
    }
    let ctx = rapier.single().ok();
    let walls = QueryFilter::default().exclude_sensors().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
    let eye = cam.single().ok().map(|g| g.translation());
    let pp = player.single().ok().map(|t| t.translation);
    // a swimmer, in which water
    let swimmer = match (swim.water, eye, pp) {
        (Some(w), Some(e), Some(_)) if !stats.dead && possession.host.is_none() => Some((w, e)),
        _ => None,
    };
    let log_on = std::env::var("DH_FISH_LOG").is_ok();
    // bodies in the water
    let floating: Vec<(Entity, Vec3, usize)> = bodies
        .iter()
        .filter(|(_, n, _, v)| n.mode == Mode::Dead && *v != Some(&Visibility::Hidden))
        .filter_map(|(e, _, t, _)| waters.at(t.translation + Vec3::Y * 0.3).map(|(w, _)| (e, t.translation, w)))
        .collect();
    eaten.retain(|e, _| floating.iter().any(|f| f.0 == *e));
    for (fe, mut f, mut a, mut t, possessed) in &mut fish {
        let Some(def) = level.scene.fish.get(f.index as usize) else { continue };
        let me = t.translation;
        // Corvo is in it: it swims as he does
        if possessed {
            if let Some(s) = f.clips.swim {
                a.play(s, true, 1.4, 0.2);
            }
            f.home = me;
            f.goal = me;
            continue;
        }
        // far from the eye the skeleton rests (`m_fSkeletonUpdateDistanceMax`)
        a.frozen = eye.is_some_and(|e| e.distance(me) > 30.0);
        if let Doing::Dead(s) = f.doing {
            // belly up, drifting to the surface
            let s = s + dt;
            f.doing = Doing::Dead(s);
            if let Some((_, surface)) = waters.at(me) {
                t.translation.y = (me.y + 0.3 * dt).min(surface - 0.05);
            }
            if s > DEAD_FOR {
                commands.entity(fe).despawn();
            }
            continue;
        }
        let water = waters.at(me).map(|w| w.0);
        // what it's doing
        let visible = |target| ctx.as_ref().is_some_and(|c| fish_target_visible(c, me, target));
        let target = swimmer.filter(|(w, e)| Some(*w) == water && e.distance(me) < NOTICE && visible(*e));
        f.doing = match (target, f.doing) {
            (Some(_), _) => Doing::Attack,
            (None, Doing::Eat(b)) if floating.iter().any(|x| x.0 == b && Some(x.2) == water && visible(x.1 + Vec3::Y * 0.2)) => Doing::Eat(b),
            _ => match floating.iter().filter(|x| Some(x.2) == water && x.1.distance(me) < NOTICE && visible(x.1 + Vec3::Y * 0.2)).min_by(|a, b| a.1.distance(me).total_cmp(&b.1.distance(me))) {
                Some(&(b, _, _)) => Doing::Eat(b),
                None => Doing::Roam,
            },
        };
        let (goal, speed) = match f.doing {
            Doing::Attack => {
                let e = target.map(|t| t.1).unwrap_or(me);
                // just short of the face
                (e - Vec3::Y * 0.15, def.attack_speed)
            }
            Doing::Eat(b) => (floating.iter().find(|x| x.0 == b).map(|x| x.1 + Vec3::Y * 0.2).unwrap_or(me), def.speed),
            _ => {
                if me.distance(f.goal) < 0.4 {
                    let a = rand::random::<f32>() * TAU;
                    let r = def.roam.max(1.0) * rand::random::<f32>().sqrt();
                    f.goal = f.home + Vec3::new(a.cos() * r, (rand::random::<f32>() - 0.5) * 0.6, a.sin() * r);
                }
                (f.goal, def.speed * 0.6)
            }
        };
        // under the surface
        let mut goal = goal;
        if let Some((_, surface)) = waters.at(me) {
            goal.y = goal.y.min(surface - DEPTH);
        }
        let to = goal - me;
        let d = to.length();
        let close = match f.doing {
            Doing::Attack => def.bite_distance + REACH,
            Doing::Eat(_) => 0.5,
            _ => 0.05,
        };
        let want = if d > close { to / d * speed } else { Vec3::ZERO };
        f.vel = f.vel.lerp(want, (dt * 3.0).min(1.0));
        let mut step = f.vel * dt;
        // not through walls: a new heading
        if let (Some(c), true) = (ctx.as_ref(), step.length() > 1e-4) {
            if c.cast_ray(me, step.normalize(), step.length() + 0.25, true, walls).is_some() {
                step = Vec3::ZERO;
                f.vel = Vec3::ZERO;
                if f.doing == Doing::Roam {
                    f.goal = f.home;
                }
            }
        }
        let next = me + step;
        // it stays in the water
        if waters.at(next).is_some() || water.is_none() {
            t.translation = next;
        } else {
            f.vel = Vec3::ZERO;
            f.goal = f.home;
        }
        // a fish keeps level-ish (at most ~25 degrees nose up or down); straight above or below
        // it keeps its heading
        let face = if d > 0.05 { to } else { f.vel };
        let h = face.with_y(0.0).length();
        if h > 0.05 {
            let face = Vec3::new(face.x, face.y.clamp(-0.45 * h, 0.45 * h), face.z);
            let target_rot = Transform::IDENTITY.looking_to(face.normalize(), Vec3::Y).rotation;
            t.rotation = t.rotation.slerp(target_rot, (dt * 6.0).min(1.0));
        }
        // the bite, the meal, the swim
        f.bite_t = (f.bite_t - dt).max(0.0);
        match f.doing {
            Doing::Attack if d <= close && f.bite_t <= 0.0 => {
                f.bite_t = def.bite_interval.max(0.1);
                if let Some(&b) = f.clips.bites.get(rand::random_range(0..f.clips.bites.len().max(1))) {
                    a.restart(b, false, 1.0, 0.05);
                }
                let dmg = def.damage[settings.difficulty.min(3) as usize];
                crate::gameplay::hurt_player(&mut stats, &mut msgs, &mut sfx, dmg);
                if log_on {
                    info!("fish #{} bites: -{dmg} ({:.0} left)", f.index, stats.health);
                }
            }
            Doing::Eat(b) if d <= 0.8 => {
                if let Some(e) = f.clips.eat {
                    a.play(e, true, 1.0, 0.2);
                }
                let te = eaten.entry(b).or_insert(0.0);
                *te += dt;
                if *te >= def.consume {
                    commands.entity(b).insert((crate::npc::ConsumedBody, Visibility::Hidden));
                    if log_on {
                        info!("fish ate a body");
                    }
                }
            }
            _ => {
                let biting = a.current().is_some_and(|c| f.clips.bites.contains(&c.clip)) && !a.finished();
                if !biting {
                    if let Some(s) = f.clips.swim {
                        let rate = (f.vel.length() / def.speed.max(0.5)).clamp(0.6, 2.2);
                        a.play(s, true, rate, 0.2);
                    }
                }
            }
        }
    }
}
