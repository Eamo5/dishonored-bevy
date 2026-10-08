//! Kismet (UE3 visual scripting) analysis.
//!
//! Dishonored spawns most of its NPCs from level scripts (`DisSeqAct_StartSpawn`) rather
//! than through `m_bSpawnOnBeginPlay`. Some of those actions run as soon as the level is
//! loaded, others only after gameplay events (alarms, objectives, cut-scenes triggered
//! later). We don't run Kismet, so we approximate: a spawner is active at level start when
//! a `StartSpawn` targeting it is reachable from a level-start event through output links,
//! sub-sequence entry points and remote events.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use upk::props::parse_struct_array;
use upk::{Package, Props, Value};

/// (package name, export index) of an actor.
pub type ActorKey = (String, i32);

struct Op {
    class: String,
    /// output index -> [(op, input index)]
    outputs: Vec<Vec<(usize, i32)>>,
    input_names: Vec<String>,
    /// variable link label -> linked variable exports
    vars: Vec<(String, Vec<i32>)>,
    parent: i32,
    pkg: usize,
    idx: i32,
}

/// An object's properties, those it leaves unset taken from its archetypes (a prefab
/// instance's sequence objects keep only what differs from the prefab's own: event names,
/// thresholds, delays).
fn read_props(pkg: &Package, idx: i32) -> Option<Props> {
    let mut props = upk::read_object(pkg, idx).ok()?.props;
    let mut arch = if idx > 0 { pkg.exports[idx as usize - 1].archetype } else { 0 };
    // (a prefab edited over time chains each version's objects to the last's)
    for _ in 0..32 {
        if arch <= 0 || arch as usize > pkg.exports.len() {
            break;
        }
        let Ok(o) = upk::read_object(pkg, arch) else { break };
        for p in o.props.0 {
            if !props.0.iter().any(|q| q.name.eq_ignore_ascii_case(&p.name) && q.index == p.index) {
                props.0.push(p);
            }
        }
        arch = pkg.exports[arch as usize - 1].archetype;
    }
    Some(props)
}

fn struct_array(pkg: &Package, props: &Props, name: &str) -> Vec<Props> {
    match props.array(name) {
        Some((c, off, sz)) => parse_struct_array(pkg, off, sz, c).unwrap_or_default(),
        None => Vec::new(),
    }
}

fn obj_list(pkg: &Package, props: &Props, name: &str) -> Vec<i32> {
    match props.get(name) {
        Some(Value::Array { count, offset, .. }) => {
            let mut r = upk::Reader::at(&pkg.data, *offset);
            (0..*count).filter_map(|_| r.i32().ok()).collect()
        }
        _ => Vec::new(),
    }
}

/// Whether a sequence object belongs to the level (not to a prefab's archetype that came along
/// with it: the prefab's own sequences are templates, the level runs its instances' copies).
fn in_world(pkg: &Package, i: i32) -> bool {
    pkg.obj_path(i).starts_with("TheWorld.")
}

fn is_op_class(c: &str) -> bool {
    (c.contains("SeqAct") || c.contains("SeqEvent") || c.contains("SeqEvt") || c.contains("SeqCond") || c == "Sequence" || c == "PrefabSequence")
        && !c.contains("SeqVar")
}

/// Spawners whose spawn action runs when the level starts.
pub fn spawners_started_at_load(levels: &[Arc<Package>]) -> HashSet<ActorKey> {
    let mut ops: Vec<Op> = Vec::new();
    let mut by_key: HashMap<(usize, i32), usize> = HashMap::new();
    for (pi, pkg) in levels.iter().enumerate() {
        for i in 1..=pkg.exports.len() as i32 {
            let cls = pkg.class_name(i);
            if !is_op_class(&cls) || !in_world(pkg, i) {
                continue;
            }
            by_key.insert((pi, i), ops.len());
            ops.push(Op { class: cls, outputs: Vec::new(), input_names: Vec::new(), vars: Vec::new(), parent: 0, pkg: pi, idx: i });
        }
    }
    // links
    for k in 0..ops.len() {
        let (pi, idx) = (ops[k].pkg, ops[k].idx);
        let pkg = &levels[pi];
        let Some(props) = read_props(pkg, idx) else { continue };
        let mut outputs = Vec::new();
        for out in struct_array(pkg, &props, "OutputLinks") {
            let mut targets = Vec::new();
            for l in struct_array(pkg, &out, "Links") {
                if let Some(op) = l.object("LinkedOp").filter(|o| *o > 0) {
                    if let Some(&t) = by_key.get(&(pi, op)) {
                        let input = match l.get("InputLinkIdx") {
                            Some(Value::Int(v)) => *v,
                            _ => 0,
                        };
                        targets.push((t, input));
                    }
                }
            }
            outputs.push(targets);
        }
        let inputs = struct_array(pkg, &props, "InputLinks")
            .iter()
            .map(|l| match l.get("LinkDesc") {
                Some(Value::Str(s)) => s.clone(),
                _ => String::new(),
            })
            .collect();
        let vars = struct_array(pkg, &props, "VariableLinks")
            .iter()
            .map(|v| {
                let desc = match v.get("LinkDesc") {
                    Some(Value::Str(s)) => s.clone(),
                    _ => String::new(),
                };
                (desc, obj_list(pkg, v, "LinkedVariables"))
            })
            .collect();
        ops[k].outputs = outputs;
        ops[k].input_names = inputs;
        ops[k].vars = vars;
        ops[k].parent = props.object("ParentSequence").unwrap_or(0);
    }

    // remote events by name, sequence entry events by parent sequence
    let mut remote: HashMap<String, Vec<usize>> = HashMap::new();
    let mut entry: HashMap<(usize, i32), Vec<usize>> = HashMap::new();
    for (k, op) in ops.iter().enumerate() {
        if op.class == "SeqEvent_RemoteEvent" || op.class == "SeqEvent_SequenceActivated" {
            let pkg = &levels[op.pkg];
            if op.class == "SeqEvent_SequenceActivated" {
                entry.entry((op.pkg, op.parent)).or_default().push(k);
            } else if let Some(Value::Name(n)) = read_props(pkg, op.idx).as_ref().and_then(|p| p.get("EventName").cloned()) {
                remote.entry(n.to_ascii_lowercase()).or_default().push(k);
            }
        }
    }

    // flood fill from level-start events
    let mut reached: HashSet<usize> = HashSet::new();
    let mut spawn_inputs: Vec<(usize, i32)> = Vec::new();
    let mut queue: VecDeque<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, o)| o.class == "SeqEvent_LevelLoaded" || o.class == "SeqEvent_LevelBeginning" || o.class == "SeqEvent_LevelStartup")
        .map(|(k, _)| k)
        .collect();
    reached.extend(queue.iter().copied());
    while let Some(k) = queue.pop_front() {
        let op = &ops[k];
        let mut next: Vec<(usize, i32)> = op.outputs.iter().flatten().copied().collect();
        if op.class == "Sequence" || op.class == "PrefabSequence" {
            for &e in entry.get(&(op.pkg, op.idx)).into_iter().flatten() {
                next.push((e, 0));
            }
        }
        if op.class == "SeqAct_ActivateRemoteEvent" {
            if let Some(Value::Name(n)) = read_props(&levels[op.pkg], op.idx).as_ref().and_then(|p| p.get("EventName").cloned()) {
                for &e in remote.get(&n.to_ascii_lowercase()).into_iter().flatten() {
                    next.push((e, 0));
                }
            }
        }
        for (t, input) in next {
            if ops[t].class.contains("StartSpawn") {
                spawn_inputs.push((t, input));
            }
            if reached.insert(t) {
                queue.push_back(t);
            }
        }
    }

    if std::env::var("DH_KISMET_DEBUG").is_ok() {
        // reverse edges: which root events lead to each StartSpawn
        let mut preds: HashMap<usize, Vec<usize>> = HashMap::new();
        for (k, op) in ops.iter().enumerate() {
            for &(t, _) in op.outputs.iter().flatten() {
                preds.entry(t).or_default().push(k);
            }
        }
        for (k, op) in ops.iter().enumerate() {
            if !op.class.contains("StartSpawn") {
                continue;
            }
            let mut seen = HashSet::new();
            let mut q = vec![k];
            let mut roots = std::collections::BTreeMap::new();
            while let Some(x) = q.pop() {
                if !seen.insert(x) {
                    continue;
                }
                let ps = preds.get(&x).cloned().unwrap_or_default();
                if ps.is_empty() {
                    let c = &ops[x].class;
                    let n = if c == "SeqEvent_RemoteEvent" || c == "SeqEvent_SequenceActivated" {
                        format!("{c}:{}", levels[ops[x].pkg].obj_path(ops[x].idx).rsplit('.').next().unwrap_or(""))
                    } else {
                        c.clone()
                    };
                    *roots.entry(n).or_insert(0) += 1;
                }
                q.extend(ps);
            }
            log::info!(
                "kismet: {} reached={} roots={:?}",
                levels[op.pkg].obj_path(op.idx),
                reached.contains(&k),
                roots
            );
        }
    }

    // named variables, for SeqVar_Named lookups
    let mut named: HashMap<(usize, String), Vec<i32>> = HashMap::new();
    for (pi, pkg) in levels.iter().enumerate() {
        for i in 1..=pkg.exports.len() as i32 {
            let c = pkg.class_name(i);
            if c.contains("SeqVar") && !c.contains("Named") && !c.contains("External") {
                if let Some(Value::Name(n)) = read_props(pkg, i).as_ref().and_then(|q| q.get("VarName").cloned()) {
                    if n != "None" {
                        named.entry((pi, n.to_ascii_lowercase())).or_default().push(i);
                    }
                }
            }
        }
    }
    let ctx = VarCtx { levels, ops: &ops, named: &named };

    // resolve the spawners targeted by reached StartSpawn inputs
    let mut out = HashSet::new();
    for (k, input) in spawn_inputs {
        let op = &ops[k];
        let name = op.input_names.get(input.max(0) as usize).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
        if ["stop", "kill", "despawn", "disable", "destroy", "remove"].iter().any(|w| name.contains(w)) {
            continue;
        }
        for (_, vars) in &op.vars {
            for &v in vars {
                for actor in ctx.resolve(op.pkg, v, 0) {
                    out.insert((levels[op.pkg].name.clone(), actor));
                }
            }
        }
    }
    out
}

struct VarCtx<'a> {
    levels: &'a [Arc<Package>],
    ops: &'a [Op],
    named: &'a HashMap<(usize, String), Vec<i32>>,
}

impl VarCtx<'_> {
    /// Actors referenced by a sequence variable (object, object list, named and external vars).
    fn resolve(&self, pi: usize, v: i32, depth: usize) -> Vec<i32> {
        let pkg = &self.levels[pi];
        if v <= 0 || v as usize > pkg.exports.len() || depth > 6 {
            return Vec::new();
        }
        let cls = pkg.class_name(v);
        let Some(p) = read_props(pkg, v) else { return Vec::new() };
        if cls.contains("SeqVar_ObjectList") {
            return obj_list(pkg, &p, "ObjList").into_iter().filter(|o| *o > 0).collect();
        }
        if cls.contains("SeqVar_Named") {
            let Some(Value::Name(want)) = p.get("FindVarName").cloned() else { return Vec::new() };
            let vars = self.named.get(&(pi, want.to_ascii_lowercase())).cloned().unwrap_or_default();
            return vars.into_iter().flat_map(|w| self.resolve(pi, w, depth + 1)).collect();
        }
        if cls.contains("SeqVar_External") {
            // bound to the enclosing Sequence op's variable link with the same label
            let Some(Value::Str(label)) = p.get("VariableLabel").cloned() else { return Vec::new() };
            let seq = p.object("ParentSequence").unwrap_or(0);
            let mut out = Vec::new();
            if let Some(op) = self.ops.iter().find(|o| o.pkg == pi && o.idx == seq) {
                for (desc, vars) in &op.vars {
                    if desc.eq_ignore_ascii_case(&label) {
                        for &w in vars {
                            out.extend(self.resolve(pi, w, depth + 1));
                        }
                    }
                }
            }
            return out;
        }
        p.object("ObjValue").filter(|o| *o > 0).into_iter().collect()
    }
}

// ---------------------------------------------------------------- graph export

use crate::format::{KActor, KInput, KLink, KOp, KOutput, KVal, KVar, Kismet};
use std::collections::BTreeMap;

fn is_var_class(c: &str) -> bool {
    c.contains("SeqVar") || c == "InterpData"
}

/// Editor-only and link properties the runtime has no use for.
const SKIP_PROPS: &[&str] = &[
    "InputLinks",
    "OutputLinks",
    "VariableLinks",
    "EventLinks",
    "ParentSequence",
    "ObjPosX",
    "ObjPosY",
    "ObjComment",
    "ObjColor",
    "DrawWidth",
    "DrawHeight",
    "OutDrawY",
    "InDrawY",
    "VarDrawX",
    "ObjInstanceVersion",
    "SequenceObjects",
    "bSuppressAutoComment",
    "bOutputObjCommentToScreen",
    "DefaultViewX",
    "DefaultViewY",
    "DefaultViewZoom",
    "m_BaseClassVersions",
    "m_FallbackChainCooked",
];

struct Exporter<'a> {
    levels: &'a [Arc<Package>],
    refs: &'a HashMap<ActorKey, KActor>,
    ops: HashMap<(usize, i32), u32>,
    vars: HashMap<(usize, i32), u32>,
    actors: HashMap<(usize, i32), u32>,
    out: Kismet,
}

impl Exporter<'_> {
    fn is_level_actor(&self, pi: usize, o: i32) -> bool {
        let pkg = &self.levels[pi];
        if o <= 0 || o as usize > pkg.exports.len() {
            return false;
        }
        let outer = pkg.exports[o as usize - 1].outer;
        outer > 0 && pkg.obj_path(outer) == "TheWorld.PersistentLevel"
    }

    fn actor(&mut self, pi: usize, o: i32) -> u32 {
        if let Some(&a) = self.actors.get(&(pi, o)) {
            return a;
        }
        let pkg = &self.levels[pi];
        let rec = match self.refs.get(&(pkg.name.clone(), o)) {
            Some(r) => r.clone(),
            None => {
                // not turned into anything: keep its name, class and placement
                let props = read_props(pkg, o);
                let loc = props.as_ref().and_then(|p| p.vector("Location")).unwrap_or([0.0; 3]);
                let rot = props.as_ref().and_then(|p| p.rotator("Rotation")).unwrap_or([0; 3]);
                let path = pkg.obj_path(o);
                KActor {
                    name: path.rsplit('.').next().unwrap_or("").to_string(),
                    class: pkg.class_name(o),
                    position: crate::xform::ue_point(loc),
                    yaw: crate::xform::ue_yaw_to_bevy(rot[1]),
                    // (the UE placement too: matinee tracks relative to it start from there)
                    ue_location: loc,
                    rotation: rot,
                    ..Default::default()
                }
            }
        };
        let a = self.out.actors.len() as u32;
        self.out.actors.push(rec);
        self.actors.insert((pi, o), a);
        a
    }

    fn object(&mut self, pi: usize, o: i32) -> Option<KVal> {
        if o == 0 {
            return None;
        }
        if let Some(&k) = self.ops.get(&(pi, o)) {
            return Some(KVal::Op(k));
        }
        if let Some(&k) = self.vars.get(&(pi, o)) {
            return Some(KVal::Var(k));
        }
        if self.is_level_actor(pi, o) {
            return Some(KVal::Actor(self.actor(pi, o)));
        }
        let pkg = &self.levels[pi];
        if o < 0 && (-o) as usize > pkg.imports.len() || o > 0 && o as usize > pkg.exports.len() {
            return None;
        }
        Some(KVal::Str(pkg.obj_path(o)))
    }

    fn value(&mut self, pi: usize, v: &Value) -> Option<KVal> {
        Some(match v {
            Value::Int(i) => KVal::Int(*i),
            Value::Float(f) => KVal::Float(*f),
            Value::Bool(b) => KVal::Bool(*b),
            Value::Byte(b) => KVal::Int(*b as i32),
            Value::Enum(s) | Value::Name(s) | Value::Str(s) => KVal::Str(s.clone()),
            Value::Object(o) => return self.object(pi, *o),
            Value::Vector(v) => KVal::Vec3(*v),
            Value::Vec2(v) => KVal::List(vec![KVal::Float(v[0]), KVal::Float(v[1])]),
            Value::LinearColor(c) => KVal::List(c.iter().map(|x| KVal::Float(*x)).collect()),
            Value::Rotator(r) => KVal::Vec3([r[0] as f32, r[1] as f32, r[2] as f32]),
            Value::Guid(g) => KVal::Str(format!("{:08x}{:08x}{:08x}{:08x}", g[0], g[1], g[2], g[3])),
            Value::Array { count, offset, size } if *count > 0 && *size == count * 4 => {
                // object references (e.g. ObjList, Targets)
                let pkg = self.levels[pi].clone();
                let mut r = upk::Reader::at(&pkg.data, *offset);
                let items: Vec<i32> = (0..*count).filter_map(|_| r.i32().ok()).collect();
                let valid = |o: i32| o != 0 && (o > 0 && (o as usize) <= pkg.exports.len() || o < 0 && ((-o) as usize) <= pkg.imports.len());
                if !items.iter().all(|&o| valid(o)) {
                    return None;
                }
                KVal::List(items.into_iter().filter_map(|o| self.object(pi, o)).collect())
            }
            _ => return None,
        })
    }

    fn props(&mut self, pi: usize, props: &Props) -> BTreeMap<String, KVal> {
        let mut out = BTreeMap::new();
        for p in &props.0 {
            if SKIP_PROPS.contains(&p.name.as_str()) {
                continue;
            }
            // level names: a map change's secondary levels, a streaming op's levels
            if p.name == "InitiallyLoadedSecondaryLevelNames" {
                if let Value::Array { count, offset, .. } = &p.value {
                    let pkg = self.levels[pi].clone();
                    let mut r = upk::Reader::at(&pkg.data, *offset);
                    let names: Vec<KVal> = (0..*count).map_while(|_| pkg.read_name(&mut r).ok()).map(KVal::Str).collect();
                    out.insert(p.name.clone(), KVal::List(names));
                }
                continue;
            }
            // a console command's commands (strings: "start L_Tower_P")
            if p.name == "Commands" {
                if let Value::Array { count, offset, .. } = &p.value {
                    let pkg = self.levels[pi].clone();
                    let mut r = upk::Reader::at(&pkg.data, *offset);
                    let cmds: Vec<KVal> = (0..*count).map_while(|_| r.fstring().ok()).map(KVal::Str).collect();
                    out.insert(p.name.clone(), KVal::List(cmds));
                }
                continue;
            }
            // `SeqAct_ModifyProperty`'s properties: (name, value) as text
            if p.name == "Properties" && matches!(p.value, Value::Array { .. }) {
                let pkg = self.levels[pi].clone();
                let items: Vec<KVal> = struct_array(&pkg, props, "Properties")
                    .iter()
                    .filter(|s| s.bool("bModifyProperty").unwrap_or(true))
                    .filter_map(|s| Some(KVal::List(vec![KVal::Str(s.name("PropertyName")?.to_string()), KVal::Str(s.name("PropertyValue").unwrap_or_default().to_string())])))
                    .collect();
                if !items.is_empty() {
                    out.insert(p.name.clone(), KVal::List(items));
                }
                continue;
            }
            // enum arrays (stored as names): the brain flags an op sets
            // (`DisSeqAct_AISetBrainFlags`: `EDisAIBrainFlags_*`)
            if p.name == "m_FlagsToModify" {
                if let Value::Array { count, offset, .. } = &p.value {
                    let pkg = self.levels[pi].clone();
                    let mut r = upk::Reader::at(&pkg.data, *offset);
                    let names: Vec<KVal> = (0..*count).map_while(|_| pkg.read_name(&mut r).ok()).map(KVal::Str).collect();
                    out.insert(p.name.clone(), KVal::List(names));
                }
                continue;
            }
            if p.name == "Levels" {
                let pkg = self.levels[pi].clone();
                let names: Vec<KVal> = struct_array(&pkg, props, "Levels").iter().filter_map(|l| l.name("LevelName").map(|n| KVal::Str(n.to_string()))).collect();
                out.insert(p.name.clone(), KVal::List(names));
                continue;
            }
            if let Some(v) = self.value(pi, &p.value) {
                let key = if p.index > 0 { format!("{}[{}]", p.name, p.index) } else { p.name.clone() };
                out.insert(key, v);
            }
        }
        out
    }
}

/// Export the level scripts of all levels as one graph.
pub fn cook(levels: &[Arc<Package>], refs: &HashMap<ActorKey, KActor>) -> Kismet {
    let mut ex = Exporter { levels, refs, ops: HashMap::new(), vars: HashMap::new(), actors: HashMap::new(), out: Kismet::default() };
    let mut op_list = Vec::new();
    let mut var_list = Vec::new();
    for (pi, pkg) in levels.iter().enumerate() {
        let first = op_list.len() as u32;
        for i in 1..=pkg.exports.len() as i32 {
            let cls = pkg.class_name(i);
            if !in_world(pkg, i) {
                continue;
            }
            if is_op_class(&cls) {
                ex.ops.insert((pi, i), op_list.len() as u32);
                op_list.push((pi, i, cls));
            } else if is_var_class(&cls) {
                ex.vars.insert((pi, i), var_list.len() as u32);
                var_list.push((pi, i, cls));
            }
        }
        ex.out.level_ops.push((first, op_list.len() as u32));
    }
    let str_of = |p: &Props, n: &str| match p.get(n) {
        Some(Value::Str(s)) | Some(Value::Name(s)) => s.clone(),
        _ => String::new(),
    };
    for (pi, i, cls) in op_list {
        let pkg = levels[pi].clone();
        let name = pkg.obj_path(i).rsplit('.').next().unwrap_or("").to_string();
        let Some(props) = read_props(&pkg, i) else {
            ex.out.ops.push(KOp { class: cls, name, ..Default::default() });
            continue;
        };
        let mut op = KOp { class: cls, name, ..Default::default() };
        op.parent = props.object("ParentSequence").and_then(|o| ex.ops.get(&(pi, o)).copied());
        for l in struct_array(&pkg, &props, "InputLinks") {
            let linked = l.object("LinkedOp").and_then(|o| ex.ops.get(&(pi, o)).copied());
            op.inputs.push(KInput { desc: str_of(&l, "LinkDesc"), linked });
        }
        for l in struct_array(&pkg, &props, "OutputLinks") {
            let delay = l.float("ActivateDelay").unwrap_or(0.0);
            let mut links = Vec::new();
            for t in struct_array(&pkg, &l, "Links") {
                if let Some(&target) = t.object("LinkedOp").and_then(|o| ex.ops.get(&(pi, o))) {
                    let input = match t.get("InputLinkIdx") {
                        Some(Value::Int(v)) => (*v).max(0) as u32,
                        _ => 0,
                    };
                    links.push((target, input, delay));
                }
            }
            let linked = l.object("LinkedOp").and_then(|o| ex.ops.get(&(pi, o)).copied());
            op.outputs.push(KOutput { desc: str_of(&l, "LinkDesc"), links, linked });
        }
        for l in struct_array(&pkg, &props, "VariableLinks") {
            let vars = obj_list(&pkg, &l, "LinkedVariables").into_iter().filter_map(|o| ex.vars.get(&(pi, o)).copied()).collect();
            let write = matches!(l.get("bWriteable"), Some(Value::Bool(true)));
            op.vars.push(KLink { desc: str_of(&l, "LinkDesc"), vars, write });
        }
        op.props = ex.props(pi, &props);
        // events an action attaches to actors (SeqAct_AttachToEvent)
        let events: Vec<KVal> = struct_array(&pkg, &props, "EventLinks")
            .iter()
            .flat_map(|l| obj_list(&pkg, l, "LinkedEvents"))
            .filter_map(|o| ex.ops.get(&(pi, o)).map(|&k| KVal::Op(k)))
            .collect();
        if !events.is_empty() {
            op.props.insert("EventLinks".into(), KVal::List(events));
        }
        // the scripts' post-processing (`ArkUberPpParameters`): the fields it overrides, a
        // group's and the field's own flag both on (the struct's defaults: every group, the
        // depth of field, colour balance but post-desaturation, exposure and gamma)
        if op.class == "DisSeqAct_UberPostProcess" {
            if let Some(Value::Struct(_, pp)) = props.get("m_Parameters") {
                let pp = Props(pp.clone());
                let sub = |p: &Props, n: &str| match p.get(n) {
                    Some(Value::Struct(_, s)) => Props(s.clone()),
                    _ => Props(Vec::new()),
                };
                let flag = |p: &Props, n: &str, d: bool| match p.get(n) {
                    Some(Value::Bool(b)) => *b,
                    _ => d,
                };
                let mut fields: Vec<KVal> = Vec::new();
                let mut put = |name: &str, v: Vec<f32>| {
                    let mut l = vec![KVal::Str(name.into())];
                    l.extend(v.into_iter().map(KVal::Float));
                    fields.push(KVal::List(l));
                };
                let (cb, hdr, dof) = (sub(&pp, "m_CBParameters"), sub(&pp, "m_HDRParameters"), sub(&pp, "m_DOFParameters"));
                if flag(&pp, "m_bOverrideCBParameters", true) {
                    for (f, n, d) in [("shadows", "CrMgYbShadTones", true), ("midtones", "CrMgYbMidTones", true), ("highlights", "CrMgYbHighTones", true)] {
                        if flag(&cb, &format!("m_bOverride{n}"), d) {
                            put(f, cb.vector(&format!("m_{n}")).unwrap_or([0.0; 3]).to_vec());
                        }
                    }
                    for (f, n, d, v) in [("balance_opacity", "Opacity", true, 1.0), ("pre_desaturation", "PreDesaturation", true, 0.0), ("post_desaturation", "PostDesaturation", false, 0.0)] {
                        if flag(&cb, &format!("m_bOverride{n}"), d) {
                            put(f, vec![cb.float(&format!("m_{n}")).unwrap_or(v)]);
                        }
                    }
                }
                if flag(&pp, "m_bOverrideHDRParameters", true) {
                    for (f, n, d, v) in [("exposure", "Exposure", true, 0.0), ("gamma", "GammaAdjustment", true, 1.0), ("film_grain", "FilmGrainNoise", false, 0.0), ("brightness", "GimpBrightness", false, 0.0), ("contrast", "GimpContrast", false, 0.0)] {
                        if flag(&hdr, &format!("m_bOverride{n}"), d) {
                            put(f, vec![hdr.float(&format!("m_{n}")).unwrap_or(v)]);
                        }
                    }
                }
                if flag(&pp, "m_bOverrideDOFParameters", true) {
                    for (f, n, v, scale) in [("focus_distance", "FocusDistance", 1000.0, 0.01905), ("in_focus_radius", "InFocusRadius", 300.0, 0.01905), ("far_blur", "FarBlurAmount", 0.0, 1.0)] {
                        if flag(&dof, &format!("m_bOverride{n}"), true) {
                            put(f, vec![dof.float(&format!("m_{n}")).unwrap_or(v) * scale]);
                        }
                    }
                }
                op.props.insert("pp_fields".into(), KVal::List(fields));
            }
        }
        // ammunition set, given or taken (type in the original's `eDisAmmoType` order:
        // bullets, explosive bullets, bolts, sleep darts, incendiary bolts, springrazors,
        // grenades, sticky grenades; amount)
        if op.class == "DisSeqAct_ModifyAmmo" {
            const TYPES: [&str; 8] = ["Bullet", "Bullet_Explosive", "Arrow", "Arrow_Sleep", "Arrow_Flare", "SpringRazor", "Grenade", "StickyGrenade"];
            let list: Vec<KVal> = struct_array(&pkg, &props, "m_Ammo")
                .iter()
                .filter_map(|e| {
                    let ty = match e.get("m_AmmoType") {
                        Some(Value::Enum(s)) | Some(Value::Name(s)) => TYPES.iter().position(|t| s.strip_prefix("eDisAmmoType_") == Some(*t))?,
                        _ => 0,
                    };
                    let n = match e.get("m_AmmoAmount") {
                        Some(Value::Int(v)) => *v,
                        _ => 0,
                    };
                    Some(KVal::List(vec![KVal::Int(ty as i32), KVal::Int(n)]))
                })
                .collect();
            op.props.insert("ammo".into(), KVal::List(list));
        }
        // new materials put on characters (slot, material path), resolved by the cooker
        if op.class == "DisSeqAct_NPCSetMaterials" {
            for (key, out) in [("m_NewBodyMaterials", "body_mat_paths"), ("m_NewHeadMaterials", "head_mat_paths")] {
                let list: Vec<KVal> = struct_array(&pkg, &props, key)
                    .iter()
                    .filter_map(|e| {
                        let m = e.object("m_pMaterial").filter(|o| *o != 0)?;
                        let slot = match e.get("m_MaterialIndex") {
                            Some(Value::Int(v)) => *v,
                            _ => 0,
                        };
                        Some(KVal::List(vec![KVal::Int(slot), KVal::Str(pkg.obj_path(m))]))
                    })
                    .collect();
                op.props.insert(out.into(), KVal::List(list));
            }
        }
        ex.out.ops.push(op);
    }
    let voices = voice_events(levels);
    let texts = blurb_texts(levels);
    for (pi, i, cls) in var_list {
        let pkg = levels[pi].clone();
        let Some(props) = read_props(&pkg, i) else {
            ex.out.vars.push(KVar { class: cls, ..Default::default() });
            continue;
        };
        let mut v = KVar { class: cls.clone(), name: str_of(&props, "VarName"), ..Default::default() };
        if v.name == "None" {
            v.name.clear();
        }
        v.parent = props.object("ParentSequence").and_then(|o| ex.ops.get(&(pi, o)).copied());
        v.props = ex.props(pi, &props);
        // Dishonored keeps matinee data in a separate InterpData object
        if cls == "InterpData" && !v.props.contains_key("InterpLength") {
            if let Some(d) = props.object("m_Data").filter(|o| *o > 0) {
                if let Some(len) = read_props(&pkg, d).and_then(|p| p.float("InterpLength")) {
                    v.props.insert("InterpLength".into(), KVal::Float(len));
                }
            }
        }
        if cls == "InterpData" {
            if let Some(m) = cook_matinee(&pkg, &props, &voices, &texts) {
                if !v.props.contains_key("InterpLength") {
                    v.props.insert("InterpLength".into(), KVal::Float(m.length));
                }
                v.props.insert("Matinee".into(), KVal::Int(ex.out.matinees.len() as i32));
                ex.out.matinees.push(m);
            }
        }
        ex.out.vars.push(v);
    }
    // conversations of the dialogue actors the scripts talk to
    let dialog_actors: Vec<((usize, i32), u32)> = ex
        .actors
        .iter()
        .filter(|((pi, o), _)| levels[*pi].class_name(*o).contains("DisDialog"))
        .map(|(k, v)| (*k, *v))
        .collect();
    // every dialogue tree of the levels
    let mut tree_ids: HashMap<(usize, i32), u32> = HashMap::new();
    for (pi, pkg) in levels.iter().enumerate() {
        for i in 1..=pkg.exports.len() as i32 {
            if !pkg.class_name(i).starts_with("DisDialogTree") {
                continue;
            }
            let path = pkg.obj_path(i);
            if ex.out.dialog_trees.iter().any(|t| t.path == path) {
                continue;
            }
            if let Some(t) = dialog_tree(pkg, i, &voices) {
                tree_ids.insert((pi, i), ex.out.dialog_trees.len() as u32);
                ex.out.dialog_trees.push(t);
            }
        }
    }
    for ((pi, o), actor) in dialog_actors {
        let pkg = &levels[pi];
        let Some(tree) = read_props(pkg, o).and_then(|p| p.object("m_pDialogTree")).filter(|t| *t > 0) else { continue };
        let Some(&tree) = tree_ids.get(&(pi, tree)) else { continue };
        ex.out.dialogs.push(crate::format::KDialog { actor, tree });
    }
    // objectives (their texts are cooked in English)
    for pkg in levels {
        for i in 1..=pkg.exports.len() as i32 {
            if pkg.class_name(i) != "DishonoredObjective" {
                continue;
            }
            let Some(props) = read_props(pkg, i) else { continue };
            let path = pkg.obj_path(i);
            if ex.out.objectives.iter().any(|o| o.path == path) {
                continue;
            }
            let mut obj = crate::format::KObjective {
                path,
                text: str_of(&props, "m_Description"),
                tasks: Vec::new(),
                optional: matches!(props.get("m_bOptional"), Some(Value::Bool(true))),
                no_markers: matches!(props.get("m_bShowHUDMarkers"), Some(Value::Bool(false))),
            };
            for t in obj_list(pkg, &props, "m_Tasks") {
                if t <= 0 {
                    continue;
                }
                let Some(tp) = read_props(pkg, t) else { continue };
                obj.tasks.push(crate::format::KTask {
                    path: pkg.obj_path(t),
                    text: str_of(&tp, "m_Description"),
                    hidden: matches!(tp.get("m_bInitiallyHidden"), Some(Value::Bool(true))),
                });
            }
            ex.out.objectives.push(obj);
        }
    }
    // the pickups too, scripted or not: what Corvo uses is named to the scripts listening to
    // everything (`DisSeqEvent_Interact`: the Prison's "Take a weapon")
    let mut extra: Vec<&ActorKey> = refs.iter().filter(|(_, r)| r.pickup.is_some()).map(|(k, _)| k).collect();
    extra.sort();
    for (pkg, o) in extra {
        if let Some(pi) = levels.iter().position(|l| l.name == *pkg) {
            ex.actor(pi, *o);
        }
    }
    ex.out
}

/// A dialogue tree: its conversations and the graph from its hooks to them.
fn dialog_tree(pkg: &Package, t: i32, voices: &HashMap<[u32; 4], Vec<String>>) -> Option<crate::format::KDialogTree> {
    use crate::format::{KConversation, KDialogNode, KDialogNodeKind, KDialogTree};
    let tp = read_props(pkg, t)?;
    let mut tree = KDialogTree { path: pkg.obj_path(t), ..Default::default() };
    let mut conv_ids: HashMap<i32, u32> = HashMap::new();
    for c in obj_list(pkg, &tp, "m_Conversations").into_iter().filter(|c| *c > 0) {
        let Some(cp) = read_props(pkg, c) else { continue };
        conv_ids.insert(c, tree.conversations.len() as u32);
        tree.conversations.push(KConversation {
            label: str_of(&cp, "m_Label"),
            steps: conversation_steps(pkg, c, voices),
            path: pkg.obj_path(c),
            once: matches!(cp.get("m_bFireConvOnlyOnce"), Some(Value::Bool(true))),
        });
    }
    let guid = |p: &Props, k: &str| match p.get(k) {
        Some(Value::Guid(g)) => format!("{:08x}{:08x}{:08x}{:08x}", g[0], g[1], g[2], g[3]),
        _ => String::new(),
    };
    // the graph, from the hooks
    let mut ids: HashMap<i32, i32> = HashMap::new();
    let mut queue: Vec<i32> = Vec::new();
    for h in obj_list(pkg, &tp, "m_DialogHooks").into_iter().filter(|h| *h > 0) {
        if !ids.contains_key(&h) {
            ids.insert(h, queue.len() as i32);
            queue.push(h);
        }
    }
    let mut k = 0;
    while k < queue.len() && k < 4000 {
        let n = queue[k];
        k += 1;
        let cls = pkg.class_name(n);
        let p = read_props(pkg, n).unwrap_or_default();
        let links: Vec<i32> = struct_array(pkg, &p, "m_OutputLinks").iter().map(|l| l.object("m_pLink").unwrap_or(0)).collect();
        // conversations reached through links (characters' trees don't list theirs)
        for &l in &links {
            if l > 0 && !conv_ids.contains_key(&l) && pkg.class_name(l) == "DisConversation" {
                if let Some(cp) = read_props(pkg, l) {
                    conv_ids.insert(l, tree.conversations.len() as u32);
                    tree.conversations.push(KConversation {
                        label: str_of(&cp, "m_Label"),
                        steps: conversation_steps(pkg, l, voices),
                        path: pkg.obj_path(l),
                        once: matches!(cp.get("m_bFireConvOnlyOnce"), Some(Value::Bool(true))),
                    });
                }
            }
        }
        let kind = if let Some(&ci) = conv_ids.get(&n) {
            KDialogNodeKind::Conversation(ci)
        } else {
            match cls.as_str() {
                c if c.starts_with("DisConv_Hook") || c == "DisConv_DialogHook" => {
                    let hook = match p.get("m_HookEnum") {
                        Some(Value::Enum(e)) | Some(Value::Name(e)) => e.clone(),
                        _ => String::new(),
                    };
                    let mut inputs = Vec::new();
                    if let Some(Value::Array { count, offset, .. }) = p.get("m_KismetDialogHooks") {
                        let mut r = upk::Reader::at(&pkg.data, *offset);
                        for _ in 0..*count {
                            match pkg.read_name(&mut r) {
                                Ok(nm) => inputs.push(nm),
                                Err(_) => break,
                            }
                        }
                    }
                    KDialogNodeKind::Hook { hook, inputs }
                }
                // a reference: the conversation it names
                "DisConv_ConversationRef" => match links.first().and_then(|c| conv_ids.get(c)) {
                    Some(&ci) => KDialogNodeKind::Conversation(ci),
                    None => KDialogNodeKind::Pass,
                },
                // lines said outside a conversation (barks, one-liners): a conversation of their own
                "DisConv_Blurb" | "DisConv_NonWord" => {
                    let ci = tree.conversations.len() as u32;
                    tree.conversations.push(KConversation { label: String::new(), steps: steps_from(pkg, Some(n), voices), path: pkg.obj_path(n), once: false });
                    KDialogNodeKind::Conversation(ci)
                }
                "DisConv_CheckStoryFlag" => KDialogNodeKind::StoryFlag(guid(&p, "m_StoryFlag")),
                "DisConv_SetStoryFlag" => KDialogNodeKind::SetStoryFlag(guid(&p, "m_StoryFlag")),
                "DisConv_ConversationFired" | "DisConv_HadConversation" => match p.object("m_pConversation").filter(|c| *c > 0) {
                    Some(c) => KDialogNodeKind::Fired(pkg.obj_path(c)),
                    None => KDialogNodeKind::Pass,
                },
                "DisConv_TimeLimit" => KDialogNodeKind::TimeLimit(p.float("m_fTimeLimitInSeconds").unwrap_or(0.0)),
                "DisConv_RandomBranch" | "DisConv_RandomSequentialBranch" => KDialogNodeKind::Random,
                "DisConv_SequentialBranch" => KDialogNodeKind::Sequential,
                "DisConv_KismetActivateRemoteEvent" => match p.get("m_EventName") {
                    Some(Value::Name(e)) | Some(Value::Str(e)) => KDialogNodeKind::Remote(e.clone()),
                    _ => KDialogNodeKind::Pass,
                },
                _ => KDialogNodeKind::Pass,
            }
        };
        // a conversation ends the walk; the rest lead on
        let outs = if matches!(kind, KDialogNodeKind::Conversation(_)) {
            Vec::new()
        } else {
            links
                .iter()
                .map(|&o| {
                    if o <= 0 {
                        return -1;
                    }
                    *ids.entry(o).or_insert_with(|| {
                        queue.push(o);
                        queue.len() as i32 - 1
                    })
                })
                .collect()
        };
        tree.nodes.push(KDialogNode { kind, outs });
    }
    Some(tree)
}

/// First link of a conversation node's outputs.
fn next_node(pkg: &Package, props: &Props) -> Option<i32> {
    struct_array(pkg, props, "m_OutputLinks").iter().find_map(|l| l.object("m_pLink").filter(|o| *o > 0))
}

/// Walk a conversation from its start along first links, collecting what it does.
/// Voice-over events of dialogue lines (blurb GUID -> Wwise event names), from every
/// `DisDialogVoiceData` (one per speaker voice) of the levels.
pub(crate) fn voice_events(levels: &[Arc<Package>]) -> HashMap<[u32; 4], Vec<String>> {
    let mut out: HashMap<[u32; 4], Vec<String>> = HashMap::new();
    for pkg in levels {
        for i in 1..=pkg.exports.len() as i32 {
            if !pkg.class_name(i).starts_with("DisDialogVoiceData") {
                continue;
            }
            let Some(p) = read_props(pkg, i) else { continue };
            for tree in struct_array(pkg, &p, "m_VoiceData") {
                for b in struct_array(pkg, &tree, "m_Blurbs") {
                    let (Some(Value::Guid(g)), Some(Value::Str(e))) = (b.get("m_GUID"), b.get("m_AkEventName")) else { continue };
                    let list = out.entry(*g).or_default();
                    if !list.contains(e) {
                        list.push(e.clone());
                    }
                }
            }
        }
    }
    out
}

/// An AI voice's barks: every hook of the dialog tree it voices, with the voiced lines
/// reachable from it (through branches and conditions, up to the first blurb).
pub fn bark_voice(pkg: &Package, voice: i32) -> Option<crate::format::BarkVoice> {
    use crate::format::{BarkHook, BarkVoice};
    let vp = read_props(pkg, voice)?;
    let tree = vp.object("m_pDialogTree").filter(|t| *t > 0)?;
    let mut events: HashMap<[u32; 4], String> = HashMap::new();
    for t in struct_array(pkg, &vp, "m_VoiceData") {
        for b in struct_array(pkg, &t, "m_Blurbs") {
            if let (Some(Value::Guid(g)), Some(Value::Str(e))) = (b.get("m_GUID"), b.get("m_AkEventName")) {
                events.entry(*g).or_insert_with(|| e.clone());
            }
        }
    }
    let tp = read_props(pkg, tree)?;
    let mut out = BarkVoice { name: pkg.obj_path(voice), hooks: Vec::new(), tree: pkg.obj_path(tree) };
    for h in obj_list(pkg, &tp, "m_DialogHooks") {
        if h <= 0 {
            continue;
        }
        let cls = pkg.class_name(h);
        let Some(hp) = read_props(pkg, h) else { continue };
        let hook = match (cls.as_str(), hp.get("m_HookEnum")) {
            ("DisConv_DialogHook", Some(Value::Enum(e))) => e.trim_start_matches("DDH_").to_string(),
            (c, _) if c.starts_with("DisConv_Hook_") => c.trim_start_matches("DisConv_Hook_").to_ascii_uppercase(),
            _ => continue,
        };
        // breadth-first to the blurbs
        let mut lines = Vec::new();
        let mut seen = HashSet::new();
        let mut queue: VecDeque<(i32, usize)> = struct_array(pkg, &hp, "m_OutputLinks").iter().filter_map(|l| l.object("m_pLink")).filter(|o| *o > 0).map(|o| (o, 0)).collect();
        while let Some((n, depth)) = queue.pop_front() {
            if !seen.insert(n) || depth > 8 || lines.len() >= 64 {
                continue;
            }
            let Some(np) = read_props(pkg, n) else { continue };
            match pkg.class_name(n).as_str() {
                "DisConv_Blurb" | "DisConv_NonWord" => {
                    let text = match np.get("m_Text") {
                        Some(Value::Str(t)) => t.trim().to_string(),
                        _ => String::new(),
                    };
                    if let Some(Value::Guid(g)) = np.get("m_iBlurbGUID") {
                        if let Some(e) = events.get(g) {
                            lines.push((e.clone(), text));
                        }
                    }
                }
                "DisConversation" | "DisConv_ConversationRef" => {}
                _ => {
                    for l in struct_array(pkg, &np, "m_OutputLinks") {
                        if let Some(o) = l.object("m_pLink").filter(|o| *o > 0) {
                            queue.push_back((o, depth + 1));
                        }
                    }
                }
            }
        }
        if !lines.is_empty() {
            match out.hooks.iter_mut().find(|x| x.hook == hook) {
                Some(x) => x.lines.extend(lines),
                None => out.hooks.push(BarkHook { hook, lines }),
            }
        }
    }
    (!out.hooks.is_empty()).then_some(out)
}

fn str_of(p: &Props, n: &str) -> String {
    match p.get(n) {
        Some(Value::Name(s)) | Some(Value::Str(s)) | Some(Value::Enum(s)) => s.clone(),
        _ => String::new(),
    }
}

fn curve(pkg: &Package, p: &Props, name: &str) -> Vec<crate::format::KCurvePoint> {
    let Some(c) = p.struct_props(name) else { return Vec::new() };
    let v3 = |q: &Props, k: &str| match q.get(k) {
        Some(Value::Vector(v)) => *v,
        Some(Value::Float(f)) => [*f, 0.0, 0.0],
        _ => [0.0; 3],
    };
    struct_array(pkg, &c, "Points")
        .iter()
        .map(|q| crate::format::KCurvePoint {
            t: q.float("InVal").unwrap_or(0.0),
            v: v3(q, "OutVal"),
            arrive: v3(q, "ArriveTangent"),
            leave: v3(q, "LeaveTangent"),
            mode: match q.get("InterpMode") {
                Some(Value::Enum(m)) if m == "CIM_Linear" => 0,
                Some(Value::Enum(m)) if m == "CIM_Constant" => 2,
                // the property default (CIM_Linear) isn't serialized
                None => 0,
                _ => 1,
            },
        })
        .collect()
}

/// A matinee's groups and tracks (Dishonored: InterpData.m_Data -> MatineeData.m_RunData).
/// A reusable AI scene (a `MatineeData` "soiree": a distraction's), cooked like a level's.
pub fn cook_soiree(pkg: &Package, idx: i32) -> Option<crate::format::KMatinee> {
    let p = read_props(pkg, idx)?;
    let mut m = cook_matinee(pkg, &p, &HashMap::new(), &HashMap::new())?;
    if let Some(len) = p.struct_props("m_RunData").and_then(|r| r.float("InterpLength")) {
        m.length = len;
    }
    Some(m)
}

fn cook_matinee(pkg: &Package, idata: &Props, voices: &HashMap<[u32; 4], Vec<String>>, texts: &HashMap<[u32; 4], String>) -> Option<crate::format::KMatinee> {
    use crate::format::{KInterpGroup, KMatinee, KTrack};
    let md = idata.object("m_Data").filter(|o| *o > 0).and_then(|d| read_props(pkg, d));
    let groups_of = |p: &Props| -> Vec<i32> {
        match p.struct_props("m_RunData") {
            Some(r) => obj_list(pkg, &r, "InterpGroups"),
            None => obj_list(pkg, p, "InterpGroups"),
        }
    };
    let groups = match &md {
        Some(p) => groups_of(p),
        None => groups_of(idata),
    };
    if groups.is_empty() {
        return None;
    }
    let length = md.as_ref().and_then(|p| p.float("InterpLength")).or(idata.float("InterpLength")).unwrap_or(5.0);
    let mut m = KMatinee { length, ..Default::default() };
    let name_of_obj = |o: i32| -> String {
        let path = pkg.obj_path(o);
        path.rsplit('.').next().unwrap_or(&path).to_string()
    };
    for g in groups.into_iter().filter(|g| *g > 0) {
        let Some(gp) = read_props(pkg, g) else { continue };
        let player = pkg.class_name(g) == "InterpGroupPlayer";
        let anim_sets = upk::props::object_array(pkg, &gp, "GroupAnimSets").into_iter().filter(|o| *o != 0).map(|o| pkg.obj_path(o)).collect();
        let mut group = KInterpGroup { name: str_of(&gp, "GroupName"), tracks: Vec::new(), stage_mark: str_of(&gp, "StageMarkGroup"), player, anim_sets };
        if group.stage_mark.is_empty() || group.stage_mark == "None" {
            group.stage_mark = str_of(&gp, "m_StageMarkGroup");
        }
        if group.stage_mark == "None" {
            group.stage_mark.clear();
        }
        if group.name == "None" {
            group.name.clear();
        }
        // UE3's default name for the player's group
        if player && group.name.is_empty() {
            group.name = "PlayerGroup".into();
        }
        for t in obj_list(pkg, &gp, "InterpTracks").into_iter().filter(|t| *t > 0) {
            let Some(tp) = read_props(pkg, t) else { continue };
            if matches!(tp.get("bDisableTrack"), Some(Value::Bool(true))) {
                continue;
            }
            let keys = |arr: &str| struct_array(pkg, &tp, arr);
            match pkg.class_name(t).as_str() {
                "InterpTrackMove" => group.tracks.push(KTrack::Move {
                    pos: curve(pkg, &tp, "PosTrack"),
                    rot: curve(pkg, &tp, "EulerTrack"),
                    relative: !matches!(tp.get("MoveFrame"), Some(Value::Enum(f)) if f == "IMF_World"),
                }),
                "InterpTrackDirector" => {
                    for k in keys("CutTrack") {
                        m.cuts.push((k.float("Time").unwrap_or(0.0), str_of(&k, "TargetCamGroup")));
                    }
                }
                "InterpTrackFade" => m.fade = curve(pkg, &tp, "FloatTrack"),
                "InterpTrackSoireeControl" => group.tracks.push(KTrack::Pins(
                    keys("SoireeControlKeys")
                        .iter()
                        .map(|k| {
                            let pin = k.object("Properties").filter(|o| *o > 0).and_then(|o| read_props(pkg, o)).map(|p| str_of(&p, "m_PinName")).unwrap_or_default();
                            (k.float("StartTime").unwrap_or(0.0), k.float("KeyLength").unwrap_or(0.0), pin)
                        })
                        .collect(),
                )),
                "InterpTrackEvent" => group.tracks.push(KTrack::Event(keys("EventTrack").iter().map(|k| (k.float("Time").unwrap_or(0.0), str_of(k, "EventName"))).collect())),
                "InterpTrackAkEvent" => group.tracks.push(KTrack::Sound(
                    keys("AkEvents").iter().filter_map(|k| Some((k.float("Time").unwrap_or(0.0), name_of_obj(k.object("Event").filter(|e| *e != 0)?)))).collect(),
                )),
                "InterpTrackVisibility" => group.tracks.push(KTrack::Visibility(
                    keys("VisibilityTrack").iter().map(|k| (k.float("Time").unwrap_or(0.0), !matches!(k.get("Action"), Some(Value::Enum(a)) if a == "EVTA_Hide"))).collect(),
                )),
                "InterpTrackToggle" => group.tracks.push(KTrack::Toggle(
                    keys("ToggleTrack").iter().map(|k| (k.float("Time").unwrap_or(0.0), !matches!(k.get("ToggleAction"), Some(Value::Enum(a)) if a == "ETTA_Off"))).collect(),
                )),
                "InterpTrackFloatProp" | "InterpTrackFloatMaterialParam" => {
                    let prop = str_of(&tp, "PropertyName");
                    group.tracks.push(KTrack::Float { prop, points: curve(pkg, &tp, "FloatTrack") });
                }
                "InterpTrackStretchAnimControl" => group.tracks.push(KTrack::Anim(
                    keys("AnimSeqs")
                        .iter()
                        .map(|k| {
                            let name = str_of(k, "AnimSeqName");
                            let looping = name.to_ascii_lowercase().contains("loop");
                            (k.float("StartTime").unwrap_or(0.0), name, k.float("AnimStartOffset").unwrap_or(0.0), k.float("AnimPlayRate").unwrap_or(1.0), looping)
                        })
                        .collect(),
                )),
                "InterpTrackDialog" => group.tracks.push(KTrack::Dialog(
                    keys("DialogKeys")
                        .iter()
                        .filter_map(|k| {
                            let props = k.object("Properties").filter(|o| *o > 0).and_then(|o| read_props(pkg, o))?;
                            let Some(Value::Guid(g)) = props.get("m_BlurbGUID") else { return None };
                            let event = voices.get(g).and_then(|v| v.first()).cloned().unwrap_or_default();
                            let text = texts.get(g).cloned().unwrap_or_default();
                            (!event.is_empty() || !text.is_empty()).then(|| (k.float("StartTime").unwrap_or(0.0), event, text))
                        })
                        .collect(),
                )),
                // Arkane's scene tracks: keys whose properties name a target (a group of the
                // matinee, or the player)
                "InterpTrackFaceTo" | "InterpTrackLookAt" | "InterpTrackLocomotion" => {
                    let arr = match pkg.class_name(t).as_str() {
                        "InterpTrackFaceTo" => "FaceToKeys",
                        "InterpTrackLookAt" => "LookAtKeys",
                        _ => "LocoKeys",
                    };
                    let mut list = Vec::new();
                    for k in keys(arr) {
                        let Some(kp) = k.object("Properties").filter(|o| *o > 0).and_then(|o| read_props(pkg, o)) else { continue };
                        let target = match kp.get("m_TargetType") {
                            Some(Value::Enum(e)) if e.contains("Player") => "Player".to_string(),
                            _ => str_of(&kp, "m_TargetName"),
                        };
                        list.push((k.float("StartTime").unwrap_or(0.0), k.float("KeyLength").unwrap_or(1.0), str_of(&kp, "m_SpeedIdxName"), target));
                    }
                    match pkg.class_name(t).as_str() {
                        "InterpTrackFaceTo" => group.tracks.push(KTrack::FaceTo(list.into_iter().map(|(a, b, _, d)| (a, b, d)).collect())),
                        "InterpTrackLookAt" => group.tracks.push(KTrack::LookAt(list.into_iter().map(|(a, b, _, d)| (a, b, d)).collect())),
                        _ => group.tracks.push(KTrack::Locomotion(list)),
                    }
                }
                "InterpTrackAttachment" => {
                    let mut list = Vec::new();
                    for k in keys("m_AttachmentKeys") {
                        let Some(kp) = k.object("Properties").filter(|o| *o > 0).and_then(|o| read_props(pkg, o)) else { continue };
                        let detach = matches!(kp.get("m_KeyType"), Some(Value::Enum(e)) if e.contains("Detach"));
                        let info = kp.struct_props("m_AttachmentInfo").unwrap_or_default();
                        let offset = match info.get("RelativeOffset") {
                            Some(Value::Vector(v)) => *v,
                            _ => [0.0; 3],
                        };
                        let rot = match info.get("RelativeRotation") {
                            Some(Value::Rotator(r)) => *r,
                            _ => [0; 3],
                        };
                        list.push((k.float("Time").unwrap_or(0.0), !detach, str_of(&info, "BoneName"), offset, rot));
                    }
                    if !list.is_empty() {
                        group.tracks.push(KTrack::Attach { target: str_of(&tp, "m_TargetName"), keys: list });
                    }
                }
                "InterpTrackAnimControl" => group.tracks.push(KTrack::Anim(
                    keys("AnimSeqs")
                        .iter()
                        .map(|k| {
                            (
                                k.float("StartTime").unwrap_or(0.0),
                                str_of(k, "AnimSeqName"),
                                k.float("AnimStartOffset").unwrap_or(0.0),
                                k.float("AnimPlayRate").unwrap_or(1.0),
                                matches!(k.get("bLooping"), Some(Value::Bool(true))),
                            )
                        })
                        .collect(),
                )),
                _ => {}
            }
        }
        m.groups.push(group);
    }
    m.cuts.sort_by(|a, b| a.0.total_cmp(&b.0));
    // the length isn't serialized (Arkane's runtime data): the last key ends it
    if md.as_ref().and_then(|p| p.float("InterpLength")).or(idata.float("InterpLength")).is_none() {
        let mut end: f32 = m.cuts.iter().map(|c| c.0).fold(0.0, f32::max);
        end = m.fade.iter().map(|k| k.t).fold(end, f32::max);
        for g in &m.groups {
            for t in &g.tracks {
                let last = match t {
                    KTrack::Move { pos, rot, .. } => pos.iter().chain(rot.iter()).map(|k| k.t).fold(0.0, f32::max),
                    KTrack::Event(k) | KTrack::Sound(k) => k.iter().map(|k| k.0).fold(0.0, f32::max),
                    KTrack::Visibility(k) | KTrack::Toggle(k) => k.iter().map(|k| k.0).fold(0.0, f32::max),
                    KTrack::Float { points, .. } => points.iter().map(|k| k.t).fold(0.0, f32::max),
                    KTrack::Anim(k) => k.iter().map(|k| k.0).fold(0.0, f32::max),
                    KTrack::Dialog(k) => k.iter().map(|k| k.0).fold(0.0, f32::max),
                    KTrack::FaceTo(k) | KTrack::LookAt(k) => k.iter().map(|k| k.0 + k.1).fold(0.0, f32::max),
                    KTrack::Locomotion(k) => k.iter().map(|k| k.0 + k.1).fold(0.0, f32::max),
                    KTrack::Attach { keys, .. } => keys.iter().map(|k| k.0).fold(0.0, f32::max),
                    KTrack::Pins(k) => k.iter().map(|k| k.0 + k.1).fold(0.0, f32::max),
                };
                end = end.max(last);
            }
        }
        m.length = if end > 0.0 { end } else { 5.0 };
    }
    Some(m)
}

/// Subtitle text of every dialogue blurb, by GUID.
pub(crate) fn blurb_texts(levels: &[Arc<Package>]) -> HashMap<[u32; 4], String> {
    let mut out = HashMap::new();
    for pkg in levels {
        for i in 1..=pkg.exports.len() as i32 {
            let c = pkg.class_name(i);
            if c != "DisConv_Blurb" && c != "DisConv_NonWord" {
                continue;
            }
            let Some(p) = read_props(pkg, i) else { continue };
            if let (Some(Value::Guid(g)), Some(Value::Str(t))) = (p.get("m_iBlurbGUID"), p.get("m_Text")) {
                out.entry(*g).or_insert_with(|| t.trim().to_string());
            }
        }
    }
    out
}

fn conversation_steps(pkg: &Package, conv: i32, voices: &HashMap<[u32; 4], Vec<String>>) -> Vec<crate::format::KDialogStep> {
    steps_from(pkg, read_props(pkg, conv).and_then(|p| next_node(pkg, &p)), voices)
}

/// What a chain of dialogue nodes says and does, from `start` along first links; a player
/// choice branches (each option's steps follow, ended by `End`).
fn steps_from(pkg: &Package, start: Option<i32>, voices: &HashMap<[u32; 4], Vec<String>>) -> Vec<crate::format::KDialogStep> {
    let mut steps = Vec::new();
    walk_steps(pkg, start, voices, &mut steps, &mut HashSet::new());
    steps
}

fn walk_steps(pkg: &Package, start: Option<i32>, voices: &HashMap<[u32; 4], Vec<String>>, steps: &mut Vec<crate::format::KDialogStep>, seen: &mut HashSet<i32>) {
    use crate::format::KDialogStep;
    let mut node = start;
    while let Some(n) = node {
        if !seen.insert(n) || steps.len() > 200 {
            break;
        }
        let Some(p) = read_props(pkg, n) else { break };
        let cls = pkg.class_name(n);
        let guid = |p: &Props, k: &str| match p.get(k) {
            Some(Value::Guid(g)) => Some(format!("{:08x}{:08x}{:08x}{:08x}", g[0], g[1], g[2], g[3])),
            _ => None,
        };
        match cls.as_str() {
            "DisConv_Blurb" | "DisConv_NonWord" => {
                if let Some(Value::Str(t)) = p.get("m_Text") {
                    let speaker = match p.get("m_iSpeaker") {
                        Some(Value::Int(i)) => *i,
                        _ => 0,
                    };
                    if let Some(Value::Guid(g)) = p.get("m_iBlurbGUID") {
                        if let Some(e) = voices.get(g).and_then(|v| v.first()) {
                            steps.push(KDialogStep::Voice(e.clone()));
                        }
                    }
                    steps.push(KDialogStep::Line(t.clone(), speaker));
                }
            }
            "DisConv_Soiree" => {
                if let Some(g) = guid(&p, "m_SoireeGUID") {
                    steps.push(KDialogStep::Matinee(g));
                }
            }
            "DisConv_KismetActivateRemoteEvent" => {
                if let Some(Value::Name(e)) = p.get("m_EventName") {
                    steps.push(KDialogStep::Remote(e.clone()));
                }
            }
            "DisConv_SetStoryFlag" => {
                if let Some(g) = guid(&p, "m_StoryFlag") {
                    steps.push(KDialogStep::SetFlag(g));
                }
            }
            // the player's answer: each option's own way on
            "DisConv_PlayerChoice" => {
                let texts: Vec<String> = struct_array(pkg, &p, "m_Choices_Static").iter().map(|c| str_of(c, "m_ChoiceText")).collect();
                let links: Vec<Option<i32>> = struct_array(pkg, &p, "m_OutputLinks").iter().map(|l| l.object("m_pLink").filter(|o| *o > 0)).collect();
                let at = steps.len();
                steps.push(KDialogStep::Choice(Vec::new()));
                let mut options = Vec::new();
                for (k, text) in texts.into_iter().enumerate() {
                    let begin = steps.len() as u32;
                    walk_steps(pkg, links.get(k).copied().flatten(), voices, steps, seen);
                    steps.push(KDialogStep::End);
                    options.push((text, begin));
                }
                steps[at] = KDialogStep::Choice(options);
                return;
            }
            _ => {}
        }
        node = if cls == "DisConversation" { None } else { next_node(pkg, &p) };
    }
}
