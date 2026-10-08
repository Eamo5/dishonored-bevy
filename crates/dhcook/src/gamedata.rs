//! Campaign-wide game data from the original tweak objects: Corvo's powers (active and
//! passive), his attributes per difficulty, bone charms, upgrades and the stores.

use crate::format::{ActivePowerDef, AttrMod, CharmDef, GameData, PassiveLevel, PassivePowerDef, StoreDef, StoreItem, UpgradeDef};
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
        for n in ["m_fBeheadRandomChance", "m_fGenericFatalityChance", "m_fSlomoFinisherRandomChance"] {
            if let Some(v) = set.float(n) {
                d.pawn.insert(format!("fatality.{n}"), v);
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
            if let Some(v) = p.float("m_fCamRecoilOnFire") {
                d.pawn.insert(format!("{key}.m_fCamRecoilOnFire"), v);
            }
        }
    }
    for (path, class, key) in [
        ("Twk_Projectiles.Twk_Proj_BulletUpgraded", "Default__DisTweaks_Bullet", "bullet."),
        ("Twk_Projectiles.Twk_Proj_Arrow", "Default__DisTweaks_Arrow", "arrow."),
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
        } else if section.ends_with("_Texts") || section == "Misc" {
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
pub fn cook(cooked: &Path, root: &Path) -> Result<GameData> {
    let game = Package::open(&cooked.join("DishonoredGame.upk")).context("DishonoredGame.upk")?;
    let startup = Package::open(&cooked.join("Startup.upk")).context("Startup.upk")?;
    let mut d = GameData { actives: actives(&game), passives: passives(&game), attributes: attributes(&game, &startup), charms: charms(&startup), upgrades: upgrades(&startup), stores: stores(cooked), ..Default::default() };
    arms(&game, &startup, &mut d);
    audiographs(cooked, &startup, &mut d);
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
        // his breath's sounds (`m_pBreathlessnessIncreaseEvent` / `..DecreaseEvent`: Wwise events
        // by name) as texts `Pawn.<property>`
        for n in ["m_pBreathlessnessIncreaseEvent", "m_pBreathlessnessDecreaseEvent"] {
            if let Some(o) = p.object(n).filter(|o| *o != 0) {
                d.texts.insert(format!("Pawn.{n}"), object_name(&startup, o));
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
    localization(cooked, &mut d);
    abstract_items(cooked, &mut d);
    journal_items(cooked, &mut d);
    chapters(&game, &mut d);
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
