//! Dishonored — a Bevy engine re-implementation that loads the original game's
//! Unreal Engine 3 content (via the `upk`/`dhcook` crates) and re-creates the
//! core gameplay systems.

mod anim;
mod audio;
mod barks;
mod matinee;
mod ui_fonts;
mod ui_images;
mod menu;
mod mission;
mod gamedata;
mod heart;
mod security;
mod wheel;
mod journal;
mod jview;
mod possession;
mod store;
mod swarm;
mod traps;
mod script_world;
mod navmesh;
mod usables;
mod audiograph;
mod animbg;
mod globalui;
mod msgbox;
mod killcam;
mod aim;
mod gore;
mod aiworld;
mod worlddamage;
mod audiorooms;
mod npcparts;
mod achievements;
mod reflections;
mod flares;
mod challenge;
mod dlc05hud;
mod dlc05brief;
mod dlc05menu;
mod dlc05results;
mod dlc05score;
mod kuwahara;
mod lightshafts;
mod breath;
mod notescreen;
mod optscreen;
mod savescreen;
mod frontend;
mod gadgets;
mod campaign;
mod save;
mod settings;
mod speakers;
mod footsteps;
mod music;
mod arms;
mod combat;
mod debug;
mod gameplay;
mod hud;
mod interact;
mod kismet;
mod npc;
mod powers;
mod script;
mod level;
mod lightmap;
mod loading;
mod particles;
mod trails;
mod ragdoll;
mod feet;
mod player;
mod postfx;
mod sky;
mod ue3mat;
mod world_light;
mod fxlight;
mod warmup;
mod bindings;
mod swim;
mod fog;
mod carry;
mod climb;
mod zoom;
mod markers;
mod choice;
mod pickpocket;
mod krust;
mod fish;
mod keyhole;
mod props;
mod water;
mod assassin;
mod musicbox;
mod distraction;
mod matparams;
mod rain;
mod propanim;
mod movie;
mod facefx;
mod darkvision;
mod dof;
mod ppgraph;
mod watchtower;
mod highlight;
mod stealth;
mod hudfx;
mod skip;
mod pickuplog;
mod targetcard;
mod gauges;
mod flash;
mod awareness;
mod location;
mod crosshair;
mod intwindow;
mod hudtext;
mod objnotify;
mod oxygen;
mod playerstate;
mod grenadeind;
mod prompts;
mod tutwindow;

use bevy::prelude::*;
use bevy::window::{PresentMode, WindowResolution};
use bevy_rapier3d::prelude::*;

#[derive(States, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GameState {
    #[default]
    Loading,
    InGame,
}

/// Command line / environment configuration.
#[derive(Resource, Clone, Debug)]
pub struct Config {
    pub map: String,
    pub max_texture_size: u32,
    pub recook: bool,
    /// Player start to use; None picks the safest one.
    pub spawn_index: Option<usize>,
}

impl Config {
    fn from_args() -> Self {
        let args: Vec<String> = std::env::args().collect();
        let get = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1).cloned());
        Config {
            // without a map, the game starts at the main menu
            map: get("--map").unwrap_or_else(|| menu::MENU_MAP.to_string()),
            max_texture_size: get("--tex").and_then(|s| s.parse().ok()).unwrap_or(2048),
            recook: args.iter().any(|a| a == "--recook"),
            spawn_index: get("--spawn").and_then(|s| s.parse().ok()),
        }
    }
}

fn main() -> AppExit {
    let config = Config::from_args();
    let mut app = App::new();
    // a command on a missing entity panics, its backtrace naming the command
    if std::env::var("DH_PANIC_ON_ERROR").is_ok() {
        app.set_error_handler(bevy::ecs::error::panic);
    }
    if std::env::var("DH_SCRIPT").is_ok() {
        // test runs live in an unfocused window; keep them at full rate instead of Bevy's
        // low-power unfocused mode so timings are meaningful
        app.insert_resource(bevy::winit::WinitSettings {
            focused_mode: bevy::winit::UpdateMode::Continuous,
            unfocused_mode: bevy::winit::UpdateMode::Continuous,
        });
    }
    app
        .insert_resource(config)
        .insert_resource(ClearColor(Color::srgb(0.02, 0.025, 0.03)))
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "Dishonored — Bevy".into(),
                        // `DH_RES=WxH` for tests (logical size)
                        resolution: std::env::var("DH_RES")
                            .ok()
                            .and_then(|r| r.split_once('x').and_then(|(w, h)| Some(WindowResolution::new(w.parse().ok()?, h.parse().ok()?))))
                            .unwrap_or(WindowResolution::new(1600, 900)),
                        present_mode: if std::env::var("DH_NOVSYNC").is_ok() { PresentMode::AutoNoVsync } else { PresentMode::AutoVsync },
                        // scripted test runs shouldn't steal focus from whoever is using the machine
                        focused: std::env::var("DH_SCRIPT").is_err(),
                        ..default()
                    }),
                    ..default()
                })
                .set(bevy::log::LogPlugin {
                    filter: "info,wgpu=error,naga=warn,bevy_render=warn,bevy_rapier3d=warn".into(),
                    ..default()
                }),
        )
        .add_plugins(RapierPhysicsPlugin::<NoUserData>::default())
        .init_state::<GameState>()
        .add_plugins((fxlight::FxLightPlugin, warmup::WarmupPlugin, swim::SwimPlugin, fog::FogPlugin, carry::CarryPlugin, climb::ClimbPlugin, zoom::ZoomPlugin, markers::MarkersPlugin, choice::ChoicePlugin, pickpocket::PickpocketPlugin, krust::KrustPlugin, fish::FishPlugin, keyhole::KeyholePlugin, props::PropsPlugin, water::WaterPlugin))
        .add_plugins((loading::LoadingPlugin, level::LevelPlugin, player::PlayerPlugin, debug::DebugPlugin, sky::SkyPlugin, lightmap::LightmapPlugin, world_light::WorldLightPlugin))
        .add_plugins((
            gameplay::GameplayPlugin,
            npc::NpcPlugin,
            combat::CombatPlugin,
            arms::ArmsPlugin,
            anim::AnimPlugin,
            interact::InteractPlugin,
            kismet::KismetPlugin,
            postfx::PostFxPlugin,
            ue3mat::Ue3Plugin,
            particles::ParticlePlugin,
            trails::TrailsPlugin,
            (ragdoll::RagdollPlugin, feet::FeetPlugin),
            powers::PowersPlugin,
            hud::HudPlugin,
            script::ScriptPlugin,
        ))
        .add_plugins((audio::GameAudioPlugin, footsteps::FootstepsPlugin, music::MusicPlugin, barks::BarksPlugin, matinee::MatineePlugin, ui_fonts::UiFontsPlugin, ui_images::UiImagesPlugin))
        .add_plugins((assassin::AssassinPlugin, musicbox::MusicBoxPlugin, traps::TrapsPlugin, script_world::ScriptWorldPlugin, navmesh::NavMeshPlugin, usables::UsablesPlugin, audiograph::AudiographPlugin, distraction::DistractionPlugin, matparams::MatParamsPlugin, rain::RainPlugin, propanim::PropAnimPlugin, movie::MoviePlugin, facefx::FaceFxPlugin, darkvision::DarkVisionPlugin, dof::DofPlugin))
        .add_plugins((frontend::FrontendPlugin, animbg::AnimBgPlugin, savescreen::SaveScreenPlugin, optscreen::OptScreenPlugin, notescreen::NoteScreenPlugin, globalui::GlobalUiPlugin, msgbox::MsgBoxPlugin, killcam::KillCamPlugin,
            aim::AimPlugin,
            gore::GorePlugin,
            lightshafts::LightShaftsPlugin,
            kuwahara::KuwaharaPlugin,
            aiworld::AiWorldPlugin,
            worlddamage::WorldDamagePlugin,
            breath::BreathPlugin))
        .add_plugins((audiorooms::AudioRoomsPlugin, npcparts::NpcPartsPlugin, achievements::AchievementsPlugin, reflections::ReflectionsPlugin))
        .add_plugins((flares::FlaresPlugin, challenge::ChallengePlugin, dlc05hud::Dlc05HudPlugin, dlc05results::Dlc05ResultsPlugin, dlc05menu::Dlc05MenuPlugin, dlc05brief::Dlc05BriefPlugin, dlc05score::Dlc05ScorePlugin))
        .add_plugins((menu::MenuPlugin, save::SavePlugin, settings::SettingsPlugin, mission::MissionPlugin, ppgraph::PostGraphPlugin, watchtower::WatchTowerPlugin, highlight::HighlightPlugin, stealth::StealthPlugin, hudfx::HudFxPlugin, skip::SkipPlugin, pickuplog::PickupLogPlugin, targetcard::TargetCardPlugin, gauges::GaugesPlugin))
        .add_plugins((flash::FlashPlugin, awareness::AwarenessPlugin, location::LocationPlugin, crosshair::CrosshairPlugin, intwindow::IntWindowPlugin, hudtext::HudTextPlugin, objnotify::ObjNotifyPlugin, oxygen::OxygenPlugin, playerstate::PlayerStatePlugin, grenadeind::GrenadeIndPlugin, prompts::PromptsPlugin, tutwindow::TutWindowPlugin))
        .add_plugins((gamedata::GameDataPlugin, journal::JournalPlugin, possession::PossessionPlugin, store::StorePlugin, swarm::SwarmPlugin, gadgets::GadgetsPlugin, campaign::CampaignPlugin, heart::HeartPlugin, security::SecurityPlugin, wheel::WheelPlugin))
        .add_systems(Startup, powers::configure_xray_gizmos)
        .run()
}
