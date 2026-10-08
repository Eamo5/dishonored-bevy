//! Mission flow: travelling between maps keeps the campaign (the player's inventory and
//! progress); leaving a mission shows its statistics first, on the original mission-stats art.

use crate::gameplay::{Campaign, PlayerStats};
use crate::hud::Paused;
use crate::save::mission_name;
use crate::ui_fonts::UiFonts;
use crate::ui_images::UiImages;
use crate::{Config, GameState};
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions};

pub struct MissionPlugin;

impl Plugin for MissionPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<TravelRequest>()
            .add_message::<ShowMissionStats>()
            .add_systems(Update, (travel, show_stats, stats_screen_input).chain().run_if(in_state(GameState::InGame)));
    }
}

/// The level scripts send the player to another map (a player start of it).
#[derive(Message, Clone)]
pub struct TravelRequest {
    pub map: String,
    pub start: usize,
}

/// The campaign scripts show a mission's statistics (`DisSeqAct_ShowMissionStats`): the
/// original screen's rows (`Twk_MissionStats_*`); closing it fires the op's "Closed".
#[derive(Message, Clone)]
pub struct ShowMissionStats {
    pub tweak: String,
    pub op: u32,
}

/// The end-of-mission screen, waiting for the player to continue: then travel, or tell the
/// campaign scripts.
#[derive(Resource)]
pub struct MissionEnd {
    map: Option<(String, usize)>,
    op: Option<u32>,
}

#[derive(Component)]
pub struct StatsScreen;

#[derive(Component)]
struct ContinueButton;

#[allow(clippy::too_many_arguments)]
fn travel(
    mut commands: Commands,
    mut requests: MessageReader<TravelRequest>,
    stats: Res<PlayerStats>,
    mut campaign: ResMut<Campaign>,
    mut config: ResMut<Config>,
    mut next: ResMut<NextState<GameState>>,
    level: Option<Res<crate::level::LevelInfo>>,
    fonts: Res<UiFonts>,
    (mut ui, mut images): (ResMut<UiImages>, ResMut<Assets<Image>>),
    mut paused: ResMut<Paused>,
    vm: Option<Res<crate::kismet::Vm>>,
    (mut timelines, window, data): (ResMut<crate::flash::MovieTimelines>, Query<&Window, With<bevy::window::PrimaryWindow>>, Res<crate::gamedata::Data>),
) {
    let Some(r) = requests.read().last().cloned() else { return };
    campaign.carry = Some(stats.clone());
    if let Some(vm) = vm.as_ref() {
        campaign.flags = vm.flags().clone();
    }
    let from = level.as_ref().map(|l| l.scene.name.clone()).unwrap_or_default();
    let done = mission_name(&from);
    // the campaign's scripts show the statistics themselves
    let scripted = vm.as_ref().is_some_and(|v| v.campaign.is_some());
    if scripted || done == mission_name(&r.map) || crate::save::is_hub(&from) || from.eq_ignore_ascii_case(crate::menu::MENU_MAP) {
        config.map = r.map;
        config.spawn_index = Some(r.start);
        next.set(GameState::Loading);
        return;
    }
    // a mission is over: its statistics first
    paused.0 = true;
    commands.insert_resource(MissionEnd { map: Some((r.map, r.start)), op: None });
    let s = &stats;
    let view = StatsView {
        title: done,
        art: None,
        rows: vec![
            ("Hostiles Killed".into(), Stat::Text((s.kills - s.civilians_killed.min(s.kills)).to_string())),
            ("Civilians Killed".into(), Stat::Text(s.civilians_killed.to_string())),
            ("Knocked Out".into(), Stat::Text(s.knockouts.to_string())),
            ("Times Detected".into(), Stat::Text(s.times_detected.to_string())),
            ("Chaos".into(), Stat::Text(s.chaos().to_string())),
        ],
        summary: Vec::new(),
        pickups: vec![("boneCharms", "Bone Charms".into(), s.charms_found, 0), ("runes", "Runes".into(), s.runes_found, 0), ("coins", "Coins".into(), s.coins_found, 0)],
    };
    if let (Ok(w), Some(tl)) = (window.single(), timelines.get("MissionStats")) {
        spawn_stats(&mut commands, tl, w, &data, &fonts, &mut ui, &mut images, &view);
    }
}

/// The campaign's statistics screen: the original rows of the mission's tweak and the story
/// outcomes reached.
#[allow(clippy::too_many_arguments)]
fn show_stats(
    mut commands: Commands,
    mut requests: MessageReader<ShowMissionStats>,
    stats: Res<PlayerStats>,
    data: Res<crate::gamedata::Data>,
    mut vm: Option<ResMut<crate::kismet::Vm>>,
    level: Option<Res<crate::level::LevelInfo>>,
    fonts: Res<UiFonts>,
    (mut ui, mut images): (ResMut<UiImages>, ResMut<Assets<Image>>),
    mut paused: ResMut<Paused>,
    (mut timelines, window): (ResMut<crate::flash::MovieTimelines>, Query<&Window, With<bevy::window::PrimaryWindow>>),
) {
    let Some(r) = requests.read().last().cloned() else { return };
    let Some(def) = data.0.mission_stats.iter().find(|m| m.name.eq_ignore_ascii_case(&r.tweak)) else {
        warn!("mission statistics {} unknown", r.tweak);
        if let Some(vm) = vm.as_mut() {
            vm.stats_closed(r.op);
        }
        return;
    };
    let s = &*stats;
    let mut rows = Vec::new();
    let mut pickups = Vec::new();
    for row in &def.stats {
        let v = s.stat(&row.stat) + row.add.as_deref().map(|a| s.stat(a)).unwrap_or(0);
        // the things found go to the pickups' panel (`_pickUps_mc`)
        let kind = match row.stat.as_str() {
            "ePlayerStat_BoneCharmFound" => Some("boneCharms"),
            "ePlayerStat_RuneFound" => Some("runes"),
            "ePlayerStat_GoldFound" => Some("coins"),
            "ePlayerStat_SokolovPaintingFound" => Some("paintings"),
            "ePlayerStat_OutsiderShrineFound" => Some("outsider"),
            _ => None,
        };
        if let Some(k) = kind {
            pickups.push((k, row.description.trim_end_matches(" Found").to_string(), v, row.max));
            continue;
        }
        let shown = if row.stat == "ePlayerStat_ChaosLevel" {
            Stat::Text(s.chaos().to_string())
        } else if row.checkbox {
            Stat::Check(if row.nonzero { v != 0 } else { v == 0 })
        } else {
            Stat::Text(v.to_string())
        };
        rows.push((row.description.clone(), shown));
    }
    let summary: Vec<String> = def
        .summary
        .iter()
        .filter(|(_, flag, want)| !flag.is_empty() && vm.as_ref().is_some_and(|v| v.flag(flag) == *want))
        .map(|(d, _, _)| d.clone())
        .collect();
    let map = level.as_ref().map(|l| l.scene.name.clone()).unwrap_or_default();
    paused.0 = true;
    commands.insert_resource(MissionEnd { map: None, op: Some(r.op) });
    // the mission's own illustration behind the statistics (`UI_MissionStatsBg_*`)
    let art = match def.name.as_str() {
        "Twk_M0_Prison" => "MissionStats_Prison",
        "Twk_M1_Overseers" => "MissionStats_Overseer",
        "Twk_M2_GoldenCat" => "MissionStats_Brothel",
        "Twk_M3_Bridge" => "MissionStats_Bridge",
        "Twk_M4_Boyle" => "MissionStats_Boyle",
        "Twk_M5_TowerReturn" => "MissionStats_TowerReturn",
        "Twk_M6_Flooded" => "MissionStats_FloodedDistrict",
        "Twk_M7_PubAssault" => "MissionStats_Hub",
        _ => "MissionStats_Lighthouse",
    };
    let view = StatsView { title: mission_name(&map), art: Some(art), rows, summary, pickups };
    if let (Ok(w), Some(tl)) = (window.single(), timelines.get("MissionStats")) {
        spawn_stats(&mut commands, tl, w, &data, &fonts, &mut ui, &mut images, &view);
    }
}

/// A statistic's value: words, or a box ticked or not.
enum Stat {
    Text(String),
    Check(bool),
}

/// What the statistics screen shows: the mission, its illustration, the rows, the outcomes
/// reached, the things found (the panel's icon frame, name, count).
struct StatsView {
    title: String,
    art: Option<&'static str>,
    rows: Vec<(String, Stat)>,
    summary: Vec<String>,
    /// (icon frame, name, found, how many there were: 0 unknown)
    pickups: Vec<(&'static str, String, u32, u32)>,
}

/// The statistics screen as `UI_MissionStats` draws it (`ms_statsScreen`): the mission's
/// illustration under the shards, blades and Corvo (`ms_Background`) and the 80% black; the
/// mission's name up its brush (`p_pauseMenu_title`, x0.85, `$TitleFont` 60 with its two dark
/// shadows); the list turned -3 degrees at (138.7,177.25) (`ms_statsList`: section bands
/// `ms_statList_section`, rows `ms_statList_item` 36.8 apart with the value on the right or its
/// tick box, a section 25 below the last); the things found on the right (`_pickUps_mc`, +2
/// degrees: "FOUND" and a line each with its icon); Continue where the help bar has it.
#[allow(clippy::too_many_arguments)]
fn spawn_stats(commands: &mut Commands, tl: std::sync::Arc<dhcook::format::Timelines>, w: &Window, data: &crate::gamedata::Data, fonts: &UiFonts, ui: &mut UiImages, images: &mut Assets<Image>, view: &StatsView) {
    use crate::flash::{concat, Clip, FlashClip, Mat};
    use bevy::text::{FontSize, LineBreak};
    const MOVIE: &str = "MissionStats";
    let pale = Color::srgb(227.0 / 255.0, 242.0 / 255.0, 214.0 / 255.0);
    let dark = Color::srgb(23.0 / 255.0, 25.0 / 255.0, 28.0 / 255.0);
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    let apply = |m: &Mat, p: Vec2| Vec2::new(m[0] * p.x + m[2] * p.y + m[4], m[1] * p.x + m[3] * p.y + m[5]);
    let turn = |deg: f32, x: f32, y: f32| -> Mat {
        let (sn, cs) = deg.to_radians().sin_cos();
        [cs, sn, -sn, cs, x, y]
    };
    let root = commands
        .spawn((StatsScreen, Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() }, BackgroundColor(Color::BLACK), GlobalZIndex(60), Pickable::IGNORE, DespawnOnExit(GameState::InGame)))
        .id();
    // a box of words placed by a matrix (its top-left corner `at`, its size, on the stage)
    let words = |commands: &mut Commands, m: &Mat, at: Vec2, size: Vec2, text: String, font: TextFont, color: Color, justify: Justify, tall: bool| {
        let sc = (m[0] * m[0] + m[1] * m[1]).sqrt();
        let c = apply(m, at + size * 0.5);
        let p = off + (c - size * 0.5) * s;
        let bx = commands
            .spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(p.x),
                    top: Val::Px(p.y),
                    width: Val::Px(size.x * s),
                    height: Val::Px(size.y * s),
                    align_items: AlignItems::Center,
                    justify_content: match justify {
                        Justify::Right => JustifyContent::FlexEnd,
                        Justify::Center => JustifyContent::Center,
                        _ => JustifyContent::FlexStart,
                    },
                    ..default()
                },
                UiTransform { rotation: Rot2::radians(m[1].atan2(m[0])), scale: Vec2::splat(sc), ..default() },
                Pickable::IGNORE,
                ChildOf(root),
            ))
            .id();
        let t = commands.spawn((Text::new(text), font, TextColor(color), TextLayout::new(justify, LineBreak::NoWrap), Pickable::IGNORE, ChildOf(bx))).id();
        if tall {
            commands.entity(t).insert(UiTransform { scale: Vec2::new(1.0, 1.2), ..default() });
        }
    };
    let title_font = |size: f32| TextFont { font: fonts.title.clone().into(), font_size: FontSize::Px(size * s), ..default() };
    let normal_font = |size: f32| TextFont { font_size: FontSize::Px(size * s), ..default() };
    // the mission's illustration (`_img_mc`, its 1280 x 720 grown 5%)
    if let Some((h, _)) = view.art.and_then(|a| ui.file(images, "missionstats", a)) {
        let (iw, ih) = (1280.0 * 1.05, 720.0 * 1.05);
        let p = off + (Vec2::new(640.0, 360.0) - Vec2::new(iw, ih) * 0.5) * s;
        commands.spawn((ImageNode::new(h), Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: Val::Px(iw * s), height: Val::Px(ih * s), ..default() }, Pickable::IGNORE, ChildOf(root)));
    }
    let list_m = turn(-3.0, 138.7, 177.25);
    let pick_m = turn(2.0, 1185.95, 378.15);
    let mut title_m: Mat = [0.85, -0.04, 0.04, 0.85, 117.3, 164.6];
    if let Some(c) = Clip::export(&tl, "ms_statsScreen") {
        let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real();
        fc.scale = s;
        fc.set_visible("_list_mc._mask_mc", false);
        // (the picture's placeholder: the illustration is drawn beneath)
        fc.set_visible("_bkgd_mc._img_mc", false);
        if let Some((m, _)) = fc.clip.placed("_title_mc") {
            title_m = m;
        }
        if let Some((m, _)) = fc.clip.placed_mut("_list_mc") {
            *m = list_m;
        }
        if let Some((m, _)) = fc.clip.placed_mut("_pickUps_mc") {
            *m = pick_m;
        }
        // the things found: one line each, the rest hidden
        let t2 = fc.tl.clone();
        for i in 0..5 {
            let name = format!("_pickUps_mc.line{i}_mc");
            match view.pickups.get(i) {
                Some((kind, _, _, _)) => {
                    if let Some(l) = fc.clip.child_mut(&format!("{name}.ic_mc")) {
                        l.goto_label(&t2, kind, false);
                    }
                }
                None => fc.set_visible(&name, false),
            }
        }
        if view.pickups.is_empty() {
            fc.set_visible("_pickUps_mc", false);
        }
        commands.spawn((fc, Node { position_type: PositionType::Absolute, left: Val::Px(off.x), top: Val::Px(off.y), ..default() }, Pickable::IGNORE, ChildOf(root)));
    }
    // the mission's name and its two dark shadows (`_txtShadow0_mc` x1.08, `_txtShadow1_mc` x1.34)
    let name = view.title.to_uppercase();
    for (m, color) in [
        (concat(&title_m, &[1.34, 0.0, 0.0, 1.34, -82.25, 31.3]), dark.with_alpha(0.8)),
        (concat(&title_m, &[1.08, 0.0, 0.0, 1.08, -143.3, -23.05]), dark.with_alpha(0.8)),
        (title_m, pale),
    ] {
        words(commands, &m, Vec2::new(0.0, -93.4), Vec2::new(932.4, 93.4), name.clone(), title_font(60.0), color, Justify::Left, true);
    }
    // the list: sections and rows
    let mut y = 0.0;
    let mut sections: Vec<(String, Vec<(String, Option<&Stat>)>)> = vec![("Mission Stats".into(), view.rows.iter().map(|(k, v)| (k.clone(), Some(v))).collect())];
    if !view.summary.is_empty() {
        sections.push(("Mission Summary".into(), view.summary.iter().map(|k| (k.clone(), None)).collect()));
    }
    let mut row_i = 0;
    for (si, (head, rows)) in sections.iter().enumerate() {
        if si > 0 {
            y += 25.0;
        }
        if let Some(c) = Clip::export(&tl, "ms_statList_section") {
            let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real();
            fc.scale = s;
            fc.m = [list_m[0], list_m[1], list_m[2], list_m[3], 0.0, 0.0];
            let p = off + apply(&list_m, Vec2::new(0.0, y)) * s;
            commands.spawn((fc, Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), ..default() }, Pickable::IGNORE, ChildOf(root)));
        }
        words(commands, &list_m, Vec2::new(19.3 + 2.0, y + 3.35), Vec2::new(385.0, 29.0), head.to_uppercase(), title_font(22.0), pale, Justify::Left, true);
        y += 38.75 + 4.0;
        for (k, v) in rows {
            if let Some(c) = Clip::export(&tl, "ms_statList_item") {
                let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real();
                fc.scale = s;
                fc.m = [list_m[0], list_m[1], list_m[2], list_m[3], 0.0, 0.0];
                let t2 = fc.tl.clone();
                if let Some(sep) = fc.clip.child_mut("sep_mc") {
                    sep.goto(&t2, row_i % 4, false);
                }
                match v {
                    Some(Stat::Check(on)) => {
                        if let Some(cb) = fc.clip.child_mut("checkBox_mc") {
                            cb.goto_label(&t2, if *on { "on" } else { "off" }, false);
                        }
                    }
                    _ => fc.set_visible("checkBox_mc", false),
                }
                let p = off + apply(&list_m, Vec2::new(30.0, y)) * s;
                commands.spawn((fc, Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), ..default() }, Pickable::IGNORE, ChildOf(root)));
            }
            words(commands, &list_m, Vec2::new(30.0 + 2.0, y + 2.0), Vec2::new(518.0, 28.6), k.clone(), normal_font(23.0), pale, Justify::Left, false);
            if let Some(Stat::Text(t)) = v {
                words(commands, &list_m, Vec2::new(30.0 + 424.05, y + 0.4), Vec2::new(181.0, 29.7), t.clone(), normal_font(24.0), Color::WHITE, Justify::Right, false);
            }
            y += 32.8 + 4.0;
            row_i += 1;
        }
    }
    // the things found: "FOUND", then "Runes 3"... right-aligned before their icons
    if !view.pickups.is_empty() {
        words(commands, &pick_m, Vec2::new(-273.95 + 2.0, -1.7 + 2.0), Vec2::new(265.0, 35.9), data.text("DisGFxMoviePlayerMissionStats_Texts", "t_Found").to_uppercase(), title_font(28.0), pale, Justify::Left, true);
        for (i, (_, name, v, max)) in view.pickups.iter().take(5).enumerate() {
            let ly = 61.0 + 40.0 * i as f32;
            // "Runes  3 / 7": found of the mission's `m_MissionStatsMaxValues`
            let count = if *max > 0 { format!("{v} / {max}") } else { v.to_string() };
            words(commands, &pick_m, Vec2::new(-44.9 - 300.0, ly - 12.8 + 2.0), Vec2::new(298.0, 29.7), format!("{name}  {count}"), normal_font(24.0), pale, Justify::Right, false);
        }
    }
    // Continue: the help bar's A (Enter)
    let words_c = data.text("DisGFxMoviePlayerBase_Texts", "t_ContinueGame").to_uppercase();
    let p = off + Vec2::new(1184.0 - 260.0, 651.0 - 20.0) * s;
    commands
        .spawn((ContinueButton, Button, Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: Val::Px(260.0 * s), justify_content: JustifyContent::FlexEnd, ..default() }, ChildOf(root)))
        .with_child((Text::new(format!("[Enter]  {words_c}")), title_font(24.0), TextColor(pale)));
}

#[allow(clippy::too_many_arguments)]
fn stats_screen_input(
    mut commands: Commands,
    end: Option<Res<MissionEnd>>,
    keys: Res<ButtonInput<KeyCode>>,
    button: Query<&Interaction, (With<ContinueButton>, Changed<Interaction>)>,
    screens: Query<Entity, With<StatsScreen>>,
    mut config: ResMut<Config>,
    mut next: ResMut<NextState<GameState>>,
    mut paused: ResMut<Paused>,
    mut cursor: Single<&mut CursorOptions>,
    mut vm: Option<ResMut<crate::kismet::Vm>>,
) {
    let Some(end) = end else { return };
    paused.0 = true;
    if cursor.grab_mode != CursorGrabMode::None {
        cursor.visible = true;
        cursor.grab_mode = CursorGrabMode::None;
    }
    let clicked = button.iter().any(|i| *i == Interaction::Pressed);
    if clicked || keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::Space) || keys.just_pressed(KeyCode::KeyE) {
        for e in &screens {
            commands.entity(e).despawn();
        }
        commands.remove_resource::<MissionEnd>();
        paused.0 = false;
        if let (Some(op), Some(vm)) = (end.op, vm.as_mut()) {
            vm.stats_closed(op);
        }
        if let Some((map, start)) = end.map.clone() {
            config.map = map;
            config.spawn_index = Some(start);
            next.set(GameState::Loading);
        }
    }
}
