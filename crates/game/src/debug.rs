//! Debug helpers: FPS overlay, and an environment-driven screenshot sequence used for
//! automated visual verification.
//!
//! - `DH_SHOTS="x,y,z,yaw,pitch;..."` (or `spawn` for the player start) — teleport the
//!   player (noclip) to each view and save `DH_SHOT_DIR/shot_N.png`.
//! - `DH_EXIT=1` — quit after the last screenshot.
//! - `DH_WALK=1` — keep physics on (no noclip) for the first view, to verify collision.

use crate::player::{Player, PlayerCamera, VirtualInput};
use crate::GameState;
use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;
use bevy::render::view::screenshot::{save_to_disk, Screenshot};
use bevy::text::FontSize;

pub struct DebugPlugin;

impl Plugin for DebugPlugin {
    fn build(&self, app: &mut App) {
        // `DH_RENDER_DIAG=1`: per-pass GPU / CPU times (the script's `log` prints them)
        if std::env::var("DH_RENDER_DIAG").is_ok() {
            app.add_plugins(bevy::render::diagnostic::RenderDiagnosticsPlugin);
        }
        if std::env::var("DH_KCC_LOG").is_ok() {
            app.add_systems(PostUpdate, kcc_census.before(bevy_rapier3d::prelude::PhysicsSet::SyncBackend));
        }
        app.add_plugins(FrameTimeDiagnosticsPlugin::default())
            .add_systems(OnEnter(GameState::InGame), setup_overlay)
            .add_systems(Update, (update_overlay, shot_sequence).run_if(in_state(GameState::InGame)))
            .add_systems(Last, jump_log.run_if(in_state(GameState::InGame)));
    }
}

#[derive(Component)]
struct Overlay;

#[derive(Resource)]
struct ShotPlan {
    views: Vec<Option<[f32; 5]>>,
    dir: String,
    current: usize,
    frames: u32,
    exit: bool,
    walk: bool,
    /// Seconds to hold "forward" before each screenshot (physics mode only).
    autowalk: f32,
}

fn setup_overlay(mut commands: Commands) {
    commands.spawn((
        Overlay,
        Text::new(""),
        TextFont { font_size: FontSize::Px(14.0), ..default() },
        TextColor(Color::srgba(1.0, 1.0, 1.0, 0.8)),
        Node { position_type: PositionType::Absolute, bottom: px(4), left: px(8), ..default() },
        // off by default (F3 toggles); test runs keep it for their screenshots
        if std::env::var("DH_SCRIPT").is_ok() || std::env::var("DH_OVERLAY").is_ok() { Visibility::Inherited } else { Visibility::Hidden },
        DespawnOnExit(GameState::InGame),
    ));
    if let Ok(s) = std::env::var("DH_SHOTS") {
        let views = s
            .split(';')
            .filter(|v| !v.trim().is_empty())
            .map(|v| {
                if v.trim() == "spawn" {
                    return None;
                }
                let n: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                (n.len() == 5).then(|| [n[0], n[1], n[2], n[3], n[4]])
            })
            .collect();
        commands.insert_resource(ShotPlan {
            views,
            dir: std::env::var("DH_SHOT_DIR").unwrap_or_else(|_| "cache/shots".into()),
            current: 0,
            frames: 0,
            exit: std::env::var("DH_EXIT").is_ok(),
            walk: std::env::var("DH_WALK").is_ok(),
            autowalk: std::env::var("DH_AUTOWALK").ok().and_then(|v| v.parse().ok()).unwrap_or(0.0),
        });
    }
}

fn update_overlay(
    diag: Res<DiagnosticsStore>,
    keys: Res<ButtonInput<KeyCode>>,
    mut q: Query<(&mut Text, &mut Visibility), With<Overlay>>,
    player: Query<(&Player, &Transform)>,
) {
    let Ok((mut text, mut vis)) = q.single_mut() else { return };
    if keys.just_pressed(KeyCode::F3) {
        *vis = if *vis == Visibility::Hidden { Visibility::Inherited } else { Visibility::Hidden };
    }
    let fps = diag
        .get(&FrameTimeDiagnosticsPlugin::FPS)
        .and_then(|d| d.smoothed())
        .unwrap_or(0.0);
    let mut s = format!("{fps:.0} fps");
    if let Ok((p, t)) = player.single() {
        s += &format!(
            "  pos {:.1} {:.1} {:.1}  yaw {:.2} pitch {:.2}{}{}",
            t.translation.x,
            t.translation.y,
            t.translation.z,
            p.yaw,
            p.pitch,
            if p.grounded { "  ground" } else { "" },
            if p.noclip { "  NOCLIP" } else { "" }
        );
    }
    text.0 = s;
}

fn shot_sequence(
    mut commands: Commands,
    plan: Option<ResMut<ShotPlan>>,
    mut player: Query<(&mut Player, &mut Transform), Without<PlayerCamera>>,
    mut exit: MessageWriter<AppExit>,
    mut vinput: ResMut<VirtualInput>,
) {
    let Some(mut plan) = plan else { return };
    if plan.current >= plan.views.len() {
        plan.frames += 1;
        if plan.exit && plan.frames > 30 {
            exit.write(AppExit::Success);
        }
        return;
    }
    let Ok((mut p, mut t)) = player.single_mut() else { return };
    if plan.frames == 0 {
        if let Some(v) = plan.views[plan.current] {
            p.noclip = !(plan.walk && plan.current == 0);
            t.translation = Vec3::new(v[0], v[1], v[2]);
            p.yaw = v[3];
            p.pitch = v[4];
        }
    }
    plan.frames += 1;
    // allow streaming/upload + physics settle (+ optional scripted walk)
    let walk_frames = if !p.noclip { (plan.autowalk * 60.0) as u32 } else { 0 };
    let wait = if plan.current == 0 { 150 } else { 45 } + walk_frames;
    vinput.forward = if walk_frames > 0 && plan.frames > wait - walk_frames && plan.frames < wait { 1.0 } else { 0.0 };
    if plan.frames == wait {
        let path = format!("{}/shot_{}.png", plan.dir, plan.current);
        let _ = std::fs::create_dir_all(&plan.dir);
        info!("screenshot {path} at {} (yaw {} pitch {})", t.translation, p.yaw, p.pitch);
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
    }
    if plan.frames > wait + 10 {
        plan.current += 1;
        plan.frames = 0;
    }
}

/// `DH_KCC_LOG`: which character controllers move each frame (once a second).
fn kcc_census(
    time: Res<Time>,
    mut acc: Local<f32>,
    q: Query<(&bevy_rapier3d::prelude::KinematicCharacterController, Option<&crate::npc::Npc>, Option<&GlobalTransform>)>,
    rapier: bevy_rapier3d::prelude::ReadRapierContext,
    shapes: Query<(&bevy_rapier3d::prelude::Collider, Option<&crate::footsteps::ColliderSurfaces>, &GlobalTransform)>,
    owners: Query<(&crate::level::LevelInstance, &crate::level::InstanceCollider)>,
) {
    *acc += time.delta_secs();
    if *acc < 1.0 {
        return;
    }
    *acc = 0.0;
    let moving: Vec<String> = q
        .iter()
        .filter(|(k, ..)| k.translation.is_some())
        .map(|(k, n, t)| format!("{} {:?} at {:.1?}", n.map(|n| n.name.as_str()).unwrap_or("player"), k.translation.unwrap(), t.map(|t| t.translation())))
        .collect();
    info!("kcc census: {} moving of {}: {:?}", moving.len(), q.iter().count(), moving);
    // the colliders around the player, with their triangle counts
    let Some(pos) = q.iter().find(|(_, n, _)| n.is_none()).and_then(|(_, _, t)| t).map(|t| t.translation()) else { return };
    let Ok(ctx) = rapier.single() else { return };
    let ball = bevy_rapier3d::prelude::Collider::ball(1.5);
    ctx.intersect_shape(pos, Quat::IDENTITY, ball.raw.0.as_ref(), bevy_rapier3d::prelude::QueryFilter::default(), |e| {
        if let Ok((c, surf, gt)) = shapes.get(e) {
            let owner = owners.iter().find(|(_, ic)| ic.0 == e).map(|(li, _)| format!("{} {}", li.class, li.actor)).unwrap_or_default();
            let aabb = c.raw.compute_local_aabb();
            info!("kcc near: tris {} extent {:.1?} at {:.1} {owner}", surf.map(|s| s.tris.len()).unwrap_or(0), (aabb.maxs - aabb.mins), gt.translation());
        }
        true
    });
}

/// Debug: report the player jumping more than 5 m in a frame (`DH_JUMP_LOG`).
pub fn jump_log(player: Query<&Transform, With<crate::player::Player>>, mut last: Local<Option<Vec3>>) {
    if std::env::var("DH_JUMP_LOG").is_err() {
        return;
    }
    let Ok(t) = player.single() else { return };
    if let Some(l) = *last {
        if l.distance(t.translation) > 5.0 {
            info!("player jumped from {l:.2} to {:.2}", t.translation);
        }
    }
    *last = Some(t.translation);
}
