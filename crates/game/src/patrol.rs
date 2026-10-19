//! Patrols as the original walks them (`DisBehaviorPatrol`, `DisPatrolManager`,
//! `DishonoredRoute`, decoded for this rewrite from the user's own Dishonored.exe, after roshan's
//! r614/blink research on the routes' adoption):
//! - A character patrols once asked to: its spawner sets it out (`m_bPatrolUponStartup`), or
//!   the scripts do (`DisSeqAct_AISetPatrol` Start, maybe from a point to walk to first). A
//!   guard order or the scripts' Stop ends it, its route let go.
//! - It takes a route (0x00c41590): one active, of its squad (`m_SupportedSquads`; none: any),
//!   not full (`m_Capacity`), within `m_AdoptionRange` of the route's own place. The nearest
//!   neglected one wins, at any distance (unwalked longer than `m_NeglectThreshold`, 30 s; every
//!   route starts neglected), else the nearest at all if it has none. Nearest is by the route's
//!   closest point; it starts there, or at the next point if it is already past that one. With
//!   none to take it stands where it is and tries again every moment.
//! - It walks it (0x00afa730): on round a circle, back and forth (`ERT_PingPong`), to the end of
//!   a line and no further, or from point to random point. Each time it comes round to where it
//!   started counts; after `m_PatrolAttentionSpan` (3) it goes over to a neglected route if
//!   there is one.
//! - A point it guards (`m_bGuardForever`, `m_fGuardDuration`, watch points) it stops on, facing
//!   the point's way (`m_bDirectionMatters`), for exactly its time (0x00af3870), or for ever.
//!   With watch points it watches each in turn for its time instead, once round
//!   (`m_WatchPointOrdering`). Other points it walks through without stopping.

use crate::level::LevelInfo;
use crate::npc::{Mode, Npc};
use crate::GameState;
use bevy::prelude::*;
use dhcook::format::Route;

pub struct PatrolPlugin;

impl Plugin for PatrolPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Patrols>()
            .add_systems(OnEnter(GameState::InGame), setup.after(crate::level::LevelSpawnSet))
            .add_systems(Update, (tally, watch_look).run_if(in_state(GameState::InGame)));
    }
}

/// The level's routes as they stand: on (`m_bIsActive`), how many walk them, and how long
/// none has.
#[derive(Resource, Default)]
pub struct Patrols {
    pub routes: Vec<RouteState>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RouteState {
    pub active: bool,
    pub adopters: u32,
    pub neglect: f32,
}

/// How long a route goes unwalked before it is neglected, where it doesn't say (s).
pub const NEGLECT: f32 = 30.0;
/// How many rounds before a guard looks for a neglected route, where its route doesn't say.
pub const SPAN: i32 = 3;
/// How near a point counts as reached: one it stops at, one it walks through (m).
const ARRIVE: f32 = 0.6;
const PASS: f32 = 1.0;

impl Patrols {
    pub fn of(routes: &[Route]) -> Self {
        // (every route starts neglected: its timer set past its threshold, 0x00a4bb50)
        Patrols { routes: routes.iter().map(|r| RouteState { active: !r.inactive, adopters: 0, neglect: r.neglect.unwrap_or(NEGLECT) + 1.0 }).collect() }
    }

    /// The scripts switch a route on, off (`SeqAct_Toggle` on a `DishonoredRoute`) or over.
    pub fn toggle(&mut self, route: usize, on: Option<bool>) {
        if let Some(r) = self.routes.get_mut(route) {
            r.active = on.unwrap_or(!r.active);
        }
    }

    fn can_adopt(&self, routes: &[Route], i: usize, squad: &str, pos: Vec3) -> bool {
        let (Some(r), Some(st)) = (routes.get(i), self.routes.get(i)) else { return false };
        st.active
            && !r.points.is_empty()
            && (r.squads.is_empty() || r.squads.iter().any(|s| s == squad))
            && r.capacity.is_none_or(|c| st.adopters < c)
            && r.range.is_none_or(|range| pos.distance(Vec3::from(r.at)) < range)
    }
}

/// A character's patrol: whether it has one, the route it walks and where on it, what it
/// guards. `on` without a route: it waits for one.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Patrol {
    pub on: bool,
    pub route: Option<usize>,
    pub idx: usize,
    pub dir: i32,
    /// where it took the route up, and how many times round since
    pub start: usize,
    pub reps: i32,
    /// a line walked to its end: it stays
    pub done: bool,
    /// a point to walk to first: on a route (route, point) or alone
    pub first: Option<First>,
    #[serde(skip)]
    pub guard: Option<Guard>,
    #[serde(skip)]
    pub last_random: Option<usize>,
    /// what it faces and watches, guarding
    #[serde(skip)]
    pub face: Option<f32>,
    #[serde(skip)]
    pub look: Option<Vec3>,
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum First {
    OnRoute(usize, usize),
    At([f32; 3]),
}

/// Standing guard at a point: its time left (for ever), and its watch.
#[derive(Clone, Debug, PartialEq)]
pub struct Guard {
    route: usize,
    idx: usize,
    timer: f32,
    forever: bool,
    watch: Option<Watch>,
}

/// Watching a point's watch points in turn: which, which way, the random order's bag, the
/// time left on this one, the round done.
#[derive(Clone, Debug, PartialEq)]
struct Watch {
    i: usize,
    dir: i32,
    bag: Vec<usize>,
    left: f32,
    done: bool,
}

/// What a patrol has its character do now: walk to `goal` (through it, `pass`), or stand
/// facing `face`, watching `look`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Order {
    pub goal: Option<Vec3>,
    pub pass: bool,
}

fn rand_in(t: [f32; 2]) -> f32 {
    let (a, b) = (t[0].min(t[1]), t[0].max(t[1]));
    a + rand::random::<f32>() * (b - a)
}

fn is_stop(r: &Route, i: usize) -> bool {
    r.stops.get(i).is_some_and(|s| *s != 0.0) || r.watch.get(i).is_some_and(|w| !w.is_empty())
}

/// The point after `idx` going `dir` (`DishonoredRoute::ResolveRouteIndex` 0x00a47130): the
/// index and the way on, or none at a line's end.
fn resolve(r: &Route, idx: i32, dir: i32) -> Option<(usize, i32)> {
    let n = r.points.len() as i32;
    let k = idx + dir;
    let line = r.kind.ends_with("Linear");
    let pingpong = r.kind.ends_with("PingPong");
    if k >= n {
        if line {
            return None;
        }
        return Some(if pingpong { ((n - 2).max(0) as usize, -1) } else { (0, dir) });
    }
    if k < 0 {
        if line {
            return None;
        }
        return Some(if pingpong { (1.min(n - 1) as usize, 1) } else { ((n - 1) as usize, dir) });
    }
    Some((k as usize, dir))
}

/// Where a guard taking up a route at `pos` starts (0x00a4c080): its nearest point, or the next
/// if it is already past that one toward it.
fn start_index(r: &Route, pos: Vec3) -> usize {
    let p = |i: usize| Vec3::from(r.points[i]);
    let n = (0..r.points.len()).min_by(|a, b| p(*a).distance_squared(pos).total_cmp(&p(*b).distance_squared(pos))).unwrap_or(0);
    match resolve(r, n as i32, 1) {
        Some((next, _)) if pos.distance(p(next)) < p(n).distance(p(next)) => next,
        Some(_) => n,
        None => r.points.len() - 1,
    }
}

impl Patrol {
    /// Asked to patrol (from a point to walk to first): its old route let go.
    pub fn begin(&mut self, st: &mut Patrols, first: Option<First>) {
        self.release(st);
        *self = Patrol { on: true, dir: 1, first, ..Default::default() };
    }

    /// No longer patrolling (a guard order, the scripts' Stop, down): its route let go.
    pub fn stop(&mut self, st: &mut Patrols) {
        self.release(st);
        *self = Patrol::default();
    }

    fn release(&mut self, st: &mut Patrols) {
        if let Some(r) = self.route.take().and_then(|r| st.routes.get_mut(r)) {
            r.adopters = r.adopters.saturating_sub(1);
        }
    }

    /// The point it walks to now (or stands at).
    pub fn point(&self, routes: &[Route]) -> Option<Vec3> {
        match self.first {
            Some(First::OnRoute(r, i)) => routes.get(r).and_then(|r| r.points.get(i)).map(|p| Vec3::from(*p)),
            Some(First::At(p)) => Some(Vec3::from(p)),
            None => self.route.and_then(|r| routes.get(r)).and_then(|r| r.points.get(self.idx)).map(|p| Vec3::from(*p)),
        }
    }

    /// Take up a route (0x00c41590): the nearest neglected one, else (with none) the nearest.
    fn adopt(&mut self, routes: &[Route], st: &mut Patrols, squad: &str, pos: Vec3, neglected_only: bool) -> bool {
        self.reps = 0;
        let mut best: [Option<(f32, usize)>; 2] = [None, None];
        for i in 0..routes.len() {
            if Some(i) == self.route || !st.can_adopt(routes, i, squad, pos) {
                continue;
            }
            let d = routes[i].points.iter().map(|q| Vec3::from(*q).distance_squared(pos)).fold(f32::MAX, f32::min);
            let neglected = st.routes[i].neglect > routes[i].neglect.unwrap_or(NEGLECT);
            let slot = &mut best[!neglected as usize];
            if slot.is_none_or(|b| d < b.0) {
                *slot = Some((d, i));
            }
        }
        let pick = best[0].or(best[1].filter(|_| self.route.is_none() && !neglected_only));
        let Some((_, i)) = pick else { return false };
        self.release(st);
        st.routes[i].adopters += 1;
        st.routes[i].neglect = 0.0;
        self.route = Some(i);
        self.idx = start_index(&routes[i], pos);
        self.start = self.idx;
        self.dir = 1;
        self.done = false;
        self.guard = None;
        self.last_random = None;
        true
    }

    /// On to the next point (0x00afa730), counting the rounds; false where it stays.
    fn next(&mut self, routes: &[Route], st: &mut Patrols, squad: &str, pos: Vec3) -> bool {
        let Some(r) = self.route.and_then(|r| routes.get(r)) else { return false };
        let n = r.points.len();
        let next = if r.kind.ends_with("Random") && n > 0 {
            let mut k = rand::random::<u32>() as usize % n;
            if Some(k) == self.last_random {
                k = (k + 1) % n;
            }
            self.last_random = Some(k);
            k
        } else {
            match resolve(r, self.idx as i32, self.dir) {
                Some((k, dir)) => {
                    self.dir = dir;
                    k
                }
                None => {
                    self.done = true;
                    return false;
                }
            }
        };
        if next == self.start {
            self.reps += 1;
            let span = r.span.unwrap_or(SPAN);
            if span >= 0 && self.reps >= span && self.adopt(routes, st, squad, pos, true) {
                return true;
            }
        }
        if next == self.idx {
            return false;
        }
        self.idx = next;
        true
    }

    fn guard_at(r: &Route, route: usize, idx: usize) -> Guard {
        let stop = r.stops.get(idx).copied().unwrap_or(0.0);
        let watch = r.watch.get(idx).filter(|w| !w.is_empty()).map(|w| {
            let order = r.watch_order.get(idx).copied().unwrap_or(0);
            let mut bag: Vec<usize> = (0..w.len()).collect();
            let i = if order == 2 { bag.swap_remove(rand::random::<u32>() as usize % bag.len()) } else { 0 };
            Watch { i, dir: 1, bag, left: rand_in(w[i].1), done: false }
        });
        Guard { route, idx, timer: stop.max(0.0), forever: stop < 0.0, watch }
    }

    /// A moment of it: where it goes, or what it faces and watches standing.
    pub fn step(&mut self, routes: &[Route], st: &mut Patrols, squad: &str, pos: Vec3, dt: f32) -> Order {
        self.face = None;
        self.look = None;
        if !self.on {
            return Order::default();
        }
        // a point the scripts sent it to first: there, it guards it if it is one to guard,
        // then takes up the nearest route from there
        if let Some(first) = self.first {
            if self.guard.is_none() {
                let at = self.point(routes).unwrap_or(pos);
                if (at - pos).with_y(0.0).length() > ARRIVE {
                    return Order { goal: Some(at), pass: false };
                }
                if let First::OnRoute(ri, i) = first {
                    if routes.get(ri).is_some_and(|r| is_stop(r, i)) {
                        self.guard = Some(Self::guard_at(&routes[ri], ri, i));
                    }
                }
            }
            if self.guard.is_some() && !self.tick_guard(routes, dt) {
                return Order::default();
            }
            self.guard = None;
            self.first = None;
            self.release(st);
            self.adopt(routes, st, squad, pos, false);
        }
        // (none to take: it stands, and tries again)
        if self.route.is_none() && !self.adopt(routes, st, squad, pos, false) {
            return Order::default();
        }
        let Some(ri) = self.route else { return Order::default() };
        let Some(r) = routes.get(ri) else { return Order::default() };
        if self.done {
            return Order::default();
        }
        if self.guard.is_some() {
            if !self.tick_guard(routes, dt) {
                return Order::default();
            }
            // its time up: on (or, with nowhere else to go, it stays and asks again)
            if self.next(routes, st, squad, pos) {
                self.guard = None;
            } else {
                if let Some(g) = &mut self.guard {
                    g.watch = None;
                }
                self.face = r.yaws.get(self.idx).copied().flatten();
                return Order::default();
            }
        }
        let Some(r) = self.route.and_then(|r| routes.get(r)) else { return Order::default() };
        let ri = self.route.unwrap_or(ri);
        let goal = Vec3::from(r.points[self.idx.min(r.points.len() - 1)]);
        let stop = is_stop(r, self.idx);
        if (goal - pos).with_y(0.0).length() > if stop { ARRIVE } else { PASS } {
            return Order { goal: Some(goal), pass: !stop };
        }
        if stop {
            self.guard = Some(Self::guard_at(r, ri, self.idx));
            self.tick_guard(routes, 0.0);
            return Order::default();
        }
        if self.next(routes, st, squad, pos) {
            if let (Some(g), Some(r)) = (self.point(routes), self.route.and_then(|r| routes.get(r))) {
                return Order { goal: Some(g), pass: !is_stop(r, self.idx) };
            }
        }
        Order::default()
    }

    /// Its guard ticking (0x00afa8e0): facing the point's way, watching; true once its time is
    /// up (never for ever).
    fn tick_guard(&mut self, routes: &[Route], dt: f32) -> bool {
        let Some(g) = &mut self.guard else { return true };
        let Some(r) = routes.get(g.route) else { return true };
        self.face = r.yaws.get(g.idx).copied().flatten();
        if let Some(w) = &mut g.watch {
            let pts = &r.watch[g.idx];
            let order = r.watch_order.get(g.idx).copied().unwrap_or(0);
            w.left -= dt;
            if w.left <= 0.0 && !w.done {
                // the next (0x00b3aaf0): in turn, back and forth, or from the bag
                match order {
                    1 => {
                        let k = w.i as i32 + w.dir;
                        if pts.len() <= 1 {
                            w.done = true;
                        } else if k >= pts.len() as i32 {
                            w.dir = -1;
                            w.i = pts.len() - 2;
                        } else if k < 0 {
                            w.done = true;
                        } else {
                            w.i = k as usize;
                        }
                    }
                    2 => {
                        if w.bag.is_empty() {
                            w.done = true;
                        } else {
                            w.i = w.bag.swap_remove(rand::random::<u32>() as usize % w.bag.len());
                        }
                    }
                    _ => {
                        w.i += 1;
                        if w.i >= pts.len() {
                            w.done = true;
                        }
                    }
                }
                if !w.done {
                    w.left = rand_in(pts[w.i].1);
                }
            }
            if !w.done {
                self.look = Some(Vec3::from(pts[w.i].0));
                return false;
            }
            g.timer = 0.0;
        } else {
            g.timer -= dt;
        }
        !g.forever && g.timer < 1e-8
    }
}

/// The routes reset with the level (all neglected).
fn setup(level: Option<Res<LevelInfo>>, mut patrols: ResMut<Patrols>) {
    *patrols = level.map(|l| Patrols::of(&l.scene.routes)).unwrap_or_default();
}

/// Who walks each route (counted afresh: a fallen guard's route freed), and how long none has
/// (0x00a437f0: counting while it has none).
fn tally(time: Res<Time>, mut patrols: ResMut<Patrols>, mut npcs: Query<&mut Npc>) {
    let mut count = vec![0u32; patrols.routes.len()];
    for mut n in &mut npcs {
        if n.is_down() && n.patrol.on {
            n.patrol = Patrol::default();
            continue;
        }
        if let Some(c) = n.patrol.route.filter(|_| n.patrol.on).and_then(|r| count.get_mut(r)) {
            *c += 1;
        }
    }
    let dt = time.delta_secs();
    for (r, c) in patrols.routes.iter_mut().zip(count) {
        r.adopters = c;
        if c < 1 {
            r.neglect += dt;
        }
    }
}

/// A guard watching its watch points looks at each (head and torso, as a scene's look), unless
/// Corvo holds its attention or a scene its head.
#[derive(Component)]
struct WatchLook;

fn watch_look(mut commands: Commands, npcs: Query<(Entity, &Npc, Has<WatchLook>, Has<crate::npc::SceneLook>, Has<crate::npc::ScriptedAnim>)>) {
    for (e, npc, mine, look, scripted) in &npcs {
        let busy = npc.attn_level >= 1 && npc.last_seen.is_some();
        let want = npc.patrol.look.filter(|_| npc.mode == Mode::Patrol && !busy && !scripted && !npc.is_down());
        match want {
            Some(target) if mine || !look => {
                commands.entity(e).try_insert((WatchLook, crate::npc::SceneLook { target, weight: 1.0 }));
            }
            None if mine => {
                commands.entity(e).try_remove::<(WatchLook, crate::npc::SceneLook)>();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(kind: &str, pts: &[[f32; 3]], stops: &[f32]) -> Route {
        Route {
            name: "r".into(),
            kind: kind.into(),
            points: pts.to_vec(),
            stops: stops.to_vec(),
            yaws: vec![Some(1.0); pts.len()],
            watch: vec![Vec::new(); pts.len()],
            watch_order: vec![0; pts.len()],
            point_names: Vec::new(),
            at: [0.0; 3],
            span: None,
            capacity: Some(1),
            squads: vec!["a".into()],
            range: None,
            inactive: false,
            neglect: None,
        }
    }

    #[test]
    fn routes_end_wrap_and_bounce_as_their_type_says() {
        let pts = [[0.0, 0.0, 0.0], [5.0, 0.0, 0.0], [10.0, 0.0, 0.0]];
        assert_eq!(resolve(&route("ERT_Circle", &pts, &[]), 2, 1), Some((0, 1)));
        assert_eq!(resolve(&route("ERT_PingPong", &pts, &[]), 2, 1), Some((1, -1)));
        assert_eq!(resolve(&route("ERT_PingPong", &pts, &[]), 0, -1), Some((1, 1)));
        assert_eq!(resolve(&route("ERT_Linear", &pts, &[]), 2, 1), None);
        // (already past the nearest point toward the next: the next)
        assert_eq!(start_index(&route("ERT_Circle", &pts, &[]), Vec3::new(6.0, 0.0, 0.0)), 2);
        assert_eq!(start_index(&route("ERT_Circle", &pts, &[]), Vec3::new(4.0, 0.0, 0.0)), 1);
    }

    #[test]
    fn a_guard_takes_a_route_of_its_squad_with_room_and_stops_only_where_it_guards() {
        let pts = [[0.0, 0.0, 0.0], [5.0, 0.0, 0.0]];
        let routes = vec![route("ERT_Circle", &pts, &[0.0, 2.0])];
        let mut st = Patrols::of(&routes);
        // not of its squad: none
        let mut p = Patrol::default();
        p.begin(&mut st, None);
        assert_eq!(p.step(&routes, &mut st, "b", Vec3::new(0.0, 0.0, 0.1), 0.1), Order::default());
        assert_eq!(p.route, None);
        // of it: taken, full for the next
        let mut q = Patrol::default();
        q.begin(&mut st, None);
        let o = q.step(&routes, &mut st, "a", Vec3::new(-3.0, 0.0, 0.0), 0.1);
        assert_eq!((q.route, o.goal), (Some(0), Some(Vec3::ZERO)));
        assert_eq!(st.routes[0].adopters, 1);
        let mut r = Patrol::default();
        r.begin(&mut st, None);
        r.step(&routes, &mut st, "a", Vec3::ZERO, 0.1);
        assert_eq!(r.route, None);
        // the first point walked through, the second stopped at for its 2 s, facing its way
        let o = q.step(&routes, &mut st, "a", Vec3::new(0.0, 0.0, 0.2), 0.1);
        assert_eq!(o, Order { goal: Some(Vec3::new(5.0, 0.0, 0.0)), pass: false });
        assert_eq!(q.step(&routes, &mut st, "a", Vec3::new(5.0, 0.0, 0.0), 0.1), Order::default());
        assert_eq!(q.face, Some(1.0));
        assert_eq!(q.step(&routes, &mut st, "a", Vec3::new(5.0, 0.0, 0.0), 1.5), Order::default());
        let o = q.step(&routes, &mut st, "a", Vec3::new(5.0, 0.0, 0.0), 1.0);
        assert_eq!(o.goal, Some(Vec3::ZERO));
    }

    #[test]
    fn an_inactive_route_is_taken_only_once_the_scripts_turn_it_on() {
        let mut r = route("ERT_Circle", &[[0.0; 3], [5.0, 0.0, 0.0]], &[]);
        r.inactive = true;
        let routes = vec![r];
        let mut st = Patrols::of(&routes);
        let mut p = Patrol::default();
        p.begin(&mut st, None);
        p.step(&routes, &mut st, "a", Vec3::ZERO, 0.1);
        assert_eq!(p.route, None);
        st.toggle(0, Some(true));
        p.step(&routes, &mut st, "a", Vec3::ZERO, 0.1);
        assert_eq!(p.route, Some(0));
    }
}
