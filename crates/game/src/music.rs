//! Interactive music, as the original music director runs it: the situation (exploration,
//! suspense, combat by intensity, chase) selects the map's Wwise music state through the
//! state events of its `DisTweaks_MusicEvents` (`scene.music`).

use crate::audio::PostEvent;
use crate::level::{LevelInfo, LevelSpawnSet};
use crate::npc::{Alert, Npc};
use crate::player::Player;
use crate::GameState;
use bevy::prelude::*;

pub struct MusicPlugin;

impl Plugin for MusicPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Director>()
            .add_systems(OnEnter(GameState::InGame), start_music.after(LevelSpawnSet))
            .add_systems(Update, direct_music.run_if(in_state(GameState::InGame)));
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Situation {
    Explore,
    Suspense,
    /// intensity level (0-based)
    Combat(usize),
    /// pursuers near (0) / far (1)
    Chase(usize),
}

#[derive(Resource, Default)]
struct Director {
    current: Option<Situation>,
    /// a different situation and how long it has held
    pending: Option<(Situation, f32)>,
    /// time left to keep the chase music after the fight
    chase_left: f32,
    tick: f32,
}

fn start_music(level: Option<Res<LevelInfo>>, mut dir: ResMut<Director>, mut out: MessageWriter<PostEvent>) {
    *dir = Director::default();
    let Some(m) = level.as_ref().and_then(|l| l.scene.music.as_ref()) else { return };
    if !m.exploration.is_empty() {
        out.write(PostEvent::named(&m.exploration, None));
        dir.current = Some(Situation::Explore);
    }
}

/// Intensity an enemy adds to a fight.
fn contribution(npc: &Npc, table: &[f32]) -> f32 {
    let n = npc.name.to_ascii_lowercase();
    let strong = n.contains("elite") || n.contains("overseer") || n.contains("assassin") || n.contains("tallboy") || n.contains("officer");
    let k = if strong { 5 } else { 3 };
    table.get(k).copied().unwrap_or(1.0)
}

fn direct_music(
    time: Res<Time>,
    level: Option<Res<LevelInfo>>,
    mut dir: ResMut<Director>,
    npcs: Query<(&Npc, &Transform)>,
    player: Query<&Transform, With<Player>>,
    mut out: MessageWriter<PostEvent>,
) {
    let Some(m) = level.as_ref().and_then(|l| l.scene.music.as_ref()) else { return };
    let Ok(pt) = player.single() else { return };
    let dt = time.delta_secs();
    dir.tick -= dt;
    if dir.tick > 0.0 {
        return;
    }
    let step = 0.25 - dir.tick.min(0.0);
    dir.tick = 0.25;
    let ppos = pt.translation;
    let fight_range = m.chase_range.first().copied().unwrap_or(20.0);
    let far_range = m.chase_range.get(1).copied().unwrap_or(45.0);
    let mut intensity = 0.0;
    let mut pursuer = f32::INFINITY;
    let mut suspicious = false;
    for (npc, t) in &npcs {
        if npc.is_down() || !npc.hostile() {
            continue;
        }
        let d = t.translation.distance(ppos);
        match npc.alert {
            Alert::Combat if (npc.sees_player && d < fight_range) || d < m.suspense_range => intensity += contribution(npc, &m.contribution),
            Alert::Combat => pursuer = pursuer.min(d),
            Alert::Suspicious if d < m.suspense_range => suspicious = true,
            _ => {}
        }
    }
    let want = if intensity > 0.0 {
        dir.chase_left = m.chase_time;
        let level = m.combat_intensity.iter().filter(|&&t| intensity >= t).count().clamp(1, m.combat.len().max(1)) - 1;
        Situation::Combat(level)
    } else if pursuer < far_range || dir.chase_left > 0.0 {
        dir.chase_left = if pursuer < far_range { m.chase_time } else { dir.chase_left - step };
        Situation::Chase(if pursuer < fight_range { 0 } else { 1 })
    } else if suspicious {
        Situation::Suspense
    } else {
        Situation::Explore
    };
    if Some(want) == dir.current {
        dir.pending = None;
        return;
    }
    // escalations are immediate; calming down waits for the situation to hold
    let escalate = match (want, dir.current) {
        (Situation::Combat(a), Some(Situation::Combat(b))) => a > b,
        (Situation::Combat(_), _) => true,
        (Situation::Suspense, Some(Situation::Explore) | None) => true,
        _ => false,
    };
    let held = match dir.pending {
        Some((s, t)) if s == want => t + step,
        _ => 0.0,
    };
    dir.pending = Some((want, held));
    if !escalate && held < 2.0 {
        return;
    }
    let event = match want {
        Situation::Explore => Some(&m.exploration),
        Situation::Suspense => Some(&m.suspense),
        Situation::Combat(k) => m.combat.get(k),
        Situation::Chase(k) => m.chase.get(k).or(m.chase.first()),
    };
    if let Some(e) = event.filter(|e| !e.is_empty()) {
        if std::env::var("DH_AUDIO_LOG").is_ok() {
            info!("music: {want:?} ({e})");
        }
        out.write(PostEvent::named(e, None));
    }
    dir.current = Some(want);
    dir.pending = None;
}
