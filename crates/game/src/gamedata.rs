//! The original campaign data (cooked from the tweak objects): Corvo's powers, his
//! attributes per difficulty, bone charms, upgrades and stores. The player's derived
//! attributes (health, mana, regeneration, speeds) come from it plus what he owns.

use crate::gameplay::PlayerStats;
use crate::GameState;
use bevy::prelude::*;
use dhcook::format::{ActivePowerDef, CharmDef, GameData, PassivePowerDef};
use std::collections::BTreeMap;

pub struct GameDataPlugin;

impl Plugin for GameDataPlugin {
    fn build(&self, app: &mut App) {
        let path = std::path::Path::new("cache/game/gamedata.json");
        let data = match std::fs::read(path).map_err(anyhow::Error::from).and_then(|d| Ok(serde_json::from_slice::<GameData>(&d)?)) {
            Ok(d) => d,
            Err(e) => {
                warn!("no game data ({}: {e}); run `dhtool cook-gamedata`", path.display());
                GameData::default()
            }
        };
        app.insert_resource(Data(data, Corvo::default()))
            .init_resource::<Attrs>()
            .add_systems(Update, derive_attributes.run_if(in_state(GameState::InGame)));
    }
}

#[derive(Resource, Default)]
pub struct Data(pub GameData, pub Corvo);

/// Corvo's own powers, passives, attributes, pawn tweaks, loadouts and mission statistics
/// screens, kept while another player's are played (by that player's name); that player's
/// interface strings, over Corvo's.
#[derive(Default)]
pub struct Corvo(Option<(String, dhcook::format::PlayerProfile)>, BTreeMap<String, String>);

impl Data {
    /// The player of a map: another's profile (Daud on The Knife of Dunwall's maps) put in
    /// place of Corvo's, or Corvo's put back. Whose it is now.
    pub fn use_profile(&mut self, map: &str) -> String {
        let m = map.to_ascii_lowercase();
        let want = self.0.profiles.iter().find(|p| p.maps.iter().any(|pre| m.starts_with(pre.as_str()))).map(|p| p.name.clone());
        let now = self.1 .0.as_ref().map(|(n, _)| n.clone());
        if want == now {
            return now.unwrap_or_else(|| "Corvo".into());
        }
        // (Corvo's back first)
        if let Some((_, c)) = self.1 .0.take() {
            self.swap(c);
        }
        self.1 .1.clear();
        if let Some(p) = want.as_ref().and_then(|w| self.0.profiles.iter().find(|p| &p.name == w)).cloned() {
            let name = p.name.clone();
            self.1 .1 = p.texts.clone();
            let corvo = self.swap(p);
            self.1 .0 = Some((name, corvo));
        }
        want.unwrap_or_else(|| "Corvo".into())
    }

    /// Put a profile's data in place, giving back what was there.
    fn swap(&mut self, p: dhcook::format::PlayerProfile) -> dhcook::format::PlayerProfile {
        let d = &mut self.0;
        dhcook::format::PlayerProfile {
            name: p.name,
            maps: p.maps,
            actives: std::mem::replace(&mut d.actives, p.actives),
            passives: std::mem::replace(&mut d.passives, p.passives),
            attributes: std::mem::replace(&mut d.attributes, p.attributes),
            pawn: std::mem::replace(&mut d.pawn, p.pawn),
            loadouts: std::mem::replace(&mut d.loadouts, p.loadouts),
            mission_stats: std::mem::replace(&mut d.mission_stats, p.mission_stats),
            power_aliases: std::mem::replace(&mut d.power_aliases, p.power_aliases),
            faction: p.faction,
            texts: Default::default(),
            darkness: std::mem::replace(&mut d.darkness, p.darkness),
        }
    }

    /// An interface string (`HUD.m_InteractionTexts[23]`): the player's own, else Corvo's.
    pub fn ui_text(&self, key: &str) -> Option<&String> {
        self.1 .1.get(key).or_else(|| self.0.texts.get(key))
    }

    /// A power by the name the player's data gives it ("Transversal": Blink).
    pub fn power_name<'a>(&'a self, name: &'a str) -> &'a str {
        self.0.power_aliases.iter().find(|(from, _)| from == name).map(|(_, to)| to.as_str()).unwrap_or(name)
    }

    /// Daud plays (The Knife of Dunwall's maps).
    pub fn is_daud(&self) -> bool {
        self.1 .0.as_ref().is_some_and(|(n, _)| n == "Daud")
    }

    /// The most of each ammunition (`eDisAmmoType` order) that can be carried: Corvo's eight,
    /// then Daud's explosive bolts, arc mines, chokedust and stun mines (none for Corvo, whose
    /// attributes have no such capacities).
    pub fn ammo_capacities(&self, stats: &PlayerStats, difficulty: u8) -> [u32; crate::gadgets::AMMO_TYPES] {
        ["BulletCapacity", "ExplosiveBulletCapacity", "ArrowCapacity", "SleepArrowCapacity", "FlareArrowCapacity", "SpringRazorCapacity", "GrenadeCapacity", "StickyGrenadeCapacity", "ExplosiveArrowCapacity", "ArcMineCapacity", "FlashSmokeCapacity", "ArcMineStunCapacity"].map(|name| {
            (self.0.attributes.get(name).map(|_| self.attribute(name, difficulty, &stats.powers, &stats.charms))
                .unwrap_or(match name {
                    "SpringRazorCapacity" | "GrenadeCapacity" | "StickyGrenadeCapacity" => 5.0,
                    "ExplosiveArrowCapacity" | "ArcMineCapacity" | "FlashSmokeCapacity" | "ArcMineStunCapacity" => 0.0,
                    _ => 10.0,
                })
                + self.0.upgrades.iter().filter(|u| stats.upgrades.contains(&u.id)).flat_map(|u| &u.attributes)
                    .filter(|(n, _)| n == name).map(|(_, value)| *value).sum::<f32>()).max(0.0) as u32
        })
    }
    pub fn active(&self, name: &str) -> Option<&ActivePowerDef> {
        self.0.actives.iter().find(|a| a.name == name)
    }
    pub fn passive(&self, name: &str) -> Option<&PassivePowerDef> {
        self.0.passives.iter().find(|a| a.name == name)
    }
    /// A setting of an active power at a level (1 or 2).
    pub fn power_f(&self, name: &str, level: u8, key: &str, default: f32) -> f32 {
        self.active(name)
            .and_then(|a| a.levels.get(level.clamp(1, 2) as usize - 1))
            .and_then(|l| l.get(key))
            .copied()
            .unwrap_or(default)
    }
    /// A Wwise event of an active power at a level.
    pub fn power_sound(&self, name: &str, level: u8, key: &str) -> Option<&str> {
        self.active(name).and_then(|a| a.level_sounds.get(level.clamp(1, 2) as usize - 1)).and_then(|l| l.get(key)).map(|s| s.as_str())
    }
    /// An interface string (`text("DisGFxMoviePlayerJournal_Texts", "t_Blink")`), with the
    /// key-binding tokens and markup of the original resolved for the keyboard.
    pub fn text(&self, section: &str, key: &str) -> String {
        let raw = self.0.texts.get(&format!("{section}.{key}")).cloned().unwrap_or_default();
        readable(&raw)
    }
    /// A HUD tweak string (`m_PressKeyToBuyPowers`...) made readable, else `fallback`.
    pub fn hud_text(&self, key: &str, fallback: &str) -> String {
        readable(self.0.texts.get(&format!("HUD.{key}")).map(|s| s.as_str()).unwrap_or(fallback))
    }
    /// One of Corvo's pawn tweak's strings (`m_HeartTargetTutorialMessage`), else `fallback`.
    pub fn pawn_text(&self, key: &str, fallback: &str) -> String {
        readable(self.0.texts.get(&format!("Pawn.{key}")).map(|s| s.as_str()).unwrap_or(fallback))
    }
    pub fn pawn(&self, key: &str, default: f32) -> f32 {
        self.0.pawn.get(key).copied().unwrap_or(default)
    }
    /// Find a bone charm (and its strength) by name.
    pub fn charm(&self, name: &str) -> Option<(&CharmDef, usize)> {
        self.0.charms.iter().find_map(|c| c.levels.iter().position(|l| l.0 == name).map(|i| (c, i)))
    }
    /// A player attribute: the difficulty's base value, modified by the passive powers owned
    /// (each level lists its full set of modifiers) and the bone charms worn.
    pub fn attribute(&self, name: &str, difficulty: u8, powers: &BTreeMap<String, u8>, charms: &[String]) -> f32 {
        let base = self.0.attributes.get(name).map(|v| v[difficulty.min(3) as usize]).unwrap_or(0.0);
        let mut v = base;
        for p in &self.0.passives {
            let lvl = powers.get(&p.name).copied().unwrap_or(0) as usize;
            let Some(l) = p.levels.get(lvl).filter(|_| lvl > 0) else { continue };
            for m in l.mods.iter().filter(|m| m.attribute == name) {
                match m.op.as_str() {
                    "SetVal" => v = m.value,
                    "AddBasePercent" => v += base * m.value / 100.0,
                    _ => v += m.value,
                }
            }
        }
        for c in charms {
            if let Some((def, i)) = self.charm(c).filter(|(d, _)| d.attribute == name) {
                v += def.levels[i].2;
            }
        }
        v
    }
}

/// The player's attributes this frame, in game units (metres, seconds).
#[derive(Resource, Clone)]
pub struct Attrs {
    pub walk: f32,
    pub sprint: f32,
    pub crouch: f32,
    /// the speeds sideways and backwards (`GroundStrafeMultiplier*`, `GroundBackwardMultiplier*`:
    /// running, sprinting, sneaking) and how fast a slowing takes hold (`m_fSpeedBlendDownSpeed`)
    pub strafe: [f32; 3],
    pub backward: [f32; 2],
    pub speed_blend_down: f32,
    /// Agility's power jump: holding jump keeps pushing up (m/s²) for a while, to a speed
    pub power_jump_accel: f32,
    pub power_jump_time: f32,
    pub power_jump_max: f32,
    /// the jump's speed (`JumpZ`, `JumpZCarryingCorpse`: m/s)
    pub jump: f32,
    pub jump_carrying: f32,
    /// the fall speeds (m/s) a landing starts to hurt at and kills at
    /// (`MaxSpeedBeforeFallingDamage`, `MaxSpeedBeforeFallingDeath`; Agility raises them)
    pub fall_hurt: f32,
    pub fall_death: f32,
    pub mantle_rate: f32,
    /// the landing's animation rate (`LandAnimRate`: Agility's +50%)
    pub land_rate: f32,
    pub choke_time: f32,
    pub melee_rate: f32,
    pub weapon_speed: f32,
    pub food_heal_bonus: f32,
    pub mana_potion_full_chance: f32,
    pub npc_gun_miss: f32,
    pub plague_damage_reduction: f32,
    pub plague_mana: f32,
    /// the armour's points off a blow (the Improved Armor upgrade's `DamageReduction`, whole)
    pub armor: f32,
    /// running and sprinting make no noise (the Silent Movement upgrades: `SilentRunning`,
    /// `SilentSprinting`: his `Footfall_Run_Player_Silent` steps)
    pub silent_running: bool,
    pub silent_sprinting: bool,
    pub mana_regen_delay: f32,
    /// mana per second while regenerating, and how far above the last expense it refills
    pub mana_regen_rate: f32,
    pub mana_regen_portion: f32,
    pub health_regen_delay: f32,
    pub health_regen_rate: f32,
    pub health_regen_limit: f32,
    pub health_elixir: f32,
    pub mana_elixir: f32,
    pub max_elixirs: u32,
    pub max_mana_elixirs: u32,
    pub adrenaline_max: f32,
    pub adrenaline_cooldown: f32,
    pub adrenaline_burn: f32,
    pub adrenaline_damage: f32,
    pub adrenaline_rat: f32,
    /// walking with a body on the shoulder (m/s)
    pub carry: f32,
    /// mana a drop assassination gives (Falling Star)
    pub drop_mana: f32,
    /// swimming speeds (m/s): without strokes, and sprinting (`WaterSpeed`)
    pub swim: f32,
    pub swim_fast: f32,
    /// drowning: damage units per step (seconds)
    pub drown_damage: f32,
    pub drown_step: f32,
    /// how hard Corvo throws what he carries (m/s, `ThrowStrength`; the Throwing Hand charm)
    pub throw: f32,
    /// his arms: a blow of his sword (`Twk_Inv_SwordCorvo`'s `m_MeleeDamage`), a pistol shot
    /// and a crossbow bolt
    pub sword_damage: f32,
    pub bullet: Shot,
    pub bolt: Shot,
    pub bolt_capacity: u32,
    pub ammo_capacity: [u32; crate::gadgets::AMMO_TYPES],
    pub bolt_break_modifier: f32,
}

impl Attrs {
    /// A blow, a shot or a blast less the armour: whole points off, never below 1 (the
    /// player's `TakeDamage`, as roshan's r614/blink research decoded it; bites, electricity
    /// and the plague go round it).
    pub fn armored(&self, damage: f32) -> f32 {
        if damage <= 0.0 || self.armor <= 0.0 {
            damage
        } else {
            (damage - self.armor).max(1.0)
        }
    }
    pub fn elixir_capacity(&self, mana: bool) -> u32 {
        if mana { self.max_mana_elixirs } else { self.max_elixirs }
    }
    /// Twist of Fortune affects automatic remedies as well as manually drunk ones.
    pub fn mana_elixir_amount(&self, max_mana: f32, roll: f32) -> f32 {
        if roll < self.mana_potion_full_chance { max_mana } else { self.mana_elixir }
    }
}

/// A projectile of Corvo's (`DisTweaks_Bullet`, `DisTweaks_Arrow`): its damage (its own, else
/// the weapon's `m_RangedDamage` times its `m_fDamageMultiplier`) and what changes it.
#[derive(Clone, Copy, Debug)]
pub struct Shot {
    pub damage: f32,
    /// to the head (`m_fDamageMultiplier_Headshot`; `m_bKillOnHeadshot`), against someone
    /// unaware (`m_fDamageMultiplier_Stealth`)
    pub headshot: f32,
    pub head_kill: bool,
    pub stealth: f32,
    /// within its close range (m), beyond its long range: the multipliers
    pub close: [f32; 2],
    pub long: [f32; 2],
}

impl Shot {
    /// The damage of a hit `d` m from the eye, to the head or not, on someone unaware of Corvo
    /// or not. (The ranges count from where the shot starts, `m_fBulletSpawnDistance` 150 uu
    /// ahead of the eye.)
    pub fn damage(&self, d: f32, head: bool, unaware: bool) -> f32 {
        if head && self.head_kill {
            return 999.0;
        }
        let d = (d - 1.5).max(0.0);
        let mut x = self.damage;
        if d < self.close[0] {
            x *= self.close[1];
        } else if d >= self.long[0] {
            x *= self.long[1];
        }
        if head {
            x *= self.headshot;
        }
        if unaware {
            x *= self.stealth;
        }
        x
    }
}

impl Default for Attrs {
    fn default() -> Self {
        Attrs {
            walk: 4.0,
            strafe: [0.7, 0.6, 0.5],
            backward: [0.8, 0.55],
            speed_blend_down: 8.0,
            sprint: 6.0,
            crouch: 2.75,
            power_jump_accel: 0.0,
            power_jump_time: 0.0,
            power_jump_max: 0.0,
            jump: 8.0,
            jump_carrying: 8.0,
            fall_hurt: 22.5,
            fall_death: 29.0,
            mantle_rate: 1.0,
            land_rate: 1.0,
            choke_time: 1.5,
            melee_rate: 1.0,
            weapon_speed: 0.9,
            food_heal_bonus: 0.0,
            mana_potion_full_chance: 0.0,
            npc_gun_miss: 0.0,
            plague_damage_reduction: 0.0,
            plague_mana: 0.0,
            armor: 0.0,
            silent_running: false,
            silent_sprinting: false,
            mana_regen_delay: 3.0,
            mana_regen_rate: 10.0,
            mana_regen_portion: 20.0,
            health_regen_delay: 10.0,
            health_regen_rate: 33.0,
            health_regen_limit: 10.0,
            health_elixir: 40.0,
            mana_elixir: 50.0,
            max_elixirs: 10,
            max_mana_elixirs: 10,
            adrenaline_max: 250.0,
            adrenaline_cooldown: 10.0,
            adrenaline_burn: 1.0,
            adrenaline_damage: 0.0,
            adrenaline_rat: 0.0,
            carry: 3.5,
            drop_mana: 0.0,
            swim: 3.5,
            swim_fast: 6.0,
            drown_damage: 1.0,
            drown_step: 1.0,
            throw: 20.0,
            sword_damage: 10.0,
            bullet: Shot { damage: 20.0, headshot: 3.0, head_kill: true, stealth: 1.0, close: [9.0, 1.5], long: [15.0, 1.0] },
            bolt: Shot { damage: 20.0, headshot: 1.2, head_kill: false, stealth: 2.0, close: [0.0, 1.0], long: [1e4, 1.0] },
            bolt_capacity: 10,
            ammo_capacity: [10, 10, 10, 10, 10, 5, 5, 5, 5, 5, 5, 5],
            bolt_break_modifier: 0.0,
        }
    }
}

/// Unreal units (centimetres) to metres.
const UU: f32 = 0.01;

fn derive_attributes(data: Res<Data>, settings: Res<crate::settings::Settings>, mut stats: ResMut<PlayerStats>, mut attrs: ResMut<Attrs>) {
    if data.0.attributes.is_empty() {
        return;
    }
    let d = settings.difficulty;
    let (powers, charms) = (stats.powers.clone(), stats.charms.clone());
    let a = |n: &str| data.attribute(n, d, &powers, &charms);
    let ammo_capacity = data.ammo_capacities(&stats, d);
    let max_health = a("HealthMax");
    let max_mana = a("ManaMax");
    if (stats.max_health - max_health).abs() > 0.01 {
        // a raised maximum fills the new room
        let gain = (max_health - stats.max_health).max(0.0);
        stats.max_health = max_health;
        stats.health = (stats.health + gain).min(max_health);
    }
    if (stats.max_mana - max_mana).abs() > 0.01 {
        stats.max_mana = max_mana;
        stats.mana = stats.mana.min(max_mana);
    }
    let step = a("ManaRegenStepTime").max(0.01);
    let h_step = a("HealthRegenRate").max(0.005);
    // a multiplier of the attributes, its own default where they have none
    let m = |n: &str, d0: f32| if data.0.attributes.contains_key(n) { a(n) } else { d0 };
    // his weapons' damage, and their projectiles
    let item = |n: &str, d0: f32| data.0.attributes.get(n).map(|v| v[d.min(3) as usize]).filter(|v| *v > 0.0).unwrap_or(d0);
    let shot = |key: &str, weapon: f32, head: f32| {
        let p = |k: &str, d0: f32| data.pawn(&format!("{key}{k}"), d0);
        let own = p("m_fDamage", 0.0);
        Shot {
            // (a bullet's: the pistol's `RangedDamage`, whole, plus its own `m_fDamage`: 10 + 20;
            // a bolt's the crossbow's times its multiplier; as roshan's r614/blink research
            // decoded the original)
            damage: if own > 0.0 { weapon.trunc() + own } else { weapon * p("m_fDamageMultiplier", 1.0) },
            headshot: p("m_fDamageMultiplier_Headshot", head),
            head_kill: p("m_bKillOnHeadshot", 0.0) > 0.5,
            stealth: p("m_fDamageMultiplier_Stealth", 1.0),
            close: [p("m_fCloseRange", 0.0) * UU, p("m_fDamageMultiplier_CloseRange", 1.0)],
            long: [p("m_fLongRange", 1e6) * UU, p("m_fDamageMultiplier_LongRange", 1.0)],
        }
    };
    *attrs = Attrs {
        walk: a("GroundSpeed") * UU,
        strafe: [m("GroundStrafeMultiplierRun", 0.7), m("GroundStrafeMultiplierSprint", 0.6), m("GroundStrafeMultiplierSneak", 0.5)],
        backward: [m("GroundBackwardMultiplierRun", 0.8), m("GroundBackwardMultiplierSprint", 0.55)],
        speed_blend_down: data.pawn("m_fSpeedBlendDownSpeed", 8.0),
        sprint: a("GroundSpeedSprint") * UU,
        crouch: a("GroundSpeedCrouch") * UU,
        // (our falling gravity is the original's: `player::GRAVITY`)
        power_jump_accel: a("HeldPowerJumpAccel_PROTOTYPE") * UU,
        power_jump_time: a("HeldPowerJumpButtonTime_PROTOTYPE"),
        power_jump_max: a("JumpZ_PowerJump") * UU,
        jump: if data.0.attributes.contains_key("JumpZ") { a("JumpZ") * UU } else { 8.0 },
        jump_carrying: if data.0.attributes.contains_key("JumpZCarryingCorpse") { a("JumpZCarryingCorpse") * UU } else { 8.0 },
        fall_hurt: if data.0.attributes.contains_key("MaxSpeedBeforeFallingDamage") { a("MaxSpeedBeforeFallingDamage") * UU } else { 22.5 },
        fall_death: if data.0.attributes.contains_key("MaxSpeedBeforeFallingDeath") { a("MaxSpeedBeforeFallingDeath") * UU } else { 29.0 },
        mantle_rate: a("MantleAnimRate"),
        land_rate: if data.0.attributes.contains_key("LandAnimRate") { a("LandAnimRate") } else { 1.0 },
        choke_time: a("ChokeTime").max(0.0),
        melee_rate: (1.0 + a("MeleeSpeed")).max(0.1),
        weapon_speed: a("SwordUnsheathedSpeedFactor").max(0.1),
        food_heal_bonus: a("FoodHealBonus"),
        mana_potion_full_chance: a("PotionManaBonusChanceRatio").clamp(0.0, 1.0),
        npc_gun_miss: a("ChanceNPCGunMiss").clamp(0.0, 1.0),
        plague_damage_reduction: a("PlagueDamageReduction").max(0.0),
        silent_running: data.0.upgrades.iter().any(|u| stats.upgrades.contains(&u.id) && u.attributes.iter().any(|(n, v)| n == "SilentRunning" && *v > 0.0)),
        silent_sprinting: data.0.upgrades.iter().any(|u| stats.upgrades.contains(&u.id) && u.attributes.iter().any(|(n, v)| n == "SilentSprinting" && *v > 0.0)),
        armor: data.0.upgrades.iter().filter(|u| stats.upgrades.contains(&u.id)).flat_map(|u| &u.attributes).filter(|(n, _)| n == "DamageReduction").map(|(_, v)| *v).sum::<f32>().trunc().max(0.0),
        plague_mana: a("ManaGainOnPlagueDamage").max(0.0),
        mana_regen_delay: a("ManaRegenInitialDelay"),
        mana_regen_rate: a("ManaRegenAmount") / step,
        mana_regen_portion: a("ManaRegenAdditivePortion"),
        health_regen_delay: a("HealthRegenInitialDelay"),
        health_regen_rate: a("HealthRegenAmount") / h_step,
        health_regen_limit: a("HealthRegenLimit"),
        health_elixir: data.pawn("m_nHealthElixirValue", 40.0) * (1.0 + a("HealthPotionPotencyBonus")),
        mana_elixir: data.pawn("m_nManaElixirValue", 50.0) * (1.0 + a("ManaPotionPotencyBonus")),
        max_elixirs: data.pawn("m_nMaxHealthElixir", 10.0) as u32,
        max_mana_elixirs: data.pawn("m_nMaxManaElixir", 10.0) as u32,
        adrenaline_max: a("AdrenalineMax"),
        adrenaline_cooldown: a("AdrenalineCooldown").max(0.0),
        adrenaline_burn: a("AdrenalineBurnRate").max(0.0),
        adrenaline_damage: a("AdrenalineMultWhenTakingDamage").max(0.0),
        adrenaline_rat: a("AdrenalineOnRatKill").max(0.0),
        carry: a("GroundSpeedCarryingCorpse") * UU,
        drop_mana: a("DropAssassinationManaBonus"),
        swim: data.pawn("swim.m_fMaxSpeedNoStroke", 350.0) * UU,
        swim_fast: a("WaterSpeed") * UU,
        drown_damage: a("DrowningDamageAmount"),
        drown_step: a("DrowningDamageStep"),
        throw: a("ThrowStrength") * 0.01,
        sword_damage: item("Sword_MeleeDamage", 10.0),
        bullet: shot("bullet.", item("Pistol_RangedDamage", 10.0), item("Pistol_RangedHeadshotMultiplier", 1.2)),
        bolt: shot("arrow.", item("Crossbow_RangedDamage", 20.0), item("Crossbow_RangedHeadshotMultiplier", 1.2)),
        bolt_capacity: ammo_capacity[2],
        ammo_capacity,
        bolt_break_modifier: a("ArrowBreakingModifier"),
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_is_played_on_its_maps_and_corvo_comes_back() {
        use dhcook::format::{ActivePowerDef, PlayerProfile};
        let mut d = GameData::default();
        d.actives.push(ActivePowerDef { name: "Blink".into(), mana: 10.0, ..Default::default() });
        d.attributes.insert("HealthMax".into(), [70.0; 4]);
        d.profiles.push(PlayerProfile {
            name: "Daud".into(),
            maps: vec!["dlc06_".into()],
            actives: vec![ActivePowerDef { name: "Blink".into(), mana: 20.0, ..Default::default() }],
            attributes: [("HealthMax".to_string(), [90.0; 4])].into_iter().collect(),
            power_aliases: vec![("Transversal".into(), "Blink".into())],
            texts: [("HUD.m_InteractionTexts[23]".to_string(), "End Void Gaze".to_string())].into_iter().collect(),
            ..Default::default()
        });
        let mut data = Data(d, Corvo::default());
        assert_eq!(data.use_profile("L_Streets1_P"), "Corvo");
        assert_eq!(data.active("Blink").map(|a| a.mana), Some(10.0));
        assert_eq!(data.use_profile("DLC06_DaudBase_P"), "Daud");
        assert_eq!(data.active("Blink").map(|a| a.mana), Some(20.0));
        assert_eq!(data.0.attributes["HealthMax"][1], 90.0);
        assert_eq!(data.power_name("Transversal"), "Blink");
        assert_eq!(data.ui_text("HUD.m_InteractionTexts[23]").map(|s| s.as_str()), Some("End Void Gaze"));
        // (again: nothing changes)
        assert_eq!(data.use_profile("DLC06_Slaughter_Ext_P"), "Daud");
        assert_eq!(data.active("Blink").map(|a| a.mana), Some(20.0));
        // back on Corvo's map: his own data
        assert_eq!(data.use_profile("L_Prison_P"), "Corvo");
        assert_eq!(data.active("Blink").map(|a| a.mana), Some(10.0));
        assert_eq!(data.0.attributes["HealthMax"][1], 70.0);
        assert_eq!(data.power_name("Transversal"), "Transversal");
        assert!(data.ui_text("HUD.m_InteractionTexts[23]").is_none());
        assert_eq!(data.0.profiles.len(), 1);
    }

    #[test]
    fn equipping_and_removing_charms_updates_combat_attributes() {
        let mut data = Data::default();
        for (name, value) in [("HealthMax", 70.0), ("ManaMax", 100.0), ("ChokeTime", 1.5), ("SwordUnsheathedSpeedFactor", 0.9)] {
            data.0.attributes.insert(name.into(), [value; 4]);
        }
        for (attribute, name, value) in [("ChokeTime", "Strong Arms", -1.5), ("MeleeSpeed", "Whirlwind II", 0.2), ("SwordUnsheathedSpeedFactor", "Fleet Fighter", 0.1)] {
            data.0.charms.push(dhcook::format::CharmDef { attribute: attribute.into(), levels: vec![(name.into(), String::new(), value)] });
        }
        let mut app = App::new();
        app.insert_resource(data).init_resource::<crate::settings::Settings>().init_resource::<PlayerStats>().init_resource::<Attrs>().add_systems(Update, derive_attributes);
        app.world_mut().resource_mut::<PlayerStats>().charms = vec!["Strong Arms".into(), "Whirlwind II".into(), "Fleet Fighter".into()];
        app.update();
        let attrs = app.world().resource::<Attrs>();
        assert_eq!(attrs.choke_time, 0.0);
        assert!((attrs.melee_rate - 1.2).abs() < 1e-6);
        assert!((attrs.weapon_speed - 1.0).abs() < 1e-6);
        app.world_mut().resource_mut::<PlayerStats>().charms.clear();
        app.update();
        let attrs = app.world().resource::<Attrs>();
        assert_eq!(attrs.choke_time, 1.5);
        assert_eq!(attrs.melee_rate, 1.0);
        assert_eq!(attrs.weapon_speed, 0.9);
    }

    #[test]
    fn twist_of_fortune_respects_chance_boundary_and_maximum_mana() {
        let attrs = Attrs { mana_elixir: 50.0, mana_potion_full_chance: 0.1, ..default() };
        assert_eq!(attrs.mana_elixir_amount(120.0, 0.099), 120.0);
        assert_eq!(attrs.mana_elixir_amount(120.0, 0.1), 50.0);
        assert_eq!(Attrs::default().mana_elixir_amount(120.0, 0.0), Attrs::default().mana_elixir);
    }

    #[test]
    fn quiver_upgrades_and_reinforced_bolts_derive_from_original_modifiers() {
        let mut data = Data::default();
        data.0.attributes.insert("ArrowCapacity".into(), [10.0; 4]);
        data.0.upgrades.push(dhcook::format::UpgradeDef { id: "Twk_Upgrade_QuiverCapacity2".into(), attributes: vec![("ArrowCapacity".into(), 10.0)], ..default() });
        data.0.upgrades.push(dhcook::format::UpgradeDef { id: "Twk_Upgrade_QuiverCapacity4".into(), attributes: vec![("ArrowCapacity".into(), 10.0)], ..default() });
        data.0.charms.push(dhcook::format::CharmDef { attribute: "ArrowBreakingModifier".into(), levels: vec![("Reinforced Bolts".into(), String::new(), -0.5)] });
        let mut app = App::new();
        app.insert_resource(data).init_resource::<crate::settings::Settings>().init_resource::<PlayerStats>().init_resource::<Attrs>().add_systems(Update, derive_attributes);
        app.update();
        assert_eq!(app.world().resource::<Attrs>().bolt_capacity, 10);
        {
            let mut stats = app.world_mut().resource_mut::<PlayerStats>();
            stats.upgrades = vec!["Twk_Upgrade_QuiverCapacity2".into(), "Twk_Upgrade_QuiverCapacity4".into()];
            stats.charms.push("Reinforced Bolts".into());
        }
        app.update();
        let attrs = app.world().resource::<Attrs>();
        assert_eq!(attrs.bolt_capacity, 30);
        assert_eq!(attrs.ammo_capacity, [10, 10, 30, 10, 10, 5, 5, 5, 0, 0, 0, 0]);
        assert_eq!(attrs.bolt_break_modifier, -0.5);
    }
}

/// Resolve the original strings' `GBA_*` binding tokens (the PC defaults) and markup.
pub fn readable(s: &str) -> String {
    const KEYS: [(&str, &str); 16] = [
        ("`GBA_Secondary`", "Right Mouse"),
        ("`GBA_Primary`", "Left Mouse"),
        ("`GBA_Block`", "Ctrl"),
        ("`GBA_Jump`", "Space"),
        ("`GBA_Journal`", "J"),
        ("`GBA_Use`", "F"),
        ("`GBA_Sneak`", "C"),
        ("`GBA_Sprint`", "Shift"),
        ("`GBA_Wheel`", "Middle Mouse"),
        ("`GBA_LeanLeft`", "Q"),
        ("`GBA_LeanRight`", "E"),
        ("`GBA_HealthElixir`", "R"),
        ("`GBA_ManaElixir`", "T"),
        ("`GBA_Zoom`", "Alt"),
        ("`GBA_QuickSave`", "F5"),
        ("`GBA_QuickLoad`", "F9"),
    ];
    let mut out = s.to_string();
    // keyboard actions name the key bound now
    use crate::bindings::Act;
    for (k, a) in [
        ("`GBA_Block`", Act::Block),
        ("`GBA_Jump`", Act::Jump),
        ("`GBA_Journal`", Act::Journal),
        ("`GBA_Use`", Act::Use),
        ("`GBA_Sneak`", Act::Crouch),
        ("`GBA_Sprint`", Act::Sprint),
        ("`GBA_LeanLeft`", Act::LeanLeft),
        ("`GBA_LeanRight`", Act::LeanRight),
        ("`GBA_HealthElixir`", Act::HealthElixir),
        ("`GBA_ManaElixir`", Act::ManaElixir),
        ("`GBA_QuickSave`", Act::QuickSave),
        ("`GBA_QuickLoad`", Act::QuickLoad),
    ] {
        if out.contains(k) {
            let h = crate::bindings::hint(a);
            out = out.replace(k, h.trim_start_matches('[').trim_end_matches(']'));
        }
    }
    for (k, v) in KEYS {
        out = out.replace(k, v);
    }
    out = out.replace("<br />", "
").replace("<br>", "
").replace("\r", "
");
    // colour markup (§C_BLUE§ ... §C§); other tags (§COUNT§, §MAX§) are their screens' to fill
    let s = '§'.len_utf8();
    let mut from = 0;
    while let Some(a) = out[from..].find('§').map(|i| from + i) {
        let Some(b) = out[a + s..].find('§') else { break };
        let tag = &out[a + s..a + s + b];
        if tag == "C" || tag.starts_with("C_") {
            out.replace_range(a..a + 2 * s + b, "");
            from = a;
        } else {
            from = a + s + b;
        }
    }
    out
}
