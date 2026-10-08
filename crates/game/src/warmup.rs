//! Shader warm-up when a level starts: behind a black screen, with the world paused, every
//! mesh is drawn regardless of the view (frustum culling off) until the pipelines of all its
//! materials are compiled, so surfaces don't appear late (blank) as they first come into view.

use crate::GameState;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::prelude::*;
use bevy::render::render_resource::PipelineCache;
use bevy::render::{Render, RenderApp, RenderSystems};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub struct WarmupPlugin;

impl Plugin for WarmupPlugin {
    fn build(&self, app: &mut App) {
        let waiting = Waiting::default();
        app.insert_resource(waiting.clone())
            .add_systems(OnEnter(GameState::InGame), begin.after(crate::level::LevelSpawnSet))
            .add_systems(Update, settle.run_if(in_state(GameState::InGame)))
            .add_systems(OnExit(GameState::InGame), |mut commands: Commands| commands.remove_resource::<Warmup>());
        if let Some(render) = app.get_sub_app_mut(RenderApp) {
            render.insert_resource(waiting).add_systems(Render, count_waiting.in_set(RenderSystems::Cleanup));
        }
    }
}

/// Pipelines the render world is still compiling.
#[derive(Resource, Clone, Default)]
struct Waiting(Arc<AtomicUsize>);

fn count_waiting(cache: Res<PipelineCache>, waiting: Res<Waiting>) {
    waiting.0.store(cache.waiting_pipelines().count(), Ordering::Relaxed);
}

/// Present while warming up.
#[derive(Resource)]
pub struct Warmup {
    started: std::time::Instant,
    frames: u32,
    settled: u32,
    most: usize,
}

/// Meshes drawn whatever the view while warming up.
#[derive(Component)]
struct WarmupCull;

#[derive(Component)]
struct WarmupCover;

/// `DH_NO_WARMUP` skips it.
pub fn enabled() -> bool {
    std::env::var("DH_NO_WARMUP").is_err()
}

fn begin(
    mut commands: Commands,
    meshes: Query<Entity, (With<Mesh3d>, Without<NoFrustumCulling>)>,
    mut time: ResMut<Time<Virtual>>,
    loading: Query<(), With<crate::loading::LoadingUi>>,
    mut text: Query<&mut Text, With<crate::loading::LoadingText>>,
) {
    if !enabled() {
        return;
    }
    let mut n = 0;
    for e in &meshes {
        commands.entity(e).try_insert((NoFrustumCulling, WarmupCull));
        n += 1;
    }
    // the loading screen stays up meanwhile (else a black screen)
    if loading.is_empty() {
        commands.spawn((
            WarmupCover,
            Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() },
            BackgroundColor(Color::BLACK),
            GlobalZIndex(i32::MAX - 1),
            DespawnOnExit(GameState::InGame),
        ));
    }
    for mut t in &mut text {
        t.0 = "Preparing".into();
    }
    time.pause();
    commands.insert_resource(Warmup { started: std::time::Instant::now(), frames: 0, settled: 0, most: 0 });
    info!("shader warm-up: {n} meshes");
}

fn settle(
    mut commands: Commands,
    warmup: Option<ResMut<Warmup>>,
    waiting: Res<Waiting>,
    culled: Query<Entity, With<WarmupCull>>,
    cover: Query<Entity, Or<(With<WarmupCover>, With<crate::loading::LoadingUi>)>>,
    mut time: ResMut<Time<Virtual>>,
    mut bar: Query<&mut Node, With<crate::loading::LoadingBar>>,
) {
    let Some(mut w) = warmup else { return };
    w.frames += 1;
    let pending = waiting.0.load(Ordering::Relaxed);
    w.most = w.most.max(pending);
    for mut n in &mut bar {
        n.width = percent(100.0 * (1.0 - pending as f32 / w.most.max(1) as f32));
    }
    w.settled = if pending == 0 && w.frames > 3 { w.settled + 1 } else { 0 };
    let elapsed = w.started.elapsed().as_secs_f32();
    if w.settled < 3 && elapsed < 20.0 {
        return;
    }
    for e in &culled {
        commands.entity(e).try_remove::<(NoFrustumCulling, WarmupCull)>();
    }
    for e in &cover {
        commands.entity(e).despawn();
    }
    time.unpause();
    commands.remove_resource::<Warmup>();
    info!("shader warm-up done in {elapsed:.1}s ({} frames)", w.frames);
}
