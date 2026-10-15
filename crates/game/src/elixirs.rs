//! The elixir counters by the power wheel (`PC_PotionsCount`, the power wheel movie's
//! `shPC_potions`), as roshan's r614/blink research read its ActionScript: at the bottom right
//! of the HUD's safe area (`GetMovieSpaceCoord(0.9).coordMax`), the health elixirs above the
//! mana ones, each with its count and the key that drinks it. They open with the wheel, the
//! health slid in from 150 to the right over 0.45 s and the mana over 0.55 s; an empty count
//! dims its icon and key (the element's `empty`); a drink plays its icon's stroke
//! (`stroke_mc`, `consume`).

use crate::bindings::{display, Act, Bindings};
use crate::flash::{Clip, Ease, FlashClip, MovieTimelines, PropsTo};
use crate::gameplay::PlayerStats;
use crate::wheel::Wheel;
use crate::GameState;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

pub struct ElixirsPlugin;

impl Plugin for ElixirsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Counters>().add_systems(Update, counters.run_if(in_state(GameState::InGame)));
    }
}

const MOVIE: &str = "powerwheel";
const MOVIE_DLC06: &str = "powerwheel_DLC06";
const SYMBOL: &str = "shPC_potions";
/// The two elements (health, mana): their clips, where the movie has them across, and how long
/// each takes to slide in (s) from `SLIDE` to the right.
const PARTS: [(&str, f32); 2] = [("_potionsHealth_mc", 0.45), ("_potionsMana_mc", 0.55)];
const PART_X: f32 = -21.95;
const SLIDE: f32 = 150.0;
/// The counters fading as the wheel closes (s).
const FADE: f32 = 0.2;

#[derive(Resource, Default)]
struct Counters {
    root: Option<Entity>,
    /// the counts shown (to see a drink), and how far through its fade-out it is
    shown: [u32; 2],
    closing: Option<f32>,
}

#[derive(Component)]
struct CountersClip;

#[allow(clippy::too_many_arguments)]
fn counters(
    mut commands: Commands,
    time: Res<Time<bevy::time::Real>>,
    mut st: ResMut<Counters>,
    wheel: Res<Wheel>,
    stats: Res<PlayerStats>,
    bind: Res<Bindings>,
    data: Res<crate::gamedata::Data>,
    mut timelines: ResMut<MovieTimelines>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut clips: Query<(&mut FlashClip, &mut Node), With<CountersClip>>,
) {
    let counts = [stats.health_elixirs, stats.mana_elixirs];
    let alive = st.root.filter(|r| clips.contains(*r));
    match (wheel.open, alive) {
        // (`Open`: made afresh, both elements slid in)
        (true, None) => {
            let movie = if data.is_daud() && timelines.get(MOVIE_DLC06).is_some() { MOVIE_DLC06 } else { MOVIE };
            let Some(tl) = timelines.get(movie) else { return };
            let Some(c) = Clip::export(&tl, SYMBOL) else { return };
            let mut fc = FlashClip::new(movie, tl.clone(), c).real().with_texts();
            for (i, (part, dur)) in PARTS.iter().enumerate() {
                fc.goto(&format!("{part}.ic"), if i == 0 { "health" } else { "mana" }, false);
                // (the gamepad's button art: the key's own label shows on PC)
                fc.set_visible(&format!("{part}.key_mc"), false);
                fc.set(part, PropsTo::default().x(PART_X + SLIDE));
                fc.tween(part, PropsTo::default().x(PART_X), *dur, Ease::StrongOut);
            }
            let root = commands
                .spawn((CountersClip, fc, Node { position_type: PositionType::Absolute, ..default() }, GlobalZIndex(71), Pickable::IGNORE, DespawnOnExit(GameState::InGame)))
                .id();
            *st = Counters { root: Some(root), shown: counts, closing: None };
            return;
        }
        (true, Some(_)) => st.closing = None,
        (false, Some(r)) => {
            let t = st.closing.unwrap_or(0.0) + time.delta_secs();
            if t >= FADE {
                commands.entity(r).despawn();
                *st = Counters::default();
                return;
            }
            st.closing = Some(t);
        }
        (false, None) => return,
    }
    let Some(root) = st.root else { return };
    let Ok((mut fc, mut n)) = clips.get_mut(root) else { return };
    let Ok(w) = window.single() else { return };
    // (at the safe area's bottom right: `hud::MovieSpace`)
    let ms = crate::hud::MovieSpace::of(w);
    let p = ms.px(ms.max);
    n.left = Val::Px(p.x);
    n.top = Val::Px(p.y);
    fc.scale = ms.s;
    fc.alpha = 1.0 - st.closing.unwrap_or(0.0) / FADE;
    for (i, (part, _)) in PARTS.iter().enumerate() {
        let count = counts[i];
        // the count (an empty one dims the element: `empty`), and its key
        let state = if count == 0 { "empty" } else { "default" };
        if fc.label(part) != Some(state) {
            fc.goto(part, state, false);
            fc.goto(&format!("{part}.quantity"), state, false);
        }
        fc.set_text(&format!("{part}.quantity.txt"), count.to_string());
        let key = bind.key(if i == 0 { Act::HealthElixir } else { Act::ManaElixir });
        fc.set_text(&format!("{part}.keyPC_mc.txt"), display(key));
        // one drunk: its stroke
        if count < st.shown[i] {
            fc.goto(&format!("{part}.ic.stroke_mc"), "consume", true);
        }
        st.shown[i] = count;
    }
}
