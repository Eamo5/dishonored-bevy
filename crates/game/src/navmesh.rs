//! Paths over the levels' navigation meshes (the original's `NavigationMeshBase` polygons):
//! the polygon a point stands on, A* over the polygons through the edges they share, then the
//! funnel through those edges for the corners a character walks by.

use crate::level::LevelInfo;
use crate::GameState;
use bevy::prelude::*;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

pub struct NavMeshPlugin;

impl Plugin for NavMeshPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PathBudget>()
            .add_systems(OnEnter(GameState::InGame), build.after(crate::level::LevelSpawnSet))
            .add_systems(Update, pylon_links.run_if(in_state(GameState::InGame)))
            .add_systems(Update, (|mut b: ResMut<PathBudget>| b.0 = PATHS_PER_FRAME).run_if(in_state(GameState::InGame)))
            .add_systems(Update, draw_paths.run_if(in_state(GameState::InGame)).run_if(|| std::env::var("DH_NAV_DRAW").is_ok()));
    }
}

/// How many paths may be searched in a frame.
const PATHS_PER_FRAME: u32 = 12;
const CELL: f32 = 4.0;
/// how far inside an edge's ends the corners keep (m)
const CLEARANCE: f32 = 0.35;

#[derive(Resource, Default)]
pub struct PathBudget(pub u32);

struct Poly {
    verts: Vec<Vec3>,
    center: Vec3,
    lo: f32,
    hi: f32,
    /// (polygon, the shared edge's ends)
    links: Vec<(u32, Vec3, Vec3)>,
}

#[derive(Resource)]
pub struct NavGrid {
    polys: Vec<Poly>,
    cells: HashMap<(i32, i32), Vec<u32>>,
    /// polygons parted from the rest (a mover's, under way): not walked onto
    parted: Vec<bool>,
    /// the movers' meshes: the pylon's actor, its polygons
    dynamic: Vec<(String, u32, u32)>,
}

/// A character's way to its goal.
#[derive(Default, Clone)]
pub struct NavState {
    pub goal: Option<Vec3>,
    pub points: Vec<Vec3>,
    pub next: usize,
    pub age: f32,
}

fn cell(x: f32, z: f32) -> (i32, i32) {
    ((x / CELL).floor() as i32, (z / CELL).floor() as i32)
}

fn build(mut commands: Commands, level: Option<Res<LevelInfo>>) {
    commands.insert_resource(PathBudget(PATHS_PER_FRAME));
    let Some(level) = level else { return };
    let nm = &level.scene.navmesh;
    if nm.polys.is_empty() {
        commands.remove_resource::<NavGrid>();
        return;
    }
    let v = |i: u32| Vec3::from(nm.verts.get(i as usize).copied().unwrap_or_default());
    let mut polys = Vec::with_capacity(nm.polys.len());
    let mut cells: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
    for (pi, p) in nm.polys.iter().enumerate() {
        let verts: Vec<Vec3> = p.verts.iter().map(|&i| v(i)).collect();
        if verts.is_empty() {
            polys.push(Poly { verts, center: Vec3::ZERO, lo: 0.0, hi: 0.0, links: Vec::new() });
            continue;
        }
        let center = verts.iter().copied().sum::<Vec3>() / verts.len() as f32;
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        let (mut mn, mut mx) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
        for q in &verts {
            lo = lo.min(q.y);
            hi = hi.max(q.y);
            mn = mn.min(Vec2::new(q.x, q.z));
            mx = mx.max(Vec2::new(q.x, q.z));
        }
        let (a, b) = (cell(mn.x, mn.y), cell(mx.x, mx.y));
        for cx in a.0..=b.0 {
            for cz in a.1..=b.1 {
                cells.entry((cx, cz)).or_default().push(pi as u32);
            }
        }
        let links = p.links.iter().map(|&(o, a, b)| (o, v(a), v(b))).collect();
        polys.push(Poly { verts, center, lo, hi, links });
    }
    info!("navmesh: {} polygons", polys.len());
    let parted = vec![false; polys.len()];
    commands.insert_resource(NavGrid { polys, cells, parted, dynamic: nm.dynamic.clone() });
}

/// The scripts joining a mover's mesh to the rest, or parting it (`ArkSeqAct_ChangePylonConnection`:
/// parted as the platform sets off, joined as it is back).
fn pylon_links(vm: Option<ResMut<crate::kismet::Vm>>, grid: Option<ResMut<NavGrid>>) {
    let (Some(mut vm), Some(mut grid)) = (vm, grid) else { return };
    if vm.pylon_links.is_empty() {
        return;
    }
    for (name, joined) in std::mem::take(&mut vm.pylon_links) {
        let ranges: Vec<(u32, u32)> = grid.dynamic.iter().filter(|d| d.0 == name).map(|d| (d.1, d.2)).collect();
        for (a, b) in ranges {
            for p in a..b.min(grid.parted.len() as u32) {
                grid.parted[p as usize] = !joined;
            }
        }
        if std::env::var("DH_NAV_LOG").is_ok() {
            info!("navmesh: {name} {}", if joined { "joined" } else { "parted" });
        }
    }
}

/// (debug) the characters' paths, and the mesh's edges near the player.
fn draw_paths(mut gizmos: Gizmos, npcs: Query<(&crate::npc::Npc, &Transform)>, grid: Option<Res<NavGrid>>, player: Query<&Transform, With<crate::player::Player>>) {
    for (n, t) in &npcs {
        if n.target.is_none() || n.nav.points.is_empty() {
            continue;
        }
        let mut prev = t.translation;
        for p in &n.nav.points[n.nav.next.min(n.nav.points.len() - 1)..] {
            let p = *p + Vec3::Y * 0.3;
            gizmos.line(prev, p, Color::srgb(1.0, 0.2, 0.1));
            prev = p;
        }
    }
    let (Some(grid), Ok(pt)) = (grid, player.single()) else { return };
    for p in &grid.polys {
        if p.center.distance(pt.translation) > 25.0 {
            continue;
        }
        for i in 0..p.verts.len() {
            let (a, b) = (p.verts[i], p.verts[(i + 1) % p.verts.len()]);
            gizmos.line(a + Vec3::Y * 0.05, b + Vec3::Y * 0.05, Color::srgba(0.2, 0.9, 1.0, 0.5));
        }
    }
}

/// Twice the signed area of a, b, c on the ground plane (the funnel's turn test).
fn tri2(a: Vec3, b: Vec3, c: Vec3) -> f32 {
    let (ax, az) = (b.x - a.x, b.z - a.z);
    let (bx, bz) = (c.x - a.x, c.z - a.z);
    bx * az - ax * bz
}

#[derive(PartialEq)]
struct Open(f32, u32);
impl Eq for Open {}
impl PartialOrd for Open {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Open {
    fn cmp(&self, o: &Self) -> Ordering {
        o.0.total_cmp(&self.0)
    }
}

impl NavGrid {
    fn inside(p: &Poly, at: Vec3) -> bool {
        let n = p.verts.len();
        if n < 3 {
            return false;
        }
        // convex, either winding: the point is on the same side of every edge
        let mut sign = 0.0f32;
        for i in 0..n {
            let (a, b) = (p.verts[i], p.verts[(i + 1) % n]);
            let c = (b.x - a.x) * (at.z - a.z) - (b.z - a.z) * (at.x - a.x);
            if c.abs() < 1e-5 {
                continue;
            }
            if sign == 0.0 {
                sign = c.signum();
            } else if c.signum() != sign {
                return false;
            }
        }
        true
    }

    /// The polygon under feet at `at` (else the nearest one close by).
    pub fn locate(&self, at: Vec3) -> Option<u32> {
        let (cx, cz) = cell(at.x, at.z);
        let mut best: Option<(f32, u32)> = None;
        for &pi in self.cells.get(&(cx, cz)).into_iter().flatten() {
            let p = &self.polys[pi as usize];
            if at.y < p.lo - 1.0 || at.y > p.hi + 1.6 || !Self::inside(p, at) {
                continue;
            }
            let dy = (at.y - (p.lo + p.hi) * 0.5).abs();
            if best.is_none_or(|b| dy < b.0) {
                best = Some((dy, pi));
            }
        }
        if best.is_some() {
            return best.map(|b| b.1);
        }
        // off the mesh (a step, a table): the nearest polygon around
        for dx in -1..=1 {
            for dz in -1..=1 {
                for &pi in self.cells.get(&(cx + dx, cz + dz)).into_iter().flatten() {
                    let p = &self.polys[pi as usize];
                    if at.y < p.lo - 1.5 || at.y > p.hi + 2.0 {
                        continue;
                    }
                    let d = (p.center - at).with_y(0.0).length() + (at.y - p.center.y).abs();
                    if d < 4.0 && best.is_none_or(|b| d < b.0) {
                        best = Some((d, pi));
                    }
                }
            }
        }
        best.map(|b| b.1)
    }

    /// The corners from feet at `from` to `to` (ending at `to`), or none when either is off the
    /// mesh or they don't connect.
    pub fn path(&self, from: Vec3, to: Vec3) -> Option<Vec<Vec3>> {
        let (start, end) = (self.locate(from)?, self.locate(to)?);
        if start == end {
            return Some(vec![to]);
        }
        // A* over the polygons
        let n = self.polys.len();
        let mut g = vec![f32::MAX; n];
        let mut came: Vec<(u32, u16)> = vec![(u32::MAX, 0); n];
        let mut open = BinaryHeap::new();
        g[start as usize] = 0.0;
        open.push(Open(self.polys[start as usize].center.distance(to), start));
        let mut found = false;
        let mut expanded = 0;
        while let Some(Open(_, cur)) = open.pop() {
            if cur == end {
                found = true;
                break;
            }
            expanded += 1;
            if expanded > 6000 {
                break;
            }
            let cp = &self.polys[cur as usize];
            for (li, &(nb, a, b)) in cp.links.iter().enumerate() {
                // (not onto a mover's mesh parted from the rest, unless it is the way's end)
                if self.parted[nb as usize] && nb != end {
                    continue;
                }
                let mid = (a + b) * 0.5;
                let cost = g[cur as usize] + cp.center.distance(mid) + mid.distance(self.polys[nb as usize].center);
                if cost < g[nb as usize] {
                    g[nb as usize] = cost;
                    came[nb as usize] = (cur, li as u16);
                    open.push(Open(cost + self.polys[nb as usize].center.distance(to), nb));
                }
            }
        }
        if !found {
            return None;
        }
        // the edges crossed, start to end
        let mut portals: Vec<(Vec3, Vec3)> = Vec::new();
        let mut cur = end;
        while cur != start {
            let (prev, li) = came[cur as usize];
            let (_, a, b) = self.polys[prev as usize].links[li as usize];
            // its left and right, going from prev into cur
            let d = self.polys[cur as usize].center - self.polys[prev as usize].center;
            let side = |p: Vec3| d.x * (p.z - a.z) - d.z * (p.x - a.x);
            let (l, r) = if side(b) > side(a) { (b, a) } else { (a, b) };
            // keep clear of the edge's ends
            let len = l.distance(r);
            let k = (CLEARANCE / len.max(1e-3)).min(0.5);
            portals.push((l.lerp(r, k), r.lerp(l, k)));
            cur = prev;
        }
        portals.reverse();
        portals.push((to, to));
        Some(funnel(from, &portals))
    }
}

/// The simple stupid funnel: the corners of the shortest way through the portals.
fn funnel(from: Vec3, portals: &[(Vec3, Vec3)]) -> Vec<Vec3> {
    let mut out = Vec::new();
    let (mut apex, mut left, mut right) = (from, from, from);
    let (mut li, mut ri) = (0usize, 0usize);
    let mut i = 0usize;
    let mut guard = 0;
    while i < portals.len() && guard < 4096 {
        guard += 1;
        let (l, r) = portals[i];
        // tighten the right side
        if tri2(apex, right, r) <= 0.0 {
            if apex == right || tri2(apex, left, r) > 0.0 {
                right = r;
                ri = i;
            } else {
                // right crossed left: the left is a corner
                out.push(left);
                apex = left;
                let k = li;
                right = apex;
                left = apex;
                ri = k;
                li = k;
                i = k + 1;
                continue;
            }
        }
        // tighten the left side
        if tri2(apex, left, l) >= 0.0 {
            if apex == left || tri2(apex, right, l) < 0.0 {
                left = l;
                li = i;
            } else {
                out.push(right);
                apex = right;
                let k = ri;
                right = apex;
                left = apex;
                ri = k;
                li = k;
                i = k + 1;
                continue;
            }
        }
        i += 1;
    }
    if let Some(&(end, _)) = portals.last() {
        if out.last() != Some(&end) {
            out.push(end);
        }
    }
    out
}

/// Where a character heading for `goal` should walk now (feet at `feet`): the next corner of
/// its path, searched again when the goal moves or the path is stale.
pub fn steer(grid: Option<&NavGrid>, budget: &mut PathBudget, nav: &mut NavState, feet: Vec3, goal: Vec3, dt: f32) -> Vec3 {
    let Some(grid) = grid else { return goal };
    nav.age += dt;
    let moved = nav.goal.is_none_or(|g| g.distance(goal) > 0.75);
    if (moved || nav.age > 4.0) && budget.0 > 0 {
        budget.0 -= 1;
        nav.goal = Some(goal);
        nav.age = 0.0;
        nav.next = 0;
        nav.points = grid.path(feet, goal).unwrap_or_default();
    }
    if nav.points.is_empty() {
        return goal;
    }
    while nav.next + 1 < nav.points.len() && (nav.points[nav.next] - feet).with_y(0.0).length() < 0.5 {
        nav.next += 1;
    }
    let p = nav.points[nav.next.min(nav.points.len() - 1)];
    if nav.next + 1 >= nav.points.len() {
        goal
    } else {
        p
    }
}
