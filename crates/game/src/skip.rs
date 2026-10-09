//! Holding to skip a scene, as the original asks (`Dis_PlayerChoice_RequestSkip` on Use and
//! Enter, held `DisTweaks_PlayerInput.m_fSkipMatineeButtonTime`), with the HUD's skip gauge
//! (`hud_skGauge_`: its rings, the blue one filling round as the button is held, the pointer at
//! its edge, and "Skip" over a brush stroke).

use crate::GameState;
use bevy::asset::embedded_asset;
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;
use bevy::ui::widget::NodeImageMode;
use bevy::window::PrimaryWindow;

pub struct SkipPlugin;

impl Plugin for SkipPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "radial.wgsl");
        app.add_plugins(UiMaterialPlugin::<RadialFill>::default())
            .init_resource::<SkipHold>()
            .add_systems(OnEnter(GameState::InGame), spawn_gauge)
            .add_systems(Update, ((skip_hold, show_gauge).chain(), choke_gauge).run_if(in_state(GameState::InGame)));
    }
}

/// `m_fSkipMatineeButtonTime` (the class default; Corvo's input tweak keeps it).
const HOLD: f32 = 1.0;
/// Where the fill and the pointer start: the gauge's gap (turns clockwise from the top).
const START: f32 = 0.533;
/// The gauge's middle on the HUD's 1280x720 stage (`skipCutScene_mc` + `_gauge_mc`).
const AT: Vec2 = Vec2::new(127.2, 617.0);
/// The HUD movie's bitmaps: text stroke, rings, the blue fill, its highlights, the pointer.
const STROKE: u32 = 449;
const RING_OUTER: u32 = 469;
const RING: u32 = 601;
const FILL: u32 = 603;
const SHINE: u32 = 605;
const POINTER: u32 = 432;

/// A ring filling round.
#[derive(AsBindGroup, Asset, TypePath, Clone, Debug)]
pub struct RadialFill {
    /// (share filled, start, alpha, -)
    #[uniform(0)]
    params: Vec4,
    #[texture(1)]
    #[sampler(2)]
    texture: Handle<Image>,
}

impl UiMaterial for RadialFill {
    fn fragment_shader() -> ShaderRef {
        "embedded://dishonored/radial.wgsl".into()
    }
}

/// How long the skip button has been held (and whether it must be let go first).
#[derive(Resource, Default)]
pub struct SkipHold {
    pub t: f32,
    spent: bool,
    shown: f32,
}

#[derive(Component)]
struct Gauge;
/// a part: middle and size on the stage (from the gauge's middle), mirrored
#[derive(Component)]
struct Part(Vec2, Vec2, bool);
#[derive(Component)]
struct Fill;
#[derive(Component)]
struct Pointer;

fn spawn_gauge(mut commands: Commands, mut ui: ResMut<crate::ui_images::UiImages>, mut images: ResMut<Assets<Image>>, mut mats: ResMut<Assets<RadialFill>>) {
    spawn_choke_gauge(&mut commands, &mut ui, &mut images, &mut mats);
    let mut img = |id: u32| ui.get(&mut images, "HUD", id).map(|(h, _)| h);
    let (Some(stroke), Some(outer), Some(ring), Some(fill), Some(shine)) = (img(STROKE), img(RING_OUTER), img(RING), img(FILL), img(SHINE)) else { return };
    let pointer = img(POINTER);
    commands
        .spawn((Gauge, Node { position_type: PositionType::Absolute, ..default() }, Visibility::Hidden, GlobalZIndex(5), Pickable::IGNORE, DespawnOnExit(GameState::InGame)))
        .with_children(|c| {
            let node = || Node { position_type: PositionType::Absolute, ..default() };
            // "Skip" over its brush stroke (`_bkgdTxt_mc`: mirrored, a fifth opaque)
            let mut st = ImageNode::new(stroke).with_color(Color::srgba(1.0, 1.0, 1.0, 0.2)).with_mode(NodeImageMode::Stretch);
            st.flip_x = true;
            c.spawn((Part(Vec2::new(76.5, 3.85), Vec2::new(234.0, 67.0), true), st, node()));
            c.spawn((Part(Vec2::ZERO, Vec2::splat(77.0), false), ImageNode::new(outer).with_mode(NodeImageMode::Stretch), node()));
            c.spawn((Part(Vec2::ZERO, Vec2::splat(64.0), false), ImageNode::new(ring).with_mode(NodeImageMode::Stretch), node()));
            for tex in [fill, shine] {
                c.spawn((Part(Vec2::ZERO, Vec2::splat(64.0), false), Fill, MaterialNode(mats.add(RadialFill { params: Vec4::new(0.0, START, 1.0, 0.0), texture: tex })), node()));
            }
            if let Some(p) = pointer {
                c.spawn((Part(Vec2::ZERO, Vec2::new(34.0, 10.0), false), Pointer, ImageNode::new(p).with_mode(NodeImageMode::Stretch), node(), UiTransform::default()));
            }
            c.spawn((Part(Vec2::new(75.0, 2.7), Vec2::new(80.0, 30.0), false), Text::new("Skip"), TextFont { font_size: FontSize::Px(20.0), ..default() }, TextColor(Color::srgb(0.92, 0.9, 0.82)), node()));
        });
}

/// The HUD's interaction gauge at the screen's middle (`hud_chGauge_`: its rings, the red
/// fill for a chokehold, `DGT_ChokeGauge`), filling as the victim goes limp.
#[derive(Component)]
struct ChokeGauge;
const CH_RING: u32 = 455;
const CH_FILL: u32 = 467;
const CH_OUTER: u32 = 469;

fn spawn_choke_gauge(commands: &mut Commands, ui: &mut crate::ui_images::UiImages, images: &mut Assets<Image>, mats: &mut Assets<RadialFill>) {
    let mut img = |id: u32| ui.get(images, "HUD", id).map(|(h, _)| h);
    let (Some(ring), Some(fill), Some(outer)) = (img(CH_RING), img(CH_FILL), img(CH_OUTER)) else { return };
    let pointer = img(POINTER);
    commands
        .spawn((ChokeGauge, Node { position_type: PositionType::Absolute, ..default() }, Visibility::Hidden, GlobalZIndex(5), Pickable::IGNORE, DespawnOnExit(GameState::InGame)))
        .with_children(|c| {
            let node = || Node { position_type: PositionType::Absolute, ..default() };
            c.spawn((Part(Vec2::ZERO, Vec2::splat(77.0), false), ImageNode::new(ring).with_mode(NodeImageMode::Stretch), node()));
            c.spawn((Part(Vec2::ZERO, Vec2::splat(77.0), false), Fill, MaterialNode(mats.add(RadialFill { params: Vec4::new(0.0, START, 1.0, 0.0), texture: fill })), node()));
            c.spawn((Part(Vec2::ZERO, Vec2::splat(77.0), false), ImageNode::new(outer).with_mode(NodeImageMode::Stretch), node()));
            if let Some(p) = pointer {
                c.spawn((Part(Vec2::ZERO, Vec2::new(34.0, 10.0), false), Pointer, ImageNode::new(p).with_mode(NodeImageMode::Stretch), node(), UiTransform::default()));
            }
        });
}

#[allow(clippy::type_complexity)]
fn choke_gauge(
    window: Query<&Window, With<PrimaryWindow>>,
    choking: Query<&crate::combat::Choking>,
    mut gauge: Query<(&mut Node, &mut Visibility, &Children), With<ChokeGauge>>,
    mut parts: Query<(&Part, &mut Node, Option<&MaterialNode<RadialFill>>, Option<&mut UiTransform>, Has<Pointer>), (Without<Gauge>, Without<ChokeGauge>)>,
    mut mats: ResMut<Assets<RadialFill>>,
) {
    let Ok((mut node, mut vis, children)) = gauge.single_mut() else { return };
    let Ok(w) = window.single() else { return };
    let Some(share) = choking.iter().next().map(|c| (c.t / c.duration.max(0.01)).clamp(0.0, 1.0)) else {
        *vis = Visibility::Hidden;
        return;
    };
    *vis = Visibility::Inherited;
    let size = Vec2::new(w.width(), w.height());
    let s = (size.x / 1280.0).min(size.y / 720.0).max(0.01);
    let off = (size - Vec2::new(1280.0, 720.0) * s) * 0.5;
    let p = off + Vec2::new(640.0, 360.0) * s;
    node.left = Val::Px(p.x);
    node.top = Val::Px(p.y);
    for c in children.iter() {
        let Ok((part, mut n, mat, tf, pointer)) = parts.get_mut(c) else { continue };
        lay_out(part, &mut n, mat, tf, pointer, share, 1.0, s, 33.0, &mut mats);
    }
}

/// A gauge part on the screen (`s` pixels per stage unit), the fill and the pointer at `share`.
#[allow(clippy::too_many_arguments)]
fn lay_out(part: &Part, n: &mut Node, mat: Option<&MaterialNode<RadialFill>>, tf: Option<Mut<UiTransform>>, pointer: bool, share: f32, alpha: f32, s: f32, radius: f32, mats: &mut Assets<RadialFill>) {
    let tl = (part.0 - part.1 * 0.5) * s;
    n.left = Val::Px(tl.x);
    n.top = Val::Px(tl.y);
    n.width = Val::Px(part.1.x * s);
    n.height = Val::Px(part.1.y * s);
    if let Some(mut m) = mat.and_then(|m| mats.get_mut(&m.0)) {
        m.params = Vec4::new(share, START, alpha, 0.0);
    }
    // the pointer rides the fill's edge, on the ring
    if let (true, Some(mut tf)) = (pointer, tf) {
        let turn = (START + share) * std::f32::consts::TAU;
        let at = Vec2::new(turn.sin(), -turn.cos()) * radius * s;
        n.left = Val::Px(at.x - part.1.x * 0.5 * s);
        n.top = Val::Px(at.y - part.1.y * 0.5 * s);
        tf.rotation = Rot2::radians(turn);
    }
}

/// The button held through a scene: at the tweak's time it is skipped (let go before the
/// next).
fn skip_hold(
    time: Res<Time<Real>>,
    keys: Res<ButtonInput<KeyCode>>,
    bind: Res<crate::bindings::Bindings>,
    st: Res<crate::matinee::MatineeState>,
    vm: Option<ResMut<crate::kismet::Vm>>,
    mut hold: ResMut<SkipHold>,
) {
    let held = keys.pressed(bind.key(crate::bindings::Act::Use)) || keys.pressed(KeyCode::Enter);
    if !held {
        hold.t = 0.0;
        hold.spent = false;
        return;
    }
    if st.camera.is_none() || hold.spent {
        hold.t = 0.0;
        return;
    }
    hold.t += time.delta_secs();
    if hold.t >= HOLD {
        hold.t = 0.0;
        hold.spent = true;
        if let Some(mut vm) = vm {
            vm.skip_cinematics();
        }
    }
}

#[allow(clippy::type_complexity)]
fn show_gauge(
    time: Res<Time<Real>>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut hold: ResMut<SkipHold>,
    mut gauge: Query<(&mut Node, &mut Visibility, &Children), With<Gauge>>,
    mut parts: Query<(&Part, &mut Node, Option<&MaterialNode<RadialFill>>, Option<&mut UiTransform>, Has<Pointer>), (Without<Gauge>, Without<ChokeGauge>)>,
    mut mats: ResMut<Assets<RadialFill>>,
) {
    let Ok((mut node, mut vis, children)) = gauge.single_mut() else { return };
    let Ok(w) = window.single() else { return };
    let share = (hold.t / HOLD).clamp(0.0, 1.0);
    // up while held, fading once let go
    hold.shown = if hold.t > 0.0 { 1.0 } else { (hold.shown - time.delta_secs() * 4.0).max(0.0) };
    if hold.shown <= 0.0 {
        *vis = Visibility::Hidden;
        return;
    }
    *vis = Visibility::Inherited;
    let size = Vec2::new(w.width(), w.height());
    let s = (size.x / 1280.0).min(size.y / 720.0).max(0.01);
    let off = (size - Vec2::new(1280.0, 720.0) * s) * 0.5;
    let p = off + AT * s;
    node.left = Val::Px(p.x);
    node.top = Val::Px(p.y);
    for c in children.iter() {
        let Ok((part, mut n, mat, tf, pointer)) = parts.get_mut(c) else { continue };
        lay_out(part, &mut n, mat, tf, pointer, share, hold.shown, s, 27.0, &mut mats);
    }
}
