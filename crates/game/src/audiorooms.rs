//! The sound's way through a level's rooms (`DishonoredAudioVolume` cells joined by
//! `DishonoredAudioPortal` doorways): a sound in another room reaches Corvo through the
//! doorways between, from the way it comes, muffled by each - its own occlusion, a shut door in
//! it (the door tweak's `m_fPlayerSoundOcclusion`), what the scripts set
//! (`DisSeqAct_SetAudioOcclusion`); the AI hears through them the same way (`..ForAI`); and the
//! room Corvo is in sets the ambience (its `m_pSoundEvent`: the street's drone, an interior's).

use crate::interact::Door;
use crate::level::{LevelInfo, LevelInstance};
use crate::GameState;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;
use std::collections::HashMap;
use std::sync::Mutex;

pub struct AudioRoomsPlugin;

impl Plugin for AudioRoomsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AudioRooms>()
            .add_systems(OnEnter(GameState::InGame), build_rooms.after(crate::level::LevelSpawnSet))
            .add_systems(Update, (portal_states, listener_room).run_if(in_state(GameState::InGame)));
    }
}

struct Cell {
    name: String,
    hulls: Vec<Collider>,
    min: Vec3,
    max: Vec3,
    state_event: String,
    environment: String,
}

struct Portal {
    name: String,
    center: Vec3,
    cells: [Option<usize>; 2],
    /// its own muffling, a shut door's in it (by how shut), the scripts': for Corvo, for the AI
    own: [f32; 2],
    door: [f32; 2],
    script: Option<[f32; 2]>,
}

impl Portal {
    fn occlusion(&self, ai: bool) -> f32 {
        let k = ai as usize;
        self.script.map(|s| s[k]).unwrap_or(0.0).max(self.own[k]).max(self.door[k]).clamp(0.0, 1.0)
    }
}

/// The way a sound comes: how far it travels, where it seems to come from (the first doorway on
/// its way), how much of it gets through.
#[derive(Clone, Copy, Debug)]
pub struct Route {
    pub dist: f32,
    pub toward: Vec3,
    pub gain: f32,
}

#[derive(Resource, Default)]
pub struct AudioRooms {
    cells: Vec<Cell>,
    portals: Vec<Portal>,
    /// each cell's doorways
    links: Vec<Vec<usize>>,
    /// the doors in the doorways (the door's instance entity, its muffling shut)
    doors: Vec<(usize, Entity, [f32; 2])>,
    linked: bool,
    /// the room Corvo hears from, and the ambience it set
    listener: Option<usize>,
    state: String,
    /// rooms by place (half-metre cells)
    cache: Mutex<HashMap<(i32, i32, i32), Option<usize>>>,
}

impl AudioRooms {
    /// The room a point is in (the smallest of those holding it).
    pub fn cell_at(&self, p: Vec3) -> Option<usize> {
        if self.cells.is_empty() {
            return None;
        }
        let key = ((p.x * 2.0).floor() as i32, (p.y * 2.0).floor() as i32, (p.z * 2.0).floor() as i32);
        if let Some(c) = self.cache.lock().ok().and_then(|m| m.get(&key).copied()) {
            return c;
        }
        let c = self
            .cells
            .iter()
            .enumerate()
            .filter(|(_, c)| p.cmpge(c.min).all() && p.cmple(c.max).all() && c.hulls.iter().any(|h| h.contains_point(Vec3::ZERO, Quat::IDENTITY, p)))
            .min_by(|a, b| (a.1.max - a.1.min).element_product().total_cmp(&(b.1.max - b.1.min).element_product()))
            .map(|(i, _)| i);
        if let Ok(mut m) = self.cache.lock() {
            if m.len() > 200_000 {
                m.clear();
            }
            m.insert(key, c);
        }
        c
    }

    /// The ways out from a listener at `to` through the doorways (for the sounds of every other
    /// room at once): None outside the rooms.
    pub fn field(&self, to: Vec3, ai: bool) -> Option<Field> {
        let a = self.cell_at(to)?;
        // (Dijkstra over the doorways, from the listener's room's)
        let n = self.portals.len();
        let mut f = Field { cell: a, to, dist: vec![f32::INFINITY; n], first: vec![usize::MAX; n], gain: vec![1.0; n] };
        let mut done = vec![false; n];
        let mut heap = std::collections::BinaryHeap::new();
        for &p in &self.links[a] {
            f.dist[p] = to.distance(self.portals[p].center);
            f.first[p] = p;
            f.gain[p] = 1.0 - self.portals[p].occlusion(ai);
            heap.push((std::cmp::Reverse(ordered(f.dist[p])), p));
        }
        while let Some((_, p)) = heap.pop() {
            if done[p] {
                continue;
            }
            done[p] = true;
            for c in self.portals[p].cells.iter().flatten() {
                if *c == a {
                    continue;
                }
                for &q in &self.links[*c] {
                    let d = f.dist[p] + self.portals[p].center.distance(self.portals[q].center);
                    if !done[q] && d < f.dist[q] {
                        f.dist[q] = d;
                        f.first[q] = f.first[p];
                        f.gain[q] = f.gain[p] * (1.0 - self.portals[q].occlusion(ai));
                        heap.push((std::cmp::Reverse(ordered(d)), q));
                    }
                }
            }
        }
        Some(f)
    }

    /// How a sound at `from` reaches `to` when they are in different rooms (else it comes
    /// straight): the shortest way through the doorways, muffled by each; walls between rooms
    /// with no way through let a little through.
    pub fn route(&self, to: Vec3, from: Vec3, ai: bool) -> Option<Route> {
        self.field(to, ai)?.route(self, from)
    }
}

/// Distances as orderable integers (millimetres) for the search's queue.
fn ordered(d: f32) -> u64 {
    (d.max(0.0) * 1000.0) as u64
}

/// A listener's ways out through the doorways: each doorway's distance along the way, the
/// first doorway of the way, what gets through so far.
pub struct Field {
    cell: usize,
    to: Vec3,
    dist: Vec<f32>,
    first: Vec<usize>,
    gain: Vec<f32>,
}

impl Field {
    /// The way to a sound at `from`: through the doorways into its room.
    pub fn route(&self, rooms: &AudioRooms, from: Vec3) -> Option<Route> {
        let b = rooms.cell_at(from)?;
        if b == self.cell {
            return None;
        }
        let best = rooms.links[b]
            .iter()
            .filter(|&&p| self.dist[p].is_finite())
            .map(|&p| (p, self.dist[p] + rooms.portals[p].center.distance(from)))
            .min_by(|x, y| x.1.total_cmp(&y.1));
        Some(match best {
            Some((p, d)) => Route { dist: d, toward: rooms.portals[self.first[p]].center, gain: self.gain[p] },
            None => Route { dist: self.to.distance(from), toward: from, gain: 0.2 },
        })
    }
}

fn build_rooms(mut rooms: ResMut<AudioRooms>, level: Option<Res<LevelInfo>>) {
    *rooms = AudioRooms::default();
    let Some(level) = level.filter(|_| std::env::var("DH_NO_ROOMS").is_err()) else { return };
    let scene = &level.scene;
    for c in &scene.audio_cells {
        let hulls: Vec<Collider> = c.hulls.iter().filter_map(|h| Collider::convex_hull(&h.iter().map(|p| Vec3::from(*p)).collect::<Vec<_>>())).collect();
        let (min, max) = c.hulls.iter().flatten().fold((Vec3::MAX, Vec3::MIN), |(lo, hi), p| (lo.min(Vec3::from(*p)), hi.max(Vec3::from(*p))));
        rooms.cells.push(Cell { name: c.name.clone(), hulls, min, max, state_event: c.state_event.clone(), environment: c.environment.clone() });
    }
    rooms.links = vec![Vec::new(); rooms.cells.len()];
    for (i, p) in scene.audio_portals.iter().enumerate() {
        let center = p.corners.iter().map(|c| Vec3::from(*c)).sum::<Vec3>() / 4.0;
        let cells = p.cells.map(|c| c.map(|c| c as usize).filter(|c| *c < rooms.cells.len()));
        for c in cells.iter().flatten() {
            rooms.links[*c].push(i);
        }
        rooms.portals.push(Portal { name: p.name.clone(), center, cells, own: p.occlusion, door: [0.0; 2], script: None });
    }
    if !rooms.cells.is_empty() {
        info!("audio: {} rooms, {} doorways", rooms.cells.len(), rooms.portals.len());
    }
}

/// The doorways' muffling: the doors in them as they stand (shut, half open, open), what the
/// scripts set.
fn portal_states(mut rooms: ResMut<AudioRooms>, level: Option<Res<LevelInfo>>, doors: Query<(Entity, &LevelInstance, &GlobalTransform, &Door)>, vm: Option<ResMut<crate::kismet::Vm>>) {
    let Some(level) = level else { return };
    if rooms.portals.is_empty() {
        return;
    }
    // (the doors in the doorways, once they stand)
    if !rooms.linked && !doors.is_empty() {
        rooms.linked = true;
        let mut found = Vec::new();
        for (e, li, gt, _) in &doors {
            let Some(occ) = level.scene.doors.get(&li.actor).map(|d| d.occlusion) else { continue };
            let at = gt.translation();
            if let Some((p, d)) = rooms.portals.iter().enumerate().map(|(i, p)| (i, p.center.distance(at))).min_by(|a, b| a.1.total_cmp(&b.1)) {
                if d < 2.0 {
                    found.push((p, e, occ));
                }
            }
        }
        rooms.doors = found;
    }
    let doors_now: Vec<(usize, [f32; 2])> = rooms.doors.iter().filter_map(|&(p, e, occ)| doors.get(e).ok().map(|(_, _, _, d)| (p, occ.map(|o| o * (1.0 - d.open.clamp(0.0, 1.0)))))).collect();
    for p in rooms.portals.iter_mut() {
        p.door = [0.0; 2];
    }
    for (p, o) in doors_now {
        let d = &mut rooms.portals[p].door;
        *d = [d[0].max(o[0]), d[1].max(o[1])];
    }
    if let Some(mut vm) = vm {
        for (names, player, ai) in std::mem::take(&mut vm.portal_occlusion) {
            for p in rooms.portals.iter_mut().filter(|p| names.contains(&p.name)) {
                p.script = Some([player, ai]);
            }
        }
    }
}

/// The room Corvo hears from sets the ambience (its state event, once on entering).
fn listener_room(mut rooms: ResMut<AudioRooms>, listener: Query<&GlobalTransform, With<SpatialListener>>, mut sfx: MessageWriter<crate::audio::PostEvent>, time: Res<Time>, mut t: Local<f32>) {
    *t -= time.delta_secs();
    if *t > 0.0 || rooms.cells.is_empty() {
        return;
    }
    *t = 0.25;
    let Ok(l) = listener.single() else { return };
    let cell = rooms.cell_at(l.translation());
    if cell == rooms.listener {
        return;
    }
    rooms.listener = cell;
    // the room's reverb (its environment's, from the banks; none outside the rooms)
    let verb = cell.and_then(|c| crate::audio::environment(&rooms.cells[c].environment));
    match verb {
        Some(r) => {
            let damp = (1.0 - 1.0 / r.hf.max(1.0)).clamp(0.0, 0.85) * 0.6;
            crate::speakers::ROOM_REVERB.set(r.decay, damp, 10f32.powf(r.wet_db / 20.0) * 4.0);
        }
        None => crate::speakers::ROOM_REVERB.set(0.0, 0.0, 0.0),
    }
    if std::env::var("DH_AUDIO_LOG").is_ok() {
        info!("audio: reverb {:?}", verb);
    }
    let Some(c) = cell else { return };
    let ev = rooms.cells[c].state_event.clone();
    if !ev.is_empty() && ev != rooms.state {
        if std::env::var("DH_AUDIO_LOG").is_ok() {
            info!("audio: room {} -> {ev}", rooms.cells[c].name);
        }
        sfx.write(crate::audio::PostEvent::named(&ev, None));
        rooms.state = ev;
    }
}
