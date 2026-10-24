//! Interaction: pickups (coins, elixirs, food, keys, notes...) and doors.

use crate::gameplay::{HudMessages, Noise, NoiseKind, PlayerStats};
use crate::level::{InstanceCollider, LevelInfo, LevelInstance, LevelSpawnSet};
use crate::player::{Player, PlayerCamera};
use crate::GameState;
use bevy::prelude::*;
use std::collections::HashMap;

pub struct InteractPlugin;

impl Plugin for InteractPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<InteractFocus>()
            .init_resource::<ForceUse>()
            .init_resource::<AmmoPickupRestore>()
            .add_systems(OnEnter(GameState::InGame), |mut pending: ResMut<AmmoPickupRestore>| pending.0.clear())
            .add_systems(Update, restore_ammo_pickups.after(crate::save::restore_npcs).before(find_focus).run_if(in_state(GameState::InGame)))
            .add_message::<Interaction>()
            .add_systems(OnEnter(GameState::InGame), setup_interactables.after(LevelSpawnSet))
            .add_message::<DoorBlast>()
            .add_systems(Update, (sing, find_focus.in_set(FocusSet), use_focus, npc_doors, wind_doors, animate_doors, break_doors).chain().run_if(in_state(GameState::InGame)));
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
    /// a corrupted charm (The Brigmore Witches: `DisDLC07Tweaks_WhaleBoneCharmCracked`)
    CorruptedCharm,
    /// a blueprint for Piero (`BP_..._AbsItm`)
    Blueprint(String),
    /// a weapon (its inventory item: `Twk_Inv_PistolEliteGuard_Player`, a sword) and the
    /// ammunition with it
    Weapon(String, Vec<(u8, u32)>),
    Loot(u32),
}

/// The round the Scavenger charm may add to an ammunition pickup's kind (`n` rounds, its
/// `chance`, the roll in [0, 1)).
fn scavenged(n: u32, chance: f32, roll: f32) -> u32 {
    u32::from(n > 1 && roll < chance)
}

impl PickupKind {
    fn at_capacity(&self, stats: &PlayerStats, attrs: &crate::gamedata::Attrs) -> bool {
        match self {
            Self::Ammo(list) => list.iter().all(|(ty, _)| attrs.ammo_capacity.get(*ty as usize).is_none_or(|cap| crate::gadgets::ammo_count(stats, *ty) >= *cap)),
            Self::HealthElixir => stats.health_elixirs >= attrs.elixir_capacity(false),
            Self::ManaElixir => stats.mana_elixirs >= attrs.elixir_capacity(true),
            _ => false,
        }
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;

    #[test]
    fn scavenger_adds_a_round_to_kinds_of_more_than_one_on_its_roll() {
        assert_eq!(scavenged(3, 0.5, 0.2), 1);
        assert_eq!(scavenged(3, 0.5, 0.7), 0);
        assert_eq!(scavenged(1, 0.5, 0.0), 0, "a single round is not added to");
        assert_eq!(scavenged(3, 0.0, 0.0), 0, "not worn");
    }

    #[test]
    fn remaining_ammo_restores_by_instance_including_late_factory_pickups() {
        let saved = vec![AmmoPickupSave { index: 4, position: [1.0, 0.0, 0.0], amounts: vec![(0, 1)] },
            AmmoPickupSave { index: 4, position: [1.0, 0.0, 0.0], amounts: vec![(0, 2)] },
            AmmoPickupSave { index: 4, position: [1.0, 0.0, 0.0], amounts: vec![(0, 3)] }];
        let saved = serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap();
        let mut app = App::new();
        app.insert_resource(AmmoPickupRestore(saved)).add_systems(Update, restore_ammo_pickups);
        let pickup = || Pickup { kind: PickupKind::Ammo(vec![(0, 5)]), label: "Bullets".into(), entities: Vec::new(), index: 4 };
        let first = app.world_mut().spawn((pickup(), Transform::from_xyz(1.0, 0.0, 0.0))).id();
        app.update();
        assert_eq!(app.world().get::<Pickup>(first).unwrap().kind, PickupKind::Ammo(vec![(0, 1)]));
        assert_eq!(app.world().resource::<AmmoPickupRestore>().0.len(), 2);
        let second = app.world_mut().spawn((pickup(), Transform::from_xyz(1.0, 0.0, 0.0))).id();
        let third = app.world_mut().spawn((pickup(), Transform::from_xyz(1.0, 0.0, 0.0))).id();
        app.update();
        assert_eq!(app.world().get::<Pickup>(first).unwrap().kind, PickupKind::Ammo(vec![(0, 1)]));
        let mut amounts: Vec<_> = [second, third].into_iter().map(|e| match &app.world().get::<Pickup>(e).unwrap().kind {
            PickupKind::Ammo(list) => list[0].1,
            _ => panic!("restored pickup lost ammunition"),
        }).collect();
        amounts.sort_unstable();
        assert_eq!(amounts, [2, 3]);
        assert!(app.world().resource::<AmmoPickupRestore>().0.is_empty());
    }

    #[test]
    fn full_elixirs_are_rejected_without_blocking_other_pickups() {
        let attrs = crate::gamedata::Attrs { max_elixirs: 10, max_mana_elixirs: 7, ..default() };
        let mut stats = PlayerStats::default();
        stats.health_elixirs = 10;
        stats.mana_elixirs = 6;
        assert!(PickupKind::HealthElixir.at_capacity(&stats, &attrs));
        assert!(!PickupKind::ManaElixir.at_capacity(&stats, &attrs));
        assert_eq!(stats.give_elixirs(true, u32::MAX, attrs.elixir_capacity(true)), 1);
        assert_eq!(stats.mana_elixirs, 7);
        assert!(PickupKind::ManaElixir.at_capacity(&stats, &attrs));
        assert!(!PickupKind::Coins(10).at_capacity(&stats, &attrs));
        assert!(!PickupKind::Weapon("Crossbow".into(), vec![(2, 1)]).at_capacity(&stats, &attrs));
        stats.health_elixirs = 12;
        assert_eq!(stats.give_elixirs(false, 1, attrs.elixir_capacity(false)), 0);
        assert_eq!(stats.health_elixirs, 12);
        stats.health_elixirs = 9;
        assert!(!PickupKind::HealthElixir.at_capacity(&stats, &attrs));
        assert_eq!(stats.give_elixirs(false, 1, attrs.elixir_capacity(false)), 1);
        assert_eq!(stats.health_elixirs, 10);
    }

    #[test]
    fn loading_preserves_rolled_ammunition_and_weapon_identity() {
        let mut app = App::new();
        app.insert_resource(AmmoPickupRestore(vec![
            AmmoPickupSave { index: 5, position: [0.0; 3], amounts: vec![(2, 2)] },
            AmmoPickupSave { index: 6, position: [0.0; 3], amounts: vec![(2, 2)] },
        ])).add_systems(Update, restore_ammo_pickups);
        let purse = app.world_mut().spawn((Pickup { kind: PickupKind::Ammo(vec![(2, 3)]), label: "Bolts".into(), entities: vec![], index: 5 }, Transform::IDENTITY)).id();
        let weapon = app.world_mut().spawn((Pickup { kind: PickupKind::Weapon("Crossbow".into(), vec![(2, 3)]), label: "Crossbow".into(), entities: vec![], index: 6 }, Transform::IDENTITY)).id();
        app.update();
        assert_eq!(app.world().get::<Pickup>(purse).unwrap().kind, PickupKind::Ammo(vec![(2, 2)]));
        assert_eq!(app.world().get::<Pickup>(weapon).unwrap().kind, PickupKind::Weapon("Crossbow".into(), vec![(2, 2)]));
    }
}

/// A pickup to take now as if Use were pressed on it (one Pull drew to Daud's hand).
#[derive(Resource, Default)]
pub struct ForceUse(pub Option<Entity>);

#[derive(Component)]
pub struct Pickup {
    pub kind: PickupKind,
    pub label: String,
    pub entities: Vec<Entity>,
    /// Index into `scene.pickups`.
    pub index: u32,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct AmmoPickupSave {
    pub index: u32,
    pub position: [f32; 3],
    pub amounts: Vec<(u8, u32)>,
}

#[derive(Resource, Default)]
pub struct AmmoPickupRestore(pub Vec<AmmoPickupSave>);

#[derive(Component)]
struct RestoredAmmoPickup;

fn restore_ammo_pickups(mut commands: Commands, mut pending: ResMut<AmmoPickupRestore>, mut pickups: Query<(Entity, &mut Pickup, &Transform), Without<RestoredAmmoPickup>>) {
    // Factory-created pickups can arrive after the initial level entities.
    // Consume each match once, including when identical factory instances overlap.
    let mut matched = std::collections::HashSet::new();
    pending.0.retain(|saved| {
        let Some((e, mut pickup, _)) = pickups.iter_mut().find(|(e, p, t)| !matched.contains(e) && p.index == saved.index && t.translation.distance_squared(Vec3::from(saved.position)) < 0.01) else { return true };
        match &mut pickup.kind {
            PickupKind::Weapon(_, ammo) => *ammo = saved.amounts.clone(),
            _ => pickup.kind = PickupKind::Ammo(saved.amounts.clone()),
        }
        matched.insert(e);
        commands.entity(e).insert(RestoredAmmoPickup);
        false
    });
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
            PickupKind::BoneCharm | PickupKind::CorruptedCharm => "Snd_UI_Whale_Bone_Charms_Amb",
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
    /// opened by a character on its way (not by Corvo or a script): who, the side it came
    /// from, and whether it has gone through
    pub npc_opened: Option<(Entity, f32, bool)>,
    /// slammed open by Wind Blast: how long since, and whether by its own slam stage (its
    /// output due as the swing ends); rattled, for so long yet
    pub slam: Option<(f32, bool)>,
    pub rattle: f32,
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
    /// what it does (the prompt's verb) and its name above it
    pub label: String,
    pub name: String,
}

/// The player used something (reported to the level scripts).
#[derive(Message, Clone, Debug)]
pub enum Interaction {
    Pickup(u32),
    /// door instance, opened (else closed), swung clockwise
    Door { instance: u32, opened: bool, cw: bool },
    DoorLocked(u32),
    /// a usable (its actor) used into a stage (its `DisSeqEvent_Used` output; 0 for one
    /// without stages)
    Usable(u32, usize),
    /// a usable's other `DisSeqEvent_Used` outputs: after its stages, "Used But Locked",
    /// "Started Holding", "Stopped Holding"
    UsableOut { actor: u32, out: usize },
    /// a prop (its instance) picked up, or broken
    Movable { instance: u32, broken: bool },
    /// a breakable (its instance) through one of its steps short of breaking: its
    /// `DisSeqEvent_BreakableBroken` output (1, "Broken 50%", for the step before the last)
    BrokenStep { instance: u32, out: usize },
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
    /// a door Wind Blast slammed open, its swing over (its "SlamOpenCW" / "SlamOpenCCW",
    /// no one's doing)
    DoorSlam { instance: u32, cw: bool },
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
    if t.contains("crackedbonecharm") {
        return (PickupKind::CorruptedCharm, "Corrupted Charm".into());
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
                npc_opened: None,
                slam: None,
                rattle: 0.0,
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
        if p.food_health.is_some() { kind = PickupKind::Food; }
        let token = p.label.starts_with('`');
        let own = (!token).then_some(p.label.as_str()).filter(|l| !l.is_empty());
        // money: the tweak's quantity
        if p.coins && p.quantity > 0 {
            kind = PickupKind::Coins(p.quantity);
            label = own.map(str::to_string).unwrap_or_else(|| if p.quantity == 1 { "1 Coin".to_string() } else { format!("{} Coins", p.quantity) });
        } else if let Some(l) = own.filter(|_| !matches!(kind, PickupKind::Key(_))) {
            label = l.to_string();
        }
        let ammo: Vec<_> = p.ammo.iter().map(|&(ty, max)| {
            let min = p.ammo_min.iter().find(|(t, _)| *t == ty).map(|(_, min)| *min).unwrap_or(max);
            (ty, crate::gadgets::roll_ammo_amount(min, max))
        }).collect();
        if !ammo.is_empty() {
            // the cooked ammunition: its type names it
            label = crate::gadgets::ammo_name(p.ammo[0].0).to_string();
            kind = PickupKind::Ammo(ammo.clone());
        }
        // a weapon: what it gives (with its bullets)
        let w = p.inv_item.to_ascii_lowercase();
        if w.contains("sword") || w.contains("pistol") || w.contains("crossbow") {
            kind = PickupKind::Weapon(p.inv_item.clone(), ammo);
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

/// A pickup's meshes' bounds in the world (theirs and their parts').
fn pickup_bounds(entities: &[Entity], bounds: &Query<(&bevy::camera::primitives::Aabb, &GlobalTransform)>, kids: &Query<&Children>) -> Option<(Vec3, Vec3)> {
    let mut out: Option<(Vec3, Vec3)> = None;
    let mut stack: Vec<Entity> = entities.to_vec();
    while let Some(e) = stack.pop() {
        if let Ok((b, gt)) = bounds.get(e) {
            let (c, h) = (Vec3::from(b.center), Vec3::from(b.half_extents));
            let m = gt.affine();
            for i in 0..8 {
                let p = m.transform_point3(c + h * Vec3::new(if i & 1 == 0 { -1.0 } else { 1.0 }, if i & 2 == 0 { -1.0 } else { 1.0 }, if i & 4 == 0 { -1.0 } else { 1.0 }));
                out = Some(out.map_or((p, p), |(lo, hi)| (lo.min(p), hi.max(p))));
            }
        }
        if let Ok(ch) = kids.get(e) {
            stack.extend(ch.iter());
        }
    }
    out
}

/// Where a ray from `from` along `dir` (unit) enters a box (0 from inside), if it does.
fn ray_box(from: Vec3, dir: Vec3, lo: Vec3, hi: Vec3) -> Option<f32> {
    let inv = dir.recip();
    let (a, b) = ((lo - from) * inv, (hi - from) * inv);
    let (near, far) = (a.min(b).max_element(), a.max(b).min_element());
    (far >= near.max(0.0)).then_some(near.max(0.0))
}

/// A pickup's words for the crosshair: what can be done (`DUI_Crosshair_Pickup`) on someone, or
/// on a body: the HUD's "Loot" (`m_InteractionTexts[30]`); lying about, its own
/// (`m_InteractText`: "Pick up", "Eat"); and its name.
fn pickup_words(level: Option<&LevelInfo>, p: &Pickup, pocket: bool) -> (String, String) {
    let own = level.and_then(|l| l.scene.pickups.get(p.index as usize)).map(|d| d.verb.as_str()).filter(|v| !v.is_empty() && !pocket);
    let text = match own {
        Some(v) => crate::gamedata::readable(&v.replace("`GBA_Use`", &crate::bindings::hint(crate::bindings::Act::Use))),
        None => format!("{} Loot", crate::bindings::hint(crate::bindings::Act::Use)),
    };
    (text, p.label.clone())
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
    level: Option<Res<LevelInfo>>,
    (bodies, carry, data, owners, stats): (
        Query<(Entity, &crate::npc::Npc, &Transform, Option<&crate::anim::Animator>), (Without<Player>, Without<crate::npc::ConsumedBody>)>,
        Res<crate::carry::Carry>,
        Res<crate::gamedata::Data>,
        Query<(&crate::npc::Npc, &Transform), Without<Player>>,
        Res<PlayerStats>,
    ),
    (bounds, kids, plants): (Query<(&bevy::camera::primitives::Aabb, &GlobalTransform)>, Query<&Children>, Query<(Entity, &crate::pickpocket::PutPocket, &Transform)>),
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
    // what is behind a wall can't be used
    let ctx = rapier.single().ok();
    let world = bevy_rapier3d::prelude::QueryFilter::default().groups(bevy_rapier3d::prelude::CollisionGroups::new(bevy_rapier3d::prelude::Group::ALL, crate::level::GROUP_WORLD));
    let blocked = |to: Vec3| -> bool {
        let d = to.length();
        ctx.as_ref().and_then(|c| c.cast_ray(eye, to / d.max(1e-3), (d - 0.35).max(0.0), true, world)).is_some()
    };
    // (score, what, what can be done, its name)
    let mut best: Option<(f32, Entity, String, String)> = None;
    for (e, p, t, pocket) in &pickups {
        let to = t.translation - eye;
        let d = to.length();
        if d > 2.4 + 0.5 {
            continue;
        }
        // in someone's pocket: from behind and unseen, or from the body
        if pocket.is_some_and(|pk| !pk.reachable(&owners, eye)) {
            continue;
        }
        let score = if pocket.is_some() {
            let c = to.normalize_or_zero().dot(fwd);
            if c < 0.82 || d > 2.4 {
                continue;
            }
            d * (2.0 - c) - 0.3
        } else {
            // lying about: the crosshair on it, as the original traces the crosshair against
            // the item (its meshes' bounds, a little forgiving for the small ones), within reach
            // and not behind a wall (as roshan's r614/blink research has it, U-112). (Taken
            // within a 35-degree cone of its origin, with no line to it, the bread on a shelf
            // was eaten while Corvo looked at the wall beside it, and things were taken
            // through walls and cupboard doors)
            match pickup_bounds(&p.entities, &bounds, &kids) {
                Some((lo, hi)) => {
                    // (the forgiveness: some 6 cm about it, the more for the smallest, up to 12)
                    let size = (hi - lo).max_element();
                    let pad = (0.12 - size * 0.25).clamp(0.06, 0.12);
                    let Some(hit) = ray_box(eye, fwd, lo - Vec3::splat(pad), hi + Vec3::splat(pad)) else { continue };
                    if hit > 2.4 || blocked(fwd * hit.max(0.36)) {
                        continue;
                    }
                    hit - 0.3
                }
                None => {
                    // (no meshes: its place, nearly dead ahead)
                    let c = to.normalize_or_zero().dot(fwd);
                    if c < 0.97 || d > 2.4 || blocked(to) {
                        continue;
                    }
                    d * (2.0 - c) - 0.3
                }
            }
        };
        // what can be taken comes first when it's looked at
        if best.as_ref().map(|b| score < b.0).unwrap_or(true) {
            let (text, label) = pickup_words(level.as_deref(), p, pocket.is_some());
            best = Some((score, e, text, label));
        }
    }
    // a pocket to plant something in (as a pocket to pick: from behind, unseen)
    for (e, p, t) in &plants {
        let to = t.translation - eye;
        let d = to.length();
        let c = to.normalize_or_zero().dot(fwd);
        if d > 2.4 || c < 0.82 || !p.reachable(&owners, eye) {
            continue;
        }
        // (before what is in his pockets: it's what Daud came for)
        let score = d * (2.0 - c) - 0.6;
        if best.as_ref().map(|b| score < b.0).unwrap_or(true) {
            let text = crate::gamedata::readable(&p.text.replace("`GBA_Use`", &crate::bindings::hint(crate::bindings::Act::Use)));
            best = Some((score, e, text, String::new()));
        }
    }
    for (e, door, gt, li) in &doors {
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
            // the door's own words (its tweak's: "Open", "Close", "Unlock"; locked, its
            // `m_FocusLockedMessage`: "Blocked from the other side", "`k needed"), its name above
            let words = level.as_ref().and_then(|l| l.scene.door(&li.actor, li.index)).map(|s| &s.texts);
            let say = |k: &str, or: &str| words.and_then(|w| w.get(k)).cloned().unwrap_or_else(|| or.to_string());
            let raw = if !door.locked {
                if door.target > 0.5 { say("close", "`GBA_Use` Close") } else { say("open", "`GBA_Use` Open") }
            } else if door.key_in(&stats.keys).is_some() {
                say("unlock", "`GBA_Use` Unlock")
            } else {
                say("focus_locked", "Locked")
            };
            // (`k: the key it wants; none named, plain "Locked")
            let raw = match door.keys.first() {
                Some(k) => raw.replace("`k", k),
                None if raw.contains("`k") => "Locked".to_string(),
                None => raw,
            };
            let text = crate::gamedata::readable(&raw.replace("`GBA_Use`", &crate::bindings::hint(crate::bindings::Act::Use)));
            best = Some((score, e, text, say("name", "")));
        }
    }
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
            best = Some((score, e, format!("{} {}", crate::bindings::hint(crate::bindings::Act::Use), u.label), u.name.clone()));
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
    paused: Res<crate::hud::Paused>,
    (keys, bind): (Res<ButtonInput<KeyCode>>, Res<crate::bindings::Bindings>),
    focus: Res<InteractFocus>,
    mut stats: ResMut<PlayerStats>,
    mut msgs: ResMut<HudMessages>,
    mut noise: MessageWriter<Noise>,
    mut used: MessageWriter<Interaction>,
    mut pickups: Query<(&mut Pickup, Option<&Song>)>,
    usables: Query<&Usable>,
    mut doors: Query<(&mut Door, &GlobalTransform, &LevelInstance)>,
    player: Query<&Transform, With<Player>>,
    (level, mut timed, data, mut ambients, mut read, attrs): (Option<Res<LevelInfo>>, ResMut<crate::audio::TimedSounds>, Res<crate::gamedata::Data>, ResMut<crate::audio::Ambients>, MessageWriter<crate::journal::ReadNote>, Res<crate::gamedata::Attrs>),
    (keyholes, peek, mut plog, mut tw, settings, switches): (Query<(), With<crate::keyhole::KeyholeDoor>>, Res<crate::keyhole::Peek>, ResMut<crate::pickuplog::PickupLog>, ResMut<crate::tutwindow::TutorialWindow>, Res<crate::settings::Settings>, Res<crate::kismet::ScriptSwitches>),
    mut force: ResMut<ForceUse>,
) {
    if paused.0 { return; }
    let use_key = bind.key(crate::bindings::Act::Use);
    // (a pickup Pull drew to Daud's hand: taken as if used)
    let forced = force.0.take();
    let Some(e) = forced.or(focus.1) else { return };
    // a shut door with a keyhole: a tap (on release) opens it, a hold looks through
    let tap = keyholes.contains(e) && doors.get(e).is_ok_and(|(d, _, _)| d.target < 0.5);
    if forced.is_some() {
    } else if tap {
        if !keys.just_released(use_key) || peek.consumed {
            return;
        }
    } else if !keys.just_pressed(use_key) {
        return;
    }
    // (the corrupted charms still lying about, this one with them: the scripts make one of
    // several, so the level's count is what is there)
    let corrupted_left = pickups.iter().filter(|(p, _)| matches!(p.kind, PickupKind::CorruptedCharm)).count() as u32;
    if let Ok((mut p, song)) = pickups.get_mut(e) {
        if p.kind.at_capacity(&stats, &attrs) {
            timed.schedule(&[(0.0, "Snd_UI_Ingame_Max_Ammo".to_string())], None);
            // (the pickup's `m_CannotUseMessage`: the elixirs' own, else the stat pickups')
            let tweak = match p.kind {
                PickupKind::HealthElixir => "Elixir_Health_twk",
                PickupKind::ManaElixir => "Elixir_Mana_twk",
                _ => "StatPickupBase",
            };
            let m = data.text("Pickup", &format!("{tweak}.m_CannotUseMessage"));
            msgs.push(if m.is_empty() { "You can't carry more ammo of this type".to_string() } else { m });
            return;
        }
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
            // (its tweak's `m_pPickupSoundEvent`)
            PickupKind::CorruptedCharm => "Snd_UI_Ingame_Charms_Pickup",
            PickupKind::Blueprint(_) => "Snd_UI_Ingame_Note_Open",
            PickupKind::Weapon(..) => "Snd_UI_Ingame_Ammo_Pickup",
        };
        timed.schedule(&[(0.0, sound.to_string())], None);
        let mut remaining_ammo = None;
        match &p.kind {
            PickupKind::Coins(v) | PickupKind::Loot(v) => {
                stats.coins += v;
                stats.coins_found += v;
                plog.add(format!("{} +{v}", if p.label.is_empty() || p.label.starts_with('`') { "Coins" } else { p.label.as_str() }), Some("Money_Small"));
            }
            PickupKind::HealthElixir => {
                stats.give_elixirs(false, 1, attrs.elixir_capacity(false));
                plog.add("Sokolov's Health Elixir", Some("HealthElixir_Small"));
            }
            PickupKind::ManaElixir => {
                stats.give_elixirs(true, 1, attrs.elixir_capacity(true));
                plog.add("Piero's Spiritual Remedy", Some("ManaElixir_Small"));
            }
            PickupKind::Food => {
                let base = level.as_ref().and_then(|l| l.scene.pickups.get(p.index as usize)).and_then(|p| p.food_health).unwrap_or(10);
                stats.eat_food(base as f32, attrs.food_heal_bonus);
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
                }
            }
            PickupKind::Ammo(list) => {
                let mut remaining = Vec::new();
                // the Scavenger charm (`Attribute_StatPickupCapacityBonusChance`): each kind of
                // more than one round may give one more (as roshan's r614/blink research found
                // the pickup's take rolling it)
                let more = data.attribute("StatPickupCapacityBonusChance", 1, &stats.powers, &stats.charms);
                let list: Vec<(u8, u32)> = list.iter().map(|&(ty, n)| (ty, n + scavenged(n, more, rand::random::<f32>()))).collect();
                for &(ty, n) in &list {
                    let (name, added) = crate::gadgets::give_ammo(&mut stats, &attrs, ty, n);
                    if added > 0 { plog.add(format!("{name} +{added}"), crate::pickuplog::ammo_icon(ty)); }
                    if n > added { remaining.push((ty, n - added)); }
                }
                remaining_ammo = Some(remaining);
            }
            PickupKind::Rune => {
                stats.runes += 1;
                stats.runes_found += 1;
                plog.add("Rune", Some("Rune_Small"));
                // the HUD tweak's `m_RuneFoundMessage` ("`c/`t Runes found"), and
                // `m_PressKeyToBuyPowers` in the tutorial window, with the rune, while his runes
                // would buy a power or an enhancement (the HUD's pickup asks the count of them)
                let total = level.as_ref().map(|l| count_kind(l, |k| matches!(k, PickupKind::Rune))).unwrap_or(0).max(stats.runes_found);
                msgs.push(format!("{}/{} Runes found", stats.runes_found, total));
                if crate::journal::affordable(&data, &stats) > 0 {
                    tw.push(data.hud_text("m_PressKeyToBuyPowers", "New Rune added<br />Press `GBA_Journal` to acquire Powers"), crate::tutwindow::IMG_RUNE);
                }
                // from his second rune on, the powers' tutorial note (`DTN_PlayYourWay_Powers`,
                // as the HUD's pickup gives it once `m_NumRunesPickedUp` passes 1, while the
                // systemic tutorials are on): the journal's key reads it while its word is up
                stats.runes_picked += 1;
                const POWERS_NOTE: &str = "DTN_PlayYourWay_Powers";
                if stats.runes_picked > 1 && settings.tutorials && !switches.0.systemic_off && !stats.tutorials.iter().any(|t| t == POWERS_NOTE) && crate::tutorials::received(&mut stats, POWERS_NOTE) {
                    msgs.push(format!("{} {}", crate::bindings::hint(crate::bindings::Act::Journal), data.text("DisGFxMoviePlayerMenuBase_Texts", "t_ReadTutorial")));
                }
            }
            PickupKind::BoneCharm => {
                stats.bone_charms += 1;
                stats.charms_found += 1;
                // `m_BoneCharmFoundMessage`, and `m_PressKeyToEquipBoneCharms` (in the tutorial
                // window, with the charm) for each; it is new until the journal shows the charms
                let total = level.as_ref().map(|l| count_kind(l, |k| matches!(k, PickupKind::BoneCharm))).unwrap_or(0).max(stats.charms_found);
                msgs.push(format!("{}/{} Bone Charms found", stats.charms_found, total));
                stats.new_charm = true;
                tw.push(data.hud_text("m_PressKeyToEquipBoneCharms", "New Bone Charm added<br />Press `GBA_Journal` to equip"), crate::tutwindow::IMG_CHARM);
                // which charm: one not found yet, at random (as the original deals them)
                let owned = stats.charms_owned.clone();
                let left: Vec<&str> = data.0.charms.iter().filter(|c| !c.corrupted).filter_map(|c| c.levels.first()).map(|l| l.0.as_str()).filter(|n| !owned.iter().any(|o| o == n)).collect();
                if !left.is_empty() {
                    let name = left[rand::random_range(0..left.len())].to_string();
                    plog.add(format!("Bone Charm: {name}"), Some("BoneCharms_Small"));
                    stats.charms_owned.push(name);
                } else {
                    plog.add("Bone Charm", Some("BoneCharms_Small"));
                }
            }
            PickupKind::CorruptedCharm => {
                // (its own count, `ePlayerStat_CrackedCharmFound`, and the HUD's
                // `m_CrackedBoneCharmFoundMessage`, `m_PressKeyToEquipCrackedBoneCharms`)
                let found = {
                    let c = stats.counters.entry("ePlayerStat_CrackedCharmFound".into()).or_default();
                    *c += 1;
                    *c
                };
                let total = (found + corrupted_left).saturating_sub(1).max(found);
                msgs.push(data.hud_text("m_CrackedBoneCharmFoundMessage", "`c/`t Corrupted Charm found").replace("`c", &found.to_string()).replace("`t", &total.to_string()));
                stats.new_charm = true;
                tw.push(data.hud_text("m_PressKeyToEquipCrackedBoneCharms", "New Corrupted Charm added<br />Press `GBA_Journal` to equip"), crate::tutwindow::IMG_CHARM);
                // which: one of the corrupted not found yet, at random
                let owned = stats.charms_owned.clone();
                let left: Vec<&str> = data.0.charms.iter().filter(|c| c.corrupted).filter_map(|c| c.levels.first()).map(|l| l.0.as_str()).filter(|n| !owned.iter().any(|o| o == n)).collect();
                if !left.is_empty() {
                    let name = left[rand::random_range(0..left.len())].to_string();
                    plog.add(format!("Corrupted Charm: {name}"), Some("DLC07_CrackedCharm_Small"));
                    stats.charms_owned.push(name);
                } else {
                    plog.add("Corrupted Charm", Some("DLC07_CrackedCharm_Small"));
                }
                // (the corrupted charms' tutorial, the first time: the journal's key reads it)
                const CRACKED_NOTE: &str = "DTN_DLC07CrackedCharms";
                if settings.tutorials && !stats.tutorials.iter().any(|t| t == CRACKED_NOTE) && crate::tutorials::received(&mut stats, CRACKED_NOTE) {
                    msgs.push(format!("{} {}", crate::bindings::hint(crate::bindings::Act::Journal), data.text("DisGFxMoviePlayerMenuBase_Texts", "t_ReadTutorial")));
                }
            }
            PickupKind::Blueprint(bp) => {
                if !stats.upgrades.contains(bp) {
                    stats.upgrades.push(bp.clone());
                }
                plog.add("Blueprint", Some("BluePrint_Small"));
                // (the blueprints' tutorial, `m_TutorialNote`, the first time: the journal's
                // key reads it while its message is up)
                if let Some(id) = data.0.item_tutorials.get(bp.as_str()).cloned() {
                    if crate::tutorials::received(&mut stats, &id) {
                        msgs.push(format!("{} {}", crate::bindings::hint(crate::bindings::Act::Journal), data.text("DisGFxMoviePlayerMenuBase_Texts", "t_ReadTutorial")));
                    }
                }
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
                    stats.no_heart = true;
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
                    let (name, added) = crate::gadgets::give_ammo(&mut stats, &attrs, ty, n);
                    if added > 0 { plog.add(format!("{name} +{added}"), crate::pickuplog::ammo_icon(ty)); }
                }
            }
        }
        if let Some(remaining) = remaining_ammo.filter(|list| !list.is_empty()) {
            p.kind = PickupKind::Ammo(remaining);
            return;
        }
        for &m in &p.entities {
            commands.entity(m).despawn();
        }
        commands.entity(e).despawn();
        used.write(Interaction::Pickup(p.index));
        return;
    }
    if let Ok(u) = usables.get(e) {
        used.write(Interaction::Usable(u.actor, 0));
        return;
    }
    if let Ok((mut door, gt, li)) = doors.get_mut(e) {
        let sounds = level.as_ref().and_then(|l| l.scene.door(&li.actor, li.index));
        let at = Some(gt.translation());
        if door.locked {
            // a key on the ring opens it (and it swings open at once, as the original's do)
            if door.key_in(&stats.keys).is_some() {
                door.locked = false;
                if let Some(s) = sounds.filter(|s| !s.unlocked.is_empty()) {
                    timed.schedule(&[(0.0, s.unlocked.clone())], at);
                }
            } else {
                if let Some(s) = sounds.filter(|s| !s.locked.is_empty()) {
                    timed.schedule(&[(0.0, s.locked.clone())], at);
                }
                // (its `m_LockedMessage`, if it has one: most say nothing but their sound)
                if let Some(m) = sounds.and_then(|s| s.texts.get("locked")).filter(|m| !m.contains("`k") || !door.keys.is_empty()) {
                    msgs.push(crate::gamedata::readable(&m.replace("`k", door.keys.first().map(String::as_str).unwrap_or_default())));
                }
                used.write(Interaction::DoorLocked(li.index));
                return;
            }
        }
        // (his to open and close from now)
        door.npc_opened = None;
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
        noise.write(Noise { corvo: true, pos: gt.translation(), radius: 5.0, kind: NoiseKind::Anomaly });
    }
}

/// Characters open the unlocked doors in their way (swinging them away, within
/// `m_fNPCClosenessToOpen`, 200 uu), as the original's path objects let them through; and close
/// one behind them once through, no one within `m_fNPCClosenessToNotClose` (400 uu) and Corvo
/// farther than `m_fPlayerClosenessToNotClose` (400 uu), unless he or a script opened it (the
/// door's tick as roshan's r614/blink research read it).
fn npc_doors(
    level: Option<Res<LevelInfo>>,
    mut timed: ResMut<crate::audio::TimedSounds>,
    mut doors: Query<(&mut Door, &GlobalTransform, &LevelInstance)>,
    npcs: Query<(Entity, &crate::npc::Npc, &Transform)>,
    player: Query<&Transform, With<crate::player::Player>>,
) {
    // behind them: the opener through, no one near, Corvo away
    let pt = player.single().ok().map(|t| t.translation);
    for (mut door, gt, li) in &mut doors {
        let Some((opener, side, through)) = door.npc_opened else { continue };
        if door.target < 0.5 {
            door.npc_opened = None;
            continue;
        }
        let at = gt.translation();
        let face = gt.rotation() * door.normal;
        let through = through || npcs.get(opener).map_or(true, |(_, n, t)| n.is_down() || (face.dot(t.translation - at) > 0.0) != (side > 0.0));
        door.npc_opened = Some((opener, side, through));
        let near = |p: Vec3| (p - at).with_y(0.0).length() < NOT_CLOSE && (p.y - at.y).abs() < 2.0;
        if !through || npcs.iter().any(|(_, n, t)| !n.is_down() && near(t.translation)) || pt.is_some_and(near) {
            continue;
        }
        door.target = 0.0;
        door.npc_opened = None;
        if let Some(s) = level.as_ref().and_then(|l| l.scene.door(&li.actor, li.index)) {
            timed.schedule(&s.close, Some(at));
        }
    }
    for (e, n, nt) in &npcs {
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
            if to.with_y(0.0).length() > TO_OPEN || to.y.abs() > 2.0 || to.with_y(0.0).normalize_or_zero().dot(dir) < -0.2 {
                continue;
            }
            let side = (gt.rotation() * door.normal).dot(nt.translation - gt.translation());
            door.dir = if side > 0.0 { -1.0 } else { 1.0 };
            door.target = 1.0;
            door.npc_opened = Some((e, side, false));
            if let Some(s) = level.as_ref().and_then(|l| l.scene.door(&li.actor, li.index)) {
                timed.schedule(&s.open, Some(gt.translation()));
            }
        }
    }
}

/// A door's reach for characters: to open it (`m_fNPCClosenessToOpen`), and to keep it open
/// (`m_fNPCClosenessToNotClose`, `m_fPlayerClosenessToNotClose`), m.
const TO_OPEN: f32 = 2.0;
const NOT_CLOSE: f32 = 4.0;

/// Wind Blast's slam and rattle forces (`m_fSlamWindblastForce`, `m_fRattleWindblastForce`: no
/// shipped door has others), and how long the slam's swing and the rattle take (its
/// `Player_Windblast_Cw`: 90 degrees in 0.2 s, back to 82, settled by 1.03 s; `Player_Rattle_Cw`).
const SLAM: f32 = 0.5;
const RATTLE: f32 = 0.3;
const SLAM_LEN: f32 = 1.033;
const RATTLE_LEN: f32 = 1.1;

/// Wind Blast's factor at a point (its damage's share there): the square of how near, times
/// the square of how near the aim within its cone.
fn wind_factor(w: &DoorBlast, at: Vec3) -> f32 {
    let to = at - w.from;
    let d = to.length();
    let (flat, bearing, cos_half) = (w.dir.with_y(0.0).normalize_or_zero(), to.with_y(0.0).normalize_or_zero(), w.half_angle.cos());
    let along = flat.dot(bearing);
    if d >= w.reach || along <= cos_half {
        return 0.0;
    }
    let near = 1.0 - d / w.reach;
    let a = if d > 1.0 { (along - cos_half) / (1.0 - cos_half) } else { 1.0 };
    near * near * a * a
}

/// The slam's swing (its clip's hinge): open by 0.2 s, back to 82 degrees, settled at 90.
fn slam_open(t: f32) -> f32 {
    if t < 0.2 {
        (t / 0.2).powi(2)
    } else if t < 0.47 {
        1.0 - 0.089 * (std::f32::consts::FRAC_PI_2 * (t - 0.2) / 0.27).sin()
    } else {
        let k = ((t - 0.47) / 0.53).clamp(0.0, 1.0);
        0.911 + 0.089 * k * k * (3.0 - 2.0 * k)
    }
}

/// Wind Blast at a closed door (0x00A2D730, decoded for this rewrite): at its slam force it
/// slams an unlocked one open, away from the blast, with its slam stage's swing and sounds (its
/// "SlamOpenCW" / "SlamOpenCCW" as the swing ends, `animate_doors`; one without opens as used);
/// at its rattle force, or locked, it rattles. Wooden doors that break with no broken mesh only
/// take the damage (`break_doors`).
fn wind_doors(
    level: Option<Res<LevelInfo>>,
    mut winds: MessageReader<DoorBlast>,
    mut doors: Query<(&mut Door, &GlobalTransform, &LevelInstance, &Visibility)>,
    mut timed: ResMut<crate::audio::TimedSounds>,
    mut used: MessageWriter<Interaction>,
) {
    let winds: Vec<DoorBlast> = winds.read().copied().collect();
    let Some(level) = level.filter(|_| !winds.is_empty()) else { return };
    for (mut door, gt, li, vis) in &mut doors {
        if door.broken || door.travel || *vis == Visibility::Hidden || door.target > 0.5 || door.open > 0.01 || door.slam.is_some() {
            continue;
        }
        let Some(def) = level.scene.door(&li.actor, li.index).filter(|d| d.slams) else { continue };
        let at = gt.translation();
        let Some((f, w)) = winds.iter().map(|w| (wind_factor(w, at + Vec3::Y), w)).max_by(|a, b| a.0.total_cmp(&b.0)) else { continue };
        if f >= SLAM && !door.locked {
            // (away from the blast, as from one using it there)
            let side = (gt.rotation() * door.normal).dot(w.from - at);
            door.dir = if side > 0.0 { -1.0 } else { 1.0 };
            door.target = 1.0;
            door.npc_opened = None;
            if def.slam_stage {
                door.slam = Some((0.0, true));
                timed.schedule(&def.slam, Some(at));
            } else {
                door.slam = Some((0.0, false));
                timed.schedule(&def.open, Some(at));
                used.write(Interaction::Door { instance: li.index, opened: true, cw: door.dir > 0.0 });
            }
            if std::env::var("DH_DOOR_LOG").is_ok() {
                info!("door {} slammed open by Wind Blast ({f:.2})", li.actor);
            }
        } else if f >= RATTLE {
            door.rattle = RATTLE_LEN;
            if std::env::var("DH_DOOR_LOG").is_ok() {
                info!("door {} rattles ({f:.2})", li.actor);
            }
        }
    }
}

fn animate_doors(
    time: Res<Time>,
    mut doors: Query<(&mut Door, &mut Transform, &LevelInstance), Without<bevy_rapier3d::prelude::Collider>>,
    mut colliders: Query<&mut Transform, (With<bevy_rapier3d::prelude::Collider>, Without<Door>)>,
    mut used: MessageWriter<Interaction>,
) {
    let dt = time.delta_secs();
    for (mut door, mut t, li) in &mut doors {
        let mut shake = 0.0;
        if door.rattle > 0.0 {
            door.rattle = (door.rattle - dt).max(0.0);
            // (a couple of degrees at the hinge, dying away)
            shake = 0.035 * (door.rattle * 26.0).sin() * door.rattle / RATTLE_LEN;
        }
        if let Some((st, stage)) = door.slam {
            let st = st + dt;
            door.open = slam_open(st).min(1.0);
            if st >= SLAM_LEN {
                door.open = 1.0;
                door.slam = None;
                if stage {
                    used.write(Interaction::DoorSlam { instance: li.index, cw: door.dir > 0.0 });
                }
            } else {
                door.slam = Some((st, stage));
            }
        } else if (door.open - door.target).abs() < 1e-3 && shake == 0.0 && door.rattle == 0.0 {
            continue;
        } else {
            let step = dt * 1.6;
            door.open = if door.target > door.open { (door.open + step).min(door.target) } else { (door.open - step).max(door.target) };
        }
        let r = Quat::from_rotation_y(door.dir * door.open * 1.6 + shake);
        t.rotation = r * door.base_rot;
        if let Some(c) = door.collider {
            if let Ok(mut ct) = colliders.get_mut(c) {
                ct.rotation = r * door.collider_rot;
            }
        }
    }
}

/// Breakable doors (`DisTweaks_Door`'s `m_pBreakSteps`) worn down by Wind Blast and blasts:
/// a blow under `m_DamageThreshold` leaves no mark; at its last step's `m_HealthThreshold` the
/// door breaks (that step: the wood's sound, the AI's alarm, the splinters, the pieces) and is gone (saved
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
        // (Wind Blast's damage by its factor)
        for w in &winds {
            dmg = dmg.max((w.damage * wind_factor(w, mid)).round());
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
        // (its last step's at its health threshold: broken short of nothing left, 0x00A284B0)
        door.health = (door.health - dmg).max(0.0);
        if door.health > def.breaks.as_ref().map_or(0.0, |b| b.threshold) {
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
