//! Shared gameplay state and messages connecting player, NPC, combat and HUD systems.

use crate::GameState;
use bevy::prelude::*;

pub struct GameplayPlugin;

impl Plugin for GameplayPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<Noise>()
            .add_message::<NpcHit>()
            .add_message::<PlayerTakedown>()
            .add_message::<Struck>()
            .add_message::<PlayerHit>()
            .add_message::<NpcStagger>()
            .init_resource::<PlayerStats>()
            .init_resource::<TimeControl>()
            .init_resource::<HudMessages>()
            .init_resource::<Campaign>()
            .add_systems(OnEnter(GameState::InGame), reset_stats.after(crate::level::LevelSpawnSet))
            .add_systems(Update, (tick_time_control, tick_messages).run_if(in_state(GameState::InGame)))
            .add_systems(Last, (tick_adrenaline, log_health).run_if(in_state(GameState::InGame)));
    }
}

pub(crate) fn tick_adrenaline(time: Res<Time>, tc: Res<TimeControl>, attrs: Res<crate::gamedata::Attrs>, switches: Res<crate::kismet::ScriptSwitches>, mut stats: ResMut<PlayerStats>) {
    stats.advance_adrenaline(time.delta_secs() * tc.world_scale(), &attrs, switches.0.adrenaline_off);
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

/// A sound the AI can hear: where, how far (the original's loudness classes, `DefaultAI.ini`
/// `m_LoudnessDistanceArray`), and what it is to them.
#[derive(Message, Clone, Copy)]
pub struct Noise {
    pub pos: Vec3,
    pub radius: f32,
    pub kind: NoiseKind,
}

/// What a noise is to those who hear it (`EAINoiseContext` as their attention takes it): an
/// anomaly (footsteps, a door, a body falling), a threat (blades meeting, a thrown thing),
/// danger (a shot, an explosion), a death rattle, a cry for help, an alarm bell. The noises'
/// loudness and contexts are those roshan's r614/blink research decoded from the original's
/// contact system and callers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoiseKind {
    Anomaly,
    Threatening,
    Danger,
    DeathRattle,
    Help,
    Alarm,
}

impl NoiseKind {
    /// A Kismet noise's context (`EAINoiseContext`: `AINC_Danger`...).
    pub fn of_context(c: &str) -> Self {
        let c = c.to_ascii_lowercase();
        if c.contains("danger") || c.contains("explosion") || c.contains("watchtower") {
            NoiseKind::Danger
        } else if c.contains("threat") || c.contains("wolzap") {
            NoiseKind::Threatening
        } else if c.contains("deathrattle") {
            NoiseKind::DeathRattle
        } else if c.contains("help") {
            NoiseKind::Help
        } else if c.contains("alarm") {
            NoiseKind::Alarm
        } else {
            NoiseKind::Anomaly
        }
    }
    /// Its slot in an attention profile's heard reactions (`Attention::heard`).
    pub fn slot(self) -> usize {
        self as usize
    }
    /// Its `EAINoiseContext` name.
    pub fn context(self) -> &'static str {
        match self {
            NoiseKind::Anomaly => "Anomaly",
            NoiseKind::Threatening => "Threatening",
            NoiseKind::Danger => "Danger",
            NoiseKind::DeathRattle => "DeathRattle",
            NoiseKind::Help => "Help",
            NoiseKind::Alarm => "AlarmBell",
        }
    }
}

impl Noise {
    /// A fighting noise: what puts the armed on their guard (and NPCs without an attention
    /// profile straight into combat).
    pub fn combat(&self) -> bool {
        matches!(self.kind, NoiseKind::Threatening | NoiseKind::Danger | NoiseKind::Help | NoiseKind::Alarm)
    }
    /// What sets civilians running (the default panic behaviour's `m_ScaryNoises`: danger,
    /// threats, an alarm bell, a cry for help, a death rattle; anything but an anomaly), or
    /// those of their own panic behaviour (`Sight::scary`: contexts by name).
    pub fn panics(&self, scary: Option<&[String]>) -> bool {
        match scary {
            Some(list) => list.iter().any(|c| c.eq_ignore_ascii_case(self.kind.context())),
            None => self.kind != NoiseKind::Anomaly,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
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
    /// An enemy's grenade, before Corvo has picked it up.
    EnemyExplosion,
    /// An enemy grenade taken and thrown back by Corvo.
    GrenadeThrowback,
    StickyGrenade,
    ExplosiveBullet,
    /// another NPC (a faction feud) or the level's own rats: not Corvo's doing
    ByOthers,
    /// a thrown thing striking someone (its tweak's `m_Damage`, `DisDamageType_Impact`)
    Impact,
    /// a spring razor's shrapnel
    SpringRazor,
    /// a wall of light's disintegration
    WallOfLight,
    /// The Knife of Dunwall: an explosive bolt's blast
    ExplosiveBolt,
    /// an arc mine's lightning
    ArcMine,
    /// a stun mine's: asleep where they stand (`DisDLC06DamageType_ArcMineStunShot`:
    /// `m_bPutToSleep`, `m_bInstaRagdoll`)
    StunMine,
    /// chokedust: stunned a while, unharmed (`DisDLC06DamageType_SmokeGrenade`:
    /// `m_bCannotKillNPC`)
    Chokedust,
    /// Corvo sliding into someone: unharmed (`m_SlideDamageInfo` 0 to characters), it stumbles
    Slide,
}

/// Corvo took someone down (killed, or knocked out): who, how, for the challenges' scoring
/// rules (`DisDLC05ScoringRule_*`, their modifiers).
#[derive(Message, Clone, Debug)]
pub struct PlayerTakedown {
    /// who (and of which faction)
    pub npc: Entity,
    pub faction: String,
    pub pawn: String,
    pub story_group: String,
    pub kind: HitKind,
    pub lethal: bool,
    /// unaware of Corvo (not in combat)
    pub unaware: bool,
    /// a shot to the head
    pub head: bool,
    /// a drop assassination, from this high (m)
    pub drop: Option<f32>,
    /// while time was bent
    pub bent: bool,
    pub hostile: bool,
    pub at: Vec3,
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
pub fn hurt_player(stats: &mut PlayerStats, sfx: &mut MessageWriter<crate::audio::PostEvent>, damage: f32) {
    if stats.dead {
        return;
    }
    stats.take_damage(damage);
    stats.damage_flash = 1.0;
    if stats.health <= 0.0 {
        stats.dead = true;
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
    /// it can't be blocked, parried or met blade to blade (`DisItemAction.m_bUnblockable`: a
    /// blow under a table)
    pub unblockable: bool,
    /// a thing thrown or a thug's fire: it sounds as itself, not as a blade
    pub missile: bool,
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
    /// no falling damage (the challenges' scripts: `KismetMod_NoFallingDamage`)
    #[serde(skip)]
    pub fall_damage_off: bool,
    /// the pistol's shots (a challenge's accuracy: Oil Drop's)
    pub shots_fired: u32,
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
    /// the one-time hints given (older saves': the rune's and charm's windows now come up with
    /// each, as the HUD raises them)
    #[serde(default)]
    pub hints: Vec<String>,
    /// a bone charm picked up and not yet seen in the journal (the HUD's `m_bHasNewBoneCharm`,
    /// in its save data): the journal's Bone Charms tab flashes
    #[serde(default)]
    pub new_charm: bool,
    /// runes picked up over the game (the HUD's `m_NumRunesPickedUp`, in its save data)
    #[serde(default)]
    pub runes_picked: u32,
    /// the mission whose start was saved last (`DisSeqAct_AutoSave`'s `m_MissionNumber`): the
    /// statistics' Replay Mission plays it again
    #[serde(default)]
    pub mission_start: Option<u8>,
    /// lethal / non-lethal totals of the missions before this one (chaos)
    pub past_kills: u32,
    pub past_knockouts: u32,
    /// times seen in the missions before this one (the game's achievements: Ghost)
    #[serde(default)]
    pub past_detected: u32,
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
    /// Daud's kit: his wristbow, no pistol and no Heart (his loadouts' items)
    #[serde(default)]
    pub no_pistol: bool,
    #[serde(default)]
    pub no_heart: bool,
    /// Blood Thirst's meter
    pub adrenaline: f32,
    /// Remaining time before adrenaline starts burning away (Sustained Rage).
    #[serde(default)]
    adrenaline_delay: f32,
    /// Accumulated within a frame; saved too if a save precedes the Last schedule.
    #[serde(default)]
    adrenaline_gain: f32,
    #[serde(default)]
    adrenaline_damage: f32,
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
    /// the blows taken (a count) and the side the last came from (0 ahead, 1 behind, 2 left,
    /// 3 right): the arms' reaction
    #[serde(skip)]
    pub blow: (u32, u8),
    /// knocked down (a wolfhound's jump attack): how many times and from which side (the arms'
    /// knockdown), and how long yet he is down
    #[serde(skip)]
    pub knockdown: (u32, u8),
    #[serde(skip)]
    pub knocked: f32,
    /// he died of a fall, or drowned (his death's clip)
    #[serde(skip)]
    pub died_falling: bool,
    #[serde(skip)]
    pub died_drowning: bool,
    /// the campaign's chaos level (`DisSeqVar_DarknessLevel`): the step of the player tweak's
    /// `m_DarknessLevels` its score reaches (see `campaign::notify_deed`)
    pub chaos_level: i32,
    /// at or past the current mission's high-chaos threshold
    pub chaos_high: bool,
    /// the chaos score (`m_DarknessScore`, `DisSeqVar_Darkness`): what the deeds and the scripts
    /// add, never below 0, kept across missions
    #[serde(default)]
    pub chaos_score: i32,
    /// this mission's deed counters (`m_DeedCounters`)
    #[serde(default)]
    pub deed_counters: Vec<i32>,
    /// the statistics grown this frame that the deeds count: which, and the victim's pawn
    #[serde(skip)]
    pub deeds_pending: Vec<(&'static str, String)>,
    /// seconds left slowed by a rat's bite (40 % off his speeds, `Devouring Swarm`'s bite)
    #[serde(skip)]
    pub slowed: f32,
    /// this mission: people met, alarms rung, bodies found by the guards
    pub npcs_met: u32,
    pub alarms_rung: u32,
    pub bodies_found: u32,
    /// counters the level scripts raise (`ePlayerStat_OutsiderShrineFound`...)
    pub counters: std::collections::BTreeMap<String, u32>,
    /// chapter notes and other journal entries (abstract item names)
    pub notes: Vec<String>,
    /// the tutorial notes received (`EDisTutorialNote` names), to read again from the pause
    /// menu's help
    #[serde(default)]
    pub tutorials: Vec<String>,
    /// the tutorial just received, while its prompt to read it is up (its name, seconds left):
    /// the journal's key reads it
    #[serde(skip)]
    pub tutorial_prompt: Option<(String, f32)>,
    /// the tutorial trackers' state (`crate::tutorials`)
    #[serde(default)]
    pub trackers: crate::tutorials::Trackers,
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
    /// Food has a per-item health value; Healthy Appetite adds its attribute bonus.
    pub fn eat_food(&mut self, base: f32, bonus: f32) {
        self.health = (self.health + (base + bonus).max(0.0)).min(self.max_health);
    }

    /// Add only what fits, preserving any legacy over-cap inventory.
    pub fn give_elixirs(&mut self, mana: bool, amount: u32, capacity: u32) -> u32 {
        let count = if mana { &mut self.mana_elixirs } else { &mut self.health_elixirs };
        let added = amount.min(capacity.saturating_sub(*count));
        *count += added;
        added
    }
    /// Record actual health lost, rather than a net health delta that healing in
    /// the same frame could hide. All combat/environment damage shares this path.
    pub fn take_damage(&mut self, damage: f32) {
        if self.dead || !damage.is_finite() || damage <= 0.0 {
            return;
        }
        let lost = damage.min(self.health.max(0.0));
        self.health = (self.health - lost).max(0.0);
        self.adrenaline_damage += lost;
    }

    pub fn gain_adrenaline(&mut self, amount: f32) {
        if amount.is_finite() && amount > 0.0 {
            self.adrenaline_gain += amount;
        }
    }

    fn advance_adrenaline(&mut self, dt: f32, attrs: &crate::gamedata::Attrs, disabled: bool) {
        let gain = std::mem::take(&mut self.adrenaline_gain) + std::mem::take(&mut self.adrenaline_damage) * attrs.adrenaline_damage;
        if self.dead || disabled || self.power("BloodThirsty") == 0 {
            self.adrenaline = 0.0;
            self.adrenaline_delay = 0.0;
            return;
        }
        if gain > 0.0 {
            self.adrenaline = (self.adrenaline + gain).min(attrs.adrenaline_max);
            self.adrenaline_delay = attrs.adrenaline_cooldown;
        } else {
            let burn_time = (dt - self.adrenaline_delay).max(0.0);
            self.adrenaline_delay = (self.adrenaline_delay - dt).max(0.0);
            self.adrenaline = (self.adrenaline - attrs.adrenaline_burn * burn_time).clamp(0.0, attrs.adrenaline_max.max(0.0));
        }
    }

    /// The scripts put his things away (`DisSeqAct_BackupAndClearInventory`) or give them back
    /// (`DisSeqAct_RestoreInventoryFromBackup`: added to what he got meanwhile).
    pub fn stash(&mut self, put_away: bool, upgrades: bool, data: &crate::gamedata::Data, difficulty: u8) {
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
            if let Some(old) = &mut self.stash {
                for (k, n) in s.items {
                    let count = old.items.entry(k).or_default();
                    *count = count.saturating_add(n);
                }
                old.bullets = old.bullets.saturating_add(s.bullets);
                old.bolts = old.bolts.saturating_add(s.bolts);
                old.sleep_darts = old.sleep_darts.saturating_add(s.sleep_darts);
                old.health_elixirs = old.health_elixirs.saturating_add(s.health_elixirs);
                old.mana_elixirs = old.mana_elixirs.saturating_add(s.mana_elixirs);
                for u in s.upgrades {
                    if !old.upgrades.contains(&u) { old.upgrades.push(u); }
                }
            } else {
                self.stash = Some(s);
            }
        } else if let Some(s) = self.stash.take() {
            // Restore upgrades first: the recovered pouch/quiver determines how
            // much of the returned ammunition fits alongside newly found gear.
            for u in s.upgrades {
                if !self.upgrades.contains(&u) {
                    self.upgrades.push(u);
                }
            }
            let attrs = crate::gamedata::Attrs { ammo_capacity: data.ammo_capacities(self, difficulty), ..default() };
            for (k, n) in s.items {
                if let Some(ty) = crate::gadgets::ammo_type(&k) {
                    crate::gadgets::give_ammo(self, &attrs, ty, n);
                } else {
                    let count = self.items.entry(k).or_default();
                    *count = count.saturating_add(n);
                }
            }
            for (ty, n) in [(0, s.bullets), (2, s.bolts), (3, s.sleep_darts)] {
                crate::gadgets::give_ammo(self, &attrs, ty, n);
            }
            self.give_elixirs(false, s.health_elixirs, data.pawn("m_nMaxHealthElixir", 10.0).max(0.0) as u32);
            self.give_elixirs(true, s.mana_elixirs, data.pawn("m_nMaxManaElixir", 10.0).max(0.0) as u32);
        }
    }
}

#[cfg(test)]
mod inventory_tests {
    #[test]
    fn food_uses_original_item_health_plus_charm_bonus_and_health_cap() {
        let mut stats = super::PlayerStats::default();
        stats.health = 10.0;
        stats.max_health = 100.0;
        stats.eat_food(5.0, 0.0); // pear
        assert_eq!(stats.health, 15.0);
        stats.eat_food(30.0, 0.0); // bluejawed hagfish eggs
        assert_eq!(stats.health, 45.0);
        stats.eat_food(5.0, 5.0);
        assert_eq!(stats.health, 55.0);
        stats.health = 98.0;
        stats.eat_food(30.0, 5.0);
        assert_eq!(stats.health, 100.0);
    }
    use super::*;

    #[test]
    fn repeated_confiscation_survives_saving_and_restores_upgrades_before_ammo() {
        let mut data = crate::gamedata::Data::default();
        data.0.upgrades.push(dhcook::format::UpgradeDef { id: "Pouch".into(), attributes: vec![("BulletCapacity".into(), 10.0)], ..default() });
        let mut stats = PlayerStats::default();
        stats.bullets = 15;
        stats.health_elixirs = 8;
        stats.items.insert(crate::gadgets::GRENADES.into(), 4);
        stats.items.insert("MissionItem".into(), 1);
        stats.upgrades.push("Pouch".into());
        stats.stash(true, true, &data, 0);
        assert_eq!(stats.bullets, 0);
        assert!(stats.upgrades.is_empty());
        stats.bullets = 2;
        stats.stash(true, true, &data, 0);
        let mut stats: PlayerStats = serde_json::from_slice(&serde_json::to_vec(&stats).unwrap()).unwrap();
        stats.bullets = 5;
        stats.health_elixirs = 5;
        stats.items.insert(crate::gadgets::GRENADES.into(), 3);
        stats.stash(false, true, &data, 0);
        assert_eq!(stats.bullets, 20);
        assert_eq!(stats.health_elixirs, 10);
        assert_eq!(stats.items[crate::gadgets::GRENADES], 5);
        assert_eq!(stats.items["MissionItem"], 1);
        assert_eq!(stats.upgrades, vec!["Pouch"]);
        assert!(stats.stash.is_none());
        stats.bullets -= 1;
        stats.stash(false, true, &data, 0);
        assert_eq!(stats.bullets, 19);
        assert_eq!(stats.items["MissionItem"], 1);
    }
}

#[cfg(test)]
mod adrenaline_tests {
    use super::*;
    use crate::gamedata::Attrs;

    fn player() -> PlayerStats {
        let mut s = PlayerStats::default();
        s.powers.insert("BloodThirsty".into(), 1);
        s
    }

    #[test]
    fn damage_and_healing_in_the_same_frame_still_trigger_vengeance() {
        let mut s = player();
        let attrs = Attrs { adrenaline_damage: 1.0, ..default() };
        s.take_damage(12.0);
        s.health += 12.0;
        s.advance_adrenaline(0.1, &attrs, false);
        assert_eq!(s.health, 100.0);
        assert_eq!(s.adrenaline, 12.0);
        s.advance_adrenaline(0.1, &attrs, false);
        assert_eq!(s.adrenaline, 12.0);
    }

    #[test]
    fn cooldown_uses_only_elapsed_time_after_the_delay_and_survives_saving() {
        let mut s = player();
        let attrs = Attrs { adrenaline_cooldown: 20.0, ..default() };
        s.gain_adrenaline(60.0);
        // Pending gains must survive a save earlier in the same frame.
        let bytes = serde_json::to_vec(&s).unwrap();
        let mut s: PlayerStats = serde_json::from_slice(&bytes).unwrap();
        s.advance_adrenaline(0.0, &attrs, false);
        s.advance_adrenaline(19.5, &attrs, false);
        assert_eq!(s.adrenaline, 60.0);
        let bytes = serde_json::to_vec(&s).unwrap();
        let mut loaded: PlayerStats = serde_json::from_slice(&bytes).unwrap();
        loaded.advance_adrenaline(1.0, &attrs, false);
        assert_eq!(loaded.adrenaline, 59.5);
        loaded.gain_adrenaline(500.0);
        loaded.advance_adrenaline(0.0, &attrs, false);
        assert_eq!(loaded.adrenaline, attrs.adrenaline_max);
        loaded.advance_adrenaline(20.0, &attrs, false);
        assert_eq!(loaded.adrenaline, attrs.adrenaline_max);
        loaded.advance_adrenaline(1.0, &attrs, false);
        assert_eq!(loaded.adrenaline, attrs.adrenaline_max - 1.0);
    }

    #[test]
    fn no_power_disabled_and_dead_players_cannot_bank_adrenaline() {
        let attrs = Attrs { adrenaline_damage: 1.0, ..default() };
        for (power, disabled, dead) in [(false, false, false), (true, true, false), (true, false, true)] {
            let mut s = player();
            if !power { s.powers.clear(); }
            s.gain_adrenaline(60.0);
            s.take_damage(10.0);
            s.dead = dead;
            s.advance_adrenaline(0.0, &attrs, disabled);
            assert_eq!(s.adrenaline, 0.0);
            s.dead = false;
            s.powers.insert("BloodThirsty".into(), 1);
            s.advance_adrenaline(0.0, &attrs, false);
            assert_eq!(s.adrenaline, 0.0);
        }
    }
}

impl Default for PlayerStats {
    fn default() -> Self {
        Self {
            fall_damage_off: false,
            shots_fired: 0,
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
            new_charm: false,
            runes_picked: 0,
            mission_start: None,
            past_kills: 0,
            past_knockouts: 0,
            past_detected: 0,
            powers: Default::default(),
            charms: Vec::new(),
            charms_owned: Vec::new(),
            upgrades: Vec::new(),
            items: Default::default(),
            weapons: true,
            no_crossbow: false,
            no_pistol: false,
            no_heart: false,
            adrenaline: 0.0,
            adrenaline_delay: 0.0,
            adrenaline_gain: 0.0,
            adrenaline_damage: 0.0,
            mana_cap: 100.0,
            health_delay: 0.0,
            dead: false,
            game_over: None,
            damage_flash: 0.0,
            hit_from: None,
            blow: (0, 0),
            knockdown: (0, 0),
            knocked: 0.0,
            died_falling: false,
            died_drowning: false,
            chaos_level: 0,
            chaos_high: false,
            chaos_score: 0,
            deed_counters: Vec::new(),
            deeds_pending: Vec::new(),
            slowed: 0.0,
            npcs_met: 0,
            alarms_rung: 0,
            bodies_found: 0,
            counters: Default::default(),
            notes: Vec::new(),
            tutorials: Vec::new(),
            tutorial_prompt: None,
            trackers: Default::default(),
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
    /// The player killed someone (pawn tweak `pawn`): the statistics, and the chaos deeds on
    /// kills.
    pub fn killed(&mut self, pawn: &str, civilian: bool) {
        self.kills += 1;
        if civilian {
            self.civilians_killed += 1;
        }
        self.deeds_pending.push(("ePlayerStat_NumKills", pawn.to_string()));
    }
    /// Dishonored's chaos: the deeds raise it (see `campaign::chaos_high`).
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
            // (the money Corvo picks up: `m_pAbstractItem` coins)
            "ePlayerStat_AmountStolen" => self.coins_found,
            "ePlayerStat_ChaosLevel" => self.chaos_level.max(0) as u32,
            // (each power bought or raised a level: Blink, given, is the first)
            "ePlayerStat_PowersAcquired" => self.powers.values().map(|l| *l as u32).sum(),
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
    /// Daud's Blink aimed standing still: the world, and he, held (0) or let go (1)
    pub aim_freeze: f32,
    /// Bend Time's own pace for Corvo (`m_PlayerDilation`: 0.85, then 0.9); how far it still
    /// has to come in or go out (0 settled .. 1: over `m_fTimeDilationWarmupTime` in,
    /// `..CooldownTime` out, seconds) and whether it was running when last looked at
    pub player_dilation: f32,
    pub bend_lag: f32,
    pub bend_on: bool,
    pub ramp_in: f32,
    pub ramp_out: f32,
    /// Blink's own layer of the world's dilation (1: none) and how fast it moves (per second):
    /// down toward `BLINK_FLOOR` over `m_fBendTimeDownTime` as Corvo lets go, back up over
    /// `m_fBendTimeUpTime` from wherever it is once he arrives (as roshan's r614/blink research
    /// decoded Blink's release and end)
    pub blink_layer: f32,
    pub blink_rate: f32,
}

/// How far Blink's layer would slow the world (`0.05`: the release asks 0.01 of the helper,
/// which keeps a layer at 0.05 at least; reached only by a Blink lasting `m_fBendTimeDownTime`,
/// a short hop leaves it near 0.94). Corvo's own pace stays (the layer's player rate is 1).
pub const BLINK_FLOOR: f32 = 0.05;

impl Default for TimeControl {
    fn default() -> Self {
        Self { bend_remaining: 0.0, world_dilation: 0.0, wheel: 1.0, scripted: None, finisher: 1.0, aim_freeze: 1.0, player_dilation: 1.0, bend_lag: 0.0, bend_on: false, ramp_in: 0.15, ramp_out: 0.25, blink_layer: 1.0, blink_rate: 0.0 }
    }
}

impl TimeControl {
    /// How far bent time holds now (0..1): coming in over its warm-up, going out over its
    /// cool-down.
    pub fn bend_ramp(&self) -> f32 {
        if self.bend_remaining > 0.0 {
            1.0 - self.bend_lag
        } else {
            self.bend_lag
        }
    }
    /// Bent time starts (or stops): its ramp turns about from where it is.
    pub fn bend_turn(&mut self) {
        let on = self.bend_remaining > 0.0;
        if on != self.bend_on {
            self.bend_on = on;
            self.bend_lag = 1.0 - self.bend_lag;
        }
    }
    pub fn world_scale(&self) -> f32 {
        // (bent time comes in and goes out over its warm-up and cool-down; the slowings
        // multiply, as the engine's time-dilation helper folds its layers each tick: the world's
        // the product of every layer's, 0x009FCBC0, decoded for this rewrite)
        let bent = 1.0 + (self.world_dilation - 1.0) * self.bend_ramp();
        let scripted = self.scripted.map(|s| s.0).unwrap_or(1.0);
        bent * scripted * self.finisher * self.aim_freeze * self.blink_layer * self.wheel
    }
    /// Corvo's pace through his bent time (`m_PlayerDilation`, as it comes in).
    pub fn player_rate(&self) -> f32 {
        1.0 + (self.player_dilation - 1.0) * self.bend_ramp()
    }
    /// The time scale of what goes on through bent time (Corvo's own: his pace in it, a
    /// finisher's and the wheel's slow motion).
    pub fn own_scale(&self) -> f32 {
        self.finisher.min(1.0) * self.wheel * self.player_rate()
    }
    /// A character's time scale: the world's, or its own if bent time passes it by.
    pub fn npc_scale(&self, out_of_bend: bool) -> f32 {
        if out_of_bend {
            self.own_scale()
        } else {
            self.world_scale()
        }
    }
}

/// The HUD tweak's `m_fTutorialCycleDelay`: a cycling tutorial at the head of the stack hands
/// the screen to the next every so often.
const TUTORIAL_CYCLE: f32 = 5.0;
/// `m_fTutorialWindowDuration`
pub const TUTORIAL_WINDOW: f32 = 10.0;

/// An entry of the HUD's tutorial stack (`DisTutorialInfo`): its words, its seconds left while
/// shown (infinite: until put away), a window's picture (else the line), whether it cycles
/// (`DTT_TutorialCyclingMessage`), its priority and seconds shown. Saved with the game, as the
/// HUD's save data keeps them.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TutorialEntry {
    pub text: String,
    #[serde(with = "forever")]
    pub secs: f32,
    pub window: Option<u8>,
    cycling: bool,
    prio: u8,
    /// (not saved: a loaded line comes up afresh)
    #[serde(skip)]
    shown: f32,
    /// shown even while the HUD is hidden (`m_bForceDisplay`: a cinematic's)
    forced: bool,
}

/// Seconds that may be infinite (until put away), saved as none: JSON has no infinity.
pub(crate) mod forever {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &f32, s: S) -> Result<S::Ok, S::Error> {
        v.is_finite().then_some(*v).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
        Ok(Option::<f32>::deserialize(d)?.unwrap_or(f32::INFINITY))
    }
}

#[derive(Resource, Default)]
pub struct HudMessages {
    pub items: Vec<(String, f32)>,
    /// the tutorial line on screen (the HUD movie's `tutorialMsg_mc`): text, seconds left,
    /// seconds shown, from the stack
    pub tutorial: Option<(String, f32, f32)>,
    /// and whether it shows while the HUD is hidden
    pub tutorial_forced: bool,
    /// the tutorial stack (`m_TutorialStack`): one entry on screen at a time, the line or the
    /// window, by priority (the scripts' lines first, then the windows and the game's own),
    /// only the shown one's time running; a cycling one at the head passes the screen round
    /// every `m_fTutorialCycleDelay`
    pub stack: Vec<TutorialEntry>,
    shown: usize,
    cycle: f32,
    logged: usize,
}

impl HudMessages {
    pub fn push(&mut self, s: impl Into<String>) {
        // (the HUD tweak's `m_fGameMessageDuration`)
        self.push_for(s, 5.0);
    }

    /// A tutorial line (the scripts' `DisSeqAct_SetTutorialMessage`, a refusal), up for so
    /// long while shown (`m_fTutorialDuration`): ahead of the rest.
    pub fn tutorial(&mut self, s: impl Into<String>, secs: f32) {
        self.add_tutorial(TutorialEntry { text: s.into(), secs, window: None, cycling: false, prio: 3, shown: 0.0, forced: false });
    }

    /// A scripts' tutorial line shown even while the HUD is hidden (`m_bForceDisplay`: "Use
    /// your mouse to look around" over the Tower's boat ride, the challenges' briefings).
    pub fn tutorial_forced(&mut self, s: impl Into<String>, secs: f32) {
        self.add_tutorial(TutorialEntry { text: s.into(), secs, window: None, cycling: false, prio: 3, shown: 0.0, forced: true });
    }

    /// The game's own tutorial line (a tracker's: `DTT_TutorialCyclingMessage`): after those
    /// up, taking turns with them.
    pub fn tutorial_cycling(&mut self, s: impl Into<String>, secs: f32) {
        self.add_tutorial(TutorialEntry { text: s.into(), secs, window: None, cycling: true, prio: 2, shown: 0.0, forced: false });
    }

    /// A tutorial window (`SetTutorialWindow`): its words and picture, up
    /// `m_fTutorialWindowDuration` while shown.
    pub fn tutorial_window(&mut self, s: impl Into<String>, img: u8) {
        self.add_tutorial(TutorialEntry { text: s.into(), secs: TUTORIAL_WINDOW, window: Some(img), cycling: false, prio: 2, shown: 0.0, forced: false });
    }

    /// Into the stack as the HUD's insert does: an entry not cycling goes before the first of
    /// no higher priority, a cycling one after the last of no lower; one already there is
    /// renewed.
    fn add_tutorial(&mut self, e: TutorialEntry) {
        if e.text.is_empty() {
            return;
        }
        if let Some(t) = self.stack.iter_mut().find(|t| t.text == e.text && t.window == e.window) {
            t.secs = e.secs;
            return;
        }
        let shown_text = self.stack.get(self.shown).map(|t| t.text.clone());
        let at = if e.cycling {
            self.stack.iter().rposition(|t| t.prio >= e.prio).map_or(0, |i| i + 1)
        } else {
            self.stack.iter().position(|t| e.prio >= t.prio).unwrap_or(self.stack.len())
        };
        self.stack.insert(at, e);
        // (the one on screen stays on screen until the stack's own turn)
        if let Some(s) = shown_text {
            self.shown = self.stack.iter().position(|t| t.text == s).unwrap_or(0);
        }
    }

    /// The stack as a save left it (the HUD's save data writes `m_TutorialStack`, each entry
    /// with its time left, and its load clears the stack before reading it back): the screen
    /// goes to its head.
    pub fn restore_tutorials(&mut self, stack: Vec<TutorialEntry>) {
        self.stack = stack;
        self.shown = 0;
        self.cycle = 0.0;
        self.tutorial = None;
        self.tutorial_forced = false;
    }

    /// A tutorial put away (its condition gone: a blow blocked, the music box's tune out of
    /// reach).
    pub fn drop_tutorial(&mut self, s: &str) {
        self.stack.retain(|t| t.text != s);
    }

    /// The tutorial window on screen: its words and picture.
    pub fn tutorial_window_shown(&self) -> Option<(String, u8)> {
        self.stack.get(self.shown).and_then(|t| t.window.map(|i| (t.text.clone(), i)))
    }

    /// The stack's time: the entry on screen runs down (gone at its end); a cycling head hands
    /// the screen on; the line on screen follows.
    fn tick_tutorials(&mut self, dt: f32) {
        if self.stack.is_empty() {
            self.shown = 0;
            self.cycle = 0.0;
            self.tutorial = None;
            if self.logged != 0 && std::env::var("DH_TUTORIAL_LOG").is_ok() {
                info!("tutorial stack: empty");
            }
            self.logged = 0;
            return;
        }
        self.shown = self.shown.min(self.stack.len() - 1);
        let e = &mut self.stack[self.shown];
        e.secs -= dt;
        e.shown += dt;
        if e.secs <= 0.0 {
            self.stack.remove(self.shown);
            self.shown = 0;
            if self.stack.is_empty() {
                self.tutorial = None;
                return;
            }
        }
        let was = self.shown;
        if self.stack[0].cycling && self.stack.len() > 1 {
            self.cycle += dt;
            if self.cycle > TUTORIAL_CYCLE {
                self.cycle -= TUTORIAL_CYCLE;
                self.shown = (self.shown + 1) % self.stack.len();
            }
        } else {
            self.shown = 0;
            self.cycle = 0.0;
        }
        if self.shown != was {
            self.stack[self.shown].shown = 0.0;
        }
        if std::env::var("DH_TUTORIAL_LOG").is_ok() {
            let now: Vec<(&str, f32, bool, bool)> = self.stack.iter().map(|t| (t.text.get(..24).unwrap_or(&t.text), t.secs, t.window.is_some(), t.cycling)).collect();
            if self.shown != was || self.stack.len() != self.logged {
                info!("tutorial stack: shown {} of {:?}", self.shown, now);
                self.logged = self.stack.len();
            }
        }
        let e = &self.stack[self.shown];
        self.tutorial = e.window.is_none().then(|| (e.text.clone(), e.secs, e.shown));
        self.tutorial_forced = e.forced;
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
    mut data: ResMut<crate::gamedata::Data>,
    script: Res<crate::campaign::CampaignScript>,
) {
    tc.bend_remaining = 0.0;
    tc.wheel = 1.0;
    tc.aim_freeze = 1.0;
    let map = level.as_ref().map(|l| l.scene.name.clone()).unwrap_or_default();
    // (whose map it is: Daud's on The Knife of Dunwall's)
    let player = data.use_profile(&map);
    if player != "Corvo" {
        info!("player: {player} on {map}");
    }
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
                s.past_detected += s.times_detected;
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
            // (a map's own loadout first: The Knife of Dunwall's maps carry theirs)
            let loadout = crate::campaign::direct_start_loadout(&map).is_some_and(|l| {
                let local = level.as_ref().is_some_and(|lv| crate::campaign::apply_loadout_from(&lv.scene.loadouts, l, &mut s));
                if local {
                    crate::campaign::alias_powers(&data, &mut s);
                }
                local || crate::campaign::apply_loadout(&data, l, &mut s)
            });
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
    /// the pursuers away from their maps after Corvo (`travel`), and the last trip's number
    pub trips: Vec<crate::travel::Trip>,
    pub next_trip: u32,
}

fn tick_time_control(time: Res<Time>, mut tc: ResMut<TimeControl>, mut sfx: MessageWriter<crate::audio::PostEvent>) {
    let was = tc.bend_remaining;
    tc.bend_remaining = (tc.bend_remaining - time.delta_secs()).max(0.0);
    // (Bend Time's warm-up and cool-down: linear, as measured by roshan's r614/blink research)
    let dt = time.delta_secs();
    tc.bend_turn();
    let span = if tc.bend_on { tc.ramp_in } else { tc.ramp_out };
    tc.bend_lag = (tc.bend_lag - dt / span.max(0.01)).max(0.0);
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
    msgs.tick_tutorials(dt);
}

#[cfg(test)]
mod time_tests {
    use super::*;

    #[test]
    fn the_worlds_slowings_multiply() {
        // (Blink's layer halfway down while a finisher slows the world: both apply)
        let tc = TimeControl { blink_layer: 0.5, finisher: 0.5, ..Default::default() };
        assert!((tc.world_scale() - 0.25).abs() < 1e-6);
        // (Blink leaves Corvo's own pace)
        assert_eq!(tc.player_rate(), 1.0);
    }

    #[test]
    fn characters_out_of_bent_time_keep_corvos_time() {
        // (the scripts stop the world, as the trials' Daud does for his duel)
        let mut tc = TimeControl { scripted: Some((0.0, 0.0)), ..Default::default() };
        assert_eq!(tc.world_scale(), 0.0);
        assert_eq!(tc.npc_scale(false), 0.0);
        assert_eq!(tc.npc_scale(true), 1.0);
        // (Corvo's own slow motion still holds them: the wheel open)
        tc.wheel = 0.1;
        assert!((tc.npc_scale(true) - 0.1).abs() < 1e-6);
        // (his Bend Time, slowing)
        let tc = TimeControl { bend_remaining: 3.0, world_dilation: 0.2, ..Default::default() };
        assert!((tc.npc_scale(false) - 0.2).abs() < 1e-6);
        assert_eq!(tc.npc_scale(true), 1.0);
    }
}

#[cfg(test)]
mod tutorial_stack_tests {
    use super::*;

    #[test]
    fn one_tutorial_on_screen_at_a_time_by_priority() {
        let mut m = HudMessages::default();
        // the game's own: a window, then its line, each in its turn
        m.tutorial_window("window", 0);
        m.tutorial_cycling("cycling", 10.0);
        m.tick_tutorials(0.1);
        assert_eq!(m.tutorial_window_shown().map(|w| w.0), Some("window".to_string()));
        assert!(m.tutorial.is_none());
        // a script's line goes ahead of them
        m.tutorial("script", 10.0);
        m.tick_tutorials(0.1);
        assert_eq!(m.tutorial.as_ref().map(|t| t.0.as_str()), Some("script"));
        assert!(m.tutorial_window_shown().is_none());
        // its time runs only while it is shown; then the window's turn, untouched
        m.tick_tutorials(10.0);
        m.tick_tutorials(0.0);
        assert_eq!(m.tutorial_window_shown().map(|w| w.0), Some("window".to_string()));
        assert!(m.stack[0].secs > TUTORIAL_WINDOW - 0.5);
        // put away, the next
        m.drop_tutorial("window");
        m.tick_tutorials(0.0);
        assert_eq!(m.tutorial.as_ref().map(|t| t.0.as_str()), Some("cycling"));
    }

    #[test]
    fn a_cycling_head_takes_turns() {
        let mut m = HudMessages::default();
        m.tutorial_cycling("a", f32::INFINITY);
        m.tutorial_cycling("b", f32::INFINITY);
        m.tick_tutorials(0.1);
        assert_eq!(m.tutorial.as_ref().map(|t| t.0.as_str()), Some("a"));
        m.tick_tutorials(TUTORIAL_CYCLE);
        assert_eq!(m.tutorial.as_ref().map(|t| t.0.as_str()), Some("b"));
        m.tick_tutorials(TUTORIAL_CYCLE);
        assert_eq!(m.tutorial.as_ref().map(|t| t.0.as_str()), Some("a"));
    }

    #[test]
    fn the_stack_comes_back_from_a_save() {
        let mut m = HudMessages::default();
        m.tutorial_forced("until hidden", f32::INFINITY);
        m.tutorial_window("a window", 1);
        m.tick_tutorials(2.0);
        let json = serde_json::to_string(&m.stack).unwrap();
        let mut n = HudMessages::default();
        n.tutorial("another game's", f32::INFINITY);
        n.restore_tutorials(serde_json::from_str(&json).unwrap());
        n.tick_tutorials(0.1);
        assert_eq!(n.stack.len(), 2);
        assert!(n.stack[0].secs.is_infinite() && n.stack[0].forced);
        assert_eq!(n.stack[1].window, Some(1));
        assert_eq!(n.tutorial.as_ref().map(|t| t.0.as_str()), Some("until hidden"));
        assert!(n.tutorial_forced);
    }
}
