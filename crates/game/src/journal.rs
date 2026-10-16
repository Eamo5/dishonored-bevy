//! The journal (`GBA_Journal`, J): its state and input; it is drawn from the original
//! `UI_Journal` movie by `jview`. Its pages in the original's order: Objectives (the level
//! scripts' objectives and tasks beside the chapter's briefing and targets, the mission's clues,
//! its items), Notes (written notes and maps, books, audiographs to play again), Powers (runes
//! buy the Outsider's gifts and their second levels), Bone Charms (worn in the sockets
//! unlocked), Inventory (`j_InventoryScreen`'s sub tabs `t_Resources`, `t_KeyRing`,
//! `t_Gadgets`, `t_Ammo`, `t_Upgrades`).

use crate::audio::PostEvent;
use crate::gamedata::Data;
use crate::gameplay::PlayerStats;
use crate::hud::Paused;
use crate::jview::{JAct, JHelp, JRow, JSub, JTab};
use crate::ui_fonts::UiFonts;
use crate::ui_images::UiImages;
use crate::GameState;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

pub struct JournalPlugin;

impl Plugin for JournalPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Journal>()
            .init_resource::<HelpPress>()
            .add_message::<ReadNote>()
            .add_plugins(crate::jview::JViewPlugin)
            .add_systems(OnEnter(GameState::InGame), |mut j: ResMut<Journal>| *j = Journal::default())
            .add_systems(Update, (toggle, interact, refresh).chain().run_if(in_state(GameState::InGame)));
    }
}

/// (tests) A help bar entry pressed by the script (`jhelp N`: `jview::HELP_*`).
#[derive(Resource, Default)]
pub struct HelpPress(pub Option<u8>);

/// A note was picked up: it opens in the journal.
#[derive(Message, Clone)]
pub struct ReadNote(pub String);

/// The pages in the original's tab order (`JournalTabsOrder`): Objectives, Logs, Powers, Bone
/// Charms, Inventory.
pub(crate) const PAGES: [u8; 5] = [PAGE_MISSION, PAGE_NOTES, PAGE_POWERS, PAGE_CHARMS, PAGE_INVENTORY];
pub(crate) const PAGE_POWERS: u8 = 0;
pub(crate) const PAGE_CHARMS: u8 = 1;
pub(crate) const PAGE_MISSION: u8 = 2;
pub(crate) const PAGE_NOTES: u8 = 3;
pub(crate) const PAGE_INVENTORY: u8 = 4;
/// the Inventory page's sub-tabs (`j_InventoryScreen._tabsContent`)
pub(crate) const INV_TABS: [&str; 5] = ["t_Resources", "t_KeyRing", "t_Gadgets", "t_Ammo", "t_Upgrades"];

#[derive(Resource)]
pub struct Journal {
    pub open: bool,
    /// times it was closed after being opened (`DisSeqEvent_JournalViewed`)
    pub viewed: u32,
    page: u8,
    /// the Objectives page's sub tab (tasks, clues, items) and the row chosen
    obj_tab: u8,
    obj_sel: usize,
    /// the Notes page's sub tab (notes, books, audiographs) and the entry read
    log_tab: u8,
    note: usize,
    /// the power shown in the details panel (`jview::POWER_SLOTS`)
    selected: usize,
    /// the bone charm chosen
    charm_sel: usize,
    /// the Inventory page's sub-tab and the item shown
    inv_tab: u8,
    inv_sel: usize,
    /// the tabs flashing (`bFlashingTab`, by the tabs' order): set as it opens, each put out as
    /// it is left
    flashing: u8,
    /// the objectives' history shown (the journal's `OnToggleObjectivesListHistory`)
    history: bool,
    /// a purchase asked first (`t_Q_BuyPower`): its words, the button chosen (0: yes)
    confirm: Option<(String, usize)>,
    /// a button of the question clicked (`msgbox`)
    pub confirm_click: Option<usize>,
    dirty: bool,
}

impl Default for Journal {
    fn default() -> Self {
        Journal { open: false, viewed: 0, page: PAGE_MISSION, obj_tab: 0, obj_sel: 0, log_tab: 0, note: 0, selected: 1, charm_sel: 0, inv_tab: 0, inv_sel: 0, flashing: 0, history: false, confirm: None, confirm_click: None, dirty: false }
    }
}

impl Journal {
    /// The question asked over it (its words, the button chosen), if any (`msgbox`).
    pub fn question(&self) -> Option<(&str, usize)> {
        self.confirm.as_ref().map(|c| (c.0.as_str(), c.1))
    }
}

/// A power's tutorial (its `EDisTutorialNote`): the Powers page's `SEE TUTORIAL` reads it.
fn power_note(name: &str) -> String {
    match name {
        "Possess" => "DTN_Possession".to_string(),
        n => format!("DTN_{n}"),
    }
}

/// A page's `EDisJournalTab` (the bit the exe flags it by).
fn tab_index(page: u8) -> usize {
    PAGES.iter().position(|p| *p == page).unwrap_or(0)
}

/// The tabs the journal has (`PowersTabAvailable`: runes, or a power or enhancement owned;
/// `BoneCharmsTabAvailable`: a charm owned): before the Mark, the Objectives, Logs and
/// Inventory only.
pub(crate) fn pages(stats: &PlayerStats) -> Vec<u8> {
    let powers = stats.runes > 0 || stats.powers.values().any(|l| *l > 0);
    let charms = !stats.charms_owned.is_empty();
    PAGES
        .iter()
        .copied()
        .filter(|p| match *p {
            PAGE_POWERS => powers,
            PAGE_CHARMS => charms,
            _ => true,
        })
        .collect()
}

/// A page's place among the tabs it has (the movie reads the flags by place).
fn place(stats: &PlayerStats, page: u8) -> usize {
    pages(stats).iter().position(|p| *p == page).unwrap_or(0)
}

/// How many power levels his runes would buy now (the exe's count over his powers and
/// enhancements, the next level of each not maxed out).
pub(crate) fn affordable(data: &Data, stats: &PlayerStats) -> usize {
    crate::jview::POWER_SLOTS.iter().filter(|(name, _)| next_cost(data, name, stats.power(name)).is_some_and(|c| stats.runes >= c)).count()
}

/// The tabs that flash as the journal opens (`Req_FlashingTabsBitfield`, by `EDisJournalTab`;
/// the movie reads each tab's bit by its place): the Powers while his runes would buy
/// something, the Bone Charms while a charm picked up is new.
fn flashing_tabs(data: &Data, stats: &PlayerStats) -> u8 {
    let mut bits = 0;
    if affordable(data, stats) > 0 {
        bits |= 1 << tab_index(PAGE_POWERS);
    }
    if stats.new_charm {
        bits |= 1 << tab_index(PAGE_CHARMS);
    }
    bits
}

/// The page a tutorial window's key opens (its `m_JournalTab`, by its picture): the rune's the
/// Powers, the bone charm's the Bone Charms, the clues' the Objectives on their sub tab
/// (`DJT_Sub_ChapterNotes`); a note's reads the note.
fn window_page(img: u8) -> Option<(u8, u8)> {
    match img {
        crate::tutwindow::IMG_RUNE => Some((PAGE_POWERS, 0)),
        crate::tutwindow::IMG_CHARM => Some((PAGE_CHARMS, 0)),
        crate::tutwindow::IMG_NOTE => Some((PAGE_MISSION, 1)),
        _ => None,
    }
}

/// An item of the Inventory page: its name, description, icon (`items/<icon>_Large`), count,
/// and which elixir it is (`bIsElixir`: true for mana), drunk from the page.
pub(crate) struct InvEntry {
    pub name: String,
    pub desc: String,
    pub icon: String,
    pub count: Option<u32>,
    pub elixir: Option<bool>,
}

/// The mission's items Corvo carries (abstract items the journal shows under Mission Items:
/// `DJIS_Mission`), with their pictures (`m_JournalIconName`).
pub(crate) fn mission_items(data: &Data, stats: &PlayerStats) -> Vec<InvEntry> {
    stats
        .notes
        .iter()
        .filter_map(|k| {
            let (section, icon) = data.0.journal_items.get(k)?;
            if section != "DJIS_Mission" {
                return None;
            }
            let (name, desc) = data.0.abstract_items.get(k).cloned().unwrap_or_default();
            let count = stats.items.get(k).copied().filter(|n| *n > 1);
            Some(InvEntry { name, desc: crate::gamedata::readable(&desc), icon: if icon.is_empty() { "MissionItems".into() } else { icon.clone() }, count, elixir: None })
        })
        .collect()
}

/// Whether an abstract item is one of the mission's items rather than something to read.
pub(crate) fn is_mission_item(data: &Data, key: &str) -> bool {
    data.0.journal_items.get(key).is_some_and(|(s, _)| s == "DJIS_Mission")
}

/// What Corvo carries under a sub-tab of the Inventory page.
pub(crate) fn inventory(data: &Data, stats: &PlayerStats, tab: u8) -> Vec<InvEntry> {
    use crate::powers::Power;
    let store = |item: &str| data.0.stores.iter().flat_map(|s| s.items.iter()).find(|i| i.item == item);
    let from_store = |item: &str, icon: &str, count: u32| -> Option<InvEntry> {
        let s = store(item)?;
        Some(InvEntry { name: crate::gamedata::readable(&s.name), desc: crate::gamedata::readable(&s.description), icon: if s.icon.is_empty() { icon.to_string() } else { s.icon.clone() }, count: Some(count), elixir: None })
    };
    let arm = |tweak: &str, icon: &str| -> InvEntry {
        let t = |k: &str| data.0.texts.get(&format!("INV.{tweak}.{k}")).map(|s| crate::gamedata::readable(s.trim())).unwrap_or_default();
        InvEntry { name: t("m_Name"), desc: t("m_Description"), icon: icon.to_string(), count: None, elixir: None }
    };
    let mut out = Vec::new();
    match tab {
        0 => {
            let coins = data.0.abstract_items.iter().find(|(k, _)| k.ends_with("Coins_AbsItm")).map(|(_, v)| v.clone());
            out.push(InvEntry {
                // (one coin by the item's name, more by the journal's `t_Coins`)
                name: match coins.as_ref().map(|c| c.0.clone()).filter(|n| !n.is_empty()) {
                    Some(one) if stats.coins == 1 => one,
                    _ => {
                        let t = data.text(J, "t_Coins").to_lowercase();
                        let mut c = t.chars();
                        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_else(|| "Coins".into())
                    }
                },
                desc: coins.map(|c| c.1).unwrap_or_default(),
                icon: "Money".into(),
                count: Some(stats.coins),
                elixir: None,
            });
            if stats.health_elixirs > 0 {
                out.extend(from_store("Elixir_Health_twk", "HealthElixir", stats.health_elixirs).map(|e| InvEntry { elixir: Some(false), ..e }));
            }
            if stats.mana_elixirs > 0 {
                out.extend(from_store("Elixir_Mana_twk", "ManaElixir", stats.mana_elixirs).map(|e| InvEntry { elixir: Some(true), ..e }));
            }
            if stats.runes > 0 {
                out.extend(from_store("Rune_twk", "Rune", stats.runes));
            }
        }
        1 => {
            let desc = data.0.texts.get("INV.Key.m_Description").cloned().unwrap_or_default();
            for k in &stats.keys {
                out.push(InvEntry { name: k.clone(), desc: desc.clone(), icon: "Key".into(), count: None, elixir: None });
            }
        }
        2 => {
            if !stats.unarmed {
                out.push(arm("Twk_Inv_SwordCorvo", "CorvoSword"));
            }
            for (p, tweak, icon) in [(Power::Pistol, "Twk_Inv_PistolCorvo", "Gun"), (Power::Crossbow, "Twk_Inv_CrossbowCorvo", "Crossbow"), (Power::Heart, "Twk_Inv_HeartCorvo", "CorvoHeart")] {
                if p.owned(stats) {
                    out.push(arm(tweak, icon));
                }
            }
            for (item, icon) in [(crate::gadgets::GRENADES, "CorvoGrenade"), (crate::gadgets::STICKY, "CorvoStickyGrenade"), (crate::gadgets::RAZORS, "SpringRazor"), ("RewireTool_twk", "RewireTool")] {
                let n = stats.items.get(item).copied().unwrap_or(0);
                if n > 0 {
                    out.extend(from_store(item, icon, n));
                }
            }
        }
        3 => {
            let flares = stats.items.get(crate::gadgets::FLARES).copied().unwrap_or(0);
            let explosive = stats.items.get(crate::gadgets::EXPLOSIVE).copied().unwrap_or(0);
            for (item, icon, n) in [
                ("Bullets_Store_Ammo_twk", "RegularBullets", stats.bullets),
                ("ExplosiveBullets_Ammo_twk", "ExplosiveBullets", explosive),
                ("Bolt_Ammo_twk", "CrossbowRegularBolt", stats.bolts),
                ("SleepDart_Ammo_twk", "CrossbowSleepDart", stats.sleep_darts),
                ("Flare_Ammo_twk", "CrossbowFlareBolt", flares),
            ] {
                if n > 0 {
                    out.extend(from_store(item, icon, n));
                }
            }
        }
        _ => {
            for u in &stats.upgrades {
                if let Some(def) = data.0.upgrades.iter().find(|d| d.id == *u) {
                    out.push(InvEntry { name: crate::gamedata::readable(&def.name), desc: crate::gamedata::readable(&def.description), icon: def.icon.clone(), count: None, elixir: None });
                }
            }
        }
    }
    out
}

/// Bone charms Corvo can wear at once (`m_MaxActivatedBoneCharmCount`, plus one for each
/// Mystical Artifact owned: `Twk_Upgrade_Statue*`'s `MaxActivatedBoneCharmCountBonus`).
pub fn charm_slots(data: &Data, stats: &PlayerStats) -> usize {
    data.pawn("m_MaxActivatedBoneCharmCount", 3.0) as usize + charm_slot_bonus(data, |id| stats.upgrades.iter().any(|u| u == id))
}

/// The slots the upgrades `owned` add (all of them: the most there can be).
pub fn charm_slot_bonus(data: &Data, owned: impl Fn(&str) -> bool) -> usize {
    data.0.upgrades.iter().filter(|u| owned(&u.id)).flat_map(|u| &u.attributes).filter(|(n, _)| n == "MaxActivatedBoneCharmCountBonus").map(|(_, v)| v.max(0.0) as usize).sum()
}

/// (game data name, journal title key, tutorial note key, illustration)
pub(crate) const ENTRIES: [(&str, &str, &str, &str); 10] = [
    ("Blink", "t_Blink", "pBlink", "BlinkBig"),
    ("DarkVision", "t_DarkVision", "pDarkVision", "DarkVisionBig"),
    ("DevouringSwarm", "t_RatSwarm", "pRatsSwarm", "RatSwarmBig"),
    ("Possess", "t_Possession", "pPossession", "PossessionBig"),
    ("BendTime", "t_BendTime", "pBendTime", "BendTimeBig"),
    ("Windblast", "t_WindBlast", "pWindBlast", "WindblastBig"),
    ("Vitality", "t_Vitality", "pVitality", "VitalityBig"),
    ("BloodThirsty", "t_BloodThirst", "pBloodThirsty", "BloodThirstyBig"),
    ("ShadowKill", "t_ShadowKill", "pShadowKill", "ShadowKillBig"),
    ("Celerity", "t_Celerity", "pCelerity", "AgilityBig"),
];
pub(crate) const J: &str = "DisGFxMoviePlayerJournal_Texts";
pub(crate) const NOTE: &str = "DisGFxMoviePlayerNote_Texts";

#[derive(Component)]
struct JournalRoot;

/// Runes to reach the next level (None: maxed out).
pub(crate) fn next_cost(data: &Data, name: &str, level: u8) -> Option<u32> {
    if level >= 2 {
        return None;
    }
    if let Some(a) = data.active(name) {
        return Some(a.runes[level as usize]);
    }
    data.passive(name).and_then(|p| p.levels.get(level as usize + 1)).map(|l| l.runes)
}

/// The journal opens (paused, the pointer free).
fn open_journal(journal: &mut Journal, paused: &mut Paused, cursor: &mut CursorOptions, sfx: &mut MessageWriter<PostEvent>) {
    if !journal.open {
        journal.open = true;
        paused.0 = true;
        cursor.visible = true;
        cursor.grab_mode = CursorGrabMode::None;
        sfx.write(PostEvent::named("UI_OpenScreen", None));
    }
    journal.dirty = true;
}

#[allow(clippy::too_many_arguments)]
fn toggle(
    mut commands: Commands,
    (keys, bind): (Res<ButtonInput<KeyCode>>, Res<crate::bindings::Bindings>),
    mut journal: ResMut<Journal>,
    mut paused: ResMut<Paused>,
    menu: Res<crate::menu::Menu>,
    (mut stats, data, mut msgs): (ResMut<PlayerStats>, Res<Data>, ResMut<crate::gameplay::HudMessages>),
    roots: Query<Entity, With<JournalRoot>>,
    mut cursor: Single<&mut CursorOptions>,
    (mut sfx, mut read): (MessageWriter<PostEvent>, MessageWriter<ReadNote>),
    mission_end: Option<Res<crate::mission::MissionEnd>>,
    note: Res<crate::notescreen::NoteScreen>,
    vm: Option<ResMut<crate::kismet::Vm>>,
    (settings, npcs, mut save): (Res<crate::settings::Settings>, Query<&crate::npc::Npc>, MessageWriter<crate::save::SaveRequest>),
) {
    // (shut by the scripts: `DisSeqAct_ToggleJournal`, the Tower's opening)
    let shut = vm.as_ref().is_some_and(|v| v.switches.journal_off);
    // the scripts open it on a tab (`DisSeqAct_OpenJournal`: the dream's powers page)
    if let Some(tab) = vm.and_then(|mut v| v.open_journal.take()) {
        let page = match tab.as_str() {
            "DJT_Powers" => PAGE_POWERS,
            "DJT_BoneCharms" | "DJT_Charms" => PAGE_CHARMS,
            "DJT_Notes" | "DJT_Journal" => PAGE_NOTES,
            _ => PAGE_MISSION,
        };
        // (a tab it hasn't: the Objectives)
        journal.page = if pages(&stats).contains(&page) { page } else { PAGE_MISSION };
        journal.flashing = flashing_tabs(&data, &stats);
        open_journal(&mut journal, &mut paused, &mut cursor, &mut sfx);
        return;
    }
    // (a note being read, or put away this frame: `notescreen`)
    if note.open.is_some() || note.shut {
        return;
    }
    // (a question asked over it answers the keys)
    if journal.open && journal.confirm.is_some() {
        return;
    }
    let _ = &data;
    // (a tutorial just received, its prompt up: the key reads it)
    if !journal.open && keys.just_pressed(bind.key(crate::bindings::Act::Journal)) && menu.open.is_none() && !stats.dead && mission_end.is_none() {
        if let Some((id, _)) = stats.tutorial_prompt.take() {
            read.write(ReadNote(format!("{}{id}", crate::tutorials::KEY)));
            return;
        }
    }
    let want = if journal.open {
        !(keys.just_pressed(bind.key(crate::bindings::Act::Journal)) || keys.just_pressed(KeyCode::Escape))
    } else {
        keys.just_pressed(bind.key(crate::bindings::Act::Journal)) && menu.open.is_none() && !stats.dead && mission_end.is_none() && !shut
    };
    if want == journal.open {
        return;
    }
    if want {
        // the page it opens on (`_root.StartTabIndex`, as the journal's key asks it): a tutorial
        // window's on screen (the window put away), else the Objectives
        let window = msgs.tutorial_window_shown().and_then(|(text, img)| window_page(img).map(|p| (text, p)));
        let (page, sub) = match window {
            Some((text, p)) => {
                msgs.drop_tutorial(&text);
                p
            }
            None => (PAGE_MISSION, 0),
        };
        let (page, sub) = if pages(&stats).contains(&page) { (page, sub) } else { (PAGE_MISSION, 0) };
        journal.page = page;
        if page == PAGE_MISSION && journal.obj_tab != sub {
            journal.obj_tab = sub;
            journal.obj_sel = 0;
        }
        journal.flashing = flashing_tabs(&data, &stats);
        open_journal(&mut journal, &mut paused, &mut cursor, &mut sfx);
        // (`PSI_Gameplay_bAutoSaveInMenu`: saved as it opens, but not in a fight:
        // `m_bDisableAutosaveInCombat`)
        if settings.auto_save_journal && !npcs.iter().any(|n| !n.is_down() && n.mode == crate::npc::Mode::Combat) {
            save.write(crate::save::SaveRequest(crate::save::AUTOSAVE_SLOT));
        }
    } else {
        journal.open = false;
        // (it fades away: `jview::JLeave`)
        for e in &roots {
            commands.entity(e).remove::<JournalRoot>().insert((crate::jview::JLeave { t: 0.0 }, UiTransform::default(), Pickable::IGNORE));
        }
        paused.0 = false;
        cursor.visible = false;
        cursor.grab_mode = CursorGrabMode::Locked;
        journal.viewed += 1;
        sfx.write(PostEvent::named("UI_CloseScreen", None));
    }
}

/// How many entries the page's list holds (its chosen one wraps within them).
fn list_len(journal: &Journal, data: &Data, stats: &PlayerStats, vm: Option<&crate::kismet::Vm>) -> usize {
    match journal.page {
        PAGE_MISSION if journal.obj_tab == 0 => crate::jview::objective_rows(vm, journal.history).len(),
        PAGE_MISSION if journal.obj_tab == 2 => mission_items(data, stats).len(),
        PAGE_NOTES => crate::jview::log_entries(data, stats, journal.log_tab).len(),
        PAGE_POWERS => crate::jview::POWER_SLOTS.len(),
        PAGE_CHARMS => stats.charms_owned.len(),
        PAGE_INVENTORY => inventory(data, stats, journal.inv_tab).len(),
        _ => 0,
    }
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::type_complexity)]
fn interact(
    mut journal: ResMut<Journal>,
    data: Res<Data>,
    mut stats: ResMut<PlayerStats>,
    keys: Res<ButtonInput<KeyCode>>,
    mut sfx: MessageWriter<PostEvent>,
    mut msgs: ResMut<crate::gameplay::HudMessages>,
    (tabs, subs, rows, acts, helps): (Query<(&Interaction, &JTab), Changed<Interaction>>, Query<(&Interaction, &JSub), Changed<Interaction>>, Query<(&Interaction, &JRow), Changed<Interaction>>, Query<&Interaction, (With<JAct>, Changed<Interaction>)>, Query<(&Interaction, &JHelp), Changed<Interaction>>),
    (playback, mut graphs, mut press, mut read, note, attrs): (Res<crate::audiograph::Playback>, MessageWriter<crate::audiograph::PlayAudiograph>, ResMut<HelpPress>, MessageWriter<ReadNote>, Res<crate::notescreen::NoteScreen>, Res<crate::gamedata::Attrs>),
    mut vm: Option<ResMut<crate::kismet::Vm>>,
    scripted: Option<Res<crate::script::Scripted>>,
) {
    if !journal.open {
        return;
    }
    // (a note read from it: `notescreen`)
    if note.open.is_some() || note.shut {
        return;
    }
    // a purchase asked first: Left / Right choose, Enter answers, Escape says no
    if let Some((_, choice)) = journal.confirm.clone() {
        let mut choice = choice;
        if keys.just_pressed(KeyCode::ArrowLeft) || keys.just_pressed(KeyCode::ArrowRight) || keys.just_pressed(KeyCode::KeyA) || keys.just_pressed(KeyCode::KeyD) {
            choice = 1 - choice.min(1);
        }
        let click = journal.confirm_click.take();
        if let Some(c) = click {
            choice = c;
        }
        if keys.just_pressed(KeyCode::Escape) {
            journal.confirm = None;
        } else if keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::Space) || keys.just_pressed(KeyCode::KeyE) || click.is_some() {
            journal.confirm = None;
            if choice == 0 {
                let (name, _) = crate::jview::POWER_SLOTS[journal.selected.min(9)];
                let level = stats.power(name);
                if let Some(cost) = next_cost(&data, name, level).filter(|c| stats.runes >= *c) {
                    stats.runes -= cost;
                    stats.powers.insert(name.to_string(), level + 1);
                    sfx.write(PostEvent::named("UI_J_BuyPower", None));
                    let what = ENTRIES.iter().find(|e| e.0 == name).map(|e| data.text(J, e.1)).unwrap_or_default();
                    msgs.push(if level == 0 { format!("{what} acquired") } else { format!("{what} upgraded") });
                }
            }
            journal.dirty = true;
        } else if let Some(c) = journal.confirm.as_mut() {
            if c.1 != choice {
                c.1 = choice;
            }
        }
        return;
    }
    // (the Bone Charms shown: the charm is new no more, as the tab's content clears the HUD's)
    if journal.page == PAGE_CHARMS && stats.new_charm {
        stats.new_charm = false;
    }
    // (a scripted run ignores the real pointer)
    let pointer = scripted.is_none();
    // the pages: their tabs, or Tab / Shift+Tab (`LB`/`RB`)
    let mut page = None;
    for (i, t) in tabs.iter().filter(|_| pointer) {
        if *i == Interaction::Pressed && journal.page != t.0 {
            page = Some(t.0);
        }
    }
    if keys.just_pressed(KeyCode::Tab) {
        let tabs = pages(&stats);
        let k = tabs.iter().position(|p| *p == journal.page).unwrap_or(0);
        let back = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
        page = Some(tabs[if back { (k + tabs.len() - 1) % tabs.len() } else { (k + 1) % tabs.len() }]);
    }
    // the Objectives' help bar: the history shown or hidden, the chosen objective's markers
    let help = helps.iter().filter(|_| pointer).find(|(i, _)| **i == Interaction::Pressed).map(|(_, h)| h.0).or(press.0.take());
    // an elixir chosen in the Inventory drunk (its help bar's `ConsumeElixir`, or its action)
    let drink = |journal: &mut Journal, stats: &mut PlayerStats, sfx: &mut MessageWriter<PostEvent>| {
        let elixir = inventory(&data, stats, journal.inv_tab).get(journal.inv_sel).and_then(|i| i.elixir);
        if let Some(mana) = elixir {
            if crate::powers::drink_elixir(stats, &attrs, mana, sfx) {
                journal.dirty = true;
            }
        }
    };
    if journal.page == PAGE_INVENTORY && help == Some(crate::jview::HELP_CONSUME) {
        drink(&mut journal, &mut stats, &mut sfx);
        return;
    }
    // the Powers' help bar: the chosen power's tutorial (`OnPowerInfosRequest`: its note read)
    if journal.page == PAGE_POWERS && help == Some(crate::jview::HELP_TUTORIAL) {
        let (name, _) = crate::jview::POWER_SLOTS[journal.selected.min(9)];
        read.write(ReadNote(format!("{}{}", crate::tutorials::KEY, power_note(name))));
        return;
    }
    if journal.page == PAGE_MISSION && journal.obj_tab == 0 {
        if let Some(h) = help {
            match h {
                crate::jview::HELP_HISTORY => {
                    journal.history = !journal.history;
                    journal.obj_sel = 0;
                    sfx.write(PostEvent::named("UI_J_ToggleHistory", None));
                }
                _ => {
                    // (locked on a finished objective)
                    let rows = crate::jview::objective_rows(vm.as_deref(), journal.history);
                    let r = rows.get(journal.obj_sel).filter(|r| rows.iter().any(|o| !o.task && o.objective == r.objective && o.state == crate::kismet::TaskState::Active));
                    if let (Some(r), Some(vm)) = (r, vm.as_mut()) {
                        let on = vm.toggle_objective_markers(&r.objective);
                        sfx.write(PostEvent::named(if on { "UI_J_MarkerOn" } else { "UI_J_MarkerOff" }, None));
                    }
                }
            }
            journal.dirty = true;
            return;
        }
    }
    if let Some(p) = page {
        // (the tab left flashes no more: `TabsHandler`)
        journal.flashing &= !(1 << place(&stats, journal.page));
        journal.page = p;
        journal.dirty = true;
        sfx.write(PostEvent::named("UI_ChangeMainTab", None));
        return;
    }
    // the sub tabs: clicked, or Left / Right (`LT`/`RT`) on the pages that have them
    let subs_n = match journal.page {
        PAGE_MISSION | PAGE_NOTES => 3,
        PAGE_INVENTORY => INV_TABS.len() as u8,
        _ => 0,
    };
    let mut sub = None;
    for (i, t) in subs.iter().filter(|_| pointer) {
        if *i == Interaction::Pressed {
            sub = Some(t.0);
        }
    }
    let lr = if keys.just_pressed(KeyCode::ArrowRight) || keys.just_pressed(KeyCode::KeyD) {
        1i32
    } else if keys.just_pressed(KeyCode::ArrowLeft) || keys.just_pressed(KeyCode::KeyA) {
        -1
    } else {
        0
    };
    // (Q / E change the sub tab; so do the arrows, but on the cards they move between them)
    let on_cards = journal.page == PAGE_INVENTORY || (journal.page == PAGE_MISSION && journal.obj_tab == 2);
    let lr_sub = if keys.just_pressed(KeyCode::KeyE) {
        1
    } else if keys.just_pressed(KeyCode::KeyQ) {
        -1
    } else if on_cards {
        0
    } else {
        lr
    };
    if subs_n > 0 && lr_sub != 0 {
        let cur = match journal.page {
            PAGE_MISSION => journal.obj_tab,
            PAGE_NOTES => journal.log_tab,
            _ => journal.inv_tab,
        };
        sub = Some(((cur as i32 + lr_sub).rem_euclid(subs_n as i32)) as u8);
    }
    if let Some(t) = sub {
        match journal.page {
            PAGE_MISSION => {
                journal.obj_tab = t;
                journal.obj_sel = 0;
            }
            PAGE_NOTES => {
                journal.log_tab = t;
                journal.note = 0;
            }
            _ => {
                journal.inv_tab = t;
                journal.inv_sel = 0;
            }
        }
        journal.dirty = true;
        sfx.write(PostEvent::named("UI_ChangeSubTab", None));
        return;
    }
    // the list: pointed at, or Up / Down (the powers' discs and the cards: all four arrows)
    let n = list_len(&journal, &data, &stats, vm.as_deref());
    let cur = match journal.page {
        PAGE_MISSION => journal.obj_sel,
        PAGE_NOTES => journal.note,
        PAGE_POWERS => journal.selected,
        PAGE_CHARMS => journal.charm_sel,
        _ => journal.inv_sel,
    };
    let mut chosen = None;
    let mut clicked = false;
    for (i, r) in rows.iter().filter(|_| pointer) {
        match i {
            Interaction::Hovered => chosen = Some(r.0),
            Interaction::Pressed => {
                chosen = Some(r.0);
                clicked = true;
            }
            Interaction::None => {}
        }
    }
    if n > 0 {
        let grid = journal.page == PAGE_POWERS;
        let step = |d: i32| Some(((cur as i32 + d).rem_euclid(n as i32)) as usize);
        // (the cards: four across)
        let cards = journal.page == PAGE_INVENTORY || (journal.page == PAGE_MISSION && journal.obj_tab == 2);
        let cols = if cards { 4 } else { 1 };
        if keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::KeyS) {
            // (the powers' row of six over the enhancements' four)
            chosen = if grid { Some(if cur < 6 { 6 + (cur * 4 / 6).min(3) } else { cur }) } else { step(cols) };
        }
        if keys.just_pressed(KeyCode::ArrowUp) || keys.just_pressed(KeyCode::KeyW) {
            chosen = if grid { Some(if cur >= 6 { ((cur - 6) * 6 / 4 + 1).min(5) } else { cur }) } else { step(-cols) };
        }
        if (grid || cards) && lr != 0 {
            chosen = step(lr);
        }
    }
    let was = cur;
    if let Some(c) = chosen.filter(|c| *c < n.max(1) && *c != cur) {
        match journal.page {
            PAGE_MISSION => journal.obj_sel = c,
            PAGE_NOTES => journal.note = c,
            PAGE_POWERS => journal.selected = c,
            PAGE_CHARMS => journal.charm_sel = c,
            _ => journal.inv_sel = c,
        }
        journal.dirty = true;
    }
    // the page's action: its button, Enter, or a click on the chosen row
    let act = acts.iter().filter(|_| pointer).any(|i| *i == Interaction::Pressed) || keys.just_pressed(KeyCode::Enter) || (clicked && chosen == Some(was));
    if !act {
        return;
    }
    match journal.page {
        PAGE_NOTES => {
            // an audiograph: play it (or stop it)
            let entries = crate::jview::log_entries(&data, &stats, journal.log_tab);
            if let Some(k) = entries.get(journal.note).and_then(|e| e.key.strip_prefix("ag:")) {
                let stop = playback.playing() == Some(k);
                graphs.write(crate::audiograph::PlayAudiograph { key: (!stop).then(|| k.to_string()), ..default() });
                sfx.write(PostEvent::named("UI_Validation", None));
            }
        }
        PAGE_CHARMS => {
            // wear the charm, or take it off
            let Some(name) = stats.charms_owned.get(journal.charm_sel).cloned() else { return };
            if let Some(k) = stats.charms.iter().position(|x| *x == name) {
                stats.charms.remove(k);
            } else if stats.charms.len() < charm_slots(&data, &stats) {
                stats.charms.push(name.clone());
            } else {
                sfx.write(PostEvent::named("UI_Failure", None));
                return;
            }
            sfx.write(PostEvent::named(if stats.charms.contains(&name) { "UI_J_BoneCharmOn" } else { "UI_J_BoneCharmOff" }, None));
            journal.dirty = true;
        }
        PAGE_POWERS => {
            let (name, _) = crate::jview::POWER_SLOTS[journal.selected.min(9)];
            let level = stats.power(name);
            let Some(cost) = next_cost(&data, name, level) else { return };
            if stats.runes < cost {
                sfx.write(PostEvent::named("UI_Failure", None));
                return;
            }
            // asked first, as the Powers page's buy does (`t_Q_BuyPower`, `_plural`: the power's
            // name and its cost; yes buys it)
            let what = ENTRIES.iter().find(|e| e.0 == name).map(|e| data.text(J, e.1)).unwrap_or_default();
            let q = data.text(J, if cost > 1 { "t_Q_BuyPower_plural" } else { "t_Q_BuyPower" }).replace("§POWER_NAME§", &what).replace("§RUNES_COST§", &cost.to_string());
            journal.confirm = Some((q, 0));
        }
        PAGE_INVENTORY => drink(&mut journal, &mut stats, &mut sfx),
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn refresh(
    mut commands: Commands,
    mut journal: ResMut<Journal>,
    data: Res<Data>,
    stats: Res<PlayerStats>,
    fonts: Res<UiFonts>,
    (mut ui, mut images): (ResMut<UiImages>, ResMut<Assets<Image>>),
    roots: Query<Entity, With<JournalRoot>>,
    vm: Option<Res<crate::kismet::Vm>>,
    (playback, mut was_playing): (Res<crate::audiograph::Playback>, Local<Option<String>>),
    mut timelines: ResMut<crate::flash::MovieTimelines>,
    window: Query<&Window, With<PrimaryWindow>>,
    config: Res<crate::Config>,
    mut last_size: Local<Vec2>,
    (entering, mut shown): (Query<&crate::jview::JEnter>, Local<Option<(u8, u8)>>),
) {
    // (an audiograph started or stopped: its button changes; the window resized)
    let playing = playback.playing().map(|s| s.to_string());
    if *was_playing != playing {
        *was_playing = playing.clone();
        journal.dirty |= journal.page == PAGE_NOTES;
    }
    let Ok(w) = window.single() else { return };
    let size = Vec2::new(w.width(), w.height());
    if *last_size != size {
        *last_size = size;
        journal.dirty = true;
    }
    if !journal.open {
        *shown = None;
        return;
    }
    if !journal.dirty {
        return;
    }
    journal.dirty = false;
    // how the page comes in: faded up as the journal opens (`Open`), slid in from the side it
    // was moved towards when its page or sub tab changes (`SetContent`), else as it was going
    let sub = match journal.page {
        PAGE_MISSION => journal.obj_tab,
        PAGE_NOTES => journal.log_tab,
        PAGE_INVENTORY => journal.inv_tab,
        _ => 0,
    };
    let order = |p: u8| PAGES.iter().position(|x| *x == p).unwrap_or(0) as i32;
    let enter = match *shown {
        None => Some(crate::jview::JEnter { t: 0.0, dur: crate::jview::OPEN_TIME, from: Vec2::ZERO }),
        Some((p, t)) if (p, t) != (journal.page, sub) => {
            let right = if p != journal.page { order(journal.page) > order(p) } else { sub > t };
            Some(crate::jview::JEnter { t: 0.0, dur: crate::jview::SLIDE_TIME, from: Vec2::new(if right { crate::jview::SLIDE } else { -crate::jview::SLIDE }, 0.0) })
        }
        _ => entering.iter().next().copied(),
    };
    *shown = Some((journal.page, sub));
    for e in &roots {
        commands.entity(e).despawn();
    }
    let Some(tl) = timelines.get("Journal") else { return };
    let lib = timelines.get("lib");
    let root = commands
        .spawn((JournalRoot, Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() }, GlobalZIndex(80), Pickable::IGNORE, DespawnOnExit(GameState::InGame)))
        .id();
    let js = crate::jview::JState {
        page: journal.page,
        obj_tab: journal.obj_tab,
        obj_sel: journal.obj_sel,
        log_tab: journal.log_tab,
        log_sel: journal.note,
        power_sel: journal.selected,
        charm_sel: journal.charm_sel,
        inv_tab: journal.inv_tab,
        inv_sel: journal.inv_sel,
        flashing: journal.flashing,
        history: journal.history,
        playing: playing.as_deref(),
    };
    let mut cx = crate::jview::Ctx { c: &mut commands, root, frame: root, st: crate::jview::Stage::of(w), tl, lib, title: fonts.title.clone().into(), ui: &mut ui, images: &mut images };
    crate::jview::build(&mut cx, &js, &data, &stats, vm.as_deref(), &config.map, enter);
}

pub fn location_map(key: &str) -> Option<(String, &'static str)> {
    let m = key.strip_prefix("map:")?;
    let image = match m {
        "DUM_Boyle" => "LevelMap_Ext_Boyle",
        "DUM_Bridge" => "LevelMap_Ext_Bridge",
        "DUM_BrothelExterior" => "LevelMap_Ext_Brothel",
        "DUM_BrothelInterior" => "LevelMap_Int_Brothel",
        "DUM_Lighthouse" => "LevelMap_Ext_LightHouse_1",
        "DUM_Lighthouse2" => "LevelMap_Ext_LightHouse_2",
        "DUM_Lighthouse3" => "LevelMap_Ext_LightHouse_3",
        "DUM_OverseerExterior" => "LevelMap_Ext_Overseer",
        "DUM_OverseerInterior1" => "LevelMap_Int_Overseer_1",
        "DUM_OverseerInterior2" => "LevelMap_Int_Overseer_2",
        "DUM_Streets" => "LevelMap_Ext_Streets",
        "DUM_TowerReturnInterior" => "LevelMap_Int_TowerReturn",
        "DUM_TowerReturnInterior2" => "LevelMap_Int_TowerReturn_02",
        _ => return None,
    };
    // `DUM_OverseerInterior1` -> "Overseer Interior 1"
    let mut title = String::new();
    for (i, c) in m.trim_start_matches("DUM_").chars().enumerate() {
        if i > 0 && (c.is_ascii_uppercase() || c.is_ascii_digit() && !title.ends_with(' ')) {
            title.push(' ');
        }
        title.push(c);
    }
    Some((title, image))
}

/// Mission clues (chapter notes) and written notes: clues first.
pub(crate) fn is_clue(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    k.contains("chapterlog") || k.contains("chapternotes") || k.starts_with("hub.") || k.starts_with("boyle.") && k.contains("notes")
}
