//! The HUD's top left as the original draws it (the HUD movie's `masterHUD_mc`, class
//! `PlayerGauges`): the left hand's power or weapon in its ring over ink splashes, with the
//! health and mana shards beside it (tilted 15 and 30 degrees). Each shard fills from its
//! base to the amount (`mask_mc` scaled, tweened 0.45 s), a pointer marks the level, and a
//! glow pulses behind it (15-40% over 2.25 s; 35-75% over 0.55 s once low: under the HUD
//! tweak's `m_LowHealthThreshold` 30%, `m_LowManaThreshold` 19%). Weapons and gadgets show
//! their ammunition beside the icon.
//!
//! As roshan's r614/blink research read the movie's `PlayerGauges` and its native caller: what
//! will come back by itself shows beyond the amount at half brightness (`maskRegen_mc`: health to
//! its regeneration's limit, mana to the cap its last spending left); a shard raised by a whale
//! bone charm or by Vitality grows (its `upgd2`, `upgd4`, `upgd6` frames: a piece more at its tip,
//! its masks taller); and a drink plays the shard's `refill_mc`.

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

/// A meter's fill, shown from `level` (uv) down (slanting by `slope`), its colour times
/// `bright`.
#[derive(AsBindGroup, Asset, TypePath, Clone, Debug)]
pub struct BarFill {
    /// (level, alpha, slope, bright)
    #[uniform(0)]
    params: Vec4,
    #[texture(1)]
    #[sampler(2)]
    texture: Handle<Image>,
    /// a colour added (rgb)
    #[uniform(3)]
    add: Vec4,
}

impl UiMaterial for BarFill {
    fn fragment_shader() -> ShaderRef {
        "embedded://dishonored/bar.wgsl".into()
    }
}

/// `masterHUD_mc` on the 1280x720 stage, as the movie places it.
const MASTER: Vec2 = crate::hud::MASTER_AT;
const FILL_TIME: f32 = 0.45;
pub(crate) const LOW_HEALTH: f32 = 0.30;
pub(crate) const LOW_MANA: f32 = 0.19;

/// A meter: where it stands (from `masterHUD_mc`), its turn, its bitmaps (glow, back, fill,
/// outline, pointer) with their places (top-left and size, the meter's own frame, up is -y),
/// the pointer's offset.
struct MeterDef {
    at: Vec2,
    turn: f32,
    glow: (u32, Vec2, Vec2),
    back: (u32, Vec2, Vec2),
    fill: (u32, Vec2, Vec2),
    outline: (u32, Vec2, Vec2),
    pointer: (u32, Vec2, Vec2),
}

const HEALTH: MeterDef = MeterDef {
    at: Vec2::new(0.1, 213.9),
    turn: 15.0,
    glow: (633, Vec2::new(-59.9, -242.9), Vec2::new(131.0, 281.0)),
    back: (636, Vec2::new(-18.95, -241.75), Vec2::new(46.0, 242.0)),
    fill: (641, Vec2::new(-18.95, -241.75), Vec2::new(46.0, 242.0)),
    outline: (660, Vec2::new(-18.95, -241.75), Vec2::new(46.0, 242.0)),
    pointer: (657, Vec2::new(6.05 - 25.1, -11.0), Vec2::new(46.0, 22.0)),
};

const MANA: MeterDef = MeterDef {
    at: Vec2::new(-1.2, 200.05),
    turn: 30.4,
    glow: (675, Vec2::new(-24.55, -194.7), Vec2::new(92.0, 216.0)),
    back: (678, Vec2::new(0.0, -199.0), Vec2::new(45.0, 201.0)),
    fill: (681, Vec2::new(0.0, -199.0), Vec2::new(46.0, 201.0)),
    outline: (689, Vec2::new(0.0, -199.0), Vec2::new(46.0, 201.0)),
    pointer: (691, Vec2::new(4.95, -6.5), Vec2::new(27.0, 13.0)),
};

/// The masks' height (`mask_mc`, `maskRegen_mc`: their shape 195 tall, scaled by the meter's
/// frame: default, `upgd2`, `upgd4`, `upgd6`; the mana's has its default and `upgd2` alone).
const MASK_H: f32 = 195.0;
const HEALTH_MASK: [f32; 4] = [1.035_904, 1.133_331, 1.138_458, 1.225_647];
const MANA_MASK: [f32; 2] = [0.846_161, 1.0];

/// A piece more at a raised shard's tip (its frame's): the frame it shows in (2 `upgd2`, 4
/// `upgd4`, 6 `upgd6`), its back's, fill's and outline's bitmaps, their size, where the back and
/// the fills stand and where the outline does (Flash matrices in the meter's frame), and how
/// bright its regeneration layer is (the `upgd4` one's has no half brightness, as the movie has
/// it).
struct Ext {
    frame: u8,
    bitmaps: [u32; 3],
    size: Vec2,
    m: [f32; 6],
    outline: [f32; 6],
    regen: f32,
}

const HEALTH_EXT: [Ext; 4] = [
    Ext { frame: 2, bitmaps: [662, 664, 666], size: Vec2::new(43.0, 31.0), m: [0.924_408, 0.381_348, -0.381_348, 0.924_408, -15.15, -228.15], outline: [0.924_408, 0.381_348, -0.381_348, 0.924_408, -14.8, -229.0], regen: 0.5 },
    Ext { frame: 4, bitmaps: [668, 670, 672], size: Vec2::new(43.0, 44.0), m: [0.990_952, 0.134_216, -0.134_216, 0.990_952, -16.2, -230.9], outline: [0.990_952, 0.134_216, -0.134_216, 0.990_952, -16.2, -230.9], regen: 1.0 },
    Ext { frame: 6, bitmaps: [668, 670, 672], size: Vec2::new(43.0, 44.0), m: [0.990_952, 0.134_216, -0.134_216, 0.990_952, -15.3, -231.0], outline: [0.990_952, 0.134_216, -0.134_216, 0.990_952, -15.3, -231.0], regen: 1.0 },
    Ext { frame: 6, bitmaps: [662, 664, 666], size: Vec2::new(43.0, 31.0), m: [0.995_331, -0.096_237, 0.096_237, 0.995_331, -14.0, -242.6], outline: [0.995_331, -0.096_237, 0.096_237, 0.995_331, -14.1, -243.55], regen: 0.5 },
];
const MANA_EXT: [Ext; 1] = [Ext { frame: 2, bitmaps: [694, 696, 698], size: Vec2::new(27.0, 55.0), m: [1.0, 0.0, 0.0, 1.0, 10.4, -198.6], outline: [1.0, 0.0, 0.0, 1.0, 10.4, -198.6], regen: 0.5 }];

/// A drink's flash over its shard (`refill_mc` "anim", at the movie's 30 frames a second): its
/// keys (frame, colour times, colour added 0-255, alpha), eased straight between. The health's
/// shard goes white twice and its flash fades (its veined glow's colour transforms); the mana's
/// glows blue-white twice (its glint's). The movie masks the flash to the shard's outline (a
/// vector shape); here it is the shard's own fill, whose alpha is that outline, and the
/// glints that cross it are left out.
const FLASH_RATE: f32 = 30.0;
const FLASH_KEYS: [&[(f32, f32, [f32; 3], f32)]; 2] = [
    &[
        (1.0, 1.0, [0.0; 3], 0.0),
        (2.0, 0.75, [57.0, 61.0, 54.0], 1.0),
        (5.0, 0.0, [227.0, 242.0, 214.0], 1.0),
        (6.0, 0.0, [227.0, 242.0, 214.0], 1.0),
        (12.0, 1.0, [0.0; 3], 1.0),
        (15.0, 1.0, [0.0; 3], 1.0),
        (19.0, 0.0, [227.0, 242.0, 214.0], 1.0),
        (20.0, 0.0, [227.0, 242.0, 214.0], 1.0),
        (25.0, 1.0, [0.0; 3], 1.0),
        (28.0, 1.0, [0.0; 3], 1.0),
        (33.0, 1.0, [0.0; 3], 0.0),
    ],
    &[
        (2.0, 0.0, [91.0, 164.0, 245.0], 0.0),
        (7.0, 0.0, [186.0, 214.0, 245.0], 1.0),
        (12.0, 0.0, [186.0, 214.0, 245.0], 1.0),
        (13.0, 0.0, [91.0, 164.0, 245.0], 0.0),
        (18.0, 0.0, [186.0, 214.0, 245.0], 1.0),
        (26.0, 0.0, [186.0, 214.0, 245.0], 1.0),
        (34.0, 0.0, [91.0, 164.0, 245.0], 0.0),
    ],
];

/// The flash at `t` s into it: (colour times, colour added 0-1, alpha); `None` once over.
fn flash_at(k: usize, t: f32) -> Option<(f32, Vec3, f32)> {
    let keys = FLASH_KEYS[k];
    let f = 1.0 + t * FLASH_RATE;
    let last = keys.last()?;
    if f >= last.0 {
        return None;
    }
    let i = keys.iter().rposition(|key| key.0 <= f).unwrap_or(0);
    let (a, b) = (keys[i], keys[(i + 1).min(keys.len() - 1)]);
    let u = if b.0 > a.0 { ((f - a.0) / (b.0 - a.0)).clamp(0.0, 1.0) } else { 0.0 };
    let add = Vec3::from(a.2).lerp(Vec3::from(b.2), u) / 255.0;
    Some((a.1 + (b.1 - a.1) * u, add, a.3 + (b.3 - a.3) * u))
}

/// A piece placed by a Flash matrix (pure turn) in a meter's frame: its top-left as unturned,
/// and its turn about its middle (degrees).
fn turned(m: [f32; 6], size: Vec2) -> (Vec2, f32) {
    let mid = Vec2::new(m[0] * size.x * 0.5 + m[2] * size.y * 0.5 + m[4], m[1] * size.x * 0.5 + m[3] * size.y * 0.5 + m[5]);
    (mid - size * 0.5, m[1].atan2(m[0]).to_degrees())
}

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
    /// health, mana, and what each will come back to by itself
    shown: [f32; 4],
    from: [f32; 4],
    to: [f32; 4],
    t: [f32; 4],
    glow: [f32; 2],
    icon: Option<crate::powers::Power>,
    /// the elixirs held (to see one drunk), and how far into its flash each shard is
    elixirs: Option<[u32; 2]>,
    flash: [Option<f32>; 2],
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
/// A fill (the amount's, or `regen` what will come back), placed by its Flash matrix, its size.
#[derive(Component)]
struct Fill {
    k: usize,
    regen: bool,
    m: [f32; 6],
    size: Vec2,
}
/// A piece shown in a meter's frame alone (`Ext::frame`).
#[derive(Component)]
struct Upgrade(usize, u8);
/// A shard's flash as an elixir is drunk.
#[derive(Component)]
struct Flash(usize);
#[derive(Component)]
struct Pointer(usize);
#[derive(Component)]
struct Icon;
#[derive(Component)]
struct Ammo;
#[derive(Component)]
pub struct GaugesRoot;

fn spawn_gauges(
    mut commands: Commands,
    mut ui: ResMut<crate::ui_images::UiImages>,
    mut images: ResMut<Assets<Image>>,
    mut mats: ResMut<Assets<BarFill>>,
    mut meters: ResMut<Meters>,
) {
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
                // the back, what will come back (half bright), the amount, the outline: each
                // with its raised shard's pieces
                let exts: &[Ext] = if k == 0 { &HEALTH_EXT } else { &MANA_EXT };
                let ext_piece = |e: &Ext, m: [f32; 6]| {
                    let (min, turn) = turned(m, e.size);
                    Piece { min, size: e.size, turn, stage: false }
                };
                let fill_m = [1.0, 0.0, 0.0, 1.0, def.fill.1.x, def.fill.1.y];
                if let Some(h) = img(def.back.0) {
                    m.spawn((local(def.back.1, def.back.2), ImageNode::new(h).with_mode(NodeImageMode::Stretch), node(), UiTransform::default()));
                }
                for e in exts {
                    if let Some(h) = img(e.bitmaps[0]) {
                        m.spawn((Upgrade(k, e.frame), ext_piece(e, e.m), ImageNode::new(h).with_mode(NodeImageMode::Stretch), node(), UiTransform::default(), Visibility::Hidden));
                    }
                }
                for regen in [true, false] {
                    let bright = if regen { 0.5 } else { 1.0 };
                    if let Some(h) = img(def.fill.0) {
                        m.spawn((Fill { k, regen, m: fill_m, size: def.fill.2 }, local(def.fill.1, def.fill.2), MaterialNode(mats.add(BarFill { params: Vec4::new(0.0, 1.0, 0.0, bright), texture: h, add: Vec4::ZERO })), node(), UiTransform::default()));
                    }
                    for e in exts {
                        if let Some(h) = img(e.bitmaps[1]) {
                            let bright = if regen { e.regen } else { 1.0 };
                            m.spawn((Fill { k, regen, m: e.m, size: e.size }, Upgrade(k, e.frame), ext_piece(e, e.m), MaterialNode(mats.add(BarFill { params: Vec4::new(0.0, 1.0, 0.0, bright), texture: h, add: Vec4::ZERO })), node(), UiTransform::default(), Visibility::Hidden));
                        }
                    }
                }
                // (a drink's flash, over the whole shard)
                if let Some(h) = img(def.fill.0) {
                    m.spawn((Flash(k), local(def.fill.1, def.fill.2), MaterialNode(mats.add(BarFill { params: Vec4::new(-1.0, 0.0, 0.0, 1.0), texture: h, add: Vec4::ZERO })), node(), UiTransform::default(), Visibility::Hidden));
                }
                if let Some(h) = img(def.outline.0) {
                    m.spawn((local(def.outline.1, def.outline.2), ImageNode::new(h).with_mode(NodeImageMode::Stretch), node(), UiTransform::default()));
                }
                for e in exts {
                    if let Some(h) = img(e.bitmaps[2]) {
                        m.spawn((Upgrade(k, e.frame), ext_piece(e, e.outline), ImageNode::new(h).with_mode(NodeImageMode::Stretch), node(), UiTransform::default(), Visibility::Hidden));
                    }
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
    data: Res<crate::gamedata::Data>,
    mut meters: ResMut<Meters>,
    mut ui: ResMut<crate::ui_images::UiImages>,
    mut images: ResMut<Assets<Image>>,
    mut mats: ResMut<Assets<BarFill>>,
    mut pieces: Query<(&Piece, &mut Node, &mut UiTransform, Option<&Meter>, Option<&Glow>, Option<&Fill>, Option<&Pointer>, Option<&mut ImageNode>, Option<&MaterialNode<BarFill>>, Has<Icon>)>,
    mut ammo: Query<&mut Text, With<Ammo>>,
    mut icon_vis: Query<&mut Visibility, (With<Icon>, Without<Upgrade>)>,
    attrs: Res<crate::gamedata::Attrs>,
    mut upgrades: Query<(&Upgrade, &mut Visibility), Without<Icon>>,
    mut flashes: Query<(&Flash, &MaterialNode<BarFill>, &mut Visibility), (Without<Icon>, Without<Upgrade>)>,
) {
    let dt = time.delta_secs();
    let Ok(w) = window.single() else { return };
    // (`masterHUD_mc` moved to the safe area's top left: `hud::MovieSpace`)
    let ms = crate::hud::MovieSpace::of(w);
    let (s, off) = (ms.s, ms.px(ms.master_shift()));
    // the levels, tweened as the movie does (`_meterFillDuration`)
    // (what will come back by itself: health to its regeneration's limit, mana to its cap)
    let limit = attrs.health_regen_limit.min(stats.max_health);
    let regen = [if stats.health < limit { limit } else { stats.health }, stats.mana.max(stats.mana_cap)];
    let levels = [
        (stats.health / stats.max_health.max(1.0)).clamp(0.0, 1.0),
        (stats.mana / stats.max_mana.max(1.0)).clamp(0.0, 1.0),
        (regen[0] / stats.max_health.max(1.0)).clamp(0.0, 1.0),
        (regen[1] / stats.max_mana.max(1.0)).clamp(0.0, 1.0),
    ];
    // each shard's frame: raised by a whale bone charm (`upgd2`), by Vitality (`upgd4`), or both
    // (`upgd6`); the mana's by its charm alone
    let (hc, hp) = data.raised_by("HealthMax", &stats.powers, &stats.charms);
    let (mc, _) = data.raised_by("ManaMax", &stats.powers, &stats.charms);
    let frames = [hc as u8 * 2 + hp as u8 * 4, mc as u8 * 2];
    let mask_h = [MASK_H * HEALTH_MASK[(frames[0] / 2) as usize], MASK_H * MANA_MASK[(frames[1] / 2) as usize]];
    for (u, mut v) in &mut upgrades {
        let want = if frames[u.0] == u.1 { Visibility::Inherited } else { Visibility::Hidden };
        if *v != want {
            *v = want;
        }
    }
    let m = &mut *meters;
    // a drink: its shard's flash (`refill_mc` "anim")
    let held = [stats.health_elixirs, stats.mana_elixirs];
    for k in 0..2 {
        if m.elixirs.is_some_and(|was| held[k] < was[k]) {
            m.flash[k] = Some(0.0);
        }
        m.flash[k] = m.flash[k].map(|t| t + dt).filter(|t| flash_at(k, *t).is_some());
    }
    m.elixirs = Some(held);
    for (f, mm, mut v) in &mut flashes {
        let now = m.flash[f.0].and_then(|t| flash_at(f.0, t));
        let want = if now.is_some() { Visibility::Inherited } else { Visibility::Hidden };
        if *v != want {
            *v = want;
        }
        if let (Some((mult, add, alpha)), Some(mut mat)) = (now, mats.get_mut(&mm.0)) {
            mat.params = Vec4::new(-1.0, alpha, 0.0, mult);
            mat.add = add.extend(0.0);
        }
    }
    for k in 0..4 {
        if (levels[k] - m.to[k]).abs() > 1e-4 {
            m.from[k] = m.shown[k];
            m.to[k] = levels[k];
            m.t[k] = 0.0;
        }
        m.t[k] = (m.t[k] + dt / FILL_TIME).min(1.0);
        m.shown[k] = m.from[k] + (m.to[k] - m.from[k]) * (1.0 - (1.0 - m.t[k]).powi(5));
        if k < 2 {
            m.glow[k] += dt;
        }
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
        let h = ui.file(&mut images, "icons", powers.selected.icon_for(data.is_daud())).map(|(h, _)| h);
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
            min.y += -mask_h[*k] * m.shown[*k];
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
        if let (Some(f), Some(mm)) = (fill, mat) {
            if let Some(mut mat) = mats.get_mut(&mm.0) {
                // the mask shows from the base (y 0) up to its height times the level: in the
                // piece's own uv, from the line where its frame's y is the mask's top
                let top = mask_h[f.k] * m.shown[f.k + if f.regen { 2 } else { 0 }];
                let (b, d, ty) = (f.m[1], f.m[3], f.m[5]);
                let dh = (d * f.size.y).abs().max(1e-3);
                mat.params.x = (-top - ty) / dh;
                mat.params.z = -b * f.size.x / dh;
            }
        }
    }
}
