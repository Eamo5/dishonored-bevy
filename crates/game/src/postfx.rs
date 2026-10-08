//! The level's Arkane post-processing (`scene.post`) as the original renders it: a colour
//! LUT baked by the game's own `FArkPpDofLutBlenderPS` (its D3D9 bytecode evaluated on the
//! CPU with the level's exposure, colour balance, gamma and contrast), applied to the linear
//! scene colour with the addressing of `FArkPpDofUber` in a full-screen pass
//! (`ark_grade.wgsl`), plus bloom and film grain. `DH_NO_POST=1` turns it off.

use crate::level::LevelInfo;
use crate::player::PlayerCamera;
use crate::GameState;
use bevy::asset::embedded_asset;
use bevy::core_pipeline::fullscreen_material::{FullscreenMaterial, FullscreenMaterialPlugin};
use bevy::core_pipeline::tonemapping::{tonemapping, Tonemapping};
use bevy::core_pipeline::Core3dSystems;
use bevy::ecs::schedule::ScheduleConfigs;
use bevy::ecs::system::BoxedSystem;
use bevy::prelude::*;
use bevy::render::extract_component::ExtractComponent;
use bevy::render::render_resource::ShaderType;
use bevy::shader::ShaderRef;
use dhcook::format::PostProcess;
use std::collections::BTreeMap;

pub struct PostFxPlugin;

impl Plugin for PostFxPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "ark_grade.wgsl");
        app.add_plugins(FullscreenMaterialPlugin::<ArkGrade>::default())
            .init_resource::<PostOverride>()
            .init_resource::<ScriptPost>()
            .init_resource::<PowerPost>()
            .init_resource::<MenuBlur>()
            .add_systems(Update, (menu_blur, attach_grade, rebake_on_brightness, animate_grade).chain().run_if(in_state(GameState::InGame)));
    }
}

pub const LUT_ENTRIES: usize = 1024;

#[derive(Component, ExtractComponent, Clone, Copy, ShaderType)]
pub struct ArkGrade {
    bloom: Vec4,
    misc: Vec4,
    lut: [UVec4; LUT_ENTRIES],
}

impl Default for ArkGrade {
    fn default() -> Self {
        ArkGrade { bloom: Vec4::ZERO, misc: Vec4::ZERO, lut: [UVec4::ZERO; LUT_ENTRIES] }
    }
}

impl FullscreenMaterial for ArkGrade {
    fn fragment_shader() -> ShaderRef {
        "embedded://dishonored/ark_grade.wgsl".into()
    }

    fn schedule_configs(system: ScheduleConfigs<BoxedSystem>) -> ScheduleConfigs<BoxedSystem> {
        // replaces the tonemapper (which is off): runs on the linear scene colour
        // (the anti-aliasing after the whole grade, on the finished image)
        system.in_set(Core3dSystems::PostProcess).after(tonemapping).before(bevy::anti_alias::fxaa::fxaa).before(bevy::anti_alias::smaa::smaa)
    }
}

/// The 256x16 colour LUT (RGBA8, blue slices side by side) for the level's settings, baked
/// by evaluating the original LUT blender pixel shader at every texel.
pub fn bake_lut(p: &PostProcess) -> Option<[UVec4; LUT_ENTRIES]> {
    let lib = crate::ue3mat::library()?;
    let code = lib.globals.get("FArkPpDofLutBlenderPS")?;
    let sh = dhcook::sm3::parse(code).ok()?;
    // constants by name: GIMP-style colour balance (slider / 100), exposure in stops, gamma
    // adjusting the 2.2 display gamma, GIMP brightness and contrast slant
    let slant = ((p.contrast.clamp(-0.99, 0.99) + 1.0) * std::f32::consts::FRAC_PI_4).tan();
    let named: BTreeMap<&str, [f32; 4]> = [
        ("gCBShad_Cr_Mg_Yb_Opacity", [p.shadows[0], p.shadows[1], p.shadows[2], p.balance_opacity]),
        ("gCBMid_Cr_Mg_Yb", [p.midtones[0], p.midtones[1], p.midtones[2], 0.0]),
        ("gCBHigh_Cr_Mg_Yb", [p.highlights[0], p.highlights[1], p.highlights[2], 0.0]),
        ("gOverlay", [0.0; 4]),
        (
            "gExposure_GammaAdjust_PreDesaturation_PostDesaturation",
            [p.exposure.exp2(), 1.0 / (2.2 * p.gamma.max(0.05)), p.pre_desaturation, p.post_desaturation],
        ),
        ("gBrightness_Contrast", [p.brightness, slant, 0.0, 0.0]),
    ]
    .into_iter()
    .collect();
    let mut consts = BTreeMap::new();
    for c in sh.ctab.iter().filter(|c| c.set == 2) {
        match named.get(c.name.as_str()) {
            Some(v) => {
                consts.insert(c.reg as u32, *v);
            }
            None => warn!("LUT blender constant {} not supplied", c.name),
        }
    }
    let mut lut = [UVec4::ZERO; LUT_ENTRIES];
    for y in 0..16u32 {
        for x in 0..256u32 {
            let uv = [(x as f32 + 0.5) / 256.0, (y as f32 + 0.5) / 16.0, 0.0, 0.0];
            let c = match dhcook::sm3::eval(&sh, &[uv], &consts) {
                Ok(c) => c,
                Err(e) => {
                    warn!("LUT blender: {e:#}");
                    return None;
                }
            };
            let b = c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u32);
            let i = (y * 256 + x) as usize;
            lut[i / 4][i % 4] = b[0] | b[1] << 8 | b[2] << 16 | b[3] << 24;
        }
    }
    Some(lut)
}

/// Settings that replace the level's for a while (under water: the water volume's), by key.
#[derive(Resource, Default)]
pub struct PostOverride(pub Option<(u32, PostProcess)>);

/// The level scripts' settings (`DisSeqAct_UberPostProcess`), by key, and the seconds the
/// change fades over; over any other override.
#[derive(Resource, Default)]
pub struct ScriptPost(pub Option<(u32, PostProcess)>, pub f32);

/// Powers' gradings over everything else, by layer (`PowerPost::DARK_VISION`: its
/// `m_StaticUberAdjustement`; the post-process graph's nodes that override the grading while
/// they run: Bend Time's, Blink's...), applied in layer order.
#[derive(Resource, Default)]
pub struct PowerPost {
    pub layers: std::collections::BTreeMap<u8, PostLayer>,
}

/// A grading layer: its fields, and the seconds it fades in and out over.
#[derive(Clone, Default)]
pub struct PostLayer {
    pub fields: Vec<(String, Vec<f32>)>,
    pub fade_in: f32,
    pub fade_out: f32,
}

impl PowerPost {
    pub const GRAPH: u8 = 0;
    pub const DARK_VISION: u8 = 8;
    /// the menus' blur, over all
    pub const MENU: u8 = 20;

    /// Sets a layer (a no-op when unchanged, so the grading is not baked again).
    pub fn set(&mut self, id: u8, layer: Option<PostLayer>) {
        match layer {
            Some(l) => {
                if self.layers.get(&id).is_none_or(|o| o.fields != l.fields) {
                    self.layers.insert(id, l);
                }
            }
            None => {
                self.layers.remove(&id);
            }
        }
    }
}

/// How far the menus' blur is in (0..1), fading over `UI_Blur`'s times.
#[derive(Resource, Default)]
pub struct MenuBlur(pub f32);

/// The screens that pause the game blur and grey it (their movie players'
/// `m_bBlurGameWhileActive`: the journal, the pause menu, the store, the mission statistics):
/// `DisGlobalUIManager`'s `UI_Blur` (far blur 0.9 from the eye, half desaturated) faded in and
/// out over its `m_fFadeInTime`/`m_fFadeOutTime`.
#[allow(clippy::too_many_arguments)]
fn menu_blur(
    time: Res<Time<bevy::time::Real>>,
    data: Res<crate::gamedata::Data>,
    journal: Res<crate::journal::Journal>,
    menu: Res<crate::menu::Menu>,
    store: Res<crate::store::Store>,
    stats: Query<(), With<crate::mission::StatsScreen>>,
    note: Res<crate::notescreen::NoteScreen>,
    mut post: ResMut<PowerPost>,
    mut blur: ResMut<MenuBlur>,
) {
    let tw = &data.0.ui_blur;
    if tw.is_empty() {
        return;
    }
    let on = journal.open || note.open.is_some() || matches!(menu.open, Some(crate::menu::MenuKind::Pause | crate::menu::MenuKind::GameOver)) || store.open.is_some() || !stats.is_empty();
    let fade_in = tw.get("m_fFadeInTime").copied().unwrap_or(0.2);
    let fade_out = tw.get("m_fFadeOutTime").copied().unwrap_or(0.2);
    let dt = time.delta_secs();
    blur.0 = if on { (blur.0 + dt / fade_in.max(1e-3)).min(1.0) } else { (blur.0 - dt / fade_out.max(1e-3)).max(0.0) };
    if on == post.layers.contains_key(&PowerPost::MENU) {
        return;
    }
    let layer = on.then(|| PostLayer { fields: uber_fields(tw, "m_Parameters."), fade_in, fade_out });
    post.set(PowerPost::MENU, layer);
}

/// The keys of the powers' gradings: the layers present (`mask << 22`) over the key of what
/// they are over.
const POWER_POST_KEY: u32 = 1 << 22;

/// Settings fields over others, by the cooker's names (the scripts' `DisSeqAct_UberPostProcess`
/// `pp_fields`, a power's adjustment).
pub fn apply_fields<'a>(p: &mut PostProcess, fields: impl IntoIterator<Item = (&'a str, &'a [f32])>) {
    for (name, v) in fields {
        let one = v.first().copied().unwrap_or(0.0);
        let three = [one, v.get(1).copied().unwrap_or(0.0), v.get(2).copied().unwrap_or(0.0)];
        match name {
            "shadows" => p.shadows = three,
            "midtones" => p.midtones = three,
            "highlights" => p.highlights = three,
            "balance_opacity" => p.balance_opacity = one,
            "pre_desaturation" => p.pre_desaturation = one,
            "post_desaturation" => p.post_desaturation = one,
            "exposure" => p.exposure = one,
            "gamma" => p.gamma = one,
            "film_grain" => p.film_grain = one,
            "brightness" => p.brightness = one,
            "contrast" => p.contrast = one,
            "focus_distance" => p.focus_distance = one,
            "in_focus_radius" => p.in_focus_radius = one,
            "far_blur" => p.far_blur = one,
            _ => {}
        }
    }
}

/// The fields an `ArkUberPpParameters` overrides, from a tweak's flattened values under
/// `prefix` (Arkane's rule: a field applies when its group's flag and its own are set; the
/// flags default to the struct's defaults).
pub fn uber_fields(params: &std::collections::BTreeMap<String, f32>, prefix: &str) -> Vec<(String, Vec<f32>)> {
    let get = |k: &str| params.get(&format!("{prefix}{k}")).copied();
    let flag = |k: &str, d: bool| get(k).map(|v| v != 0.0).unwrap_or(d);
    let mut out = Vec::new();
    if flag("m_bOverrideCBParameters", true) {
        for (f, n) in [("shadows", "CrMgYbShadTones"), ("midtones", "CrMgYbMidTones"), ("highlights", "CrMgYbHighTones")] {
            if flag(&format!("m_CBParameters.m_bOverride{n}"), true) {
                let c = |a: &str| get(&format!("m_CBParameters.m_{n}.{a}")).unwrap_or(0.0);
                out.push((f.to_string(), vec![c("x"), c("y"), c("z")]));
            }
        }
        for (f, n, d, v) in [("balance_opacity", "Opacity", true, 1.0), ("pre_desaturation", "PreDesaturation", true, 0.0), ("post_desaturation", "PostDesaturation", false, 0.0)] {
            if flag(&format!("m_CBParameters.m_bOverride{n}"), d) {
                out.push((f.to_string(), vec![get(&format!("m_CBParameters.m_{n}")).unwrap_or(v)]));
            }
        }
    }
    if flag("m_bOverrideHDRParameters", true) {
        for (f, n, d, v) in [("exposure", "Exposure", true, 0.0), ("gamma", "GammaAdjustment", true, 1.0), ("film_grain", "FilmGrainNoise", false, 0.0), ("brightness", "GimpBrightness", false, 0.0), ("contrast", "GimpContrast", false, 0.0)] {
            if flag(&format!("m_HDRParameters.m_bOverride{n}"), d) {
                out.push((f.to_string(), vec![get(&format!("m_HDRParameters.m_{n}")).unwrap_or(v)]));
            }
        }
    }
    if flag("m_bOverrideDOFParameters", true) {
        for (f, n, v, scale) in [("focus_distance", "FocusDistance", 1000.0, 0.01905), ("in_focus_radius", "InFocusRadius", 300.0, 0.01905), ("far_blur", "FarBlurAmount", 0.0, 1.0)] {
            if flag(&format!("m_DOFParameters.m_bOverride{n}"), true) {
                out.push((f.to_string(), vec![get(&format!("m_DOFParameters.m_{n}")).unwrap_or(v) * scale]));
            }
        }
    }
    out
}

/// A LUT crossfade in progress: from, to, seconds done and in all.
#[derive(Default)]
struct LutFade(Option<(Box<[UVec4; LUT_ENTRIES]>, Box<[UVec4; LUT_ENTRIES]>, f32, f32)>);

fn lerp_lut(a: &[UVec4; LUT_ENTRIES], b: &[UVec4; LUT_ENTRIES], k: f32) -> [UVec4; LUT_ENTRIES] {
    let mut out = [UVec4::ZERO; LUT_ENTRIES];
    for i in 0..LUT_ENTRIES {
        for c in 0..4 {
            let (x, y) = (a[i][c], b[i][c]);
            let mut v = 0u32;
            for byte in 0..4 {
                let (p, q) = (((x >> (8 * byte)) & 255) as f32, ((y >> (8 * byte)) & 255) as f32);
                v |= ((p + (q - p) * k).round() as u32 & 255) << (8 * byte);
            }
            out[i][c] = v;
        }
    }
    out
}

/// The level's settings with the player's brightness (it scales the display gamma).
fn post_with_brightness(level: &LevelInfo, settings: &crate::settings::Settings) -> PostProcess {
    let mut p = level.scene.post.clone();
    p.gamma *= settings.brightness.clamp(0.5, 2.0);
    p
}

/// Brightness changed in the options, or an override came or went: the LUT is baked again
/// (overrides' LUTs are kept).
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn rebake_on_brightness(
    time: Res<Time<bevy::time::Real>>,
    settings: Res<crate::settings::Settings>,
    level: Option<Res<LevelInfo>>,
    (water, script, power): (Res<PostOverride>, Res<ScriptPost>, Res<PowerPost>),
    mut grades: Query<&mut ArkGrade, With<PlayerCamera>>,
    mut baked: Local<Option<(f32, Option<u32>)>>,
    mut cache: Local<std::collections::HashMap<(u32, u32), Box<[UVec4; LUT_ENTRIES]>>>,
    mut fade: Local<LutFade>,
    (mut last_mask, mut fade_outs): (Local<u32>, Local<std::collections::BTreeMap<u8, f32>>),
) {
    let Some(level) = level else { return };
    if grades.is_empty() {
        *baked = None;
        cache.clear();
        fade.0 = None;
        return;
    }
    // a crossfade under way
    if let Some((from, to, t, len)) = fade.0.as_mut() {
        *t += time.delta_secs();
        let k = (*t / len.max(1e-3)).min(1.0);
        let lut = lerp_lut(from, to, k);
        for mut g in &mut grades {
            g.lut = lut;
        }
        if k >= 1.0 {
            fade.0 = None;
        }
    }
    // the scripts' over the water's, the powers' over all
    let under = if script.0.is_some() { &script.0 } else { &water.0 };
    let mask = power.layers.keys().fold(0u32, |m, k| m | 1 << k);
    let powered = (mask != 0).then(|| {
        let mut p = under.as_ref().map(|o| o.1).unwrap_or(level.scene.post);
        for l in power.layers.values() {
            apply_fields(&mut p, l.fields.iter().map(|(n, v)| (n.as_str(), v.as_slice())));
        }
        ((mask << 22) | (under.as_ref().map(|o| o.0).unwrap_or(0) & (POWER_POST_KEY - 1)), p)
    });
    // (the seconds a layer coming fades in over; a layer going, out)
    let added = mask & !*last_mask;
    let fade_in = power.layers.iter().filter(|(k, _)| added & 1 << **k != 0).map(|(_, l)| l.fade_in).fold(0.0, f32::max);
    let fade_out = fade_outs.iter().filter(|(k, _)| (*last_mask & !mask) & 1 << **k != 0).map(|(_, s)| *s).fold(0.0, f32::max);
    *last_mask = mask;
    *fade_outs = power.layers.iter().map(|(k, l)| (*k, l.fade_out)).collect();
    let over = if powered.is_some() { &powered } else { under };
    let key = (settings.brightness, over.as_ref().map(|o| o.0));
    let was_powered = baked.is_some_and(|b| b.1.is_some_and(|k| k >= POWER_POST_KEY));
    if baked.is_none() {
        *baked = Some((settings.brightness, None));
    }
    if *baked == Some(key) {
        return;
    }
    *baked = Some(key);
    let lut = match over {
        Some((k, p)) => {
            let ck = (*k, settings.brightness.to_bits());
            if !cache.contains_key(&ck) {
                let mut p = *p;
                p.gamma *= settings.brightness.clamp(0.5, 2.0);
                if let Some(l) = bake_lut(&p) {
                    cache.insert(ck, Box::new(l));
                }
            }
            cache.get(&ck).map(|l| **l)
        }
        None => bake_lut(&post_with_brightness(&level, &settings)),
    };
    // (the scripts' film grain with it)
    let grain = over.as_ref().map(|o| o.1.film_grain).unwrap_or(level.scene.post.film_grain);
    let secs = if powered.is_some() && fade_in > 0.0 {
        fade_in
    } else if powered.is_some() || was_powered {
        fade_out
    } else if script.0.is_some() || over.is_none() {
        script.1
    } else {
        0.0
    };
    if let Some(lut) = lut {
        for mut g in &mut grades {
            g.misc.w = grain / 255.0;
            if secs > 0.01 {
                fade.0 = Some((Box::new(g.lut), Box::new(lut), 0.0, secs));
            } else {
                g.lut = lut;
            }
        }
    }
}

fn attach_grade(
    mut commands: Commands,
    level: Option<Res<LevelInfo>>,
    cams: Query<Entity, (With<PlayerCamera>, Without<ArkGrade>)>,
    settings: Res<crate::settings::Settings>,
) {
    let Some(level) = level else { return };
    if std::env::var("DH_UE3_DEBUG").as_deref() == Ok("id") {
        for e in &cams {
            commands.entity(e).insert((ArkGrade::default(), Tonemapping::None));
        }
        return;
    }
    if std::env::var("DH_NO_POST").is_ok() {
        return;
    }
    let post = post_with_brightness(&level, &settings);
    let p = &post;
    for e in &cams {
        let Some(lut) = bake_lut(p) else {
            warn!("post: no LUT blender shader; keeping the default tonemapper");
            return;
        };
        let grade = ArkGrade {
            bloom: Vec3::from(p.bloom_tint).extend(if p.bloom { p.bloom_scale } else { 0.0 }),
            // linear-space bloom threshold; grain in display units (original noise / 255)
            misc: Vec4::new(0.0, 0.25 + p.bloom_threshold * 0.5, 1.0, p.film_grain / 255.0),
            lut,
        };
        commands.entity(e).insert((grade, Tonemapping::None));
        info!(
            "post: balance S{:?} M{:?} H{:?} exposure {} gamma {} contrast {} grain {} bloom {}x{}",
            p.shadows, p.midtones, p.highlights, p.exposure, p.gamma, p.contrast, p.film_grain, p.bloom_scale, p.bloom_threshold
        );
    }
}

fn animate_grade(time: Res<Time>, mut grades: Query<&mut ArkGrade>) {
    for mut g in &mut grades {
        g.misc.x = time.elapsed_secs() % 1000.0;
    }
}
