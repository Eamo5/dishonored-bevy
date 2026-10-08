//! Shared gameplay state and messages connecting player, NPC, combat and HUD systems.

use crate::GameState;
use bevy::prelude::*;

pub struct GameplayPlugin;

impl Plugin for GameplayPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<Noise>()
            .add_message::<NpcHit>()
            .add_message::<Struck>()
            .add_message::<PlayerHit>()
            .add_message::<NpcStagger>()
            .init_resource::<PlayerStats>()
            .init_resource::<TimeControl>()
            .init_resource::<HudMessages>()
            .init_resource::<Campaign>()
            .add_systems(OnEnter(GameState::InGame), reset_stats.after(crate::level::LevelSpawnSet))
            .add_systems(Update, (tick_time_control, tick_messages).run_if(in_state(GameState::InGame)))
            .add_systems(Last, log_health.run_if(in_state(GameState::InGame)));
    }
}

/// `DH_HP_LOG`: each change of Corvo's health, where he stands (debug).
fn log_health(stats: Res<PlayerStats>, mut last: Local<Option<f32>>, player: Query<&Transform, With<crate::player::Player>>, time: Res<Time>) {
    if std::env::var("DH_HP_LOG").is_err() {
        return;
    }
    let h = stats.health;
    if last.is_some_and(|l| (l - h).abs() > 0.01) {
        let at = player.single().map(|t| t.translation).unwrap_or_default();
        info!("hp: {:.1} -> {h:.1} at {at:.2} (t {:.2})", last.unwrap(), time.elapsed_secs());
    }
    *last = Some(h);
}

/// A sound the AI can hear.
#[derive(Message, Clone, Copy)]
pub struct Noise {
    pub pos: Vec3,
    pub radius: f32,
    /// Combat noise (sword clashes, screams) puts listeners straight into combat.
    pub combat: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HitKind {
    Sword,
    Assassinate,
    Choke,
    SleepDart,
    Bolt,
    Bullet,
    Windblast,
    /// Blood Thirst's adrenaline fatality
    Fatality,
    /// eaten by rats
    Rats,
    /// an incendiary bolt: immolated
    Fire,
    /// grenades, explosive bullets
    Explosion,
    /// another NPC (a faction feud) or the level's own rats: not Corvo's doing
    ByOthers,
}

#[derive(Message, Clone, Copy)]
pub struct NpcHit {
    pub npc: Entity,
    pub damage: f32,
    pub kind: HitKind,
    pub from: Vec3,
}

/// A weapon struck something that isn't a person (a river krust's shell): the owner of the
/// collider it hit (`Strikeable`).
#[derive(Message, Clone, Copy)]
pub struct Struck {
    pub target: Entity,
    pub damage: f32,
    pub kind: HitKind,
    pub at: Vec3,
}

/// Corvo takes damage from something other than a blade (a krust's spit, a fish's bite): the
/// red flash, a cry of pain, or death.
pub fn hurt_player(stats: &mut PlayerStats, msgs: &mut HudMessages, sfx: &mut MessageWriter<crate::audio::PostEvent>, damage: f32) {
    if stats.dead {
        return;
    }
    stats.health = (stats.health - damage).max(0.0);
    stats.damage_flash = 1.0;
    if stats.health <= 0.0 {
        stats.dead = true;
        msgs.push("You died");
        sfx.write(crate::audio::PostEvent::named("Snd_VO_Bark_P_Death_cue_ak", None));
        sfx.write(crate::audio::PostEvent::named("VS_Death_Jingle", None));
    } else {
        sfx.write(crate::audio::PostEvent::named("Snd_VO_Bark_Player_Pain_low_cue_ak", None));
    }
}

/// A collider blades, bolts and bullets can strike, and whose it is.
#[derive(Component)]
pub struct Strikeable(pub Entity);

/// An NPC attack that connected (before block/parry resolution).
#[derive(Message, Clone, Copy)]
pub struct PlayerHit {
    pub npc: Entity,
    pub from: Vec3,
    pub damage: f32,
    /// a kick (`DisTweaks_NPCBash`): it breaks his block and shoves him back
    pub kick: bool,
    /// a big blow (`m_fRandomBigHitChance`): it leaves him reeling
    pub big: bool,
    /// a shove out of someone's way (`DisTweaks_NPCPush`): no harm meant
    pub push: bool,
}

/// Interrupt an NPC (parried, blinded...).
#[derive(Message, Clone, Copy)]
pub struct NpcStagger {
    pub npc: Entity,
    pub secs: f32,
    /// its blow parried by Corvo (it plays its parry lost)
    pub parried: bool,
}

#[derive(Resource, Clone)]
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PlayerStats {
    pub health: f32,
    pub max_health: f32,
    pub mana: f32,
    pub max_mana: f32,
    /// Seconds before mana starts regenerating.
    pub mana_delay: f32,
    pub coins: u32,
    pub health_elixirs: u32,
    pub mana_elixirs: u32,
    pub sleep_darts: u32,
    pub bolts: u32,
    pub bullets: u32,
    pub kills: u32,
    pub knockouts: u32,
    pub times_detected: u32,
    /// this mission: civilians killed, coins / runes / bone charms found
    pub civilians_killed: u32,
    pub coins_found: u32,
    pub runes_found: u32,
    pub charms_found: u32,
    /// carried through the campaign
    pub runes: u32,
    pub bone_charms: u32,
    /// the one-time hints given (the HUD tweak's "New Rune added..." messages)
    #[serde(default)]
    pub hints: Vec<String>,
    /// lethal / non-lethal totals of the missions before this one (chaos)
    pub past_kills: u32,
    pub past_knockouts: u32,
    /// powers owned and their level (1, 2): `Blink`, `Vitality`... (cooked game data names)
    pub powers: std::collections::BTreeMap<String, u8>,
    /// bone charms worn / found (by name)
    pub charms: Vec<String>,
    pub charms_owned: Vec<String>,
    /// upgrades bought (`Twk_Upgrade_...`) and blueprints found (`BP_..._AbsItm`)
    pub upgrades: Vec<String>,
    /// other gear by store item (`Flare_Ammo_twk`, `RewireTool_twk`...)
    pub items: std::collections::BTreeMap<String, u32>,
    /// the pistol and crossbow (given at the Hound Pits)
    pub weapons: bool,
    /// only the pistol so far (the Prison's, picked up)
    #[serde(default)]
    pub no_crossbow: bool,
    /// Blood Thirst's meter
    pub adrenaline: f32,
    /// mana regenerates up to here (a portion above the last expense)
    pub mana_cap: f32,
    /// seconds before health regenerates
    pub health_delay: f32,
    pub dead: bool,
    /// the scripts ended the mission (`DisSeqAct_GameOver`): why
    #[serde(skip)]
    pub game_over: Option<String>,
    pub damage_flash: f32,
    /// where the last blow came from (the HUD's directional damage)
    #[serde(skip)]
    pub hit_from: Option<Vec3>,
    /// the campaign's chaos level (`DisSeqVar_DarknessLevel`): missions played lethally add to it
    pub chaos_level: i32,
    /// at or past the current mission's high-chaos threshold
    pub chaos_high: bool,
    /// this mission: people met, alarms rung, bodies found by the guards
    pub npcs_met: u32,
    pub alarms_rung: u32,
    pub bodies_found: u32,
    /// counters the level scripts raise (`ePlayerStat_OutsiderShrineFound`...)
    pub counters: std::collections::BTreeMap<String, u32>,
    /// chapter notes and other journal entries (abstract item names)
    pub notes: Vec<String>,
    /// the keys on Corvo's ring (`DisKeyRing`: names doors' `m_MatchingKeys` list)
    #[serde(default)]
    pub keys: Vec<String>,
    /// the scripts took his sword (`DisSeqAct_RemoveInventoryItem`: the Tower's opening)
    #[serde(default)]
    pub unarmed: bool,
    /// his hands emptied by the scripts (`DisSeqAct_EquipItemType` DishonoredItemEmpty: the
    /// Hound Pits): attacking or blocking draws the sword again
    #[serde(default)]
    pub sheathed: bool,
    /// the story's current chapter (`DisSeqAct_SetCurrentChapter` tag)
    pub chapter: String,
    /// his things put away by the scripts (`DisSeqAct_BackupAndClearInventory`), until given
    /// back
    pub stash: Option<Stash>,
}

/// What `DisSeqAct_BackupAndClearInventory` takes away (all but his coins).
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Stash {
    pub items: std::collections::BTreeMap<String, u32>,
    pub bullets: u32,
    pub bolts: u32,
    pub sleep_darts: u32,
    pub health_elixirs: u32,
    pub mana_elixirs: u32,
    pub upgrades: Vec<String>,
}

impl PlayerStats {
    /// The scripts put his things away (`DisSeqAct_BackupAndClearInventory`) or give them back
    /// (`DisSeqAct_RestoreInventoryFromBackup`: added to what he got meanwhile).
    pub fn stash(&mut self, put_away: bool, upgrades: bool) {
        if put_away {
            let s = Stash {
                items: std::mem::take(&mut self.items),
                bullets: std::mem::take(&mut self.bullets),
                bolts: std::mem::take(&mut self.bolts),
                sleep_darts: std::mem::take(&mut self.sleep_darts),
                health_elixirs: std::mem::take(&mut self.health_elixirs),
                mana_elixirs: std::mem::take(&mut self.mana_elixirs),
                upgrades: if upgrades { std::mem::take(&mut self.upgrades) } else { Vec::new() },
            };
            self.stash = Some(s);
        } else if let Some(s) = self.stash.take() {
            for (k, n) in s.items {
                *self.items.entry(k).or_default() += n;
            }
            self.bullets += s.bullets;
            self.bolts += s.bolts;
            self.sleep_darts += s.sleep_darts;
            self.health_elixirs += s.health_elixirs;
            self.mana_elixirs += s.mana_elixirs;
            for u in s.upgrades {
                if !self.upgrades.contains(&u) {
                    self.upgrades.push(u);
                }
            }
        }
    }
}

impl Default for PlayerStats {
    fn default() -> Self {
        Self {
            health: 100.0,
            max_health: 100.0,
            mana: 100.0,
            max_mana: 100.0,
            mana_delay: 0.0,
            coins: 0,
            health_elixirs: 2,
            mana_elixirs: 2,
            sleep_darts: 10,
            bolts: 10,
            bullets: 6,
            kills: 0,
            knockouts: 0,
            times_detected: 0,
            civilians_killed: 0,
            coins_found: 0,
            runes_found: 0,
            charms_found: 0,
            runes: 0,
            bone_charms: 0,
            hints: Vec::new(),
            past_kills: 0,
            past_knockouts: 0,
            powers: Default::default(),
            charms: Vec::new(),
            charms_owned: Vec::new(),
            upgrades: Vec::new(),
            items: Default::default(),
            weapons: true,
            no_crossbow: false,
            adrenaline: 0.0,
            mana_cap: 100.0,
            health_delay: 0.0,
            dead: false,
            game_over: None,
            damage_flash: 0.0,
            hit_from: None,
            chaos_level: 0,
            chaos_high: false,
            npcs_met: 0,
            alarms_rung: 0,
            bodies_found: 0,
            counters: Default::default(),
            notes: Vec::new(),
            keys: Vec::new(),
            unarmed: false,
            sheathed: false,
            chapter: String::new(),
            stash: None,
        }
    }
}

impl PlayerStats {
    pub fn power(&self, name: &str) -> u8 {
        self.powers.get(name).copied().unwrap_or(0)
    }
    /// Pay for a power: regeneration will only give back a portion above what's left.
    pub fn spend_mana(&mut self, amount: f32, attrs: &crate::gamedata::Attrs) {
        self.mana = (self.mana - amount).max(0.0);
        self.mana_cap = (self.mana + attrs.mana_regen_portion).min(self.max_mana);
        self.mana_delay = attrs.mana_regen_delay;
    }
    /// Dishonored's chaos: lethal play raises it (see `campaign::chaos_high`).
    pub fn chaos(&self) -> &'static str {
        if self.chaos_high {
            "High"
        } else {
            "Low"
        }
    }
    /// A mission statistic by the original's name (`ePlayerStat_HostileKill`...).
    pub fn stat(&self, name: &str) -> u32 {
        match name {
            "ePlayerStat_HostileKill" => self.kills - self.civilians_killed.min(self.kills),
            "ePlayerStat_CivilianKill" => self.civilians_killed,
            "ePlayerStat_NumKills" => self.kills,
            "ePlayerStat_NumAlarmsTriggered" => self.alarms_rung,
            "ePlayerStat_NumCorpsesDiscovered" => self.bodies_found,
            "ePlayerStat_NPCsAlerted" => self.times_detected,
            "ePlayerStat_RuneFound" => self.runes_found,
            "ePlayerStat_BoneCharmFound" => self.charms_found,
            "ePlayerStat_GoldFound" => self.coins_found,
            "ePlayerStat_ChaosLevel" => self.chaos_level.max(0) as u32,
            other => self.counters.get(other).copied().unwrap_or(0),
        }
    }
}

/// Bend Time: the world's (NPCs', projectiles') time scale while it lasts.
#[derive(Resource)]
pub struct TimeControl {
    pub bend_remaining: f32,
    /// world time scale while bent (level 1 slows, level 2 stops)
    pub world_dilation: f32,
    /// the quick-access wheel's slow motion (1: closed)
    pub wheel: f32,
    /// the level scripts' bent time (`DisSeqAct_BendTime`): the world's dilation, how long
    pub scripted: Option<(f32, f32)>,
    /// a finisher's slow motion (1: none)
    pub finisher: f32,
}

impl Default for TimeControl {
    fn default() -> Self {
        Self { bend_remaining: 0.0, world_dilation: 0.0, wheel: 1.0, scripted: None, finisher: 1.0 }
    }
}

impl TimeControl {
    pub fn world_scale(&self) -> f32 {
        let bent = if self.bend_remaining > 0.0 { self.world_dilation } else { 1.0 };
        let scripted = self.scripted.map(|s| s.0).unwrap_or(1.0);
        bent.min(scripted).min(self.finisher) * self.wheel
    }
}

#[derive(Resource, Default)]
pub struct HudMessages {
    pub items: Vec<(String, f32)>,
    /// the tutorial up (the HUD movie's `tutorialMsg_mc`, one at a time): text, seconds left,
    /// seconds shown
    pub tutorial: Option<(String, f32, f32)>,
}

impl HudMessages {
    pub fn push(&mut self, s: impl Into<String>) {
        // (the HUD tweak's `m_fGameMessageDuration`)
        self.push_for(s, 5.0);
    }

    /// A tutorial, up for so long (`m_fTutorialDuration`), in place of the one up.
    pub fn tutorial(&mut self, s: impl Into<String>, secs: f32) {
        let s = s.into();
        match self.tutorial.as_mut() {
            Some(t) if t.0 == s => t.1 = secs,
            _ => self.tutorial = Some((s, secs, 0.0)),
        }
    }

    /// A message up for so long.
    pub fn push_for(&mut self, s: impl Into<String>, secs: f32) {
        let s = s.into();
        if self.items.last().map(|(t, _)| *t == s).unwrap_or(false) {
            return;
        }
        self.items.push((s, secs));
        if self.items.len() > 5 {
            self.items.remove(0);
        }
    }
}

/// A new level: the campaign carries over (refilled), a new mission restarts its counters;
/// anything else (a map started directly) begins afresh.
fn reset_stats(
    mut stats: ResMut<PlayerStats>,
    mut tc: ResMut<TimeControl>,
    mut campaign: ResMut<Campaign>,
    level: Option<Res<crate::level::LevelInfo>>,
    data: Res<crate::gamedata::Data>,
    script: Res<crate::campaign::CampaignScript>,
) {
    tc.bend_remaining = 0.0;
    tc.wheel = 1.0;
    let map = level.as_ref().map(|l| l.scene.name.clone()).unwrap_or_default();
    let mission = crate::save::mission_name(&map);
    match campaign.carry.take() {
        Some(mut s) => {
            s.health = s.max_health;
            s.mana = s.max_mana;
            s.mana_cap = s.max_mana;
            s.adrenaline = 0.0;
            s.dead = false;
            s.game_over = None;
            s.damage_flash = 0.0;
            // (the campaign's scripts begin and end missions themselves)
            if mission != campaign.mission && script.0.is_none() {
                s.past_kills += s.kills;
                s.past_knockouts += s.knockouts;
                s.kills = 0;
                s.knockouts = 0;
                s.times_detected = 0;
                s.civilians_killed = 0;
                s.coins_found = 0;
                s.runes_found = 0;
                s.charms_found = 0;
            }
            *stats = s;
        }
        None => {
            // a map started directly: what Corvo would have by then (Blink from the
            // Outsider's dream, weapons from the Hound Pits) and runes to spend
            let mut s = PlayerStats::default();
            let idx = crate::save::mission_index(&map);
            // the original's loadout for starting the mission, where it has one
            let loadout = crate::campaign::direct_start_loadout(&map).is_some_and(|l| crate::campaign::apply_loadout(&data, l, &mut s));
            if !loadout {
                s.weapons = idx >= 2;
                if idx >= 3 || (idx == 2 && !map.to_ascii_lowercase().starts_with("l_outsiderdream")) {
                    s.powers.insert("Blink".into(), 1);
                    s.runes = 3 * (idx as u32 - 2);
                }
            }
            *stats = s;
            campaign.travel = None;
            campaign.resume = None;
            campaign.script_state = None;
            campaign.levels = None;
        }
    }
    campaign.mission = mission;
}

/// What carries from one map to the next.
#[derive(Resource, Default)]
pub struct Campaign {
    /// the player's state when leaving the last map
    pub carry: Option<PlayerStats>,
    /// the mission being played
    pub mission: String,
    /// the level scripts' story flags so far
    pub flags: std::collections::HashMap<String, bool>,
    /// the campaign scripts' state carried to the next map, the map change op to resume
    /// there (relative to the campaign's first op), the travel destination and chapter
    pub script_state: Option<(Vec<crate::kismet::Val>, Vec<crate::kismet::OpState>)>,
    pub resume: Option<u32>,
    pub travel: Option<String>,
    /// the sublevels the map change streams in with the next map
    pub levels: Option<Vec<String>>,
    /// what Corvo took at the Hound Pits, kept from one visit to the next (the hub's level
    /// state, `DisSeqAct_SaveLevelState`): pickups by `campaign::hub_key`
    pub hub_taken: std::collections::BTreeSet<String>,
    /// the maps' states kept for a return within the mission (`DisSeqAct_SaveLevelState`):
    /// the map, its packages, its state (a save game's, as JSON)
    pub level_states: Vec<(String, Vec<String>, String)>,
}

fn tick_time_control(time: Res<Time>, mut tc: ResMut<TimeControl>, mut sfx: MessageWriter<crate::audio::PostEvent>) {
    let was = tc.bend_remaining;
    tc.bend_remaining = (tc.bend_remaining - time.delta_secs()).max(0.0);
    if was > 0.0 && tc.bend_remaining <= 0.0 {
        sfx.write(crate::audio::PostEvent::named("Snd_Power_P_Bend_Time_Stop", None));
        sfx.write(crate::audio::PostEvent::named("Snd_UI_Ingame_Slomo_End", None));
    }
}

fn tick_messages(time: Res<Time>, mut msgs: ResMut<HudMessages>) {
    let dt = time.delta_secs();
    for m in msgs.items.iter_mut() {
        m.1 -= dt;
    }
    msgs.items.retain(|m| m.1 > 0.0);
    if let Some(t) = msgs.tutorial.as_mut() {
        t.1 -= dt;
        t.2 += dt;
        if t.1 <= 0.0 {
            msgs.tutorial = None;
        }
    }
}
