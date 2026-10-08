//! The HUD's top left as the original draws it (the HUD movie's `masterHUD_mc`, class
//! `PlayerGauges`): the left hand's power or weapon in its ring over ink splashes, with the
//! health and mana shards beside it (tilted 15 and 30 degrees). Each shard fills from its
//! base to the amount (`mask_mc` scaled, tweened 0.45 s), a pointer marks the level, and a
//! glow pulses behind it (15-40% over 2.25 s; 35-75% over 0.55 s once low: under the HUD
//! tweak's `m_LowHealthThreshold` 30%, `m_LowManaThreshold` 19%). Weapons and gadgets show
//! their ammunition beside the icon.

use crate::gameplay::PlayerStats;
use crate::powers::Powers;
use crate::GameState;
use bevy::asset::embedded_asset;
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;
use bevy::ui::widget::NodeImageMode;
use bevy::window::PrimaryWindow;

pub struct GaugesPlugin;

impl Plugin for GaugesPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "bar.wgsl");
        app.add_plugins(UiMaterialPlugin::<BarFill>::default())
            .init_resource::<Meters>()
            .add_systems(OnEnter(GameState::InGame), spawn_gauges)
            .add_systems(Update, update_gauges.run_if(in_state(GameState::InGame)));
    }
}

/// A meter's fill, shown from `level` (uv) down.
#[derive(AsBindGroup, Asset, TypePath, Clone, Debug)]
pub struct BarFill {
    /// (level, alpha, -, -)
    #[uniform(0)]
    params: Vec4,
    #[texture(1)]
    #[sampler(2)]
    texture: Handle<Image>,
}

impl UiMaterial for BarFill {
    fn fragment_shader() -> ShaderRef {
        "embedded://dishonored/bar.wgsl".into()
    }
}

/// `masterHUD_mc` on the 1280x720 stage.
const MASTER: Vec2 = Vec2::new(95.5, 54.0);
const FILL_TIME: f32 = 0.45;
pub(crate) const LOW_HEALTH: f32 = 0.30;
pub(crate) const LOW_MANA: f32 = 0.19;

/// A meter: where it stands (from `masterHUD_mc`), its turn, its bitmaps (glow, back, fill,
/// outline, pointer) with their places (top-left and size, the meter's own frame, up is -y),
/// the fill mask's height, the pointer's offset.
struct MeterDef {
    at: Vec2,
    turn: f32,
    glow: (u32, Vec2, Vec2),
    back: (u32, Vec2, Vec2),
    fill: (u32, Vec2, Vec2),
    outline: (u32, Vec2, Vec2),
    pointer: (u32, Vec2, Vec2),
    mask: f32,
}

const HEALTH: MeterDef = MeterDef {
    at: Vec2::new(0.1, 213.9),
    turn: 15.0,
    glow: (633, Vec2::new(-59.9, -242.9), Vec2::new(131.0, 281.0)),
    back: (636, Vec2::new(-18.95, -241.75), Vec2::new(46.0, 242.0)),
    fill: (641, Vec2::new(-18.95, -241.75), Vec2::new(46.0, 242.0)),
    outline: (660, Vec2::new(-18.95, -241.75), Vec2::new(46.0, 242.0)),
    pointer: (657, Vec2::new(6.05 - 25.1, -11.0), Vec2::new(46.0, 22.0)),
    mask: 195.0 * 1.036,
};

const MANA: MeterDef = MeterDef {
    at: Vec2::new(-1.2, 200.05),
    turn: 30.4,
    glow: (675, Vec2::new(-24.55, -194.7), Vec2::new(92.0, 216.0)),
    back: (678, Vec2::new(0.0, -199.0), Vec2::new(45.0, 201.0)),
    fill: (681, Vec2::new(0.0, -199.0), Vec2::new(46.0, 201.0)),
    outline: (689, Vec2::new(0.0, -199.0), Vec2::new(46.0, 201.0)),
    pointer: (691, Vec2::new(4.95, -6.5), Vec2::new(27.0, 13.0)),
    mask: 195.0 * 0.846,
};

/// The ring's splashes (`_bkgd_mc`): bitmap, middle, size, turn — and where the icon sits.
const BACKGROUND: [(u32, Vec2, Vec2, f32); 4] = [
    (623, Vec2::new(133.55, 122.9), Vec2::new(225.0, 236.0), 0.0),
    (625, Vec2::new(138.4, 174.9), Vec2::new(169.0, 172.0), 0.0),
    (627, Vec2::new(172.4, 207.4), Vec2::new(98.0, 98.0), 0.0),
    (629, Vec2::new(181.45, 176.1), Vec2::new(22.0, 153.0), -45.0),
];
/// `_equipment_mc` (the `itemIcons` symbol, centred, at 65%): the icon, its `quantity` below
const ICON_AT: Vec2 = Vec2::new(196.75, 221.7);
const ICON_SIZE: f32 = 65.0;

/// The meters' shown levels (tweened) and their glows' phases.
#[derive(Resource, Default)]
struct Meters {
    shown: [f32; 2],
    from: [f32; 2],
    to: [f32; 2],
    t: [f32; 2],
    glow: [f32; 2],
    icon: Option<crate::powers::Power>,
}

#[derive(Component)]
struct Piece {
    /// top-left and size on the stage, or in its meter's frame
    min: Vec2,
    size: Vec2,
    turn: f32,
    stage: bool,
}
#[derive(Component)]
struct Meter(usize);
#[derive(Component)]
struct Glow(usize);
#[derive(Component)]
struct Fill(usize);
#[derive(Component)]
struct Pointer(usize);
#[derive(Component)]
struct Icon;
#[derive(Component)]
struct Ammo;
#[derive(Component)]
pub struct GaugesRoot;

fn spawn_gauges(mut commands: Commands, mut ui: ResMut<crate::ui_images::UiImages>, mut images: ResMut<Assets<Image>>, mut mats: ResMut<Assets<BarFill>>, mut meters: ResMut<Meters>) {
    *meters = Meters::default();
    let mut img = |id: u32| ui.get(&mut images, "HUD", id).map(|(h, _)| h);
    let node = || Node { position_type: PositionType::Absolute, ..default() };
    let piece = |min: Vec2, size: Vec2, turn: f32| Piece { min, size, turn, stage: true };
    let local = |min: Vec2, size: Vec2| Piece { min, size, turn: 0.0, stage: false };
    let root = commands.spawn((GaugesRoot, Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() }, GlobalZIndex(2), Pickable::IGNORE, DespawnOnExit(GameState::InGame))).id();
    commands.entity(root).with_children(|r| {
        // the ring and the left hand's icon (`DHE_Equipment`)
        r.spawn((node(), crate::hud::HudPart("DHE_Equipment"))).with_children(|c| {
            for (id, mid, size, turn) in BACKGROUND {
                if let Some(h) = img(id) {
                    c.spawn((piece(mid - size * 0.5, size, turn), ImageNode::new(h).with_mode(NodeImageMode::Stretch), node(), UiTransform::default()));
                }
            }
            c.spawn((Icon, piece(ICON_AT - Vec2::splat(ICON_SIZE * 0.5), Vec2::splat(ICON_SIZE), 0.0), ImageNode::default().with_mode(NodeImageMode::Stretch), node(), UiTransform::default(), Visibility::Hidden));
            c.spawn((
                Ammo,
                piece(ICON_AT + Vec2::new(-34.0, 20.5), Vec2::new(68.0, 22.0), 0.0),
                Text::new(""),
                TextFont { font_size: FontSize::Px(18.0), ..default() },
                TextColor(Color::srgb(0.92, 0.9, 0.82)),
                TextLayout::justify(Justify::Center),
                node(),
                UiTransform::default(),
            ));
        });
        // the shards
        for (k, def) in [&HEALTH, &MANA].into_iter().enumerate() {
            let part = if k == 0 { "DHE_Health" } else { "DHE_Mana" };
            r.spawn((Meter(k), piece(MASTER + def.at, Vec2::ZERO, def.turn), node(), UiTransform::default(), crate::hud::HudPart(part))).with_children(|m| {
                if let Some(h) = img(def.glow.0) {
                    m.spawn((Glow(k), local(def.glow.1, def.glow.2), ImageNode::new(h).with_mode(NodeImageMode::Stretch), node(), UiTransform::default()));
                }
                if let Some(h) = img(def.back.0) {
                    m.spawn((local(def.back.1, def.back.2), ImageNode::new(h).with_mode(NodeImageMode::Stretch), node(), UiTransform::default()));
                }
                if let Some(h) = img(def.fill.0) {
                    m.spawn((Fill(k), local(def.fill.1, def.fill.2), MaterialNode(mats.add(BarFill { params: Vec4::new(0.0, 1.0, 0.0, 0.0), texture: h })), node(), UiTransform::default()));
                }
                if let Some(h) = img(def.outline.0) {
                    m.spawn((local(def.outline.1, def.outline.2), ImageNode::new(h).with_mode(NodeImageMode::Stretch), node(), UiTransform::default()));
                }
                if let Some(h) = img(def.pointer.0) {
                    m.spawn((Pointer(k), local(def.pointer.1, def.pointer.2), ImageNode::new(h).with_mode(NodeImageMode::Stretch), node(), UiTransform::default()));
                }
            });
        }
    });
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn update_gauges(
    time: Res<Time<Real>>,
    window: Query<&Window, With<PrimaryWindow>>,
    stats: Res<PlayerStats>,
    powers: Res<Powers>,
    mut meters: ResMut<Meters>,
    mut ui: ResMut<crate::ui_images::UiImages>,
    mut images: ResMut<Assets<Image>>,
    mut mats: ResMut<Assets<BarFill>>,
    mut pieces: Query<(&Piece, &mut Node, &mut UiTransform, Option<&Meter>, Option<&Glow>, Option<&Fill>, Option<&Pointer>, Option<&mut ImageNode>, Option<&MaterialNode<BarFill>>, Has<Icon>)>,
    mut ammo: Query<&mut Text, With<Ammo>>,
    mut icon_vis: Query<&mut Visibility, With<Icon>>,
) {
    let dt = time.delta_secs();
    let Ok(w) = window.single() else { return };
    let size = Vec2::new(w.width(), w.height());
    let s = (size.x / 1280.0).min(size.y / 720.0).max(0.01);
    let off = (size - Vec2::new(1280.0, 720.0) * s) * 0.5;
    // the levels, tweened as the movie does (`_meterFillDuration`)
    let levels = [
        (stats.health / stats.max_health.max(1.0)).clamp(0.0, 1.0),
        (stats.mana / stats.max_mana.max(1.0)).clamp(0.0, 1.0),
    ];
    let m = &mut *meters;
    for k in 0..2 {
        if (levels[k] - m.to[k]).abs() > 1e-4 {
            m.from[k] = m.shown[k];
            m.to[k] = levels[k];
            m.t[k] = 0.0;
        }
        m.t[k] = (m.t[k] + dt / FILL_TIME).min(1.0);
        m.shown[k] = m.from[k] + (m.to[k] - m.from[k]) * (1.0 - (1.0 - m.t[k]).powi(5));
        m.glow[k] += dt;
    }
    // the glows: back and forth between their alphas (faster and brighter once low)
    let glow_alpha = |k: usize, t: f32, level: f32| {
        let low = level < if k == 0 { LOW_HEALTH } else { LOW_MANA };
        let (secs, lo, hi) = if low { (0.55, 0.35, 0.75) } else { (2.25, 0.15, 0.40) };
        let phase = (t / secs) % 2.0;
        let x = if phase < 1.0 { phase } else { 2.0 - phase };
        lo + (hi - lo) * (1.0 - (1.0 - x).powi(5))
    };
    // the icon of what the left hand holds, and its ammunition
    if m.icon != Some(powers.selected) {
        m.icon = Some(powers.selected);
        let h = ui.file(&mut images, "icons", powers.selected.icon()).map(|(h, _)| h);
        for (_, _, _, _, _, _, _, im, _, is_icon) in &mut pieces {
            if let (true, Some(mut im)) = (is_icon, im) {
                if let Some(h) = &h {
                    im.image = h.clone();
                }
            }
        }
        for mut v in &mut icon_vis {
            *v = if h.is_some() { Visibility::Inherited } else { Visibility::Hidden };
        }
    }
    if let Ok(mut t) = ammo.single_mut() {
        let a = powers.selected.ammo(&stats).map(|n| n.to_string()).unwrap_or_default();
        if t.0 != a {
            t.0 = a;
        }
    }
    for (p, mut n, mut tf, _, glow, fill, pointer, im, mat, _) in &mut pieces {
        let mut min = p.min;
        if let Some(Pointer(k)) = pointer {
            let def = if *k == 0 { &HEALTH } else { &MANA };
            min.y += -def.mask * m.shown[*k];
        }
        // stage pieces (the ring's, the meters themselves) stand on the stage; a meter's own
        // pieces in its frame
        let at = if p.stage { off + min * s } else { min * s };
        n.left = Val::Px(at.x);
        n.top = Val::Px(at.y);
        n.width = Val::Px(p.size.x * s);
        n.height = Val::Px(p.size.y * s);
        if p.turn != 0.0 {
            tf.rotation = Rot2::degrees(p.turn);
        }
        if let (Some(Glow(k)), Some(mut im)) = (glow, im) {
            im.color.set_alpha(glow_alpha(*k, m.glow[*k], m.shown[*k]));
        }
        if let (Some(Fill(k)), Some(mm)) = (fill, mat) {
            if let Some(mut f) = mats.get_mut(&mm.0) {
                let def = if *k == 0 { &HEALTH } else { &MANA };
                // the mask shows from the base (y 0) up to its height times the level
                let top = -def.mask * m.shown[*k];
                f.params.x = ((top - def.fill.1.y) / def.fill.2.y).clamp(0.0, 1.0);
            }
        }
    }
}
