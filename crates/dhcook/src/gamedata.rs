//! Campaign-wide game data from the original tweak objects: Corvo's powers (active and
//! passive), his attributes per difficulty, bone charms, upgrades and the stores.

use crate::format::{AimAssist, AssistGroup, AssistInfo, AssistObject, AchievementDef, ActivePowerDef, AnimRumble, RumbleSample, BarkGroup, BarkHookRule, BarkRules, ChallengeDef, AttrMod, CharmDef, GalleryItemDef, GameData, PassiveLevel, PassivePowerDef, StatInfoDef, StoreDef, StoreItem, UpgradeDef};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::Path;
use upk::{Package, Props, Value};

/// The active powers: (component name, tweak object). Their mana and rune costs come from the
/// tweak; where the shipped tweak leaves them at zero (the game sets them natively) the
/// published values are used.
const ACTIVES: [(&str, &str, f32, [u32; 2]); 6] = [
    ("Blink", "Twk_Powers.Blink.Twk_Blink", 20.0, [0, 3]),
    ("DarkVision", "Twk_Powers.DarkVision.Twk_DarkVision", 20.0, [1, 2]),
    ("BendTime", "Twk_Powers.BendTime.Twk_BendTime", 60.0, [2, 8]),
    ("Possess", "Twk_Powers_Possess.Twk_Powers_Possess", 60.0, [3, 5]),
    ("DevouringSwarm", "Twk_Powers_DevouringSwarm.Twk_DevouringSwarm", 40.0, [3, 4]),
    ("Windblast", "Twk_Powers.Windblast.Twk_WindBlast", 50.0, [3, 4]),
];

fn props(pkg: &Package, idx: i32) -> Option<Props> {
    upk::read_object(pkg, idx).ok().map(|o| o.props)
}

/// An enum's value names, in order (a `UEnum`'s names: the export ends with their count and
/// the names).
pub fn enum_names(pkg: &Package, name: &str) -> Vec<String> {
    let Some(i) = (1..=pkg.exports.len() as i32).find(|&i| pkg.class_name(i) == "Enum" && pkg.obj_name(i) == name) else { return Vec::new() };
    let Ok(d) = pkg.export_data(i) else { return Vec::new() };
    let i32_at = |p: usize| i32::from_le_bytes([d[p], d[p + 1], d[p + 2], d[p + 3]]);
    for p in 0..d.len().saturating_sub(4) {
        let n = i32_at(p);
        if (2..=400).contains(&n) && p + 4 + n as usize * 8 == d.len() {
            let ids: Vec<i32> = (0..n as usize).map(|k| i32_at(p + 4 + k * 8)).collect();
            if ids.iter().all(|&v| v >= 0 && (v as usize) < pkg.names.len()) {
                return ids.iter().map(|&v| pkg.names[v as usize].clone()).collect();
            }
        }
    }
    Vec::new()
}

/// The player statistics the achievements read and the achievements (`Twk_PlayerStats`).
/// The Dunwall City Trials' challenges, as the challenge menu lists them
/// (`DisDLC05GameInfo.m_Challenges`).
fn challenges(game: &Package, d: &mut GameData) {
    let Some(p) = game.find_export("Default__DisDLC05GameInfo").and_then(|i| props(game, i)) else { return };
    let Some((c, off, sz)) = p.array("m_Challenges") else { return };
    for s in upk::props::parse_struct_array(game, off, sz, c).unwrap_or_default() {
        let text = |n: &str| match s.get(n) {
            Some(Value::Str(t)) => t.clone(),
            _ => String::new(),
        };
        let medals = |n: &str| {
            let mut m = [0; 3];
            for (k, v) in m.iter_mut().enumerate() {
                if let Some(Value::Int(i)) = s.get_idx(n, k as i32) {
                    *v = *i;
                }
            }
            m
        };
        let launch = text("m_LaunchCommand");
        d.challenges.push(ChallengeDef {
            id: text("m_ID"),
            name: text("m_Name"),
            description: text("m_Description"),
            expert_description: text("m_ExpertModeDesc"),
            kind: s.name("m_Type").unwrap_or_default().to_string(),
            leaderboard: s.name("m_Leaderboard").unwrap_or_default().to_string(),
            medals: medals("m_NormalModeMedalThresholds"),
            expert_medals: medals("m_ExpertModeMedalThresholds"),
            map: launch.strip_prefix("start ").unwrap_or(&launch).trim().to_string(),
            can_end_early: s.bool("m_bCanEndChallengeEarly").unwrap_or(false),
        });
    }
    log::info!("{} challenges", d.challenges.len());
    if let Some((c, off, sz)) = p.array("m_Gallery") {
        for s in upk::props::parse_struct_array(game, off, sz, c).unwrap_or_default() {
            let flag = |n: &str| s.bool(n).unwrap_or(false);
            d.gallery.push(GalleryItemDef {
                id: match s.get("m_ID") {
                    Some(Value::Str(t)) => t.clone(),
                    _ => String::new(),
                },
                normal: flag("m_bUnlockedInNormal"),
                expert: flag("m_bUnlockedInExpert"),
                all_normal: flag("m_bUnlockedWhenNormalCompleted"),
                all_expert: flag("m_bUnlockedWhenExpertCompleted"),
                challenge: s.name("m_UnlockChallenge").unwrap_or_default().to_string(),
                stars: s.int("m_UnlockMedal").unwrap_or(0),
            });
        }
        log::info!("{} pieces of the trials' gallery", d.gallery.len());
    }
}

fn player_stats(game: &Package, startup: &Package, d: &mut GameData) {
    let Some(p) = startup.find_export("Twk_PlayerStats.Twk_PlayerStats").and_then(|i| props(startup, i)) else { return };
    let names_of = |pkg: &Package, props: &Props, name: &str| -> Vec<String> {
        upk::props::object_array(pkg, props, name).into_iter().filter(|&o| o != 0).map(|o| pkg.obj_path(o).rsplit('.').next().unwrap_or("").to_string()).collect()
    };
    if let Some((c, off, sz)) = p.array("m_StatInfos") {
        for s in upk::props::parse_struct_array(startup, off, sz, c).unwrap_or_default() {
            let pair = |v: Option<&Value>| match v {
                Some(Value::Struct(_, f)) => Props(f.clone()).name("m_TweaksName").filter(|n| *n != "None").map(str::to_string),
                _ => None,
            };
            let excluded = match s.array("m_ExcludedTweaksNamePairs") {
                Some((c, off, sz)) => upk::props::parse_struct_array(startup, off, sz, c).unwrap_or_default().iter().filter_map(|x| x.name("m_TweaksName").filter(|n| *n != "None").map(str::to_string)).collect(),
                None => Vec::new(),
            };
            d.stat_infos.push(StatInfoDef {
                stat: s.name("m_Stat").unwrap_or_default().to_string(),
                text: match s.get("m_Text") {
                    Some(Value::Str(t)) => t.clone(),
                    _ => String::new(),
                },
                damage_types: names_of(startup, &s, "m_DamageTypes"),
                tweaks: pair(s.get("m_TweaksNamePair")).into_iter().collect(),
                excluded,
            });
        }
    }
    let names = enum_names(game, "EAchievement");
    for (i, name) in names.iter().enumerate() {
        if name.ends_with("_MAX") {
            break;
        }
        let mut a = AchievementDef { name: name.clone(), ..Default::default() };
        if let Some(Value::Struct(_, f)) = p.get_idx("m_Achievements", i as i32) {
            let f = Props(f.clone());
            a.kismet = matches!(f.get("m_bEvaluatedAtKismet"), Some(Value::Bool(true)));
            if let Some((c, off, sz)) = f.array("m_StatEvals") {
                for e in upk::props::parse_struct_array(startup, off, sz, c).unwrap_or_default() {
                    let stat = match e.get("m_StatIndex") {
                        Some(Value::Int(v)) => *v as u32,
                        _ => 0,
                    };
                    a.evals.push((stat, e.name("m_eValueInequality").unwrap_or("eValueInequality_GreaterThan").to_string(), e.float("m_fThreshold").unwrap_or(0.0)));
                    a.streaks.push([e.float("m_fStreakValue").unwrap_or(0.0), e.float("m_fStreakTime").unwrap_or(0.0)]);
                }
            }
        }
        d.achievements.push(a);
    }
    log::info!("player stats: {} statistics, {} achievements", d.stat_infos.len(), d.achievements.len());
}

fn object_name(pkg: &Package, o: i32) -> String {
    let p = pkg.obj_path(o);
    p.rsplit('.').next().unwrap_or(&p).to_string()
}

/// Every number in a property list (nested structs as `a.b`, static arrays as `a[i]`);
/// object references to Wwise events go to `sounds`.
fn flatten(pkg: &Package, props: &[upk::props::Prop], prefix: &str, out: &mut BTreeMap<String, f32>, sounds: &mut BTreeMap<String, String>) {
    for p in props {
        let key = if p.index > 0 { format!("{prefix}{}[{}]", p.name, p.index) } else { format!("{prefix}{}", p.name) };
        match &p.value {
            Value::Float(f) => {
                out.insert(key, *f);
            }
            Value::Int(i) => {
                out.insert(key, *i as f32);
            }
            Value::Bool(b) => {
                out.insert(key, if *b { 1.0 } else { 0.0 });
            }
            Value::Byte(b) => {
                out.insert(key, *b as f32);
            }
            Value::Vector(v) => {
                for (c, x) in ["x", "y", "z"].iter().zip(v) {
                    out.insert(format!("{key}.{c}"), *x);
                }
            }
            Value::Struct(_, sub) => flatten(pkg, sub, &format!("{key}."), out, sounds),
            Value::Object(o) if *o != 0 => {
                let cls = pkg.class_name(*o);
                if cls == "AkEvent" || pkg.obj_path(*o).contains("Snd_") {
                    sounds.insert(key, object_name(pkg, *o));
                }
            }
            _ => {}
        }
    }
}

/// Daud's profile in The Knife of Dunwall (`DisDLC06GameInfo.m_CampaignPawnTweak`:
/// `DLC06_Twk_Player_Daud_Release`): his powers over Corvo's they play as (Transversal is
/// Blink, his Void Gaze Dark Vision: each tweak falls back to Corvo's), Summon Assassin, his
/// passives, attributes (over Corvo's: `m_pAttributeTweaks[1]`, the class default's under
/// them), pawn tweaks, the campaign's start loadouts and mission statistics screens, his
/// faction.
fn dlc06_profile(cooked: &Path, game: &Package, base: &GameData) -> Option<crate::format::PlayerProfile> {
    let dir = cooked.join("..").join("DLC").join("PCConsole").join("DLC06");
    let pkg = Package::open(&dir.join("DLC06_Twk_Player_Daud_SF.upk")).ok()?;
    let pawn_idx = pkg.find_export("DLC06_Twk_Player_Daud.DLC06_Twk_Player_Daud_Release")?;
    let pawn = props(&pkg, pawn_idx)?;
    let mut p = crate::format::PlayerProfile {
        name: "Daud".into(),
        maps: vec!["dlc06_".into()],
        power_aliases: vec![("Transversal".into(), "Blink".into()), ("DaudDarkVision".into(), "DarkVision".into())],
        ..Default::default()
    };
    // his active powers, each over Corvo's it plays as (the tweaks' own fallback), except
    // what a tweak keeps to itself (`m_FallbackSkip`)
    for (key, field, mana) in [("Blink", "m_pTransversalPowerTweaks", 20.0), ("DarkVision", "m_pDarkVisionPowerTweaks", 20.0), ("BendTime", "m_pBendTimePowerTweaks", 60.0), ("SummonAssassin", "m_pSummonAssassinPowerTweaks", 40.0)] {
        let Some(o) = pawn.object(field).filter(|o| *o > 0) else { continue };
        let Some(obj) = props(&pkg, o) else { continue };
        let cls = pkg.class_name(o);
        let def = game.find_export(&format!("Default__{cls}")).and_then(|d| props(game, d));
        let skip: Vec<String> = struct_array(&pkg, &obj, "m_FallbackSkip").iter().map(|f| s(f, "m_PropertyPathName")).collect();
        let mut a = base.actives.iter().find(|a| a.name == key).cloned().unwrap_or_else(|| ActivePowerDef { name: key.to_string(), mana, ..Default::default() });
        // (Daud's Blink stops time while he aims standing still: `DisDLC06ActivePowerComponent_Transversal`)
        if key == "Blink" {
            a.params.insert("Transversal".into(), 1.0);
        }
        // (his Void Gaze: runes and charms only at level 1, `DisDLC06Tweaks_DarkVision`)
        if key == "DarkVision" {
            a.params.insert("VoidGaze".into(), 1.0);
        }
        if skip.iter().any(|f| f == "m_Levels") || a.levels.is_empty() {
            a.levels = vec![BTreeMap::new(); 2];
            a.level_sounds = vec![BTreeMap::new(); 2];
        }
        for i in 0..2 {
            let (dv, ds) = level_values(game, def.as_ref(), &Props(Vec::new()), "m_Levels", i);
            let (ov, os) = level_values(&pkg, None, &obj, "m_Levels", i);
            a.levels[i as usize].extend(dv.into_iter().chain(ov));
            a.level_sounds[i as usize].extend(ds.into_iter().chain(os));
        }
        let mut sounds = BTreeMap::new();
        if let Some(d) = def.as_ref() {
            let top: Vec<upk::props::Prop> = d.0.iter().filter(|p| p.name != "m_Levels").cloned().collect();
            flatten(game, &top, "", &mut a.params, &mut sounds);
        }
        let top: Vec<upk::props::Prop> = obj.0.iter().filter(|p| p.name != "m_Levels").cloned().collect();
        flatten(&pkg, &top, "", &mut a.params, &mut sounds);
        // (a power without levels of its own: its sounds at both)
        if key == "SummonAssassin" {
            for l in a.level_sounds.iter_mut() {
                l.extend(sounds.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
        }
        if let Some(m) = obj.int("m_ManaCost").filter(|m| *m > 0) {
            a.mana = m as f32;
        }
        // (its rune costs its own)
        a.runes = [0, 1].map(|i| match obj.get_idx("m_RuneCosts", i) {
            Some(Value::Int(v)) => (*v).max(0) as u32,
            _ => 0,
        });
        p.actives.push(a);
    }
    // his passives (`m_PassivePowers`: Shadow Kill, Celerity, Vitality, Blood Thirsty, Arcane Bond)
    p.passives = struct_array(&pkg, &pawn, "m_PassivePowers")
        .iter()
        .map(|pw| PassivePowerDef {
            name: s(pw, "m_Name"),
            levels: struct_array(&pkg, pw, "m_Levels")
                .iter()
                .map(|l| PassiveLevel {
                    runes: l.int("m_RuneCost").unwrap_or(0).max(0) as u32,
                    mods: struct_array(&pkg, l, "m_Modifiers")
                        .iter()
                        .map(|m| AttrMod {
                            attribute: s(m, "m_AttributeName").trim_start_matches("Attribute_").to_string(),
                            op: s(m, "m_ModType").trim_start_matches("eDisAttributeModifierType_").to_string(),
                            value: m.float("m_fModValue").unwrap_or(0.0),
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect();
    // his attributes over Corvo's (the DLC class's defaults, then his own)
    p.attributes = base.attributes.clone();
    let def = game.find_export("Default__DisDLC06Tweaks_PlayerPawn_Attributes").and_then(|d| props(game, d));
    let own = match pawn.get_idx("m_pAttributeTweaks", 1) {
        Some(Value::Object(o)) if *o > 0 => props(&pkg, *o),
        _ => None,
    };
    for a in [def.as_ref(), own.as_ref()].into_iter().flatten() {
        for prop in &a.0 {
            let Value::Struct(_, sub) = &prop.value else { continue };
            let e = p.attributes.entry(prop.name.trim_start_matches("m_").to_string()).or_insert([0.0; 4]);
            for f in sub {
                let v = match f.value {
                    Value::Float(v) => v,
                    Value::Int(v) => v as f32,
                    _ => continue,
                };
                match f.name.as_str() {
                    "m_fBaseValue1_Easy" => e[0] = v,
                    "m_fBaseValue2_Normal" => e[1] = v,
                    "m_fBaseValue3_Hard" => e[2] = v,
                    "m_fBaseValue4_VeryHard" => e[3] = v,
                    _ => {}
                }
            }
        }
    }
    // his pawn's numbers over Corvo's
    p.pawn = base.pawn.clone();
    let top: Vec<upk::props::Prop> = pawn.0.iter().filter(|p| !matches!(p.value, Value::Struct(..))).cloned().collect();
    flatten(&pkg, &top, "", &mut p.pawn, &mut BTreeMap::new());
    // his gadgets' numbers: the explosive bolt's (`arrow_explosive.`), the placed arc and
    // stun mines' (`arcmine.`, `stunmine.`), chokedust's grenade component (`chokedust.`);
    // their blasts' (`<key>explosion.`, with `m_RadialDamage` at normal difficulty)
    let scalars = |t: &upk::props::Props| -> Vec<upk::props::Prop> { t.0.iter().filter(|p| matches!(p.value, Value::Float(..) | Value::Int(..) | Value::Bool(..))).cloned().collect() };
    for (path, key, blast) in [
        ("DLC06_Twk_Inv_PlayerSpecific.DLC06_Proj_WristBolt_Explosive", "arrow_explosive.", "m_pExplosionTweak"),
        ("DLC06_Inv_ArcMine.DLC06_ArcMine_Placed", "arcmine.", "m_pExplosion"),
        ("DLC06_Inv_ArcMine.DLC06_StunMine_Placed", "stunmine.", "m_pExplosion"),
        ("DLC06_Proj_Flashsmoke.DLC06_Proj_ChokeDust.DisDLC06Tweaks_GrenadeComponent_0", "chokedust.", "m_pExplosionTweaks"),
    ] {
        let Some(t) = pkg.find_export(path).and_then(|i| props(&pkg, i)) else { continue };
        flatten(&pkg, &scalars(&t), key, &mut p.pawn, &mut BTreeMap::new());
        let Some(Value::Object(o)) = t.get(blast) else { continue };
        let Some(x) = (*o > 0).then(|| props(&pkg, *o)).flatten() else { continue };
        flatten(&pkg, &scalars(&x), &format!("{key}explosion."), &mut p.pawn, &mut BTreeMap::new());
        if let Some(Value::Struct(_, s)) = x.get("m_RadialDamage") {
            if let Some(v) = Props(s.clone()).float("m_fBaseValue2_Normal") {
                p.pawn.insert(format!("{key}explosion.m_RadialDamage"), v);
            }
        }
    }
    p.faction = match pawn.get("m_pFactionTweak") {
        Some(Value::Object(o)) if *o != 0 => object_name(&pkg, *o),
        _ => "DLC06_Fctn_Daud_Default".into(),
    };
    // his HUD's strings (`DLC06_Twk_UI.int`: `Twk_DisDLC06MoviePlayerHUD`) as `HUD.<key>`
    if let Ok(raw) = std::fs::read(cooked.join("..").join("Localization").join("INT").join("DLC06_Twk_UI.int")) {
        let mut hud = false;
        for line in decode_int(&raw).lines() {
            let line = line.trim();
            if line.starts_with('[') && line.ends_with(']') {
                hud = line[1..line.len() - 1].split_whitespace().next() == Some("Twk_DisDLC06MoviePlayerHUD");
                continue;
            }
            let Some((k, v)) = line.split_once('=').filter(|_| hud) else { continue };
            let v = v.trim();
            let v = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(v).to_string();
            p.texts.insert(format!("HUD.{}", k.trim()), v);
        }
    }
    p.darkness = darkness(&pkg, &pawn);
    // the campaign's start loadouts and mission statistics screens
    if let Ok(full) = Package::open(&dir.join("DLC06_DishonoredGameFull_P.upk")) {
        let mut tmp = GameData::default();
        campaign_tweaks(&full, &mut tmp);
        p.loadouts = tmp.loadouts;
        p.mission_stats = tmp.mission_stats;
    }
    Some(p)
}

/// The values of struct property `name[i]`, the class default's under the object's own.
fn level_values(pkg: &Package, def: Option<&Props>, obj: &Props, name: &str, i: i32) -> (BTreeMap<String, f32>, BTreeMap<String, String>) {
    let mut out = BTreeMap::new();
    let mut sounds = BTreeMap::new();
    for p in [def, Some(obj)].into_iter().flatten() {
        if let Some(Value::Struct(_, sub)) = p.get_idx(name, i) {
            flatten(pkg, sub, "", &mut out, &mut sounds);
        }
    }
    (out, sounds)
}

/// The mission statistics screens and start loadouts the campaign scripts name.
fn campaign_tweaks(pkg: &Package, d: &mut GameData) {
    use crate::format::{MissionStatRow, MissionStatsDef};
    let guid = |p: &Props, n: &str| match p.get(n) {
        Some(Value::Guid(g)) => format!("{:08x}{:08x}{:08x}{:08x}", g[0], g[1], g[2], g[3]),
        _ => String::new(),
    };
    for i in 1..=pkg.exports.len() as i32 {
        match pkg.class_name(i).as_str() {
            "DisTweaks_MissionStats" => {
                let Some(p) = props(pkg, i) else { continue };
                let mut m = MissionStatsDef { name: object_name(pkg, i), mission: p.int("m_MissionNumber").unwrap_or(-1), ..Default::default() };
                let maxes = i32_array(pkg, &p, "m_MissionStatsMaxValues");
                for (k, r) in struct_array(pkg, &p, "m_MissionStats").into_iter().enumerate() {
                    m.stats.push(MissionStatRow {
                        max: maxes.get(k).copied().unwrap_or(0).max(0) as u32,
                        description: s(&r, "m_Description"),
                        stat: s(&r, "m_Stat"),
                        add: r.bool("m_bAddOtherStat").unwrap_or(false).then(|| s(&r, "m_AdditionalStat")),
                        checkbox: r.bool("m_bCheckbox").unwrap_or(false),
                        nonzero: r.bool("m_bCheckIfNonZero").unwrap_or(false),
                    });
                }
                for r in struct_array(pkg, &p, "m_MissionSummaryItems") {
                    m.summary.push((s(&r, "m_Description"), guid(&r, "m_StoryFlag"), r.bool("m_bStoryFlagValue").unwrap_or(true)));
                }
                d.mission_stats.push(m);
            }
            "DisTweaks_PlayerLoadout" => {
                if let Some(l) = loadout(pkg, i) {
                    d.loadouts.push(l);
                }
            }
            _ => {}
        }
    }
    log::info!("campaign: {} mission stats screens, {} loadouts", d.mission_stats.len(), d.loadouts.len());
}

/// The player loadouts a package holds (the campaign's, or a map's own).
pub fn loadouts_in(pkg: &Package) -> Vec<crate::format::LoadoutDef> {
    (1..=pkg.exports.len() as i32).filter(|&i| pkg.class_name(i) == "DisTweaks_PlayerLoadout").filter_map(|i| loadout(pkg, i)).collect()
}

fn loadout(pkg: &Package, i: i32) -> Option<crate::format::LoadoutDef> {
    use crate::format::LoadoutDef;
    {
        {
            {
                let p = props(pkg, i)?;
                let mut l = LoadoutDef { name: object_name(pkg, i), ..Default::default() };
                if let Some(inv) = p.struct_props("m_InventoryLoadout") {
                    l.items = upk::props::object_array(pkg, &inv, "m_Items").into_iter().filter(|&o| o != 0).map(|o| object_name(pkg, o)).collect();
                    for a in struct_array(pkg, &inv, "m_AbstractItems") {
                        if let Some(o) = a.object("m_pItem").filter(|&o| o != 0) {
                            l.abstract_items.push((object_name(pkg, o), a.int("m_Quantity").unwrap_or(1).max(0) as u32));
                        }
                    }
                    for a in struct_array(pkg, &inv, "m_Ammo") {
                        let ty = s(&a, "m_AmmoType").trim_start_matches("eDisAmmoType_").to_string();
                        if !ty.is_empty() {
                            l.ammo.push((ty, a.int("m_AmmoAmount").unwrap_or(0).max(0) as u32));
                        }
                    }
                    let el = |i: i32| match inv.get_idx("m_ElixirCounts", i) {
                        Some(Value::Int(v)) => Some((*v).max(0) as u32),
                        _ => None,
                    };
                    if el(0).is_some() || el(1).is_some() {
                        l.elixirs = Some([el(0).unwrap_or(0), el(1).unwrap_or(0)]);
                    }
                }
                for a in struct_array(pkg, &p, "m_StartingPowers") {
                    l.powers.push((s(&a, "m_PowerToGive"), a.int("m_PowerLevel").unwrap_or(1).clamp(0, 9) as u8));
                }
                l.upgrades = upk::props::object_array(pkg, &p, "m_StartingUpgrades").into_iter().filter(|&o| o != 0).map(|o| object_name(pkg, o)).collect();
                Some(l)
            }
        }
    }
}

/// Read a localization file (UTF-16 or 8-bit).
fn read_int(path: &Path) -> Option<String> {
    let raw = std::fs::read(path).ok()?;
    Some(if raw.starts_with(&[0xff, 0xfe]) {
        let u: Vec<u16> = raw[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&u)
    } else {
        String::from_utf8_lossy(&raw).into_owned()
    })
}

/// Every abstract item's name and text (`[Group.Name DisAbstractItem]`).
fn abstract_items(cooked: &Path, d: &mut GameData) {
    let dir = cooked.join("..").join("Localization").join("INT");
    let Ok(rd) = std::fs::read_dir(&dir) else { return };
    // the keys' descriptions: the most common is a key's (`DisTweaks_Key`)
    let mut key_descs: BTreeMap<String, usize> = BTreeMap::new();
    for e in rd.flatten() {
        let p = e.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        if !name.to_ascii_lowercase().ends_with(".int") || name.starts_with("DLC") {
            continue;
        }
        let Some(text) = read_int(&p) else { continue };
        let mut key: Option<String> = None;
        let (mut title, mut body) = (String::new(), String::new());
        // characters' names: `[Pawn:Sub DisTweaks_InteractableInterface]` m_Name
        let pawns = name.starts_with("Pwn_") || name.starts_with("Twk_Pawn") || name.starts_with("Twk_Pwn");
        let mut pawn: Option<String> = None;
        // Corvo's arms' names and descriptions (`[Twk_Inv_SwordCorvo:..
        // DisTweaks_InventoryItem_PlayerSpecific]`) as `INV.<tweak>.m_Name`; a key's
        // (`DisTweaks_Key`) as `INV.Key.m_Description`
        let mut inv: Option<String> = None;
        let mut flush = |key: &mut Option<String>, title: &mut String, body: &mut String| {
            if let Some(k) = key.take() {
                // an audiograph's card (`[AbstractInv_Graphs.AG_.. DisAbstractItemAudioLog]`)
                if let Some(a) = k.strip_prefix("ag:").and_then(|k| d.audiographs.get_mut(k)) {
                    if !title.is_empty() {
                        a.title = std::mem::take(title);
                    }
                    if !body.is_empty() {
                        a.description = std::mem::take(body);
                    }
                } else if !k.starts_with("ag:") && (!title.is_empty() || !body.is_empty()) {
                    d.abstract_items.insert(k, (std::mem::take(title), std::mem::take(body)));
                }
            }
            title.clear();
            body.clear();
        };
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') && line.ends_with(']') {
                flush(&mut key, &mut title, &mut body);
                let head = &line[1..line.len() - 1];
                inv = if let Some(k) = head.strip_suffix(" DisTweaks_InventoryItem_PlayerSpecific") {
                    k.split(':').next().map(|t| t.to_string())
                } else if head.ends_with(" DisTweaks_Key") {
                    Some("Key".to_string())
                } else {
                    None
                };
                key = head
                    .strip_suffix(" DisAbstractItem")
                    .map(|k| k.to_string())
                    .or_else(|| head.strip_suffix(" DisAbstractItemAudioLog").map(|k| format!("ag:{k}")));
                pawn = pawns
                    .then(|| line[1..line.len() - 1].strip_suffix(" DisTweaks_InteractableInterface"))
                    .flatten()
                    .and_then(|k| k.split_once(':'))
                    .map(|(p, _)| p.to_string());
                continue;
            }
            if let Some(t) = &inv {
                if let Some((k, v)) = line.split_once('=') {
                    let v = v.trim().trim_matches('"').trim();
                    let k = k.trim();
                    if t == "Key" {
                        if k == "m_Description" && !v.is_empty() {
                            *key_descs.entry(v.to_string()).or_default() += 1;
                        }
                    } else if !v.is_empty() && (k == "m_Name" || k == "m_Description") {
                        d.texts.entry(format!("INV.{t}.{k}")).or_insert_with(|| v.to_string());
                    }
                }
                continue;
            }
            if let Some(p) = &pawn {
                if let Some(v) = line.strip_prefix("m_Name=") {
                    let v = v.trim().trim_matches('"');
                    if !v.is_empty() {
                        d.pawn_names.insert(p.clone(), v.to_string());
                    }
                }
                continue;
            }
            if key.is_none() {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            let v = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(v).replace("\\n", "\n");
            match k.trim() {
                "m_ItemName" => title = v,
                "m_Description" => body = v,
                _ => {}
            }
        }
        flush(&mut key, &mut title, &mut body);
    }
    if let Some((desc, _)) = key_descs.into_iter().max_by_key(|(_, n)| *n) {
        d.texts.insert("INV.Key.m_Description".into(), desc);
    }
    log::info!("gamedata: {} abstract items (notes, clues...), {} character names", d.abstract_items.len(), d.pawn_names.len());
}

/// The story's chapters (`Twk_ChapterInfoList`): titles, descriptions and targets.
/// The tutorial notes (`DisGFxMoviePlayerNote`'s `m_TutorialNotes`, indexed by
/// `EDisTutorialNote`), and the abstract items that bring one (`m_TutorialNote`: in the
/// `*Abst*` packages).
fn tutorial_notes(cooked: &Path, game: &Package, d: &mut GameData) {
    let ids = enum_names(game, "EDisTutorialNote");
    if let Some(p) = game.find_export("Default__DisGFxMoviePlayerNote").and_then(|i| props(game, i)) {
        for (k, id) in ids.iter().enumerate() {
            let Some(Value::Struct(_, s)) = p.get_idx("m_TutorialNotes", k as i32) else { continue };
            let s = Props(s.clone());
            let (Some(name), Some(image)) = (s.name("m_NoteName"), s.name("m_ImagePackageName")) else { continue };
            d.tutorial_notes.push(crate::format::TutorialNote { id: id.clone(), name: name.to_string(), image: image.to_string() });
        }
    }
    let Ok(rd) = std::fs::read_dir(cooked) else { return };
    let mut files: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.contains("Abst") && n.ends_with(".upk"))).collect();
    files.sort();
    for f in files {
        let Ok(pkg) = Package::open(&f) else { continue };
        for i in pkg.exports_of_class("DisAbstractItem").collect::<Vec<_>>() {
            if let Some(n) = props(&pkg, i).and_then(|p| p.name("m_TutorialNote").map(str::to_string)) {
                d.item_tutorials.insert(pkg.obj_name(i).to_string(), n);
            }
        }
    }
    log::info!("gamedata: {} tutorial notes, {} items bringing one", d.tutorial_notes.len(), d.item_tutorials.len());
}

fn chapters(pkg: &Package, d: &mut GameData) {
    let Some(list) = pkg.find_export("Twk_ChapterInfoList.Twk_ChapterInfoList").and_then(|i| props(pkg, i)) else { return };
    for e in struct_array(pkg, &list, "m_ChapterList") {
        let tag = s(&e, "m_ChapterTag");
        let Some(info) = e.object("m_pChapterInfo").filter(|&o| o > 0).and_then(|o| props(pkg, o)) else { continue };
        let mut c = crate::format::ChapterDef { tag, title: s(&info, "m_ChapterTitle"), description: s(&info, "m_ChapterDescription"), targets: Vec::new() };
        // a static array of objects (`m_ChapterTargets[i]`)
        let targets: Vec<i32> = info.all("m_ChapterTargets").filter_map(|p| if let Value::Object(o) = p.value { Some(o) } else { None }).collect();
        for t in targets.into_iter().filter(|&o| o > 0) {
            let Some(tp) = props(pkg, t) else { continue };
            let portrait = s(&tp, "m_TargetPortraitPath");
            c.targets.push((s(&tp, "m_TargetName"), portrait.rsplit('.').next().unwrap_or(&portrait).to_string()));
        }
        d.chapters.push(c);
    }
    log::info!("gamedata: {} chapters", d.chapters.len());
}

/// `DefaultGame.ini`: the missions' maps and the chaos thresholds.
fn missions_ini(cooked: &Path, d: &mut GameData) {
    let Some(game_dir) = cooked.parent() else { return };
    let Ok(text) = std::fs::read_to_string(game_dir.join("Config").join("DefaultGame.ini")) else { return };
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix(".m_MissionsGame=") {
            let num = rest.split("MissionNumber=").nth(1).and_then(|r| r.split(',').next()).and_then(|v| v.trim().parse::<i32>().ok());
            let maps: Vec<String> = rest
                .split("Maps=(")
                .nth(1)
                .and_then(|r| r.split(')').next())
                .map(|m| m.split(',').map(|x| x.trim().trim_matches('"').trim().to_string()).filter(|x| !x.is_empty()).collect())
                .unwrap_or_default();
            if let Some(n) = num {
                d.missions.push((n, maps));
            }
        } else if let Some(rest) = line.strip_prefix("m_MissionChaosThreshold[") {
            let mut it = rest.splitn(2, "]=");
            if let (Some(i), Some(v)) = (it.next().and_then(|i| i.parse::<usize>().ok()), it.next().and_then(|v| v.trim().parse::<i32>().ok())) {
                if d.chaos_thresholds.len() <= i {
                    d.chaos_thresholds.resize(i + 1, 0);
                }
                d.chaos_thresholds[i] = v;
            }
        }
    }
}

/// A player pawn tweak's chaos (`m_DarknessLevels`, `m_Deeds`).
fn darkness(pkg: &Package, pawn: &Props) -> crate::format::DarknessDef {
    // (deed tweaks by object name: `DLC06_Timsh_MS_Pawns.Guards.DLC06_Pwn_GeneralTurnbull_1`)
    let tweak = |p: &Props| -> String {
        let n = s(p, "m_TweaksName");
        let n = n.rsplit('.').next().unwrap_or("").to_string();
        if n.eq_ignore_ascii_case("none") {
            String::new()
        } else {
            n
        }
    };
    let deeds = struct_array(pkg, pawn, "m_Deeds")
        .iter()
        .map(|e| crate::format::DeedDef {
            stat: s(e, "m_Stat"),
            threshold: e.int("m_Threshold").unwrap_or(1).max(1),
            value: e.int("m_Value").unwrap_or(0),
            tweak: match e.get("m_TweaksNamePair") {
                Some(Value::Struct(_, sub)) => tweak(&Props(sub.clone())),
                _ => String::new(),
            },
            excluded: struct_array(pkg, e, "m_ExcludedTweaksNamePairs").iter().map(tweak).filter(|t| !t.is_empty()).collect(),
        })
        .collect();
    crate::format::DarknessDef { levels: i32_array(pkg, pawn, "m_DarknessLevels"), deeds }
}

fn struct_array(pkg: &Package, props: &Props, name: &str) -> Vec<Props> {
    match props.array(name) {
        Some((count, offset, size)) if count > 0 => upk::props::parse_struct_array(pkg, offset, size, count).unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn s(p: &Props, name: &str) -> String {
    match p.get(name) {
        Some(Value::Str(s)) | Some(Value::Name(s)) | Some(Value::Enum(s)) => s.clone(),
        _ => String::new(),
    }
}

fn i32_array(pkg: &Package, props: &Props, name: &str) -> Vec<i32> {
    match props.array(name) {
        Some((count, offset, size)) if size >= count * 4 => {
            (0..count).map(|k| i32::from_le_bytes(pkg.data[offset + k * 4..offset + k * 4 + 4].try_into().unwrap())).collect()
        }
        _ => Vec::new(),
    }
}

fn actives(game: &Package) -> Vec<ActivePowerDef> {
    let mut out = Vec::new();
    for (name, path, mana, runes) in ACTIVES {
        let Some(idx) = game.find_export(path) else {
            log::warn!("gamedata: no tweak {path}");
            continue;
        };
        let Some(obj) = props(game, idx) else { continue };
        let cls = game.class_name(idx);
        let def = game.find_export(&format!("Default__{cls}")).and_then(|d| props(game, d));
        let mut d = ActivePowerDef { name: name.to_string(), mana, runes, ..Default::default() };
        for i in 0..2 {
            let (v, snd) = level_values(game, def.as_ref(), &obj, "m_Levels", i);
            d.levels.push(v);
            d.level_sounds.push(snd);
        }
        let mut sounds = BTreeMap::new();
        for p in [def.as_ref(), Some(&obj)].into_iter().flatten() {
            let top: Vec<upk::props::Prop> = p.0.iter().filter(|p| p.name != "m_Levels").cloned().collect();
            flatten(game, &top, "", &mut d.params, &mut sounds);
        }
        // the tweak's own costs where it has them
        if let Some(m) = obj.int("m_ManaCost").filter(|m| *m > 0) {
            d.mana = m as f32;
        }
        let r: Vec<u32> = (0..2).map(|i| match obj.get_idx("m_RuneCosts", i) {
            Some(Value::Int(v)) => *v as u32,
            _ => 0,
        }).collect();
        if r.iter().any(|v| *v > 0) {
            d.runes = [r[0], r[1]];
        }
        out.push(d);
    }
    out
}

fn passives(game: &Package) -> Vec<PassivePowerDef> {
    let Some(idx) = game.find_export("Default__DishonoredPowersComponent") else { return Vec::new() };
    let Some(p) = props(game, idx) else { return Vec::new() };
    struct_array(game, &p, "m_Powers")
        .iter()
        .map(|pw| PassivePowerDef {
            name: s(pw, "m_Name"),
            levels: struct_array(game, pw, "m_Levels")
                .iter()
                .map(|l| PassiveLevel {
                    runes: l.int("m_RuneCost").unwrap_or(0).max(0) as u32,
                    mods: struct_array(game, l, "m_Modifiers")
                        .iter()
                        .map(|m| AttrMod {
                            attribute: s(m, "m_AttributeName").trim_start_matches("Attribute_").to_string(),
                            op: s(m, "m_ModType").trim_start_matches("eDisAttributeModifierType_").to_string(),
                            value: m.float("m_fModValue").unwrap_or(0.0),
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect()
}

/// `DisAttribute` structs: the four difficulty values, class default under the object.
fn attributes(game: &Package, startup: &Package) -> BTreeMap<String, [f32; 4]> {
    let mut out: BTreeMap<String, [f32; 4]> = BTreeMap::new();
    let def = game.find_export("Default__DisTweaks_PlayerPawn_Attributes").and_then(|d| props(game, d));
    let obj = startup.find_export("Twk_Pawn_Corvo.Twk_Pawn_Corvo_Release.pAttributeTweaksNormal").and_then(|d| props(startup, d));
    for p in [def.as_ref(), obj.as_ref()].into_iter().flatten() {
        for prop in &p.0 {
            let Value::Struct(_, sub) = &prop.value else { continue };
            let name = prop.name.trim_start_matches("m_").to_string();
            let e = out.entry(name).or_insert([0.0; 4]);
            for f in sub {
                let v = match f.value {
                    Value::Float(v) => v,
                    Value::Int(v) => v as f32,
                    _ => continue,
                };
                match f.name.as_str() {
                    "m_fBaseValue1_Easy" => e[0] = v,
                    "m_fBaseValue2_Normal" => e[1] = v,
                    "m_fBaseValue3_Hard" => e[2] = v,
                    "m_fBaseValue4_VeryHard" => e[3] = v,
                    _ => {}
                }
            }
        }
    }
    out
}

/// The pistol's and crossbow's aim assist (`DisTweaks_ProjectileAttack`): each field the fire
/// tweak's own, else as the class default holds it (a struct's fields its struct's defaults:
/// the crossbow's reach is `DisAimAssistChoiceInfo`'s 2000 uu).
fn aim_assist(game: &Package, startup: &Package) -> Vec<AimAssist> {
    let Some(def) = game.find_export("Default__DisTweaks_ProjectileAttack").and_then(|i| props(game, i)) else { return Vec::new() };
    // (a layer: the tweak's props and package, over the default's)
    #[derive(Clone)]
    struct L<'a> {
        own: Props,
        def: Props,
        pkgs: (&'a Package, &'a Package),
    }
    impl<'a> L<'a> {
        fn sub(&self, n: &str, i: i32) -> L<'a> {
            let s = |p: &Props| match p.get_idx(n, i) {
                Some(Value::Struct(_, s)) => Props(s.clone()),
                _ => Props::default(),
            };
            L { own: s(&self.own), def: s(&self.def), pkgs: self.pkgs }
        }
        fn f(&self, n: &str) -> f32 {
            self.own.float(n).or(self.def.float(n)).unwrap_or(0.0)
        }
        fn b(&self, n: &str) -> bool {
            self.own.bool(n).or(self.def.bool(n)).unwrap_or(false)
        }
        fn object(&self, n: &str) -> String {
            let path = |pkg: &Package, p: &Props| p.object(n).filter(|o| *o != 0).map(|o| pkg.obj_path(o));
            path(self.pkgs.0, &self.own).or_else(|| path(self.pkgs.1, &self.def)).unwrap_or_default()
        }
        /// an array of structs: the tweak's own, else the default's (each its own layer)
        fn array(&self, n: &str) -> Vec<L<'a>> {
            let parse = |pkg: &Package, p: &Props| p.array(n).map(|(c, off, sz)| upk::props::parse_struct_array(pkg, off, sz, c).unwrap_or_default());
            if let Some(v) = parse(self.pkgs.0, &self.own) {
                return v.into_iter().map(|own| L { own, def: Props::default(), pkgs: (self.pkgs.0, self.pkgs.0) }).collect();
            }
            parse(self.pkgs.1, &self.def).unwrap_or_default().into_iter().map(|def| L { own: Props::default(), def, pkgs: (self.pkgs.1, self.pkgs.1) }).collect()
        }
        fn info(&self) -> AssistInfo {
            // (`m_fPriorityWeightScale`'s own default is 1)
            let priority = self.own.float("m_fPriorityWeightScale").or(self.def.float("m_fPriorityWeightScale")).unwrap_or(1.0);
            AssistInfo {
                on: self.b("m_bUseAimAssist"),
                priority,
                safe: [self.f("m_fAimAssist_SafePercent_X"), self.f("m_fAimAssist_SafePercent_Y")],
                max: [self.f("m_fAimAssist_MaxSize_Horiz"), self.f("m_fAimAssist_MaxSize_Vert")],
            }
        }
    }
    let mut out = Vec::new();
    for (weapon, fire) in [
        ("pistol", "Twk_Inv_PlayerSpecific.Twk_Inv_PistolCorvo.DisTweaks_InventoryItem_PlayerSpecific_0.DisTweaks_FirePistol_1"),
        ("crossbow", "Twk_Inv_PlayerSpecific.Twk_Inv_CrossbowCorvo.DisTweaks_InventoryItem_PlayerSpecific_1.DisTweaks_FireCrossbow_0"),
    ] {
        let Some(own) = startup.find_export(fire).and_then(|i| props(startup, i)) else { continue };
        let top = L { own, def: def.clone(), pkgs: (startup, game) };
        let choice = top.sub("m_AimAssistChoiceInfo", 0);
        let mut groups: [AssistGroup; 4] = Default::default();
        for (k, (platform, group)) in [("m_AimAssistSettings_Mouse", "m_NormalAssist"), ("m_AimAssistSettings_Mouse", "m_ZoomedAssist"), ("m_AimAssistSettings_Gamepad", "m_NormalAssist"), ("m_AimAssistSettings_Gamepad", "m_ZoomedAssist")].into_iter().enumerate() {
            let g = top.sub(platform, 0).sub(group, 0);
            let pawns = g.sub("m_Pawns", 0);
            let mut regions = [AssistInfo { priority: 1.0, ..Default::default() }; 7];
            for (i, r) in regions.iter_mut().enumerate() {
                *r = pawns.sub("m_HitRegions", i as i32).info();
            }
            let objects = g
                .array("m_ObjectsOfInterest")
                .iter()
                .map(|o| AssistObject {
                    tweak: o.object("m_pTweaksType"),
                    class: o.object("m_pActorType").rsplit('.').next().unwrap_or("").to_string(),
                    info: o.sub("m_AimAssistInfo", 0).info(),
                })
                .collect();
            groups[k] = AssistGroup {
                max_dist: g.b("m_bUseGroupMaxDistance").then(|| g.f("m_fMaxAimAssistDistance") * 0.01),
                regions,
                skip_neutral: pawns.b("m_bDisableAssistForNeutralPawns"),
                skip_friendly: pawns.b("m_bDisableAssistForFriendlyPawns"),
                objects,
            };
        }
        let on = top.b("m_bUseAimAssist");
        out.push(AimAssist {
            weapon: weapon.into(),
            mouse: on && top.b("m_bUseAimAssist_Mouse"),
            pad: on && top.b("m_bUseAimAssist_Gamepad"),
            max_angle: choice.f("m_fMaxAimAssistAngle"),
            max_dist: choice.f("m_fMaxAimAssistDistance") * 0.01,
            angle_weight: choice.f("m_fAimAssistAngleWeight"),
            dist_weight: choice.f("m_fAimAssistDistWeight"),
            reduce: choice.array("m_AimAssistDistReduceSettings").iter().map(|r| [r.f("m_fDistanceToReduce") * 0.01, r.f("m_fAimAssistWindowScale")]).collect(),
            blend: top.f("m_fAimAssistBlendTime"),
            groups,
        });
    }
    out
}

/// Corvo's arms (`Twk_Inv_PlayerSpecific`): each weapon's damage by difficulty, from its item
/// attribute tweak (the Normal slot, `m_pAttributeTweak[1]`), as attributes `Sword_MeleeDamage`,
/// `Pistol_RangedDamage`, `Crossbow_RangedDamage`...; and the numbers of the projectiles they
/// fire, the class default's under the tweak's own (`bullet.m_fDamage`, `arrow.m_fDamageMultiplier_Stealth`...).
fn arms(game: &Package, startup: &Package, d: &mut GameData) {
    for (item, key) in [("Twk_Inv_PlayerSpecific.Twk_Inv_SwordCorvo", "Sword"), ("Twk_Inv_PlayerSpecific.Twk_Inv_PistolCorvo", "Pistol"), ("Twk_Inv_PlayerSpecific.Twk_Inv_CrossbowCorvo", "Crossbow")] {
        let Some(p) = startup.find_export(item).and_then(|i| props(startup, i)) else { continue };
        let Some(Value::Object(o)) = p.get_idx("m_pAttributeTweak", 1).cloned() else { continue };
        let Some(a) = props(startup, o) else { continue };
        for n in ["m_MeleeDamage", "m_RangedDamage", "m_RangedHeadshotMultiplier"] {
            let Some(Value::Struct(_, sub)) = a.get(n) else { continue };
            let sub = Props(sub.clone());
            let v = ["m_fBaseValue1_Easy", "m_fBaseValue2_Normal", "m_fBaseValue3_Hard", "m_fBaseValue4_VeryHard"].map(|k| sub.float(k).unwrap_or(0.0));
            d.attributes.insert(format!("{key}_{}", &n[2..]), v);
        }
    }
    // the crossbow's kill cam (`DisTweaks_FireCrossbow`'s `m_KillCamSettings` over its class's) as
    // `killcam.*`: times, scales, distances, the offset and pitch
    let fire = "Twk_Inv_PlayerSpecific.Twk_Inv_CrossbowCorvo.DisTweaks_InventoryItem_PlayerSpecific_1.DisTweaks_FireCrossbow_0";
    let def = ["Default__DisTweaks_ProjectileAttack", "Default__DisTweaks_FireCrossbow"].iter().filter_map(|c| game.find_export(c).and_then(|i| props(game, i))).collect::<Vec<_>>();
    let inst = startup.find_export(fire).and_then(|i| props(startup, i));
    for p in def.iter().chain(inst.iter()) {
        let Some(Value::Struct(_, sub)) = p.get("m_KillCamSettings") else { continue };
        let k = Props(sub.clone());
        for n in ["m_fWitnessKillTime", "m_fFollowFOV", "m_fFollowTimeScale", "m_fEndFollowTimeScale", "m_fTimeOut", "m_fSlowDownDistance", "m_fCameraStopDistance"] {
            if let Some(v) = k.float(n) {
                d.pawn.insert(format!("killcam.{n}"), v);
            }
        }
        if let Some(o) = k.vector("m_Offset") {
            for (i, a) in ["x", "y", "z"].iter().enumerate() {
                d.pawn.insert(format!("killcam.m_Offset.{a}"), o[i]);
            }
        }
        if let Some(r) = k.rotator("m_Rotation") {
            d.pawn.insert("killcam.m_Rotation.pitch".into(), r[0] as f32);
        }
    }
    // the finishers' chance of a beheading (`DisFatalityMoveSet.m_fBeheadRandomChance`, the
    // class's under Corvo's sword's)
    let corvo_fatality = "Twk_Inv_PlayerSpecific.Twk_Inv_SwordCorvo.DisTweaks_InventoryItem_PlayerSpecific_1.DisTweaks_Fatality_1";
    let fat = [game.find_export("Default__DisTweaks_Fatality").and_then(|i| props(game, i)), startup.find_export(corvo_fatality).and_then(|i| props(startup, i))];
    for p in fat.iter().flatten() {
        let Some(Value::Struct(_, sub)) = p.get("m_FatalityMoveSets") else { continue };
        let set = Props(sub.clone());
        for n in ["m_fBeheadRandomChance", "m_fGenericFatalityChance", "m_fSlomoFinisherRandomChance", "m_fMinTimeBetweenRandomSlomoFinisher", "m_fChanceSlomoFinisherWillBeNormalSpeed"] {
            if let Some(v) = set.float(n) {
                d.pawn.insert(format!("fatality.{n}"), v);
            }
        }
    }
    // how many fast finishers, or how long, before a full one (`DisTweaks_Finisher`, the class's
    // under Corvo's sword's)
    for p in [game.find_export("Default__DisTweaks_Finisher").and_then(|i| props(game, i)), startup.find_export(corvo_fatality).and_then(|i| props(startup, i))].iter().flatten() {
        for n in ["m_NumFinishersBeforeSlow_Min", "m_NumFinishersBeforeSlow_Max"] {
            if let Some(v) = p.int(n) {
                d.pawn.insert(format!("finisher.{n}"), v as f32);
            }
        }
        for n in ["m_fTimeBeforeSlow_Min", "m_fTimeBeforeSlow_Max"] {
            if let Some(v) = p.float(n) {
                d.pawn.insert(format!("finisher.{n}"), v);
            }
        }
    }
    // the pistol's and crossbow's aim: their dispersion by difficulty
    // (`DisTweaks_WeaponRanged_Attributes`: `<weapon>.DispersionMin.<difficulty>`...), when it
    // goes to its widest (`DisTweaks_WeaponRanged.m_bMaxDispersionOn*` over the class's) and the
    // kick of a shot (`DisTweaks_ProjectileAttack.m_fCamRecoilOnFire` over the class's)
    let ranged = game.find_export("Default__DisTweaks_WeaponRanged").and_then(|i| props(game, i));
    let attack = game.find_export("Default__DisTweaks_ProjectileAttack").and_then(|i| props(game, i));
    for (key, wep, fire) in [
        ("pistol", "Twk_Inv_PlayerSpecific.Twk_Inv_PistolCorvo", "Twk_Inv_PlayerSpecific.Twk_Inv_PistolCorvo.DisTweaks_InventoryItem_PlayerSpecific_0.DisTweaks_FirePistol_1"),
        ("crossbow", "Twk_Inv_PlayerSpecific.Twk_Inv_CrossbowCorvo", fire),
    ] {
        let w = startup.find_export(wep).and_then(|i| props(startup, i));
        for p in ranged.iter().chain(w.iter()) {
            for n in ["m_bMaxDispersionOnSprint", "m_bMaxDispersionOnJump", "m_bMaxDispersionOnBlock", "m_bMaxDispersionOnMelee"] {
                if let Some(b) = p.bool(n) {
                    d.pawn.insert(format!("{key}.{n}"), b as u8 as f32);
                }
            }
        }
        let attrs = w.as_ref().and_then(|p| p.object("m_pAttributeTweaks")).filter(|o| *o > 0).and_then(|o| props(startup, o));
        for n in ["DispersionMin", "DispersionMax", "DispersionInterpolationSpeed"] {
            let Some(Value::Struct(_, sub)) = attrs.as_ref().and_then(|a| a.get(&format!("m_{n}"))) else { continue };
            let a = Props(sub.clone());
            for (i, f) in ["m_fBaseValue1_Easy", "m_fBaseValue2_Normal", "m_fBaseValue3_Hard", "m_fBaseValue4_VeryHard"].iter().enumerate() {
                if let Some(v) = a.float(f) {
                    d.pawn.insert(format!("{key}.{n}.{i}"), v);
                }
            }
        }
        let f = startup.find_export(fire).and_then(|i| props(startup, i));
        for p in attack.iter().chain(f.iter()) {
            if let Some(v) = p.float("m_fArrowFireSpeed") {
                d.pawn.insert(format!("{key}.m_fArrowFireSpeed"), v);
            }
            if let Some(v) = p.float("m_fCamRecoilOnFire") {
                d.pawn.insert(format!("{key}.m_fCamRecoilOnFire"), v);
            }
        }
    }
    for (path, class, key) in [
        ("Twk_Projectiles.Twk_Proj_BulletUpgraded", "Default__DisTweaks_Bullet", "bullet."),
        ("Twk_Projectiles.Twk_Proj_Arrow", "Default__DisTweaks_Arrow", "arrow."),
        ("Twk_Projectiles.Twk_Proj_Arrow_Sleep", "Default__DisTweaks_Arrow", "arrow_sleep."),
        ("Twk_Projectiles.Twk_Proj_Arrow_Flare", "Default__DisTweaks_Arrow_Flare", "arrow_flare."),
    ] {
        let def = game.find_export(class).and_then(|i| props(game, i));
        let obj = startup.find_export(path).and_then(|i| props(startup, i));
        for (pkg, p) in [(game, def), (startup, obj)] {
            let Some(p) = p else { continue };
            let top: Vec<upk::props::Prop> = p.0.iter().filter(|p| matches!(p.value, Value::Float(..) | Value::Int(..) | Value::Bool(..))).cloned().collect();
            flatten(pkg, &top, key, &mut d.pawn, &mut BTreeMap::new());
        }
    }
}

/// The audiographs: the cards (`AudioGraph_Twk_SF`: their `m_AudioLogName`, names and words)
/// and the recordings (Startup's `Dlg_AudioGraphs`: each `DisConv_Hook_PlayAudioLog`'s
/// `m_AudioLogNames[i]` plays its `m_OutputLinks[i]` blurb and those after it; a blurb's
/// `m_iBlurbGUID` names its Wwise event in its speaker's voice data).
fn audiographs(cooked: &Path, startup: &Package, d: &mut GameData) {
    let guid = |v: Option<&Value>| match v {
        Some(Value::Guid(g)) => Some(*g),
        _ => None,
    };
    // the blurbs' events
    let mut events: BTreeMap<[u32; 4], String> = BTreeMap::new();
    for v in startup.exports_of_class("DisDialogVoiceData").collect::<Vec<_>>() {
        if !startup.obj_path(v).starts_with("Dlg_AudioGraphs.") {
            continue;
        }
        let Some(p) = props(startup, v) else { continue };
        for vd in struct_array(startup, &p, "m_VoiceData") {
            for b in struct_array(startup, &vd, "m_Blurbs") {
                if let (Some(g), Some(Value::Str(e))) = (guid(b.get("m_GUID")), b.get("m_AkEventName")) {
                    events.insert(g, e.clone());
                }
            }
        }
    }
    // the recordings by log name
    let mut logs: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for h in startup.exports_of_class("DisConv_Hook_PlayAudioLog").collect::<Vec<_>>() {
        let Some(p) = props(startup, h) else { continue };
        let names: Vec<String> = match p.array("m_AudioLogNames") {
            Some((count, offset, size)) if size >= count * 8 => (0..count)
                .map(|k| {
                    let at = offset + k * 8;
                    let i = i32::from_le_bytes(startup.data[at..at + 4].try_into().unwrap());
                    let n = i32::from_le_bytes(startup.data[at + 4..at + 8].try_into().unwrap());
                    startup.name_of(i, n)
                })
                .collect(),
            _ => continue,
        };
        let links: Vec<i32> = struct_array(startup, &p, "m_OutputLinks").iter().map(|l| l.object("m_pLink").unwrap_or(0)).collect();
        for (name, first) in names.iter().zip(links) {
            let mut lines = Vec::new();
            let mut at = first;
            while at > 0 && lines.len() < 16 && startup.class_name(at) == "DisConv_Blurb" {
                let Some(b) = props(startup, at) else { break };
                if let Some(e) = guid(b.get("m_iBlurbGUID")).and_then(|g| events.get(&g)) {
                    lines.push((e.clone(), s(&b, "m_Text")));
                }
                at = struct_array(startup, &b, "m_OutputLinks").first().and_then(|l| l.object("m_pLink")).unwrap_or(0);
            }
            logs.insert(name.clone(), lines);
        }
    }
    // the cards
    let Ok(sf) = Package::open(&cooked.join("AudioGraph_Twk_SF.upk")) else { return };
    for i in sf.exports_of_class("DisAbstractItemAudioLog").collect::<Vec<_>>() {
        let Some(p) = props(&sf, i) else { continue };
        let path = sf.obj_path(i);
        let key = path.split_once('.').map(|x| x.1.to_string()).unwrap_or(path);
        let log = p.name("m_AudioLogName").unwrap_or_default().to_string();
        let lines = logs.get(&log).cloned().unwrap_or_default();
        d.audiographs.insert(key, crate::format::Audiograph { log, title: s(&p, "m_ItemName"), description: s(&p, "m_Description"), lines });
    }
    log::info!("gamedata: {} audiographs ({} with recordings)", d.audiographs.len(), d.audiographs.values().filter(|a| !a.lines.is_empty()).count());
}

fn charms(startup: &Package) -> Vec<CharmDef> {
    let Some(idx) = startup.find_export("Twk_Pawn_Corvo.Twk_Pawn_Corvo_Release.pCharmListTweaks") else { return Vec::new() };
    let Some(p) = props(startup, idx) else { return Vec::new() };
    struct_array(startup, &p, "m_Charms")
        .iter()
        .map(|c| CharmDef {
            attribute: s(c, "m_AttributeName").trim_start_matches("Attribute_").to_string(),
            levels: struct_array(startup, c, "m_Levels").iter().map(|l| (s(l, "m_Name"), s(l, "m_Text"), l.float("m_fValue").unwrap_or(0.0))).collect(),
        })
        .collect()
}

fn upgrades(startup: &Package) -> Vec<UpgradeDef> {
    startup
        .exports_of_class("DisTweaks_Upgrade")
        .filter_map(|i| {
            let p = props(startup, i)?;
            Some(UpgradeDef {
                id: object_name(startup, i),
                name: s(&p, "m_Name"),
                description: s(&p, "m_Description"),
                icon: s(&p, "m_JournalIconName"),
                attributes: struct_array(startup, &p, "m_Attributes")
                    .iter()
                    .map(|a| (s(a, "m_AttributeName").trim_start_matches("Attribute_").to_string(), a.float("m_fValue").unwrap_or(0.0)))
                    .collect(),
                replaces: i32_array(startup, &p, "m_RevertedUpgrades").into_iter().filter(|o| *o != 0).map(|o| object_name(startup, o)).collect(),
            })
        })
        .collect()
}

fn stores(cooked: &Path) -> Vec<StoreDef> {
    let mut out: Vec<StoreDef> = Vec::new();
    for name in ["L_Pub_Craftsman", "L_Pub_FromPrison_Script", "L_Streets1_Script", "L_Streets2_Script"] {
        let Ok(pkg) = Package::open(&cooked.join(format!("{name}.upk"))) else { continue };
        for i in pkg.exports_of_class("DisTweaks_Store").collect::<Vec<_>>() {
            let id = object_name(&pkg, i);
            if out.iter().any(|s| s.id == id) {
                continue;
            }
            let Some(p) = props(&pkg, i) else { continue };
            let items = struct_array(&pkg, &p, "m_StoreItems");
            if items.is_empty() {
                continue;
            }
            let sections = match p.array("m_StoreSubSections") {
                Some((count, offset, size)) => {
                    let mut r = upk::Reader::at(&pkg.data[..offset + size], offset);
                    (0..count).filter_map(|_| r.fstring().ok()).collect()
                }
                None => Vec::new(),
            };
            out.push(StoreDef {
                id,
                sections,
                items: items
                    .iter()
                    .map(|it| StoreItem {
                        name: s(it, "m_Name"),
                        description: s(it, "m_Description"),
                        icon: s(it, "m_StoreIconName"),
                        section: it.int("m_SubSectionIndex").unwrap_or(0),
                        coins: struct_array(&pkg, it, "m_Cost").iter().map(|c| c.int("m_Quantity").unwrap_or(0).max(0) as u32).sum(),
                        item: it.object("m_Item").filter(|o| *o != 0).map(|o| object_name(&pkg, o)).unwrap_or_default(),
                        requires: i32_array(&pkg, it, "m_Prerequisites").into_iter().filter(|o| *o != 0).map(|o| object_name(&pkg, o)).collect(),
                        quantity: it.object("m_Item").filter(|o| *o != 0).and_then(|o| pickup_quantity(&pkg, o)).unwrap_or(1),
                    })
                    .collect(),
            });
        }
    }
    out
}

/// The game's interface strings, loading hints and map names (UTF-16 INI).
fn localization(cooked: &Path, d: &mut GameData) {
    // the interface tweaks' strings: the HUD's (`m_InteractionTexts[i]`...) as `HUD.<key>`, the
    // others' by their tweak (`Twk_GFxMoviePlayerMenuBase.<key>`)
    if let Ok(raw) = std::fs::read(cooked.join("..").join("Localization").join("INT").join("Twk_InGameUI.int")) {
        let mut prefix = String::new();
        for line in decode_int(&raw).lines() {
            let line = line.trim();
            if line.starts_with('[') && line.ends_with(']') {
                let tweak = line[1..line.len() - 1].split_whitespace().next().unwrap_or("");
                prefix = if tweak == "Twk_GFxMoviePlayerHUD" { "HUD".into() } else { tweak.to_string() };
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            let v = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(v).to_string();
            d.texts.insert(format!("{prefix}.{}", k.trim()), v);
        }
    }
    // Corvo's pawn tweak's strings (`m_HeartTargetTutorialMessage`...) as `Pawn.<key>` (its
    // first section, `Twk_Pawn_Corvo`)
    if let Ok(raw) = std::fs::read(cooked.join("..").join("Localization").join("INT").join("Twk_Pawn_Corvo.int")) {
        let mut sections = 0;
        for line in decode_int(&raw).lines() {
            let line = line.trim();
            if line.starts_with('[') {
                sections += 1;
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            let v = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(v);
            if sections == 1 && !v.is_empty() {
                d.texts.insert(format!("Pawn.{}", k.trim()), v.to_string());
            }
        }
    }
    // the options' words (`Settings.int`: the settings, their values, the keys' names) as
    // `Settings.<section>.<key>`; the powers', ammunition's and keys' (`RPG.int`: each power's
    // level descriptions and mana consumption, `DisUISelectionType`) as `RPG.<section>.<key>`
    for (file, prefix) in [("Settings.int", "Settings"), ("RPG.int", "RPG")] {
        let Ok(raw) = std::fs::read(cooked.join("..").join("Localization").join("INT").join(file)) else { continue };
        let mut section = String::new();
        for line in decode_int(&raw).lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(';') {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                section = line[1..line.len() - 1].to_string();
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let v = v.trim();
                let v = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(v);
                // (`\r`: a line break)
                d.texts.insert(format!("{prefix}.{section}.{}", k.trim()), v.replace("\\r", "\n").trim().to_string());
            }
        }
    }
    let path = cooked.join("..").join("Localization").join("INT").join("DishonoredGame.int");
    let Ok(raw) = std::fs::read(&path) else {
        log::warn!("gamedata: no {}", path.display());
        return;
    };
    let text = if raw.starts_with(&[0xff, 0xfe]) {
        let u: Vec<u16> = raw[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&u)
    } else {
        String::from_utf8_lossy(&raw).into_owned()
    };
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].to_string();
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        let v = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(v).to_string();
        if section == "MapNames" {
            d.map_names.insert(k.trim().to_string(), v);
        } else if let Some(set) = section.strip_prefix("DisBinkOverlayManager_HintSet_") {
            if k.trim() == "Hint" {
                d.hints.entry(set.to_string()).or_default().push(v);
            }
        } else if section.ends_with("_Texts") || section == "Misc" || section == "DisTweaks_Possessable" {
            d.texts.insert(format!("{section}.{}", k.trim()), v);
        }
    }
}

/// Where the journal shows the abstract items (`DisAbstractItem`'s `m_JournalDisplaySection`,
/// `DJIS_Mission` unless set: Mission Items) and their pictures (`m_JournalIconName`), read from
/// the packages the localisation names for them (`<package>_SF.upk`), keyed as their texts are
/// (`abstract_items.AbsInv_CarryingCampbell`, `HolgersEye_AbsItm`).
fn journal_items(cooked: &Path, d: &mut GameData) {
    let dir = cooked.join("..").join("Localization").join("INT");
    let Ok(rd) = std::fs::read_dir(&dir) else { return };
    let mut names: Vec<String> = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        let Some(stem) = p.file_stem().and_then(|n| n.to_str()).map(|s| s.to_string()) else { continue };
        if stem.starts_with("DLC") || !p.extension().is_some_and(|x| x.eq_ignore_ascii_case("int")) {
            continue;
        }
        if read_int(&p).is_some_and(|t| t.contains(" DisAbstractItem]")) {
            names.push(stem);
        }
    }
    names.sort();
    for n in names {
        let Some(path) = [format!("{n}_SF.upk"), format!("{n}.upk")].into_iter().map(|f| cooked.join(f)).find(|p| p.exists()) else { continue };
        let Ok(pkg) = Package::open(&path) else { continue };
        for i in 1..=pkg.exports.len() as i32 {
            let class = pkg.class_name(i);
            if !class.starts_with("DisAbstractItem") {
                continue;
            }
            let Some(p) = props(&pkg, i) else { continue };
            let full = pkg.obj_path(i);
            let Some((_, key)) = full.split_once('.') else { continue };
            let default = if class == "DisAbstractItem" { "DJIS_Mission" } else { "" };
            let section = p.name("m_JournalDisplaySection").unwrap_or(default).to_string();
            let icon = s(&p, "m_JournalIconName");
            // (one left at the class's section with no picture, the tutorials' notes, is read)
            if p.get("m_JournalDisplaySection").is_none() && icon.is_empty() {
                continue;
            }
            d.journal_items.insert(key.to_string(), (section, icon));
        }
    }
    log::info!("gamedata: {} journal items ({} mission items)", d.journal_items.len(), d.journal_items.values().filter(|v| v.0 == "DJIS_Mission").count());
}

/// A localisation file's text (UTF-16 with its mark, else UTF-8).
fn decode_int(raw: &[u8]) -> String {
    if raw.starts_with(&[0xff, 0xfe]) {
        let u: Vec<u16> = raw[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&u)
    } else {
        String::from_utf8_lossy(raw).into_owned()
    }
}

/// How much a pickup tweak gives (`m_Quantity` / `m_nAmount`), local or imported.
fn pickup_quantity(pkg: &Package, o: i32) -> Option<u32> {
    let p = if o > 0 { props(pkg, o) } else { None }?;
    ["m_Quantity", "m_nQuantity", "m_Amount", "m_nAmount", "m_iQuantity"].iter().find_map(|k| p.int(k)).map(|v| v.max(1) as u32)
}

/// A dialogue tree's graph (the Heart's `Dlg_HeartGadget`), with each line's subtitle and
/// voice event.
fn conv_graph(pkg: &std::sync::Arc<Package>, tree: &str) -> crate::format::ConvGraph {
    use crate::format::{ConvGraph, ConvNode};
    let prefix = format!("{tree}.");
    let ids: Vec<i32> = (1..=pkg.exports.len() as i32).filter(|&i| pkg.class_name(i).starts_with("DisConv_") && pkg.obj_path(i).starts_with(&prefix)).collect();
    let index: std::collections::HashMap<i32, i32> = ids.iter().enumerate().map(|(k, &i)| (i, k as i32)).collect();
    let voices = crate::kismet::voice_events(std::slice::from_ref(pkg));
    let texts = crate::kismet::blurb_texts(std::slice::from_ref(pkg));
    let mut g = ConvGraph::default();
    for &i in &ids {
        let cls = pkg.class_name(i);
        let p = props(pkg, i).unwrap_or_default();
        let mut n = ConvNode { class: cls.trim_start_matches("DisConv_").to_string(), ..Default::default() };
        n.outputs = struct_array(pkg, &p, "m_OutputLinks").iter().map(|l| l.object("m_pLink").and_then(|o| index.get(&o).copied()).unwrap_or(-1)).collect();
        n.looping = p.bool("m_bLooping").unwrap_or(false);
        n.label = s(&p, "m_Label");
        match n.class.as_str() {
            "RandomBranch" => {
                if let Some((count, offset, size)) = p.array("m_Branches") {
                    if size >= count * 4 {
                        n.weights = (0..count).map(|k| f32::from_le_bytes(pkg.data[offset + k * 4..offset + k * 4 + 4].try_into().unwrap())).collect();
                    }
                }
            }
            "SpeakerInStoryGroup" => {
                n.groups = struct_array(pkg, &p, "m_Branches")
                    .iter()
                    .map(|b| (b.object("m_pStoryGroup").filter(|o| *o != 0).map(|o| object_name(pkg, o)).unwrap_or_default(), b.bool("m_bExactMatch").unwrap_or(false)))
                    .collect();
            }
            "CheckStoryFlag" => {
                if let Some(Value::Guid(g)) = p.get("m_StoryFlag") {
                    n.flag = format!("{:08x}{:08x}{:08x}{:08x}", g[0], g[1], g[2], g[3]);
                }
            }
            c if c.starts_with("Hook") || c == "DialogHook" => {
                let hook = s(&p, "m_HookEnum").trim_start_matches("DDH_").to_string();
                if !hook.is_empty() {
                    g.hooks.insert(hook, g.nodes.len() as u32);
                }
            }
            _ => {}
        }
        if let Some(Value::Guid(guid)) = p.get("m_iBlurbGUID") {
            n.text = texts.get(guid).cloned().unwrap_or_else(|| s(&p, "m_Text"));
            n.event = voices.get(guid).and_then(|v| v.first()).cloned().unwrap_or_default();
        }
        g.nodes.push(n);
    }
    g
}

/// Cook `game/gamedata.json`.
/// How far the AI hear each noise loudness (metres): `DefaultAI.ini`'s
/// `[DishonoredGame.DisAINoiseManager] m_LoudnessDistanceArray`, one per `EAINoiseLoudness` in
/// the enum's order (whisper 3 m, the numbered levels 5, 15, 30 and 60 m; roshan's r614/blink
/// research pointed to the table, where we had guessed it native).
pub fn noise_loudness(cooked: &Path, game: &Package) -> BTreeMap<String, f32> {
    let names = enum_names(game, "EAINoiseLoudness");
    let Ok(ini) = std::fs::read_to_string(cooked.join("..").join("Config").join("DefaultAI.ini")) else { return BTreeMap::new() };
    let mut section = false;
    let mut dists = Vec::new();
    for line in ini.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line.eq_ignore_ascii_case("[DishonoredGame.DisAINoiseManager]");
            continue;
        }
        if let Some(v) = line.strip_prefix("m_LoudnessDistanceArray=").filter(|_| section) {
            dists.push(v.trim().parse::<f32>().unwrap_or(0.0) * 0.01);
        }
    }
    names.into_iter().zip(dists).collect()
}

/// The native hook table's rows (`eDisDialogHook` value, fire group, refire min, max, flags)
/// where they differ from the common one (Casual, 1 s, 1 s, 0x80): built in code by the
/// executable (`FUN_00c8d820`), as roshan's r614/blink research read it.
const HOOK_ROWS: &[(u8, u8, f32, f32, u32)] = &[
    (1, 12, 0.0, 0.0, 0x4000), (2, 12, 0.0, 0.0, 0), (3, 12, 1.0, 1.0, 0x2000), (4, 0, 1.0, 1.0, 0),
    (5, 0, 1.0, 1.0, 0x2000), (6, 0, 1.0, 1.0, 0x10), (7, 0, 1.0, 1.0, 0x2010), (8, 0, 1.0, 1.0, 0),
    (9, 0, 1.0, 1.0, 0), (10, 12, 0.0, 0.0, 0x500), (13, 0, 1.0, 1.0, 0xc0), (14, 0, 1.0, 1.0, 0xc0),
    (15, 1, 1.0, 1.0, 0x80), (16, 7, 1.0, 1.0, 0x80), (17, 8, 1.0, 1.0, 0x80), (18, 8, 1.0, 1.0, 0x80),
    (19, 8, 1.0, 1.0, 0x80), (20, 6, 1.0, 1.0, 0x80), (21, 5, 1.0, 1.0, 0x80), (22, 1, 1.0, 1.0, 0x80),
    (23, 5, 1.0, 1.0, 0x80), (24, 1, 1.0, 1.0, 0x80), (25, 6, 1.0, 1.0, 0x82), (26, 5, 1.0, 1.0, 0x80),
    (27, 5, 1.0, 1.0, 0x80), (28, 5, 1.0, 1.0, 0x80), (29, 5, 1.0, 1.0, 0x80), (30, 5, 1.0, 1.0, 0x80),
    (31, 5, 1.0, 1.0, 0xa0), (32, 5, 1.0, 1.0, 0xa0), (33, 5, 1.0, 1.0, 0x80), (34, 5, 1.0, 1.0, 0x88),
    (35, 5, 1.0, 1.0, 0x80), (36, 5, 1.0, 1.0, 0x80), (37, 1, 1.0, 1.0, 0x80), (38, 1, 1.0, 1.0, 0x80),
    (39, 1, 1.0, 1.0, 0x80), (40, 8, 1.0, 1.0, 0x88), (41, 8, 1.0, 1.0, 0x88), (42, 11, 1.0, 1.0, 0x88),
    (43, 8, 1.0, 1.0, 0x88), (44, 7, 1.0, 1.0, 0x80), (45, 7, 1.0, 1.0, 0x80), (46, 7, 8.0, 8.0, 0x80),
    (47, 10, 1.0, 1.0, 0x88), (48, 12, 1.0, 1.0, 0x88), (49, 5, 1.0, 1.0, 0x80), (50, 5, 1.0, 1.0, 0x80),
    (51, 7, 1.0, 1.0, 0x88), (52, 6, 1.0, 1.0, 0x88), (53, 6, 1.0, 1.0, 0x88), (54, 6, 1.0, 1.0, 0x88),
    (55, 6, 1.0, 1.0, 0x8a), (56, 6, 1.0, 1.0, 0x89), (57, 4, 0.0, 0.0, 0x8e), (58, 4, 0.0, 0.0, 0x86),
    (59, 4, 0.0, 0.0, 0x86), (60, 4, 0.0, 0.0, 0x8a), (61, 4, 0.0, 0.0, 0x8a), (62, 4, 1.0, 1.0, 0x8e),
    (63, 7, 1.0, 1.0, 0x80), (64, 10, 1.0, 1.0, 0x88), (65, 7, 8.0, 8.0, 0x80), (66, 10, 1.0, 1.0, 0x88),
    (67, 10, 1.0, 1.0, 0xc8), (68, 5, 1.0, 1.0, 0x80), (69, 9, 1.0, 1.0, 0x80), (70, 9, 1.0, 1.0, 0x80),
    (71, 3, 1.0, 1.0, 0x80), (72, 12, 1.0, 1.0, 0x80), (73, 12, 1.0, 1.0, 0x80), (74, 3, 1.0, 1.0, 0x80),
    (75, 3, 1.0, 1.0, 0x80), (76, 3, 1.0, 1.0, 0x80), (77, 3, 1.0, 1.0, 0x80), (78, 3, 1.0, 1.0, 0x80),
    (79, 3, 1.0, 1.0, 0x80), (80, 3, 1.0, 1.0, 0x80), (81, 3, 1.0, 1.0, 0x8a), (82, 2, 1.0, 1.0, 0x80),
    (83, 12, 1.0, 1.0, 0x88), (84, 12, 1.0, 1.0, 0x88), (85, 7, 1.0, 1.0, 0x88), (86, 7, 1.0, 1.0, 0x88),
    (87, 8, 1.0, 1.0, 0x88), (88, 8, 1.0, 1.0, 0x88), (89, 6, 1.0, 1.0, 0x89), (90, 8, 1.0, 1.0, 0x88),
    (91, 8, 1.0, 1.0, 0x88), (92, 8, 1.0, 1.0, 0x88), (93, 8, 1.0, 1.0, 0x88), (94, 8, 1.0, 1.0, 0x88),
    (95, 8, 1.0, 1.0, 0x88), (96, 8, 1.0, 1.0, 0x88), (99, 6, 1.0, 1.0, 0x88), (100, 6, 1.0, 1.0, 0x88),
    (101, 6, 1.0, 1.0, 0x88), (102, 6, 1.0, 1.0, 0x88), (103, 6, 1.0, 1.0, 0x88), (104, 6, 1.0, 1.0, 0x88),
    (105, 6, 1.0, 1.0, 0x88), (106, 6, 1.0, 1.0, 0x88), (107, 2, 1.0, 1.0, 0x80), (108, 2, 1.0, 1.0, 0x1080),
    (109, 2, 1.0, 1.0, 0x80), (110, 2, 1.0, 1.0, 0xa0), (111, 2, 1.0, 1.0, 0xa0), (112, 2, 1.0, 1.0, 0x80),
    (113, 2, 1.0, 1.0, 0x80), (114, 2, 1.0, 1.0, 0x80), (115, 6, 1.0, 1.0, 0x88), (116, 6, 1.0, 1.0, 0x80),
    (117, 3, 1.0, 1.0, 0x80), (118, 12, 0.0, 0.0, 0x300), (119, 12, 0.0, 0.0, 0xc04), (120, 12, 0.0, 0.0, 0xc04),
    (121, 12, 0.0, 0.0, 0xc04), (122, 7, 1.0, 1.0, 0x86), (123, 7, 1.0, 1.0, 0x86), (124, 7, 1.0, 1.0, 0x86),
    (125, 7, 1.0, 1.0, 0x86),
];

/// A force feedback waveform's samples (`ForceFeedbackWaveform.Samples`).
fn waveform(pkg: &Package, idx: i32) -> Vec<RumbleSample> {
    let Some(p) = props(pkg, idx) else { return Vec::new() };
    let Some((c, off, sz)) = p.array("Samples") else { return Vec::new() };
    let shape = |s: &Props, n: &str| match s.name(n).unwrap_or("WF_Constant") {
        "WF_LinearIncreasing" => 1,
        "WF_LinearDecreasing" => 2,
        "WF_Sin0to90" => 3,
        "WF_Sin90to180" => 4,
        "WF_Sin0to180" => 5,
        "WF_Noise" => 6,
        _ => 0,
    };
    upk::props::parse_struct_array(pkg, off, sz, c)
        .unwrap_or_default()
        .iter()
        .map(|s| RumbleSample { left: s.byte("LeftAmplitude").unwrap_or(0), right: s.byte("RightAmplitude").unwrap_or(0), left_fn: shape(s, "LeftFunction"), right_fn: shape(s, "RightFunction"), duration: s.float("Duration").unwrap_or(0.0) })
        .collect()
}

/// The pad's rumbles: the active powers' (`m_RumbleAndShake`, by level), the Heart's beats, the
/// refused power's, the contact system's (`DisContactSub_Rumble`: for the striker, the struck,
/// or all within a radius), and those Corvo's clips play (`AnimNotify_Rumble` in the `Ply_*`
/// sets).
fn rumbles(game: &Package, startup: &Package, d: &mut GameData) {
    let mut radius = BTreeMap::new();
    let mut put = |key: String, pkg: &Package, obj: Option<i32>| {
        if let Some(o) = obj.filter(|o| *o > 0) {
            let w = waveform(pkg, o);
            if !w.is_empty() {
                d.rumbles.insert(key, w);
            }
        }
    };
    for (name, path, _, _) in ACTIVES {
        let Some(p) = game.find_export(path).and_then(|i| props(game, i)) else { continue };
        for level in 0..2 {
            let w = match p.get_idx("m_RumbleAndShake", level) {
                Some(Value::Struct(_, s)) => Props(s.clone()).object("m_pRumbleCustomWaveForm"),
                _ => None,
            };
            put(format!("power.{name}.{level}"), game, w);
        }
    }
    if let Some(p) = startup.find_export("Twk_Inv_HeartBase.Twk_Inv_HeartBase").and_then(|i| props(startup, i)) {
        for (k, s) in struct_array(startup, &p, "m_Heartbeats").iter().enumerate() {
            put(format!("heart.beat.{k}"), startup, s.object("m_pRumbleWaveform"));
        }
        put("heart.unequipped".into(), startup, p.object("m_pUnequippedHeartBeatsRumbleWaveform"));
    }
    if let Some(p) = startup.find_export("Twk_Pawn_DefaultPlayer.Twk_Pawn_DefaultPlayer").and_then(|i| props(startup, i)) {
        put("player.disabled_power".into(), startup, p.object("m_pDisabledPowerForceFeedback"));
    }
    // (the wheel's each item pointed at)
    if let Some(p) = game.find_export("Twk_InGameUI.Twk_GFxMoviePlayerPowerWheel").and_then(|i| props(game, i)) {
        put("wheel.select".into(), game, p.object("m_pSelectionForceFeedback"));
    }
    // the contact system's intersections with a rumble
    if let Some(p) = game.find_export("Phy_ContactSystem.ContactSystem.Dis_ContactSystem").and_then(|i| props(game, i)) {
        let kind = |o: Option<i32>| o.filter(|o| *o != 0).map(|o| game.obj_name(o).trim_start_matches("DisContactType_").to_string());
        for row in struct_array(game, &p, "m_Intersections") {
            let (Some(striker), Some(struck)) = (kind(row.object("m_pStriker")), kind(row.object("m_pStruck"))) else { continue };
            for sub in struct_array(game, &row, "m_SubIntersections") {
                let Some(o) = sub.object("m_pSubIntersectObj").filter(|o| *o > 0) else { continue };
                if game.class_name(o) != "DisContactSub_Rumble" {
                    continue;
                }
                let Some(r) = props(game, o) else { continue };
                let base = format!("contact.{striker}.{struck}");
                put(format!("{base}.striker"), game, r.struct_props("m_RumbleForStriker").and_then(|s| s.object("m_pCustomWaveForm")));
                put(format!("{base}.struck"), game, r.struct_props("m_RumbleForStruck").and_then(|s| s.object("m_pCustomWaveForm")));
                if r.bool("m_bDoRumbleForRadius") == Some(true) {
                    if let Some(s) = r.struct_props("m_RumbleForRadius") {
                        put(format!("{base}.radius"), game, s.object("m_pCustomWaveForm"));
                        radius.insert(format!("{base}.radius"), s.float("m_fProbeRadius").unwrap_or(0.0));
                    }
                }
            }
        }
    }
    d.rumble_radius = radius;
    // Corvo's clips': each `Ply_*` set's sequences' rumble notifies
    for i in startup.exports_of_class("AnimSequence").collect::<Vec<_>>() {
        let path = startup.obj_path(i);
        if !path.starts_with("Ply_") {
            continue;
        }
        let Some(p) = props(startup, i) else { continue };
        let Some((c, off, sz)) = p.array("Notifies") else { continue };
        let clip = p.name("SequenceName").unwrap_or_default().to_string();
        let set = path.split('.').next().unwrap_or_default().to_string();
        for n in upk::props::parse_struct_array(startup, off, sz, c).unwrap_or_default() {
            let Some(no) = n.object("Notify").filter(|o| *o > 0) else { continue };
            if startup.class_name(no) != "AnimNotify_Rumble" {
                continue;
            }
            let Some(w) = props(startup, no).and_then(|np| np.object("WaveForm")).filter(|o| *o > 0) else { continue };
            let wave = waveform(startup, w);
            if !wave.is_empty() {
                d.anim_rumbles.push(AnimRumble { set: set.clone(), clip: clip.clone(), time: n.float("Time").unwrap_or(0.0), wave });
            }
        }
    }
}

/// Each player item's arm pitch keys (`DisTweaks_InventoryItem_PlayerSpecific` and the empty
/// hands' `DisTweaks_ItemEmpty_PlayerSpecific`: `m_ArmPitch_OffsetKeys`), by the item's tweak.
fn arm_pitch(startup: &Package) -> BTreeMap<String, Vec<[f32; 2]>> {
    let mut out = BTreeMap::new();
    for class in ["DisTweaks_InventoryItem_PlayerSpecific", "DisTweaks_ItemEmpty_PlayerSpecific"] {
        for i in startup.exports_of_class(class).collect::<Vec<_>>() {
            let Some(p) = props(startup, i) else { continue };
            let Some((c, off, sz)) = p.array("m_ArmPitch_OffsetKeys") else { continue };
            let keys: Vec<[f32; 2]> = upk::props::parse_struct_array(startup, off, sz, c)
                .unwrap_or_default()
                .iter()
                .map(|k| {
                    let f = |n: &str| match k.get(n) {
                        Some(Value::Float(v)) => *v,
                        _ => 0.0,
                    };
                    [f("m_fArmPitch"), f("m_fArmPitch_Difference")]
                })
                .collect();
            let path = startup.obj_path(i);
            let Some(item) = path.split('.').nth(1) else { continue };
            if !keys.is_empty() {
                out.insert(item.to_string(), keys);
            }
        }
    }
    out
}

/// The barks' manager (`Default__DisTweaks_ConvSettings` over the native hook table).
fn bark_rules(game: &Package) -> BarkRules {
    let hooks = enum_names(game, "eDisDialogHook");
    let groups = enum_names(game, "eDisDialogHookFireGroup");
    let group_of = |n: &str| groups.iter().position(|g| g == n).map(|i| i as u8);
    let mut r = BarkRules {
        groups: groups.iter().map(|g| BarkGroup { name: g.trim_start_matches("eDisDialogHookFireGroup_").to_string(), ..Default::default() }).collect(),
        ..Default::default()
    };
    for (i, h) in hooks.iter().enumerate() {
        let Some(name) = h.strip_prefix("DDH_").filter(|n| *n != "NOT_SET" && *n != "INVALID") else { continue };
        let (_, group, lo, hi, flags) = HOOK_ROWS.iter().copied().find(|row| row.0 as usize == i).unwrap_or((0, 0, 1.0, 1.0, 0x80));
        r.hooks.insert(name.to_string(), BarkHookRule { group, refire: [lo, hi], flags });
    }
    let Some(p) = game.find_export("Default__DisTweaks_ConvSettings").and_then(|i| props(game, i)) else { return r };
    r.at_once = [p.int("m_MaxDialogHooksAtOnce").unwrap_or(5), p.int("m_MaxDialogHooksAtOnce_Quiet").unwrap_or(5)];
    r.max_distance = p.float("m_fMaxDialogDistance").unwrap_or(5000.0) * 0.01;
    r.loud_distance = p.float("m_fDialogDistanceConsideredLoud").unwrap_or(2000.0) * 0.01;
    r.weight_rand = p.float("m_fHookWeightRandRange").unwrap_or(0.2);
    let structs = |n: &str| p.array(n).and_then(|(c, off, sz)| upk::props::parse_struct_array(game, off, sz, c).ok()).unwrap_or_default();
    let int = |s: &Props, n: &str| match s.get(n) {
        Some(Value::Int(i)) => *i,
        _ => 0,
    };
    let float = |s: &Props, n: &str| match s.get(n) {
        Some(Value::Float(f)) => *f,
        _ => 0.0,
    };
    let en = |s: &Props, n: &str| match s.get(n) {
        Some(Value::Enum(e)) => e.clone(),
        _ => String::new(),
    };
    for s in structs("m_FireGroupConfig") {
        let Some(g) = group_of(&en(&s, "m_FireGroup")).and_then(|g| r.groups.get_mut(g as usize)) else { continue };
        g.per_group = [int(&s, "m_MaxPlaying_PerGroup"), int(&s, "m_MaxPlaying_PerGroup_Quiet")];
        g.per_initiator = [int(&s, "m_MaxPlaying_PerInitiator"), int(&s, "m_MaxPlaying_PerInitiator_Quiet")];
        g.ignore_global = matches!(s.get("m_bIgnoreGlobalCap"), Some(Value::Bool(true)));
        g.min_between = float(&s, "m_fMinTimeBetweenFiring");
    }
    for s in structs("m_DialogHookOverrides") {
        let hook = en(&s, "m_DialogHookToOverride");
        let Some(h) = hook.strip_prefix("DDH_").and_then(|n| r.hooks.get_mut(n)) else { continue };
        h.refire = [float(&s, "m_fRefire_DelayMin"), float(&s, "m_fRefire_DelayMax")];
        if let Some(g) = group_of(&en(&s, "m_FireGroup")) {
            h.group = g;
        }
    }
    r
}

pub fn cook(cooked: &Path, root: &Path) -> Result<GameData> {
    let game = Package::open(&cooked.join("DishonoredGame.upk")).context("DishonoredGame.upk")?;
    let startup = Package::open(&cooked.join("Startup.upk")).context("Startup.upk")?;
    let mut d = GameData { actives: actives(&game), passives: passives(&game), attributes: attributes(&game, &startup), charms: charms(&startup), upgrades: upgrades(&startup), stores: stores(cooked), ..Default::default() };
    if let Ok(engine) = Package::open(&cooked.join("Engine.upk")) {
        if let Some(gravity) = engine.find_export("Default__WorldInfo").and_then(|i| props(&engine, i)).and_then(|p| p.float("DefaultGravityZ")) {
            d.pawn.insert("world.DefaultGravityZ".into(), gravity);
        }
    }
    d.noise_loudness = noise_loudness(cooked, &game);
    arms(&game, &startup, &mut d);
    d.aim_assist = aim_assist(&game, &startup);
    // the crosshair's slide onto the aim assist's target (`Twk_GFxMoviePlayerHUD`'s
    // `m_fCrosshairInterpolationSpeed`) and the aim's friction (`Twk_Pawn_PlayerInput`: the look's
    // slowing across and up, and whether zoomed or not; the class default's under Corvo's)
    if let Some(v) = game.find_export("Twk_InGameUI.Twk_GFxMoviePlayerHUD").and_then(|i| props(&game, i)).and_then(|p| p.float("m_fCrosshairInterpolationSpeed")) {
        d.pawn.insert("hud.m_fCrosshairInterpolationSpeed".into(), v);
    }
    // the HUD's low health and mana (`m_LowHealthThreshold`, `m_LowManaThreshold`: health and
    // mana points, as the HUD compares them) and whether the gauges then stay
    // (`m_bAlwaysShowPlayerInfoWhenLowOn*`), the tweak's over the class default's
    let hud: Vec<Props> = ["Twk_InGameUI.Twk_GFxMoviePlayerHUD", "Default__DisTweaks_GFxMoviePlayerHUD"].iter().filter_map(|p| game.find_export(p).and_then(|i| props(&game, i))).collect();
    for n in ["m_LowHealthThreshold", "m_LowManaThreshold"] {
        if let Some(v) = hud.iter().find_map(|p| p.int(n)) {
            d.pawn.insert(format!("hud.{n}"), v as f32);
        }
    }
    for n in ["m_bAlwaysShowPlayerInfoWhenLowOnHealth", "m_bAlwaysShowPlayerInfoWhenLowOnMana"] {
        d.pawn.insert(format!("hud.{n}"), hud.iter().find_map(|p| p.bool(n)).unwrap_or(false) as u8 as f32);
    }
    let input: Vec<Props> = ["Twk_Pawn_DefaultPlayer.Twk_Pawn_PlayerInput", "Default__DisTweaks_PlayerInput"].iter().filter_map(|p| game.find_export(p).and_then(|i| props(&game, i))).collect();
    for n in ["m_fFrictionMult_Horiz", "m_fFrictionMult_Vert"] {
        if let Some(v) = input.iter().find_map(|p| p.float(n)) {
            d.pawn.insert(format!("input.{n}"), v);
        }
    }
    for n in ["m_bUseAimFriction_Zoomed", "m_bUseAimFriction_NonZoomed"] {
        d.pawn.insert(format!("input.{n}"), input.iter().find_map(|p| p.bool(n)).unwrap_or(false) as u8 as f32);
    }
    audiographs(cooked, &startup, &mut d);
    player_stats(&game, &startup, &mut d);
    challenges(&game, &mut d);
    d.bark_rules = bark_rules(&game);
    d.arm_pitch = arm_pitch(&startup);
    rumbles(&game, &startup, &mut d);
    // Summoned swarms have distinct level tweaks, inheriting the swarm defaults.
    for level in 1..=2 {
        let prefix = format!("swarm.{level}.");
        for name in ["Default__DisTweaks_RatSwarm".to_string(), format!("Twk_RatSwarm.Twk_DevouringSwarm_Lvl{level}")] {
            if let Some(p) = game.find_export(&name).and_then(|i| props(&game, i)) {
                let top: Vec<_> = p.0.iter().filter(|p| matches!(p.value, Value::Float(..) | Value::Int(..) | Value::Bool(..))).cloned().collect();
                flatten(&game, &top, &prefix, &mut d.pawn, &mut BTreeMap::new());
            }
        }
    }
    // the player tweaks' class defaults (what Corvo's tweak leaves unchanged), and the
    // swimming state's
    if let Some(p) = game.find_export("Default__DisTweaks_PlayerPawn").and_then(|i| props(&game, i)) {
        let top: Vec<upk::props::Prop> = p.0.iter().filter(|p| !matches!(p.value, Value::Struct(..))).cloned().collect();
        flatten(&game, &top, "", &mut d.pawn, &mut BTreeMap::new());
    }
    if let Some(p) = game.find_export("Default__StatePlayerMasterSwim").and_then(|i| props(&game, i)) {
        flatten(&game, &p.0, "swim.", &mut d.pawn, &mut BTreeMap::new());
    }
    if let Some(p) = game.find_export("Default__DishonoredPawn").and_then(|i| props(&game, i)) {
        let top: Vec<upk::props::Prop> = p.0.iter().filter(|p| matches!(p.value, Value::Float(..))).cloned().collect();
        flatten(&game, &top, "pawn.", &mut d.pawn, &mut BTreeMap::new());
    }
    if let Some(p) = startup.find_export("Twk_Pawn_Corvo.Twk_Pawn_Corvo_Release").and_then(|i| props(&startup, i)) {
        let top: Vec<upk::props::Prop> = p.0.iter().filter(|p| !matches!(p.value, Value::Struct(..))).cloned().collect();
        flatten(&startup, &top, "", &mut d.pawn, &mut BTreeMap::new());
        d.darkness = darkness(&startup, &p);
        log::info!("gamedata: chaos: {} levels, {} deeds", d.darkness.levels.len(), d.darkness.deeds.len());
        // his breath's sounds (`m_pBreathlessnessIncreaseEvent` / `..DecreaseEvent`: Wwise events
        // by name) as texts `Pawn.<property>`
        for n in ["m_pBreathlessnessIncreaseEvent", "m_pBreathlessnessDecreaseEvent"] {
            if let Some(o) = p.object(n).filter(|o| *o != 0) {
                d.texts.insert(format!("Pawn.{n}"), object_name(&startup, o));
            }
        }
        // and its own strings the localisation leaves out (the PC's elixir tutorials,
        // `m_AcquiredElixirTutorialMessage_PC[i]`...), each element as `Pawn.<property>[i]`
        for q in &p.0 {
            if let Value::Str(s) = &q.value {
                if !s.is_empty() {
                    d.texts.entry(format!("Pawn.{}[{}]", q.name, q.index)).or_insert_with(|| s.clone());
                    if q.index == 0 {
                        d.texts.entry(format!("Pawn.{}", q.name)).or_insert_with(|| s.clone());
                    }
                }
            }
        }
    }
    // the game parameter his breath sets (`m_BreathlessnessRTPC`, the class's)
    if let Some(Value::Name(n)) = game.find_export("Default__DisTweaks_PlayerPawn").and_then(|i| props(&game, i)).and_then(|p| p.get("m_BreathlessnessRTPC").cloned()) {
        d.texts.insert("Pawn.m_BreathlessnessRTPC".into(), n);
    }
    // the menus' blur (`m_bBlurGameWhileActive`: journal, pause, notes, store, mission stats)
    if let Some(Value::Object(o)) = game.find_export("Default__DisGlobalUIManager").and_then(|i| props(&game, i)).and_then(|p| p.get("m_pBlurTweaks").cloned()) {
        if let Some(p) = props(&game, o) {
            flatten(&game, &p.0, "", &mut d.ui_blur, &mut BTreeMap::new());
        }
    }
    // possession: how long each kind of host can be ridden, per level
    for (path, slot) in [("Twk_Possessable_BaseNPC.Twk_Possessable_BaseAnimal", 0), ("Twk_Possessable_BaseNPC.Twk_Possessable_BaseNPC", 1)] {
        let Some(p) = startup.find_export(path).and_then(|i| props(&startup, i)) else { continue };
        let mut t = [0.0f32; 2];
        for (i, v) in t.iter_mut().enumerate() {
            if let Some(Value::Struct(_, sub)) = p.get_idx("m_Levels", i as i32) {
                *v = sub.iter().find(|f| f.name == "m_fPossessDuration").and_then(|f| if let Value::Float(x) = f.value { Some(x) } else { None }).unwrap_or(0.0);
            }
        }
        if slot == 0 {
            d.possess_animal = t;
        } else {
            d.possess_human = t;
        }
    }
    // the post-process graph's nodes that grade the image their own way while they run
    for i in startup.exports_of_class("ArkPpNodeMaterial").collect::<Vec<_>>() {
        if !startup.obj_path(i).starts_with("AltScreen_Effects.PostProcessChain.Test_PPG.") {
            continue;
        }
        let Some(p) = props(&startup, i) else { continue };
        if !matches!(p.get("m_bOverrideUberPp"), Some(Value::Bool(true))) {
            continue;
        }
        let Some(Value::Name(effect)) = p.get("EffectName") else { continue };
        let mut fields = BTreeMap::new();
        if let Some(Value::Struct(_, sub)) = p.get("m_UberParameters") {
            flatten(&startup, sub, "", &mut fields, &mut BTreeMap::new());
        }
        d.post_nodes.insert(effect.to_string(), fields);
    }
    // Daud (The Knife of Dunwall), where its packages are installed
    if let Some(p) = dlc06_profile(cooked, &game, &d) {
        log::info!("gamedata: profile {}: {} active / {} passive powers, {} attributes, {} loadouts", p.name, p.actives.len(), p.passives.len(), p.attributes.len(), p.loadouts.len());
        d.profiles.push(p);
    }
    localization(cooked, &mut d);
    abstract_items(cooked, &mut d);
    journal_items(cooked, &mut d);
    chapters(&game, &mut d);
    tutorial_notes(cooked, &game, &mut d);
    missions_ini(cooked, &mut d);
    // the campaign: the persistent level's scripts run the flow between missions
    let dir = root.join("game");
    std::fs::create_dir_all(&dir)?;
    match Package::open(&cooked.join("DishonoredGameFull_P.upk")) {
        Ok(full) => {
            let full = std::sync::Arc::new(full);
            campaign_tweaks(&full, &mut d);
            let k = crate::kismet::cook(std::slice::from_ref(&full), &std::collections::HashMap::new());
            log::info!("campaign: {} script ops, {} variables", k.ops.len(), k.vars.len());
            std::fs::write(dir.join("campaign.json"), serde_json::to_vec(&k)?)?;
        }
        Err(e) => log::warn!("campaign scripts (DishonoredGameFull_P): {e:#}"),
    }
    let startup = std::sync::Arc::new(startup);
    d.heart = conv_graph(&startup, "Dlg_HeartGadget.Dlg_HeartGadget");
    log::info!("gamedata: heart {} nodes, hooks {:?}", d.heart.nodes.len(), d.heart.hooks.keys().collect::<Vec<_>>());
    std::fs::write(dir.join("gamedata.json"), serde_json::to_vec_pretty(&d)?)?;
    log::info!(
        "gamedata: {} active / {} passive powers, {} attributes, {} charms, {} upgrades, {} stores",
        d.actives.len(),
        d.passives.len(),
        d.attributes.len(),
        d.charms.len(),
        d.upgrades.len(),
        d.stores.len()
    );
    Ok(d)
}
