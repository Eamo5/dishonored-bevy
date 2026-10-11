//! Playback of the original animations (cooked from the game's AnimSets) on character rigs.
//!
//! Clips are stored as sparse keyframes per bone; frames between keys interpolate linearly,
//! exactly as the original Edge runtime does. An [`Animator`] has a base layer and an
//! overlay layer; each plays one clip at a time and crossfades from the previous one. The
//! overlay only affects the bones its clips animate (e.g. the left arm casting a power).

use bevy::prelude::*;
use dhcook::format::{AnimClip, AnimFile, SkeletonDef};
use std::collections::HashMap;
use std::sync::Arc;

pub struct AnimPlugin;

impl Plugin for AnimPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, pose_animators.in_set(AnimPose).before(TransformSystems::Propagate));
    }
}

/// Animated poses are written to the joints' transforms.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct AnimPose;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct ClipId {
    set: u16,
    clip: u16,
}

/// The clips available to one skeleton, bound to its joints.
pub struct CharAnims {
    sets: Vec<Arc<AnimFile>>,
    /// the sets' names (their anim sets' paths), when known
    set_names: Vec<String>,
    /// per set: file bone -> skeleton joint
    maps: Vec<Vec<Option<u16>>>,
    by_name: HashMap<String, ClipId>,
}

impl CharAnims {
    /// Bind AnimSets to a skeleton by bone name. Later sets override clips of the same
    /// name in earlier ones (as AnimSets do in the original).
    pub fn new(skeleton: &SkeletonDef, sets: Vec<Arc<AnimFile>>) -> CharAnims {
        let index: HashMap<String, u16> =
            skeleton.bones.iter().enumerate().map(|(i, b)| (b.name.to_ascii_lowercase(), i as u16)).collect();
        let maps: Vec<Vec<Option<u16>>> = sets
            .iter()
            .map(|s| s.bones.iter().map(|b| index.get(&b.to_ascii_lowercase()).copied()).collect())
            .collect();
        let mut by_name = HashMap::new();
        for (si, s) in sets.iter().enumerate() {
            for (ci, c) in s.clips.iter().enumerate() {
                by_name.insert(c.name.to_ascii_lowercase(), ClipId { set: si as u16, clip: ci as u16 });
            }
        }
        CharAnims { sets, set_names: Vec::new(), maps, by_name }
    }

    /// Its sets' names (their anim sets' paths, in its sets' order).
    pub fn with_names(mut self, names: Vec<String>) -> CharAnims {
        self.set_names = names;
        self
    }

    /// What the cooker found the notifies mark on its clips (`scene.clip_marks`): those of the
    /// set each clip plays from.
    pub fn marks(&self, scene: &dhcook::format::Scene) -> HashMap<ClipId, dhcook::format::ClipMarks> {
        let mut out = HashMap::new();
        for m in &scene.clip_marks {
            let Some(id) = self.find(&m.clip) else { continue };
            if self.set_names.get(id.set as usize).is_some_and(|n| *n == m.set) {
                out.insert(id, m.clone());
            }
        }
        out
    }

    pub fn find(&self, name: &str) -> Option<ClipId> {
        self.by_name.get(&name.to_ascii_lowercase()).copied()
    }

    /// First of `names` that exists.
    pub fn first(&self, names: &[&str]) -> Option<ClipId> {
        names.iter().find_map(|n| self.find(n))
    }

    /// All of `names` that exist.
    pub fn all(&self, names: &[&str]) -> Vec<ClipId> {
        names.iter().filter_map(|n| self.find(n)).collect()
    }

    pub fn clip(&self, id: ClipId) -> &AnimClip {
        &self.sets[id.set as usize].clips[id.clip as usize]
    }

    /// The first clip whose name starts with `prefix` and ends with `suffix` (any case), by its
    /// own name.
    pub fn find_like(&self, prefix: &str, suffix: &str) -> Option<String> {
        let (p, s) = (prefix.to_ascii_lowercase(), suffix.to_ascii_lowercase());
        self.sets.iter().flat_map(|set| set.clips.iter()).map(|c| &c.name).find(|n| {
            let l = n.to_ascii_lowercase();
            l.starts_with(&p) && l.ends_with(&s)
        }).cloned()
    }

    /// A bone's place at the clip's start (its own track's first key, in the file's units),
    /// e.g. a sync clip's `anchor_jnt`: where its partner stands.
    pub fn bone_start(&self, id: ClipId, bone: &str) -> Option<Vec3> {
        let set = &self.sets[id.set as usize];
        let b = set.bones.iter().position(|n| n.eq_ignore_ascii_case(bone))? as u16;
        set.clips[id.clip as usize].translations.iter().find(|t| t.bone == b).and_then(|t| t.values.first()).map(|v| Vec3::from(*v))
    }

    pub fn duration(&self, id: ClipId) -> f32 {
        self.clip(id).duration.max(1e-3)
    }

    /// Ground speed of the clip's root motion (m/s).
    pub fn root_speed(&self, id: ClipId) -> f32 {
        let c = self.clip(id);
        let m = Vec3::from(c.root_motion);
        Vec2::new(m.x, m.z).length().max(Vec2::new(m.x, m.y).length()) / c.duration.max(1e-3)
    }

    /// An additive clip's (`bIsAdditive`: each bone's difference from a reference pose) change
    /// to the joints it moves at time `t`, weighted, gathered into `out` (joint, rotation,
    /// translation).
    pub fn additive(&self, id: ClipId, t: f32, w: f32, out: &mut Vec<(usize, Quat, Vec3)>) {
        let c = self.clip(id);
        let map = &self.maps[id.set as usize];
        let f = (t.rem_euclid(c.duration.max(1e-3)) * c.rate).clamp(0.0, (c.frames.max(1) - 1) as f32);
        let slot = |j: usize, out: &mut Vec<(usize, Quat, Vec3)>| -> usize {
            match out.iter().position(|d| d.0 == j) {
                Some(i) => i,
                None => {
                    out.push((j, Quat::IDENTITY, Vec3::ZERO));
                    out.len() - 1
                }
            }
        };
        for tr in &c.rotations {
            let Some(Some(j)) = map.get(tr.bone as usize) else { continue };
            let (k, a) = key_at(&tr.frames, f);
            let q0 = Quat::from_array(tr.values[k]);
            let q = if a > 0.0 {
                let mut q1 = Quat::from_array(tr.values[k + 1]);
                if q0.dot(q1) < 0.0 {
                    q1 = -q1;
                }
                (q0 * (1.0 - a) + q1 * a).normalize()
            } else {
                q0.normalize()
            };
            let i = slot(*j as usize, out);
            out[i].1 = Quat::IDENTITY.slerp(q, w) * out[i].1;
        }
        for tr in &c.translations {
            let Some(Some(j)) = map.get(tr.bone as usize) else { continue };
            let (k, a) = key_at(&tr.frames, f);
            let v0 = Vec3::from(tr.values[k]);
            let v = if a > 0.0 { v0.lerp(Vec3::from(tr.values[k + 1]), a) } else { v0 };
            let i = slot(*j as usize, out);
            out[i].2 += v * w;
        }
    }

    /// Write the clip's pose at time `t` into the joints it animates.
    fn sample(&self, id: ClipId, t: f32, rot: &mut [Quat], trans: &mut [Vec3]) {
        let c = self.clip(id);
        let map = &self.maps[id.set as usize];
        let f = (t * c.rate).clamp(0.0, (c.frames.max(1) - 1) as f32);
        for tr in &c.rotations {
            let Some(Some(j)) = map.get(tr.bone as usize) else { continue };
            let Some(slot) = rot.get_mut(*j as usize) else { continue };
            let (k, a) = key_at(&tr.frames, f);
            let q0 = Quat::from_array(tr.values[k]);
            *slot = if a > 0.0 {
                let mut q1 = Quat::from_array(tr.values[k + 1]);
                if q0.dot(q1) < 0.0 {
                    q1 = -q1;
                }
                (q0 * (1.0 - a) + q1 * a).normalize()
            } else {
                q0.normalize()
            };
        }
        for tr in &c.translations {
            let Some(Some(j)) = map.get(tr.bone as usize) else { continue };
            let Some(slot) = trans.get_mut(*j as usize) else { continue };
            let (k, a) = key_at(&tr.frames, f);
            let v0 = Vec3::from(tr.values[k]);
            *slot = if a > 0.0 { v0.lerp(Vec3::from(tr.values[k + 1]), a) } else { v0 };
        }
    }
}

/// Key index at or before frame `f`, and the blend towards the next key.
fn key_at(frames: &[u16], f: f32) -> (usize, f32) {
    let n = frames.len();
    if n <= 1 {
        return (0, 0.0);
    }
    let k = frames.partition_point(|&x| (x as f32) <= f).saturating_sub(1).min(n - 1);
    if k + 1 >= n {
        return (k, 0.0);
    }
    let (a, b) = (frames[k] as f32, frames[k + 1] as f32);
    (k, ((f - a) / (b - a).max(1e-6)).clamp(0.0, 1.0))
}

#[derive(Clone, Copy, Debug)]
pub struct Playing {
    pub clip: ClipId,
    pub t: f32,
    pub speed: f32,
    pub looping: bool,
    /// just started: notifies at time 0 still fire
    fresh: bool,
}

/// One clip at a time, crossfading from the previous one.
#[derive(Default)]
pub struct Layer {
    pub cur: Option<Playing>,
    prev: Option<Playing>,
    fade: f32,
    fade_len: f32,
    weight: f32,
    target: f32,
    weight_rate: f32,
}

impl Layer {
    fn advance(&mut self, lib: &CharAnims, dt: f32, sounds: &mut Vec<String>) {
        let step = |p: &mut Playing| {
            let d = lib.duration(p.clip);
            p.t += dt * p.speed;
            p.t = if p.looping { p.t.rem_euclid(d) } else { p.t.clamp(0.0, d) };
        };
        if let Some(c) = &mut self.cur {
            let t0 = if c.fresh { -1.0 } else { c.t };
            c.fresh = false;
            step(c);
            // sound notifies the playhead crossed (forward playback, with wrap-around)
            let clip = lib.clip(c.clip);
            if !clip.sounds.is_empty() && c.speed > 0.0 && dt > 0.0 {
                let t1 = c.t;
                for (t, ev) in &clip.sounds {
                    let hit = if t1 >= t0 { *t > t0 && *t <= t1 } else { *t > t0 || *t <= t1 };
                    if hit {
                        sounds.push(ev.clone());
                    }
                }
            }
        }
        if let Some(p) = &mut self.prev {
            step(p);
        }
        self.fade = (self.fade - dt).max(0.0);
        if self.fade <= 0.0 {
            self.prev = None;
        }
        if self.weight != self.target {
            let s = dt * self.weight_rate;
            self.weight = if self.weight < self.target { (self.weight + s).min(self.target) } else { (self.weight - s).max(self.target) };
        }
        if self.weight <= 0.0 && self.target <= 0.0 {
            self.cur = None;
            self.prev = None;
        }
    }

    fn play(&mut self, clip: ClipId, looping: bool, speed: f32, blend: f32) {
        if let Some(c) = &mut self.cur {
            if c.clip == clip {
                c.speed = speed;
                c.looping = looping;
                return;
            }
        }
        self.restart(clip, looping, speed, blend);
    }

    fn restart(&mut self, clip: ClipId, looping: bool, speed: f32, blend: f32) {
        self.prev = if blend > 0.0 { self.cur } else { None };
        self.fade_len = blend;
        self.fade = blend;
        self.cur = Some(Playing { clip, t: 0.0, speed, looping, fresh: true });
    }

    fn finished(&self, lib: &CharAnims) -> bool {
        match self.cur {
            Some(c) => !c.looping && c.t >= lib.duration(c.clip) - 1e-3,
            None => true,
        }
    }
}

/// Plays clips on a rig (`joints` are the skeleton's joint entities).
#[derive(Component)]
pub struct Animator {
    pub lib: Arc<CharAnims>,
    joints: Vec<Entity>,
    parents: Vec<i32>,
    bind_rot: Vec<Quat>,
    bind_t: Vec<Vec3>,
    base: Layer,
    overlay: Layer,
    /// Multiplies the playback rate (Bend Time).
    pub time_scale: f32,
    /// Skip posing (e.g. far away); the clock still runs.
    pub frozen: bool,
    rot: Vec<Quat>,
    trans: Vec<Vec3>,
    rot_a: Vec<Quat>,
    trans_a: Vec<Vec3>,
    rot_b: Vec<Quat>,
    trans_b: Vec<Vec3>,
    /// play animation sound notifies (off for silent rigs)
    pub sounds: bool,
    pending: Vec<String>,
}

impl Animator {
    pub fn new(lib: Arc<CharAnims>, skeleton: &SkeletonDef, joints: Vec<Entity>) -> Animator {
        let bind_rot: Vec<Quat> = skeleton.bones.iter().map(|b| Quat::from_array(b.rotation).normalize()).collect();
        let bind_t: Vec<Vec3> = skeleton.bones.iter().map(|b| Vec3::from(b.translation)).collect();
        Animator {
            lib,
            joints,
            parents: skeleton.bones.iter().map(|b| b.parent).collect(),
            rot: bind_rot.clone(),
            trans: bind_t.clone(),
            rot_a: bind_rot.clone(),
            trans_a: bind_t.clone(),
            rot_b: bind_rot.clone(),
            trans_b: bind_t.clone(),
            bind_rot,
            bind_t,
            base: Layer { weight: 1.0, target: 1.0, ..Default::default() },
            overlay: Layer::default(),
            time_scale: 1.0,
            frozen: false,
            sounds: true,
            pending: Vec::new(),
        }
    }

    /// Switch to `clip` (crossfading over `blend` seconds); keeps playing if it's current.
    pub fn play(&mut self, clip: ClipId, looping: bool, speed: f32, blend: f32) {
        self.base.play(clip, looping, speed, blend);
    }

    /// Start `clip` from the beginning even if it's already playing.
    pub fn restart(&mut self, clip: ClipId, looping: bool, speed: f32, blend: f32) {
        self.base.restart(clip, looping, speed, blend);
    }

    /// What the base layer plays.
    pub fn current(&self) -> Option<Playing> {
        self.base.cur
    }

    /// Seek the current clip (seconds).
    pub fn seek(&mut self, t: f32) {
        if let Some(c) = &mut self.base.cur {
            c.t = t;
        }
    }

    /// The current one-shot clip has reached its end.
    pub fn finished(&self) -> bool {
        self.base.finished(&self.lib)
    }

    /// Play `clip` on the overlay layer, fading the layer in.
    pub fn play_overlay(&mut self, clip: ClipId, looping: bool, speed: f32, blend: f32) {
        self.overlay.play(clip, looping, speed, blend);
        self.overlay_in(blend);
    }

    pub fn restart_overlay(&mut self, clip: ClipId, looping: bool, speed: f32, blend: f32) {
        self.overlay.restart(clip, looping, speed, blend);
        self.overlay_in(blend);
    }

    fn overlay_in(&mut self, blend: f32) {
        self.overlay.target = 1.0;
        self.overlay.weight_rate = 1.0 / blend.max(0.01);
        if self.overlay.weight <= 0.0 {
            // fresh start: no crossfade from a stale clip
            self.overlay.prev = None;
        }
    }

    /// Fade the overlay layer out.
    pub fn stop_overlay(&mut self, blend: f32) {
        self.overlay.target = 0.0;
        self.overlay.weight_rate = 1.0 / blend.max(0.01);
    }

    pub fn overlay_finished(&self) -> bool {
        self.overlay.finished(&self.lib)
    }

    pub fn overlay_is(&self, clip: Option<ClipId>) -> bool {
        clip.is_some() && self.overlay.target > 0.0 && self.overlay.cur.map(|c| c.clip) == clip
    }

    /// Joint `j` relative to the skeleton root (current pose).
    pub fn model(&self, j: usize) -> Transform {
        let mut q = Quat::IDENTITY;
        let mut t = Vec3::ZERO;
        let mut i = j as i32;
        // accumulate from the joint up to the root: x_parent = R_i * x + T_i
        while i >= 0 && (i as usize) < self.rot.len() {
            let r = self.rot[i as usize];
            t = r * t + self.trans[i as usize];
            q = r * q;
            i = self.parents[i as usize];
            if i as usize >= self.rot.len() {
                break;
            }
        }
        Transform::from_translation(t).with_rotation(q)
    }

    /// Joint `j` relative to the skeleton root with additive changes on some joints (each's
    /// rotation after the pose's, its translation added: UE3's additive blending).
    pub fn model_with(&self, j: usize, deltas: &[(usize, Quat, Vec3)]) -> Transform {
        let mut q = Quat::IDENTITY;
        let mut t = Vec3::ZERO;
        let mut i = j as i32;
        while i >= 0 && (i as usize) < self.rot.len() {
            let (mut r, mut tr) = (self.rot[i as usize], self.trans[i as usize]);
            if let Some(d) = deltas.iter().find(|d| d.0 == i as usize) {
                r = d.1 * r;
                tr += d.2;
            }
            t = r * t + tr;
            q = r * q;
            i = self.parents[i as usize];
            if i as usize >= self.rot.len() {
                break;
            }
        }
        Transform::from_translation(t).with_rotation(q)
    }

    /// Joint `j` relative to the skeleton root in the bind pose.
    pub fn bind_model(&self, j: usize) -> Transform {
        let mut q = Quat::IDENTITY;
        let mut t = Vec3::ZERO;
        let mut i = j as i32;
        while i >= 0 && (i as usize) < self.bind_rot.len() {
            let r = self.bind_rot[i as usize];
            t = r * t + self.bind_t[i as usize];
            q = r * q;
            i = self.parents[i as usize];
        }
        Transform::from_translation(t).with_rotation(q)
    }

    fn advance(&mut self, dt: f32) {
        let lib = self.lib.clone();
        let mut pending = std::mem::take(&mut self.pending);
        pending.clear();
        self.base.advance(&lib, dt, &mut pending);
        // the overlay's notifies only while it's audible
        let mut ov = Vec::new();
        self.overlay.advance(&lib, dt, &mut ov);
        if self.overlay.weight > 0.5 {
            pending.extend(ov);
        }
        self.pending = pending;
    }

    fn pose(&mut self) {
        self.rot.copy_from_slice(&self.bind_rot);
        self.trans.copy_from_slice(&self.bind_t);
        for li in 0..2 {
            let layer = if li == 0 { &self.base } else { &self.overlay };
            let Some(cur) = layer.cur else { continue };
            let weight = if li == 0 { 1.0 } else { smooth(layer.weight) };
            if weight <= 0.0 {
                continue;
            }
            let prev = layer.prev;
            let wprev = smooth((layer.fade / layer.fade_len.max(1e-4)).clamp(0.0, 1.0));
            self.rot_a.copy_from_slice(&self.rot);
            self.trans_a.copy_from_slice(&self.trans);
            self.lib.sample(cur.clip, cur.t, &mut self.rot_a, &mut self.trans_a);
            if let Some(p) = prev {
                self.rot_b.copy_from_slice(&self.rot);
                self.trans_b.copy_from_slice(&self.trans);
                self.lib.sample(p.clip, p.t, &mut self.rot_b, &mut self.trans_b);
                mix(&mut self.rot_a, &mut self.trans_a, &self.rot_b, &self.trans_b, wprev);
            }
            if weight >= 1.0 {
                self.rot.copy_from_slice(&self.rot_a);
                self.trans.copy_from_slice(&self.trans_a);
            } else {
                let (ra, ta) = (std::mem::take(&mut self.rot_a), std::mem::take(&mut self.trans_a));
                mix(&mut self.rot, &mut self.trans, &ra, &ta, weight);
                self.rot_a = ra;
                self.trans_a = ta;
            }
        }
    }
}

fn smooth(w: f32) -> f32 {
    let w = w.clamp(0.0, 1.0);
    w * w * (3.0 - 2.0 * w)
}

/// `a = lerp(a, b, w)` per joint (normalized quaternion lerp).
fn mix(rot: &mut [Quat], trans: &mut [Vec3], rot_b: &[Quat], trans_b: &[Vec3], w: f32) {
    if w <= 0.0 {
        return;
    }
    for i in 0..rot.len() {
        let a = rot[i];
        let mut b = rot_b[i];
        if a.dot(b) < 0.0 {
            b = -b;
        }
        rot[i] = (a * (1.0 - w) + b * w).normalize();
        trans[i] = trans[i].lerp(trans_b[i], w);
    }
}

fn pose_animators(
    time: Res<Time>,
    mut animators: Query<(&mut Animator, &GlobalTransform)>,
    mut transforms: Query<&mut Transform, Without<Animator>>,
    mut sounds: MessageWriter<crate::audio::PostEvent>,
) {
    let dt = time.delta_secs();
    for (mut a, gt) in &mut animators {
        let scale = a.time_scale;
        a.advance(dt * scale);
        if a.sounds {
            for ev in &a.pending {
                sounds.write(crate::audio::PostEvent::named(ev, Some(gt.translation())));
            }
        }
        if a.frozen {
            continue;
        }
        a.pose();
        let a = a.into_inner();
        for (i, &e) in a.joints.iter().enumerate() {
            if let Ok(mut t) = transforms.get_mut(e) {
                if t.rotation != a.rot[i] || t.translation != a.trans[i] {
                    t.rotation = a.rot[i];
                    t.translation = a.trans[i];
                }
            }
        }
    }
}
