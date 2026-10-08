//! Interaction: pickups (coins, elixirs, food, keys, notes...) and doors.

use crate::gameplay::{HudMessages, Noise, PlayerStats};
use crate::level::{InstanceCollider, LevelInfo, LevelInstance, LevelSpawnSet};
use crate::player::{Player, PlayerCamera};
use crate::GameState;
use bevy::prelude::*;
use std::collections::HashMap;

pub struct InteractPlugin;

impl Plugin for InteractPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<InteractFocus>()
            .add_message::<Interaction>()
            .add_systems(OnEnter(GameState::InGame), setup_interactables.after(LevelSpawnSet))
            .add_message::<DoorBlast>()
            .add_systems(Update, (sing, find_focus.in_set(FocusSet), use_focus, npc_doors, animate_doors, break_doors).chain().run_if(in_state(GameState::InGame)));
    }
}

/// What the player looks at is known.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct FocusSet;

#[derive(Clone, Debug, PartialEq)]
pub enum PickupKind {
    Coins(u32),
    HealthElixir,
    ManaElixir,
    Food,
    Key(String),
    Note,
    /// by the original ammo type (`m_AmmoRanges`): (type, amount)
    Ammo(Vec<(u8, u32)>),
    Rune,
    BoneCharm,
    /// a blueprint for Piero (`BP_..._AbsItm`)
    Blueprint(String),
    /// a weapon (its inventory item: `Twk_Inv_PistolEliteGuard_Player`, a sword) and the
    /// ammunition with it
    Weapon(String, Vec<(u8, u32)>),
    Loot(u32),
}

#[derive(Component)]
pub struct Pickup {
    pub kind: PickupKind,
    pub label: String,
    pub entities: Vec<Entity>,
    /// Index into `scene.pickups`.
    pub index: u32,
}

/// A rune's or bone charm's song (the original `Snd_UI_Rune_Amb` / `Snd_UI_Whale_Bone_Charms_Amb`
/// that leads the player to it): its ambient emitter.
#[derive(Component)]
pub struct Song(pub usize);

/// Give the runes and charms of the level their song (once the level's ambients are set up).
pub fn sing(mut commands: Commands, mut ambients: ResMut<crate::audio::Ambients>, pickups: Query<(Entity, &Pickup, &Transform), Without<Song>>) {
    for (e, p, t) in &pickups {
        let ev = match p.kind {
            PickupKind::Rune => "Snd_UI_Rune_Amb",
            PickupKind::BoneCharm => "Snd_UI_Whale_Bone_Charms_Amb",
            _ => continue,
        };
        let i = ambients.add(ev, t.translation);
        commands.entity(e).insert(Song(i));
    }
}

#[derive(Component)]
pub struct Door {
    pub base_rot: Quat,
    pub open: f32,
    pub target: f32,
    pub collider: Option<Entity>,
    pub collider_rot: Quat,
    pub dir: f32,
    /// the way its panel swings with `dir` 1, local: across its face (the mesh's thin axis; the
    /// panel runs from the hinge along the other, +Z for most, +X for the cells')
    pub normal: Vec3,
    pub locked: bool,
    /// the keys that open it (`m_MatchingKeys`)
    pub keys: Vec<String>,
    /// it can be broken down (`m_pBreakSteps`); it leads to another map (its use goes on to
    /// `DisSeqAct_GotoPlayerTravelDestination`)
    pub breakable: bool,
    pub travel: bool,
    /// what is left of a breakable one (`m_Health`), and whether it lies in pieces
    pub health: f32,
    pub broken: bool,
}

/// A blast of wind (Wind Blast) through the doors before it: from, which way, how far, its
/// half angle (radians), its damage.
#[derive(Message, Clone, Copy)]
pub struct DoorBlast {
    pub from: Vec3,
    pub dir: Vec3,
    pub reach: f32,
    pub half_angle: f32,
    pub damage: f32,
}

impl Door {
    /// The key on Corvo's ring that opens it.
    pub fn key_in<'a>(&self, ring: &'a [String]) -> Option<&'a String> {
        ring.iter().find(|k| self.keys.iter().any(|m| m.eq_ignore_ascii_case(k)))
    }
}

/// The level's ways out to other maps, by script actor (`kismet::travel_actors`).
#[derive(Resource, Default)]
pub struct TravelActors(pub std::collections::HashSet<u32>);

/// Something the level scripts can be used on (levers, buttons, ...): `actor` indexes
/// `scene.kismet.actors`.
#[derive(Component)]
pub struct Usable {
    pub actor: u32,
    pub label: String,
}

/// The player used something (reported to the level scripts).
#[derive(Message, Clone, Debug)]
pub enum Interaction {
    Pickup(u32),
    /// door instance, opened (else closed), swung clockwise
    Door { instance: u32, opened: bool, cw: bool },
    DoorLocked(u32),
    Usable(u32),
    /// a prop (its instance) picked up, or broken
    Movable { instance: u32, broken: bool },
    /// a prop (its instance) knocked against something, at this speed (m/s)
    Knocked { instance: u32, speed: f32 },
    /// a usable object (its actor) locked or unlocked
    UsableLock { actor: u32, locked: bool },
    /// a whale oil receptacle (its actor's name, where) got a tank or lost it
    Receptacle { actor: String, at: Vec3, plugged: bool },
    /// a character (its spawner) stopped at a distractor (its name, where), or went on
    Distracted { distractor: String, at: Vec3, spawner: u32, start: bool },
    /// a spawner's body: picked up (0), dropped (1), discovered (2)
    Corpse { spawner: u32, what: u32 },
    /// a door's keyhole looked through (else left)
    Keyhole { instance: u32, used: bool },
    /// a tripwire tripped (`scene.traps`)
    Tripwire(u32),
}

/// What the player is currently looking at (for the HUD prompt). `.0` is true while an
/// interactable is focused so other systems on the Use key don't also consume it; `.2` is what
/// can be done (the HUD tweak's `m_InteractionTexts`), `.3` the name of what it is done to
/// (the interaction window's title).
#[derive(Resource, Default)]
pub struct InteractFocus(pub bool, pub Option<Entity>, pub String, pub String);

fn classify(class: &str, tweak: &str, name: &str) -> (PickupKind, String) {
    let t = tweak.to_ascii_lowercase();
    let n = name.to_ascii_lowercase();
    let num = t
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|s| s.parse::<u32>().ok())
        .filter(|v| *v > 0 && *v <= 500)
        .last();
    if t.contains("coin") || t.contains("money") || t.contains("pouch") {
        let v = num.unwrap_or(10);
        return (PickupKind::Coins(v), format!("{v} Coins"));
    }
    if class == "DisElixirHealth" || t.contains("elixir_health") || t.contains("healthelixir") || t.contains("sokolov") {
        return (PickupKind::HealthElixir, "Sokolov's Elixir".into());
    }
    if t.contains("mana") || t.contains("piero") || t.contains("remedy") {
        return (PickupKind::ManaElixir, "Piero's Spiritual Remedy".into());
    }
    if class.starts_with("DisKey") || t.contains("key") {
        return (PickupKind::Key(name.to_string()), "Key".into());
    }
    if class.contains("Note") || t.contains("note") || t.contains("letter") || t.contains("book") || t.contains("audiolog") {
        return (PickupKind::Note, "Note".into());
    }
    if t.contains("food") || t.contains("meat") || t.contains("apple") || t.contains("bread") || t.contains("cheese") || t.contains("tea") || t.contains("pear") {
        return (PickupKind::Food, "Food".into());
    }
    if t.contains("bolt") || t.contains("ammo") || t.contains("bullet") || t.contains("dart") {
        let ty = if t.contains("sleep") || t.contains("dart") {
            3
        } else if t.contains("flare") {
            4
        } else if t.contains("bolt") {
            2
        } else if t.contains("explosive") {
            1
        } else if t.contains("springrazor") {
            5
        } else if t.contains("sticky") {
            7
        } else if t.contains("grenade") {
            6
        } else {
            0
        };
        return (PickupKind::Ammo(vec![(ty, 2)]), "Ammunition".into());
    }
    if t.contains("rune") {
        return (PickupKind::Rune, "Rune".into());
    }
    if t.starts_with("bp_") || t.contains("blueprint") {
        let base = n.split("_twk").next().unwrap_or(&n).split("_AbsItm").next().unwrap_or(&n);
        return (PickupKind::Blueprint(format!("{base}_AbsItm")), "Blueprint".into());
    }
    if t.contains("bonecharm") || t.contains("bone_charm") {
        return (PickupKind::BoneCharm, "Bone Charm".into());
    }
    let v = num.unwrap_or(5).min(50);
    let label = n
        .split('_')
        .next()
        .unwrap_or("Item")
        .trim_start_matches("Dis")
        .trim_start_matches("Dishonored")
        .to_string();
    (PickupKind::Loot(v), if label.is_empty() { "Valuables".into() } else { label })
}

fn setup_interactables(
    mut commands: Commands,
    level: Option<Res<LevelInfo>>,
    instances: Query<(Entity, &LevelInstance, &Transform, Option<&InstanceCollider>)>,
    data: Res<crate::gamedata::Data>,
    (campaign, script): (Res<crate::gameplay::Campaign>, Res<crate::campaign::CampaignScript>),
) {
    let Some(level) = level else { return };
    let loaded = crate::campaign::initial_levels(&level.scene, &campaign, &script);
    let travel = crate::kismet::travel_actors(&level.scene.kismet);
    if !travel.is_empty() {
        info!("{} ways out to other maps: {:?}", travel.len(), travel.iter().filter_map(|a| level.scene.kismet.actors.get(*a as usize)).map(|a| a.name.as_str()).collect::<Vec<_>>());
    }
    let travel_door = |name: &str| level.scene.kismet.actors.iter().enumerate().any(|(i, a)| a.name == name && travel.contains(&(i as u32)));
    commands.insert_resource(TravelActors(travel.clone()));
    let mut by_actor: HashMap<&str, Vec<Entity>> = HashMap::new();
    for (e, li, t, col) in &instances {
        by_actor.entry(li.actor.as_str()).or_default().push(e);
        if li.class == "DisDoor" {
            let def = level.scene.door(&li.actor, li.index);
            // (one left open: swung there on the first frame)
            let open = def.and_then(|d| d.open_start);
            commands.entity(e).insert(Door {
                base_rot: t.rotation,
                open: if open.is_some() { 0.99 } else { 0.0 },
                target: if open.is_some() { 1.0 } else { 0.0 },
                collider: col.map(|c| c.0),
                collider_rot: t.rotation,
                dir: if open == Some(false) { -1.0 } else { 1.0 },
                normal: level
                    .scene
                    .instances
                    .get(li.index as usize)
                    .and_then(|i| level.scene.meshes.get(i.mesh as usize))
                    .map(|m| if m.max[0] - m.min[0] <= m.max[2] - m.min[2] { Vec3::X } else { Vec3::NEG_Z })
                    .unwrap_or(Vec3::X),
                locked: def.is_some_and(|d| d.locked_start),
                keys: def.map(|d| d.keys.clone()).unwrap_or_default(),
                breakable: def.is_some_and(|d| d.breakable),
                travel: travel_door(&li.actor),
                health: def.map(|d| d.health).unwrap_or(0.0),
                broken: false,
            });
        }
    }
    let mut n = 0;
    for (pi, p) in level.scene.pickups.iter().enumerate() {
        // a streamed sublevel's pickups wait for it; the scripts' factories make theirs
        if p.factory || crate::campaign::pickup_unloaded(&level.scene, &loaded, pi as u32) {
            continue;
        }
        let entities = by_actor.get(p.name.as_str()).cloned().unwrap_or_default();
        let pos = Vec3::from(p.position);
        commands.spawn((make_pickup(&level, &data, pi, entities), Transform::from_translation(pos), DespawnOnExit(GameState::InGame)));
        n += 1;
    }
    info!("{n} pickups");
}

/// How many of the level's pickups are of a kind (its runes, its bone charms).
pub fn count_kind(level: &LevelInfo, f: impl Fn(&PickupKind) -> bool) -> u32 {
    level.scene.pickups.iter().filter(|p| f(&classify(&p.class, &p.kind, &p.name).0)).count() as u32
}

/// A pickup of the scene: what it gives and its name.
pub fn make_pickup(level: &LevelInfo, data: &crate::gamedata::Data, pi: usize, entities: Vec<Entity>) -> Pickup {
    let p = &level.scene.pickups[pi];
    {
        let (mut kind, mut label) = classify(&p.class, &p.kind, &p.name);
        // a blueprint: the upgrade its item names
        if let (PickupKind::Blueprint(bp), Some(it)) = (&mut kind, p.item.rsplit('.').next().filter(|s| !s.is_empty())) {
            *bp = it.to_string();
        }
        // the tweak's own name; "`i" / "`k" stand for its item's (and the key's) name
        let token = p.label.starts_with('`');
        let own = (!token).then_some(p.label.as_str()).filter(|l| !l.is_empty());
        // money: the tweak's quantity
        if p.coins && p.quantity > 0 {
            kind = PickupKind::Coins(p.quantity);
            label = own.map(str::to_string).unwrap_or_else(|| if p.quantity == 1 { "1 Coin".to_string() } else { format!("{} Coins", p.quantity) });
        } else if let Some(l) = own.filter(|_| !matches!(kind, PickupKind::Key(_))) {
            label = l.to_string();
        }
        if !p.ammo.is_empty() {
            // the cooked ammunition: its type names it
            let mut probe = PlayerStats::default();
            label = crate::gadgets::give_ammo(&mut probe, p.ammo[0].0, 0).to_string();
            kind = PickupKind::Ammo(p.ammo.clone());
        }
        // a weapon: what it gives (with its bullets)
        let w = p.inv_item.to_ascii_lowercase();
        if w.contains("sword") || w.contains("pistol") || w.contains("crossbow") {
            kind = PickupKind::Weapon(p.inv_item.clone(), p.ammo.clone());
            label = own.map(str::to_string).unwrap_or_else(|| (if w.contains("sword") { "Sword" } else if w.contains("pistol") { "Pistol" } else { "Crossbow" }).to_string());
        }
        // notes, keys, valuables and blueprints carry their own names
        if let Some((t, _)) = crate::journal::location_map(&p.item) {
            label = format!("Map: {t}");
        } else if let Some((name, _)) = data.0.abstract_items.get(&p.item).filter(|(n, _)| !n.is_empty()) {
            if token || matches!(kind, PickupKind::Note | PickupKind::Loot(_) | PickupKind::Key(_) | PickupKind::Blueprint(_)) {
                label = name.clone();
            }
        }
        Pickup { kind, label, entities, index: pi as u32 }
    }
}

#[allow(clippy::type_complexity)]
fn find_focus(
    mut focus: ResMut<InteractFocus>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    pickups: Query<(Entity, &Pickup, &Transform, Option<&crate::pickpocket::Pocket>)>,
    doors: Query<(Entity, &Door, &GlobalTransform, &LevelInstance)>,
    usables: Query<(Entity, &Usable, &Transform)>,
    player: Query<&Player>,
    rapier: bevy_rapier3d::prelude::ReadRapierContext,
    (bodies, carry, data, owners, stats): (
        Query<(Entity, &crate::npc::Npc, &Transform, Option<&crate::anim::Animator>), Without<Player>>,
        Res<crate::carry::Carry>,
        Res<crate::gamedata::Data>,
        Query<(&crate::npc::Npc, &Transform), Without<Player>>,
        Res<PlayerStats>,
    ),
) {
    focus.0 = false;
    focus.1 = None;
    focus.2.clear();
    focus.3.clear();
    let Ok(cg) = cam.single() else { return };
    if player.single().map(|p| p.locked).unwrap_or(true) {
        return;
    }
    // with a body on the shoulder, [F] puts it down
    if let Some(b) = carry.body {
        focus.0 = true;
        focus.1 = Some(b);
        // (what he can do with it is the context line's: `prompts`)
        return;
    }
    let eye = cg.translation();
    let fwd = cg.forward().as_vec3();
    // (score, what, what can be done, its name)
    let mut best: Option<(f32, Entity, String, String)> = None;
    for (e, p, t, pocket) in &pickups {
        let to = t.translation - eye;
        let d = to.length();
        if d > 2.4 {
            continue;
        }
        // in someone's pocket: from behind and unseen, or from the body
        if pocket.is_some_and(|pk| !pk.reachable(&owners, eye)) {
            continue;
        }
        let c = to.normalize_or_zero().dot(fwd);
        if c < 0.82 {
            continue;
        }
        // what can be taken comes first when it's looked at
        let score = d * (2.0 - c) - 0.3;
        if best.as_ref().map(|b| score < b.0).unwrap_or(true) {
            // (`DUI_Crosshair_Pickup`)
            best = Some((score, e, format!("{} Loot", crate::bindings::hint(crate::bindings::Act::Use)), p.label.clone()));
        }
    }
    for (e, door, gt, _) in &doors {
        if door.broken {
            continue;
        }
        let to = gt.translation() + Vec3::Y * 1.0 - eye;
        let d = to.length();
        if d > 2.6 {
            continue;
        }
        let c = to.normalize_or_zero().dot(fwd);
        if c < 0.6 {
            continue;
        }
        let score = d * (2.0 - c) + 0.3;
        if best.as_ref().map(|b| score < b.0).unwrap_or(true) {
            let what = if !door.locked {
                if door.target > 0.5 { "Close door" } else { "Open door" }
            } else if door.key_in(&stats.keys).is_some() {
                "Unlock door"
            } else {
                "Locked"
            };
            best = Some((score, e, format!("{} {what}", crate::bindings::hint(crate::bindings::Act::Use)), String::new()));
        }
    }
    // what is behind a wall can't be used
    let ctx = rapier.single().ok();
    let world = bevy_rapier3d::prelude::QueryFilter::default().groups(bevy_rapier3d::prelude::CollisionGroups::new(bevy_rapier3d::prelude::Group::ALL, crate::level::GROUP_WORLD));
    let blocked = |to: Vec3| -> bool {
        let d = to.length();
        ctx.as_ref().and_then(|c| c.cast_ray(eye, to / d.max(1e-3), (d - 0.35).max(0.0), true, world)).is_some()
    };
    for (e, u, t) in &usables {
        let to = t.translation - eye;
        let d = to.length();
        if d > 2.4 {
            continue;
        }
        let c = to.normalize_or_zero().dot(fwd);
        if c < 0.75 || blocked(to) {
            continue;
        }
        let score = d * (2.0 - c) + 0.1;
        if best.as_ref().map(|b| score < b.0).unwrap_or(true) {
            best = Some((score, e, format!("{} {}", crate::bindings::hint(crate::bindings::Act::Use), u.label), String::new()));
        }
    }
    // bodies (the unconscious and the dead) can be picked up
    for (e, npc, t, anim) in &bodies {
        if !npc.is_down() || !anim.is_some_and(crate::carry::can_carry) {
            continue;
        }
        let to = t.translation - Vec3::Y * 0.75 - eye;
        let d = to.length();
        if d > 2.4 {
            continue;
        }
        let c = to.normalize_or_zero().dot(fwd);
        if c < 0.8 || blocked(to) {
            continue;
        }
        let score = d * (2.0 - c) + 0.2;
        if best.as_ref().map(|b| score < b.0).unwrap_or(true) {
            let name = data.0.pawn_names.get(&npc.pawn).cloned().unwrap_or_else(|| "Body".to_string());
            // (`DUI_Crosshair_Carry`)
            best = Some((score, e, format!("Hold {} Carry", crate::bindings::hint(crate::bindings::Act::Use)), name));
        }
    }
    if let Some((_, e, text, title)) = best {
        focus.0 = true;
        focus.1 = Some(e);
        focus.2 = text;
        focus.3 = title;
    }
}

#[allow(clippy::too_many_arguments)]
pub fn use_focus(
    mut commands: Commands,
    (keys, bind): (Res<ButtonInput<KeyCode>>, Res<crate::bindings::Bindings>),
    focus: Res<InteractFocus>,
    mut stats: ResMut<PlayerStats>,
    mut msgs: ResMut<HudMessages>,
    mut noise: MessageWriter<Noise>,
    mut used: MessageWriter<Interaction>,
    pickups: Query<(&Pickup, Option<&Song>)>,
    usables: Query<&Usable>,
    mut doors: Query<(&mut Door, &GlobalTransform, &LevelInstance)>,
    player: Query<&Transform, With<Player>>,
    (level, mut timed, data, mut ambients, mut read): (Option<Res<LevelInfo>>, ResMut<crate::audio::TimedSounds>, Res<crate::gamedata::Data>, ResMut<crate::audio::Ambients>, MessageWriter<crate::journal::ReadNote>),
    (keyholes, peek, mut plog, mut tw): (Query<(), With<crate::keyhole::KeyholeDoor>>, Res<crate::keyhole::Peek>, ResMut<crate::pickuplog::PickupLog>, ResMut<crate::tutwindow::TutorialWindow>),
) {
    let use_key = bind.key(crate::bindings::Act::Use);
    let Some(e) = focus.1 else { return };
    // a shut door with a keyhole: a tap (on release) opens it, a hold looks through
    let tap = keyholes.contains(e) && doors.get(e).is_ok_and(|(d, _, _)| d.target < 0.5);
    if tap {
        if !keys.just_released(use_key) || peek.consumed {
            return;
        }
    } else if !keys.just_pressed(use_key) {
        return;
    }
    if let Ok((p, song)) = pickups.get(e) {
        // a blade while he has one: left there (the original's cannot-pick-up sound)
        if matches!(&p.kind, PickupKind::Weapon(w, _) if w.to_ascii_lowercase().contains("sword")) && !stats.unarmed {
            timed.schedule(&[(0.0, "Snd_UI_Ingame_Max_Ammo".to_string())], None);
            return;
        }
        // found: its song stops
        if let Some(s) = song {
            ambients.set_enabled(s.0, false);
        }
        let sound = match &p.kind {
            PickupKind::Coins(_) | PickupKind::Loot(_) => "Snd_UI_Ingame_Gold_Pickup",
            PickupKind::HealthElixir | PickupKind::ManaElixir => "Snd_UI_Ingame_Elixir_Pickup",
            PickupKind::Food => "Snd_UI_Ingame_Food_Pickup",
            PickupKind::Key(_) => "Snd_UI_Ingame_Key_Pickup",
            PickupKind::Note => "Snd_UI_Ingame_Note_Open",
            PickupKind::Ammo(_) => "Snd_UI_Ingame_Ammo_Pickup",
            PickupKind::Rune => "Snd_UI_Rune_Pickup",
            PickupKind::BoneCharm => "Snd_UI_Ingame_Generic_Pickup",
            PickupKind::Blueprint(_) => "Snd_UI_Ingame_Note_Open",
            PickupKind::Weapon(..) => "Snd_UI_Ingame_Ammo_Pickup",
        };
        timed.schedule(&[(0.0, sound.to_string())], None);
        match &p.kind {
            PickupKind::Coins(v) | PickupKind::Loot(v) => {
                stats.coins += v;
                stats.coins_found += v;
                plog.add(format!("{} +{v}", if p.label.is_empty() || p.label.starts_with('`') { "Coins" } else { p.label.as_str() }), Some("Money_Small"));
            }
            PickupKind::HealthElixir => {
                stats.health_elixirs += 1;
                plog.add("Sokolov's Health Elixir", Some("HealthElixir_Small"));
            }
            PickupKind::ManaElixir => {
                stats.mana_elixirs += 1;
                plog.add("Piero's Spiritual Remedy", Some("ManaElixir_Small"));
            }
            PickupKind::Food => {
                stats.health = (stats.health + 10.0).min(stats.max_health);
                plog.add(if p.label.is_empty() { "Food".to_string() } else { p.label.clone() }, None);
            }
            PickupKind::Key(_) => {
                // onto the ring, by the name doors know it by
                let name = level.as_ref().and_then(|l| l.scene.pickups.get(p.index as usize)).map(|sp| sp.key.clone()).filter(|k| !k.is_empty()).unwrap_or_else(|| p.label.clone());
                if !stats.keys.contains(&name) {
                    stats.keys.push(name.clone());
                }
                plog.add(name, Some("Key_Small"));
            }
            PickupKind::Note => {
                // the note's text (its abstract item) goes to the journal, which opens on it
                let item = level.as_ref().and_then(|l| l.scene.pickups.get(p.index as usize)).map(|sp| sp.item.clone()).unwrap_or_default();
                if data.0.abstract_items.contains_key(&item) || crate::journal::location_map(&item).is_some() {
                    // (counted, for the scripts that ask how many: rat viscera)
                    *stats.items.entry(item.clone()).or_default() += 1;
                    if !stats.notes.contains(&item) {
                        stats.notes.push(item.clone());
                    }
                    if crate::journal::is_mission_item(&data, &item) {
                        // (a mission's item, not a note: to the pickup log and the journal's
                        // Mission Items)
                        let name = data.0.abstract_items.get(&item).map(|a| a.0.clone()).unwrap_or_default();
                        let icon = data.0.journal_items.get(&item).map(|j| format!("{}_Small", j.1)).filter(|i| i.len() > 6);
                        plog.add_named(name, icon);
                    } else {
                        read.write(crate::journal::ReadNote(item));
                    }
                } else {
                    msgs.push("You read the note");
                }
            }
            PickupKind::Ammo(list) => {
                for &(ty, n) in list {
                    let name = crate::gadgets::give_ammo(&mut stats, ty, n);
                    plog.add(format!("{name} +{n}"), crate::pickuplog::ammo_icon(ty));
                }
            }
            PickupKind::Rune => {
                stats.runes += 1;
                stats.runes_found += 1;
                plog.add("Rune", Some("Rune_Small"));
                // the HUD tweak's `m_RuneFoundMessage` ("`c/`t Runes found"), and the first time
                // `m_PressKeyToBuyPowers` in the tutorial window, with the rune
                let total = level.as_ref().map(|l| count_kind(l, |k| matches!(k, PickupKind::Rune))).unwrap_or(0).max(stats.runes_found);
                msgs.push(format!("{}/{} Runes found", stats.runes_found, total));
                if !stats.hints.iter().any(|h| h == "rune") {
                    stats.hints.push("rune".into());
                    tw.push(data.hud_text("m_PressKeyToBuyPowers", "New Rune added<br />Press `GBA_Journal` to acquire Powers"), crate::tutwindow::IMG_RUNE);
                }
            }
            PickupKind::BoneCharm => {
                stats.bone_charms += 1;
                stats.charms_found += 1;
                // `m_BoneCharmFoundMessage`, and the first time `m_PressKeyToEquipBoneCharms` (in
                // the tutorial window, with the charm)
                let total = level.as_ref().map(|l| count_kind(l, |k| matches!(k, PickupKind::BoneCharm))).unwrap_or(0).max(stats.charms_found);
                msgs.push(format!("{}/{} Bone Charms found", stats.charms_found, total));
                if !stats.hints.iter().any(|h| h == "charm") {
                    stats.hints.push("charm".into());
                    tw.push(data.hud_text("m_PressKeyToEquipBoneCharms", "New Bone Charm added<br />Press `GBA_Journal` to equip"), crate::tutwindow::IMG_CHARM);
                }
                // which charm: one not found yet, at random (as the original deals them)
                let owned = stats.charms_owned.clone();
                let left: Vec<&str> = data.0.charms.iter().filter_map(|c| c.levels.first()).map(|l| l.0.as_str()).filter(|n| !owned.iter().any(|o| o == n)).collect();
                if !left.is_empty() {
                    let name = left[rand::random_range(0..left.len())].to_string();
                    plog.add(format!("Bone Charm: {name}"), Some("BoneCharms_Small"));
                    stats.charms_owned.push(name);
                } else {
                    plog.add("Bone Charm", Some("BoneCharms_Small"));
                }
            }
            PickupKind::Blueprint(bp) => {
                if !stats.upgrades.contains(bp) {
                    stats.upgrades.push(bp.clone());
                }
                plog.add("Blueprint", Some("BluePrint_Small"));
                msgs.push("Blueprint found: bring it to Piero");
            }
            PickupKind::Weapon(item, ammo) => {
                // his sword back (the Prison's), or a pistol before the Hound Pits' crossbow
                let w = item.to_ascii_lowercase();
                if w.contains("sword") {
                    stats.unarmed = false;
                    stats.sheathed = false;
                } else if w.contains("crossbow") {
                    stats.weapons = true;
                    stats.no_crossbow = false;
                } else if !stats.weapons {
                    stats.weapons = true;
                    stats.no_crossbow = true;
                }
                let icon = if w.contains("sword") {
                    Some(if w.contains("elite") || w.contains("city") { "City_Sword_Small" } else if w.contains("overseer") { "Overseer_sabre_Small" } else if w.contains("assassin") { "Assassins_Sword_Small" } else if w.contains("thug") { "ThugSword_Small" } else { "CorvoSword_Small" })
                } else if w.contains("crossbow") {
                    Some("Crossbow_Small")
                } else if w.contains("elite") {
                    Some("EliteGun_Small")
                } else {
                    Some("Gun_Small")
                };
                plog.add(p.label.clone(), icon);
                for &(ty, n) in ammo {
                    let name = crate::gadgets::give_ammo(&mut stats, ty, n);
                    plog.add(format!("{name} +{n}"), crate::pickuplog::ammo_icon(ty));
                }
            }
        }
        for &m in &p.entities {
            commands.entity(m).despawn();
        }
        commands.entity(e).despawn();
        used.write(Interaction::Pickup(p.index));
        return;
    }
    if let Ok(u) = usables.get(e) {
        used.write(Interaction::Usable(u.actor));
        return;
    }
    if let Ok((mut door, gt, li)) = doors.get_mut(e) {
        let sounds = level.as_ref().and_then(|l| l.scene.door(&li.actor, li.index));
        let at = Some(gt.translation());
        if door.locked {
            // a key on the ring opens it (and it swings open at once, as the original's do)
            if let Some(k) = door.key_in(&stats.keys).cloned() {
                door.locked = false;
                if let Some(s) = sounds.filter(|s| !s.unlocked.is_empty()) {
                    timed.schedule(&[(0.0, s.unlocked.clone())], at);
                }
                msgs.push(format!("Unlocked: {k}"));
            } else {
                if let Some(s) = sounds.filter(|s| !s.locked.is_empty()) {
                    timed.schedule(&[(0.0, s.locked.clone())], at);
                }
                msgs.push("Locked");
                used.write(Interaction::DoorLocked(li.index));
                return;
            }
        }
        if door.target > 0.5 {
            door.target = 0.0;
            if let Some(s) = sounds {
                timed.schedule(&s.close, at);
            }
            used.write(Interaction::Door { instance: li.index, opened: false, cw: door.dir > 0.0 });
        } else {
            // swing away from the player
            if let Ok(pt) = player.single() {
                let side = (gt.rotation() * door.normal).dot(pt.translation - gt.translation());
                door.dir = if side > 0.0 { -1.0 } else { 1.0 };
            }
            door.target = 1.0;
            if let Some(s) = sounds {
                timed.schedule(&s.open, at);
            }
            used.write(Interaction::Door { instance: li.index, opened: true, cw: door.dir > 0.0 });
        }
        noise.write(Noise { pos: gt.translation(), radius: 5.0, combat: false });
    }
}

/// Characters open the unlocked doors in their way (swinging them away), as the original's
/// path objects let them through.
fn npc_doors(
    level: Option<Res<LevelInfo>>,
    mut timed: ResMut<crate::audio::TimedSounds>,
    mut doors: Query<(&mut Door, &GlobalTransform, &LevelInstance)>,
    npcs: Query<(&crate::npc::Npc, &Transform)>,
) {
    for (n, nt) in &npcs {
        let v = n.velocity.with_y(0.0);
        if n.is_down() || n.target.is_none() || v.length() < 0.3 {
            continue;
        }
        let dir = v.normalize();
        for (mut door, gt, li) in &mut doors {
            if door.locked || door.broken || door.target > 0.5 {
                continue;
            }
            let to = gt.translation() - nt.translation;
            if to.with_y(0.0).length() > 1.6 || to.y.abs() > 2.0 || to.with_y(0.0).normalize_or_zero().dot(dir) < -0.2 {
                continue;
            }
            let side = (gt.rotation() * door.normal).dot(nt.translation - gt.translation());
            door.dir = if side > 0.0 { -1.0 } else { 1.0 };
            door.target = 1.0;
            if let Some(s) = level.as_ref().and_then(|l| l.scene.door(&li.actor, li.index)) {
                timed.schedule(&s.open, Some(gt.translation()));
            }
        }
    }
}

fn animate_doors(
    time: Res<Time>,
    mut doors: Query<(&mut Door, &mut Transform), Without<bevy_rapier3d::prelude::Collider>>,
    mut colliders: Query<&mut Transform, (With<bevy_rapier3d::prelude::Collider>, Without<Door>)>,
) {
    for (mut door, mut t) in &mut doors {
        if (door.open - door.target).abs() < 1e-3 {
            continue;
        }
        let step = time.delta_secs() * 1.6;
        door.open = if door.target > door.open { (door.open + step).min(door.target) } else { (door.open - step).max(door.target) };
        let r = Quat::from_rotation_y(door.dir * door.open * 1.6);
        t.rotation = r * door.base_rot;
        if let Some(c) = door.collider {
            if let Ok(mut ct) = colliders.get_mut(c) {
                ct.rotation = r * door.collider_rot;
            }
        }
    }
}

/// Breakable doors (`DisTweaks_Door`'s `m_pBreakSteps`) worn down by Wind Blast and blasts:
/// a blow under `m_DamageThreshold` leaves no mark; at no `m_Health` left the door breaks (its
/// last step: the wood's sound, the AI's alarm, the splinters, the pieces) and is gone (saved
/// so: `save.rs`'s `broken_doors`).
#[allow(clippy::too_many_arguments)]
fn break_doors(
    mut commands: Commands,
    level: Option<Res<LevelInfo>>,
    assets: Option<Res<crate::level::GameAssets>>,
    (mut winds, mut blasts): (MessageReader<DoorBlast>, MessageReader<crate::gadgets::Explosion>),
    mut doors: Query<(&mut Door, &LevelInstance, &Transform, &mut Visibility)>,
    (mut sfx, mut noise, mut fx): (MessageWriter<crate::audio::PostEvent>, MessageWriter<Noise>, MessageWriter<crate::particles::SpawnEffect>),
) {
    let (Some(level), Some(assets)) = (level, assets) else { return };
    let winds: Vec<DoorBlast> = winds.read().copied().collect();
    let blasts: Vec<(Vec3, f32, f32)> = blasts.read().map(|b| (b.at, b.radius, b.damage)).collect();
    for (mut door, li, t, mut vis) in &mut doors {
        // (one hidden by the level's scripts is not there to break)
        if !door.breakable || door.broken || *vis == Visibility::Hidden {
            continue;
        }
        // (a map cooked before doors had their health is left whole)
        let Some(def) = level.scene.door(&li.actor, li.index).filter(|d| d.health > 0.0) else { continue };
        let mid = t.translation + Vec3::Y * 1.0;
        let mut dmg = 0.0f32;
        for w in &winds {
            let to = mid - w.from;
            let d = to.length();
            if d <= w.reach && to.normalize_or_zero().dot(w.dir).acos() <= w.half_angle {
                dmg = dmg.max(w.damage);
            }
        }
        for (at, radius, damage) in &blasts {
            let d = at.distance(mid);
            if d <= *radius {
                dmg = dmg.max(*damage * (1.0 - d / radius.max(0.01)).max(0.3));
            }
        }
        if dmg <= 0.0 || dmg < def.threshold {
            continue;
        }
        door.health -= dmg;
        if door.health > 0.0 {
            continue;
        }
        door.broken = true;
        *vis = Visibility::Hidden;
        if let Some(c) = door.collider {
            commands.entity(c).insert(bevy_rapier3d::prelude::ColliderDisabled);
        }
        if let Some(b) = &def.breaks {
            crate::props::break_pieces(&mut commands, &assets, b, t, &mut sfx, &mut noise, &mut fx);
        }
        info!("door {} breaks", li.actor);
    }
}
