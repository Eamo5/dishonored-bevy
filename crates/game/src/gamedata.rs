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
        app.insert_resource(Data(data))
            .init_resource::<Attrs>()
            .add_systems(Update, derive_attributes.run_if(in_state(GameState::InGame)));
    }
}

#[derive(Resource, Default)]
pub struct Data(pub GameData);

impl Data {
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
    /// Agility's power jump: holding jump keeps pushing up (m/s²) for a while, to a speed
    pub power_jump_accel: f32,
    pub power_jump_time: f32,
    pub power_jump_max: f32,
    /// fall speeds (multipliers of the base thresholds) before damage / death
    pub fall_damage: f32,
    pub mantle_rate: f32,
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
    pub adrenaline_max: f32,
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
    /// The damage of a hit `d` m away, to the head or not, on someone unaware of Corvo or not.
    pub fn damage(&self, d: f32, head: bool, unaware: bool) -> f32 {
        if head && self.head_kill {
            return 999.0;
        }
        let mut x = self.damage;
        if d <= self.close[0] {
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
            sprint: 6.0,
            crouch: 2.75,
            power_jump_accel: 0.0,
            power_jump_time: 0.0,
            power_jump_max: 0.0,
            fall_damage: 1.0,
            mantle_rate: 1.0,
            mana_regen_delay: 3.0,
            mana_regen_rate: 10.0,
            mana_regen_portion: 20.0,
            health_regen_delay: 10.0,
            health_regen_rate: 33.0,
            health_regen_limit: 10.0,
            health_elixir: 40.0,
            mana_elixir: 50.0,
            max_elixirs: 10,
            adrenaline_max: 250.0,
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
    let base = |n: &str| data.attribute(n, d, &BTreeMap::new(), &[]);
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
    // his weapons' damage, and their projectiles
    let item = |n: &str, d0: f32| data.0.attributes.get(n).map(|v| v[d.min(3) as usize]).filter(|v| *v > 0.0).unwrap_or(d0);
    let shot = |key: &str, weapon: f32, head: f32| {
        let p = |k: &str, d0: f32| data.pawn(&format!("{key}{k}"), d0);
        let own = p("m_fDamage", 0.0);
        Shot {
            damage: if own > 0.0 { own } else { weapon * p("m_fDamageMultiplier", 1.0) },
            headshot: p("m_fDamageMultiplier_Headshot", head),
            head_kill: p("m_bKillOnHeadshot", 0.0) > 0.5,
            stealth: p("m_fDamageMultiplier_Stealth", 1.0),
            close: [p("m_fCloseRange", 0.0) * UU, p("m_fDamageMultiplier_CloseRange", 1.0)],
            long: [p("m_fLongRange", 1e6) * UU, p("m_fDamageMultiplier_LongRange", 1.0)],
        }
    };
    *attrs = Attrs {
        walk: a("GroundSpeed") * UU,
        sprint: a("GroundSpeedSprint") * UU,
        crouch: a("GroundSpeedCrouch") * UU,
        // the original's gravity is 1500 uu/s² (15 m/s²): scale so heights match ours
        power_jump_accel: a("HeldPowerJumpAccel_PROTOTYPE") * UU * crate::player::GRAVITY / 15.0,
        power_jump_time: a("HeldPowerJumpButtonTime_PROTOTYPE"),
        power_jump_max: a("JumpZ_PowerJump") * UU * (crate::player::GRAVITY / 15.0).sqrt(),
        fall_damage: a("MaxSpeedBeforeFallingDamage") / base("MaxSpeedBeforeFallingDamage").max(1.0),
        mantle_rate: a("MantleAnimRate"),
        mana_regen_delay: a("ManaRegenInitialDelay"),
        mana_regen_rate: a("ManaRegenAmount") / step,
        mana_regen_portion: a("ManaRegenAdditivePortion"),
        health_regen_delay: a("HealthRegenInitialDelay"),
        health_regen_rate: a("HealthRegenAmount") / h_step,
        health_regen_limit: a("HealthRegenLimit"),
        health_elixir: data.pawn("m_nHealthElixirValue", 40.0) * (1.0 + a("HealthPotionPotencyBonus")),
        mana_elixir: data.pawn("m_nManaElixirValue", 50.0) * (1.0 + a("ManaPotionPotencyBonus")),
        max_elixirs: data.pawn("m_nMaxHealthElixir", 10.0) as u32,
        adrenaline_max: a("AdrenalineMax"),
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
    };
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
    // colour markup (§C_BLUE§ ... §C§)
    while let Some(a) = out.find('§') {
        let Some(b) = out[a + '§'.len_utf8()..].find('§') else { break };
        let tag = &out[a + '§'.len_utf8()..a + '§'.len_utf8() + b];
        if tag.starts_with('C') {
            out.replace_range(a..a + 2 * '§'.len_utf8() + b, "");
        } else {
            break;
        }
    }
    out
}
