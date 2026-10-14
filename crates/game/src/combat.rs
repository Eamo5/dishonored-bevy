//! Player melee: Corvo's sword (view model), attacks, assassinations, blocking/parrying,
//! chokeholds, and receiving damage.

use crate::gameplay::{HitKind, HudMessages, Noise, NoiseKind, NpcHit, NpcStagger, PlayerHit, PlayerStats};
use crate::level::{GameAssets, LevelSpawnSet};
use crate::npc::{Alert, Mode, Npc};
use crate::player::{Player, PlayerCamera};
use crate::world_light::{LitActor, WorldLighting};
use crate::GameState;
use bevy::camera::visibility::RenderLayers;
use bevy::light::NotShadowCaster;
use bevy::mesh::MeshTag;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions};
use bevy_rapier3d::prelude::{CollisionGroups, Group, QueryFilter, ReadRapierContext};
use std::f32::consts::FRAC_PI_2;

pub struct CombatPlugin;

impl Plugin for CombatPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Specials>()
            .init_resource::<Versus>()
            .init_resource::<FinisherPace>()
            .add_systems(OnEnter(GameState::InGame), spawn_view_model.after(LevelSpawnSet).after(crate::player::spawn_player))
            .add_systems(
                Update,
                (sword_input, assassinating, chokehold, receive_hits, versus, animate_view_model, special_moves).chain().after(crate::gadgets::grenade_focus).run_if(in_state(GameState::InGame)),
            );
    }
}

pub const VIEW_LAYER: usize = 1;

/// A sword lock (`StatePlayerMasterVersus`): Corvo's blow and a guard's met (their attack
/// zones within `m_fVersusZoneMaxTime` of each other). Corvo mashes [Primary]
/// (`DUI_Context_Versus`: "Mash `GBA_Primary` !") and his presses, against his
/// `MeleeVersusMiniGame_WinBig` / `_WinMedium` / `_LoseMedium`, settle it: a big win leaves
/// the guard reeling (a fatality's opening), a medium one staggers it, a stand-off parts the
/// blades, fewer presses and Corvo is thrown back.
#[derive(Resource, Default)]
pub struct Versus {
    pub npc: Option<Entity>,
    pub t: f32,
    pub presses: u32,
    /// locks begun and settled (the arms play each), and how the last went (1 big win,
    /// 2 medium win, 3 stand-off, 4 lost)
    pub seq: u32,
    pub done_seq: u32,
    pub outcome: u8,
}

/// How long the blades stay locked (s).
const VERSUS_TIME: f32 = 1.6;
/// How near in time the two blows must land to lock (s: `m_fVersusZoneMaxTime` is 0.07 on
/// Corvo's attack zones; widened for the guards' coarser blow timing).
const VERSUS_ZONE: f32 = 0.12;
/// When Corvo's blow lands into his swing (s): in the last `m_fDamageZoneMaxTime` (0.05 s) of
/// his attack clips' `DishonoredNotify_AttackZone` (`Sword_Ready_Attack{Right,Left}_A_Small` end
/// at 0.346 / 0.343 s), as the original's zone handler opens it (roshan's r614/blink research
/// decoded it and measured the blow 0.30-0.35 s after the press).
const PLAYER_HIT: f32 = 0.29;
/// How far ahead of the eye a wall stops Corvo's blade (m: our reading of his swing's sweep).
const BLADE_WALL_REACH: f32 = 1.3;
/// The view's jolts (uu/s, `player::CameraShake`): his blade off a wall
/// (`DisTweaks_MeleeAttackPlayer.m_fCamShake_OnHitEnv`), off a parrying blade
/// (`DisTweaks_MeleeAttack.m_fCamShake_OnParry`), and a blocked blow's damage times
/// `DisTweaks_MeleeBlockPlayer.m_fBlockCameraShakeScale`.
const CAM_SHAKE_ON_HIT_ENV: f32 = 200.0;
const CAM_SHAKE_PARRIED: f32 = 175.0;
const CAM_SHAKE_BLOCK_SCALE: f32 = 6.0;

/// The sound of Corvo's sword on what it struck: the contact system's for its surface, else the
/// plain environment's.
fn sword_impact_sound<'a>(scene: &'a dhcook::format::Scene, struck: &str) -> Option<&'a str> {
    let find = |s: &str| scene.sword_impacts.iter().find(|i| i.against == s && !i.sound.is_empty()).map(|i| i.sound.as_str());
    find(struck).or_else(|| struck.starts_with("Env_").then(|| find("Environment")).flatten())
}

/// Can these two blows lock? A guard's blade blow (not a kick or a leap) landing within the
/// zone of Corvo's, each facing the other (Corvo within `m_fVersusAngle` 20 degrees of it, it
/// within its parry tweak's 90).
fn can_lock(npc: &Npc, nt: &Transform, at: Vec3, fwd: Vec3, dt_hit: impl Fn(f32) -> bool) -> bool {
    let Some((swing, t)) = npc.swing.as_ref().zip(npc.attack_t) else { return false };
    if npc.mode != Mode::Combat || !npc.has_sword || swing.no_versus || matches!(swing.kind, crate::npc::MoveKind::Bash | crate::npc::MoveKind::Jump) || !dt_hit(swing.hit - t) {
        return false;
    }
    let to_npc = (nt.translation - at).with_y(0.0).normalize_or_zero();
    fwd.with_y(0.0).normalize_or_zero().dot(to_npc) > 20f32.to_radians().cos() && npc.forward().dot(-to_npc) > 45f32.to_radians().cos()
}

/// The side a blow from `from` comes at Corvo (at `at`, facing `yaw`): 0 ahead, 1 behind,
/// 2 left, 3 right (`ECardinalDirection`, as the hit reactions are chosen).
pub fn side_of(from: Vec3, at: Vec3, yaw: f32) -> u8 {
    let to = (from - at).with_y(0.0);
    let fwd = Quat::from_rotation_y(yaw) * Vec3::NEG_Z;
    let (f, r) = (to.dot(fwd), to.dot(fwd.cross(Vec3::Y)));
    if f.abs() >= r.abs() {
        if f >= 0.0 { 0 } else { 1 }
    } else if r >= 0.0 {
        3
    } else {
        2
    }
}

/// The blades lock: the guard's blow is spent, both hold.
fn lock_blades(v: &mut Versus, e: Entity, npc: &mut Npc, at: Vec3, sfx: &mut MessageWriter<crate::audio::PostEvent>, noise: &mut MessageWriter<Noise>) {
    v.npc = Some(e);
    v.t = 0.0;
    v.presses = 0;
    v.seq += 1;
    npc.attack_t = None;
    npc.swing = None;
    npc.versus = true;
    npc.gesture(4);
    info!("sword lock with {}", npc.name);
    sfx.write(crate::audio::PostEvent::named("Snd_Imp_Sword_vs_Sword_ak", Some(at)));
    noise.write(Noise { pos: at, radius: 8.0, kind: NoiseKind::Threatening });
}

/// The lock under way: Corvo's presses, then how it ends.
#[allow(clippy::too_many_arguments)]
fn versus(
    time: Res<Time>,
    paused: Res<crate::hud::Paused>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut v: ResMut<Versus>,
    data: Res<crate::gamedata::Data>,
    stats: Res<PlayerStats>,
    mut player: Query<(&Transform, &mut Player, &mut Sword), Without<Npc>>,
    mut npcs: Query<(&mut Npc, &Transform), Without<Player>>,
    mut msgs: ResMut<HudMessages>,
    mut sfx: MessageWriter<crate::audio::PostEvent>,
) {
    if paused.0 { return; }
    let Some(e) = v.npc else { return };
    let Ok((pt, mut p, mut sword)) = player.single_mut() else { return };
    let Ok((mut npc, nt)) = npcs.get_mut(e) else {
        v.npc = None;
        return;
    };
    if npc.is_down() || stats.dead {
        npc.versus = false;
        v.npc = None;
        return;
    }
    v.t += time.delta_secs();
    if mouse.just_pressed(MouseButton::Left) {
        v.presses += 1;
    }
    // Corvo faces it at a blade's length, his eyes level
    let to = (nt.translation - pt.translation).with_y(0.0);
    let k = (time.delta_secs() * 10.0).min(1.0);
    if to.length() > 0.1 {
        let want = (-to.x).atan2(-to.z);
        let d = (want - p.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
        p.yaw += d * k;
    }
    p.pitch += (-0.05 - p.pitch) * k;
    let gap = (to.length() - 1.3).clamp(-1.0, 1.0);
    let pull = to.normalize_or_zero() * gap * 6.0;
    p.velocity = Vec3::new(pull.x, p.velocity.y, pull.z);
    sword.swing = 0.0;
    sword.blocking = false;
    let attr = |k: &str, d: f32| data.0.attributes.get(k).map(|a| a[1]).unwrap_or(d);
    let (big, medium, lose) = (attr("MeleeVersusMiniGame_WinBig", 10.0), attr("MeleeVersusMiniGame_WinMedium", 5.0), attr("MeleeVersusMiniGame_LoseMedium", 2.0));
    // (the Sword Crossing upgrade, `Twk_Upgrade_Sword`'s `VersusMiniGameBonus` 3: so many presses
    // to the good, as we read "more effective when locking blades")
    let bonus: f32 = data.0.upgrades.iter().filter(|u| stats.upgrades.contains(&u.id)).flat_map(|u| &u.attributes).filter(|(a, _)| a == "VersusMiniGameBonus").map(|(_, b)| *b).sum();
    let n = v.presses as f32 + bonus.max(0.0);
    if n < big && v.t < VERSUS_TIME {
        return;
    }
    v.npc = None;
    npc.versus = false;
    v.done_seq = v.seq;
    let away = (pt.translation - nt.translation).with_y(0.0).normalize_or_zero();
    v.outcome = if n >= big {
        // its guard broken wide: the next blow is a fatality
        npc.stagger = 2.0;
        npc.gesture(5);
        msgs.push("Sword lock won");
        1
    } else if n >= medium {
        npc.stagger = 0.9;
        npc.gesture(6);
        2
    } else if n >= lose {
        // the blades part
        npc.stagger = 0.4;
        npc.attack_cd = npc.attack_cd.max(0.6);
        sword.recoil = 0.3;
        p.velocity += away * 2.0;
        3
    } else {
        // Corvo is thrown back, off balance, and it presses on
        npc.gesture(7);
        npc.attack_cd = 0.0;
        sword.recoil = 0.9;
        p.velocity += away * 5.0;
        sfx.write(crate::audio::PostEvent::named("Snd_Imp_Sword_vs_Sword_ak", Some(nt.translation + Vec3::Y)));
        4
    };
    info!("sword lock: {} presses in {:.2} s -> {}", v.presses, v.t, ["", "big win", "win", "stand-off", "lost"][v.outcome as usize]);
}

/// What Corvo could do to someone now, as his attacks would find them (the HUD's special
/// interaction icons): assassinate, choke, a drop assassination, a Blood Thirst fatality.
#[derive(Resource, Default)]
pub struct Specials {
    pub assassinate: bool,
    pub choke: bool,
    pub drop: bool,
    pub adrenaline: bool,
}

/// The moves within reach (the targets the attack and the chokehold would take).
#[allow(clippy::too_many_arguments)]
fn special_moves(
    mut sp: ResMut<Specials>,
    (stats, attrs, possession, carry, held): (Res<PlayerStats>, Res<crate::gamedata::Attrs>, Res<crate::possession::Possession>, Res<crate::carry::Carry>, Res<crate::props::Held>),
    player: Query<(&Transform, &Player), Without<Npc>>,
    grabbed: Query<(), With<crate::npc::Grabbed>>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    npcs: Query<(Entity, &Npc, &Transform), Without<Player>>,
) {
    *sp = Specials::default();
    let (Ok((pt, p)), Ok(cg)) = (player.single(), cam.single()) else { return };
    if stats.dead || p.locked || possession.host.is_some() || carry.carrying() || held.busy() || !grabbed.is_empty() {
        return;
    }
    let eye = cg.translation();
    let fwd = cg.forward().as_vec3();
    let armed = !stats.sheathed && !stats.unarmed;
    // falling onto someone
    if armed && !p.grounded && p.velocity.y < -2.5 {
        let feet = pt.translation - Vec3::Y * (crate::player::STAND_HALF + crate::player::RADIUS);
        sp.drop = npcs.iter().filter(|(_, n, _)| !n.is_down() && n.kind != crate::npc::Kind::Creature).any(|(_, _, nt)| {
            let d = nt.translation + Vec3::Y * crate::npc::NPC_CENTER - feet;
            d.with_y(0.0).length() < 1.6 && d.y < 0.6 && d.y > -6.0
        });
        return;
    }
    let to_player = |nt: &Transform| (pt.translation - nt.translation).with_y(0.0).normalize_or_zero();
    // the sword's reach: someone unaware of him, from behind
    if armed {
        if let Some((_, npc, nt)) = nearest_target(eye, fwd, 2.4, 0.55, npcs.iter()).and_then(|(e, _)| npcs.get(e).ok()) {
            let behind = npc.forward().dot(to_player(nt)) < -0.2;
            let unaware = npc.alert != Alert::Combat || !npc.sees_player;
            sp.assassinate = unaware && (behind || npc.alert == Alert::Unaware) && npc.mode != Mode::Combat;
        }
        if stats.power("BloodThirsty") > 0 && stats.adrenaline >= attrs.adrenaline_max {
            sp.adrenaline = nearest_target(eye, fwd, 2.6, 0.5, npcs.iter()).is_some();
        }
    }
    // the chokehold's: from behind, or someone off balance
    if let Some((_, npc, nt)) = nearest_target(eye, fwd, 1.7, 0.6, npcs.iter()).and_then(|(e, _)| npcs.get(e).ok()) {
        let behind = npc.forward().dot(to_player(nt)) < 0.0;
        let fighting = npc.mode == Mode::Combat && npc.sees_player;
        sp.choke = !npc.is_down() && npc.kind != crate::npc::Kind::Story && if fighting { npc.stagger > 0.0 } else { behind || npc.alert == Alert::Unaware };
    }
}

#[derive(Component, Default)]
pub struct Sword {
    /// > 0 while a swing is in progress (seconds elapsed)
    pub swing: f32,
    pub swing_dir: f32,
    pub hit_done: bool,
    pub hold: f32,
    pub blocking: bool,
    pub block_t: f32,
    /// after a tap parry, a plain block for a while (`m_fForceBlockTimeOnPerfectParryTap`):
    /// no other parry until it ends
    pub forced_block: f32,
    pub parry_flash: f32,
    pub recoil: f32,
    /// swings stopped short (the arms play each's bounce), and whether the last met the world
    /// (`eDisWeaponMeleeState_BigHit`, `HitEnvironment`)
    pub bounce_seq: u32,
    pub bounce_wall: bool,
    /// drop assassinations made (the arms play each), and the side the victim was taken from
    /// (0 front, 1 back, 2 left, 3 right)
    pub drop_seq: u32,
    pub drop_dir: u8,
    /// ground assassinations made, and Corvo's clip for the last (`Sword_Ready_Assassination_*`)
    pub assassin_seq: u32,
    pub assassin_clip: String,
}

/// A synchronized assassination under way (`Sword_Ready_Assassination_<Fast?><Side><_CarryCorpse?>_Master`
/// with the victim's `Generic_Assassination_..._Slave`): Corvo held still for his clip while
/// the victim, set where its clip's `anchor_jnt` puts him, plays its own.
#[derive(Component)]
pub struct Assassinating {
    pub npc: Entity,
    pub t: f32,
    master: String,
    slave: String,
    len: Option<f32>,
    /// its slow motion played (the kill cam's), its Blood Thirst slow motion
    slomo: bool,
    adrenaline: bool,
    /// real seconds it has taken
    real: f32,
}

#[derive(Component)]
pub struct ViewModel;

/// Animated sword frame inside the view model (the hand's IK target).
#[derive(Component)]
pub struct SwordGrip;

/// The sword meshes' parent: under the grip, or in Corvo's hand once the arms exist.
#[derive(Component)]
pub struct SwordModel;

/// Corvo's sword as its own rig (`PlySword_Anim_as`): it folds and unfolds with the arms'
/// equip clips (`arms::animate_sword`).
#[derive(Component)]
pub struct SwordRig;

/// Seconds of a chokehold before the victim goes limp, without `ChokeTime`: the choke's loop
/// (`Sword_Choke_Loop_Master`, 5.03 s). The original's hold is `ChokeTime` (1.5 s, less with
/// Strong Arms) and then the loop, about 6.5 s, as roshan's r614/blink research decoded it.
pub const CHOKE_TIME: f32 = 5.033;

/// Active chokehold on an NPC.
#[derive(Component)]
pub struct Choking {
    pub npc: Entity,
    pub t: f32,
    pub duration: f32,
}

pub fn spawn_view_model(
    mut commands: Commands,
    assets: Option<Res<GameAssets>>,
    mut wl: Option<ResMut<WorldLighting>>,
    cam: Query<Entity, With<PlayerCamera>>,
    player: Query<Entity, With<Player>>,
    level: Option<Res<crate::level::LevelInfo>>,
) {
    let Ok(cam) = cam.single() else { return };
    let slot = wl.as_mut().map(|w| w.alloc_slot()).unwrap_or(0);
    if let Ok(p) = player.single() {
        commands.entity(p).insert((LitActor { slot, probe_height: 0.5, brightness: 0.4, color: Vec3::splat(0.05), sun: 0.0, dominant: None }, Sword::default()));
    }
    // the view model renders in the scene camera as UE3's foreground group (its materials
    // project it with their own field of view in front of everything)
    commands.entity(cam).insert((RenderLayers::from_layers(&[0, VIEW_LAYER]), IsDefaultUiCamera));
    let root = commands
        .spawn((ViewModel, Transform::from_xyz(0.24, -0.21, -0.42), Visibility::default(), RenderLayers::layer(VIEW_LAYER)))
        .id();
    commands.entity(cam).add_child(root);
    if let Some(parts) = assets.as_ref().and_then(|a| a.props.get("player_sword")) {
        // the sword mesh points along +X from the grip: aim it forward and slightly up
        let grip = commands
            .spawn((
                SwordGrip,
                Transform::from_rotation(Quat::from_rotation_x(0.35) * Quat::from_rotation_y(FRAC_PI_2) * Quat::from_rotation_x(FRAC_PI_2)),
                Visibility::default(),
                RenderLayers::layer(VIEW_LAYER),
            ))
            .id();
        commands.entity(root).add_child(grip);
        let model = commands.spawn((SwordModel, Transform::IDENTITY, Visibility::default(), RenderLayers::layer(VIEW_LAYER))).id();
        commands.entity(grip).add_child(model);
        // its own rig where the map has it cooked (the folding blade), else the sword at rest
        let rig = level
            .as_ref()
            .and_then(|l| l.scene.prop_rigs.get("player_sword"))
            .and_then(|t| assets.as_ref().and_then(|a| a.npc_types.get(*t as usize)))
            .and_then(|v| v.as_ref());
        if let Some(rv) = rig {
            let bones = &rv.skeleton.bones;
            let joints: Vec<Entity> = bones
                .iter()
                .map(|b| commands.spawn((Transform::from_translation(Vec3::from(b.translation)).with_rotation(Quat::from_array(b.rotation).normalize()), Visibility::Inherited)).id())
                .collect();
            for (i, b) in bones.iter().enumerate() {
                let parent = if b.parent >= 0 && (b.parent as usize) < i { joints[b.parent as usize] } else { model };
                commands.entity(parent).add_child(joints[i]);
            }
            for (mesh, mat) in &rv.parts.view_parts {
                let mut m = commands.spawn((
                    Mesh3d(mesh.clone()),
                    MeshTag(slot),
                    bevy::mesh::skinning::SkinnedMesh { inverse_bindposes: rv.inverse_bindposes.clone(), joints: joints.clone() },
                    NotShadowCaster,
                    bevy::camera::visibility::NoFrustumCulling,
                    RenderLayers::layer(VIEW_LAYER),
                ));
                mat.apply(&mut m);
                let m = m.id();
                commands.entity(model).add_child(m);
            }
            if let Some(lib) = rv.anims.clone() {
                let mut anim = crate::anim::Animator::new(lib, &rv.skeleton, joints);
                anim.sounds = false;
                commands.entity(model).insert((SwordRig, anim));
            }
        } else {
            for (mesh, mat) in &parts.view_parts {
                let mut m = commands.spawn((Mesh3d(mesh.clone()), MeshTag(slot), NotShadowCaster, RenderLayers::layer(VIEW_LAYER)));
                mat.apply(&mut m);
                let m = m.id();
                commands.entity(model).add_child(m);
            }
        }
    }
}

/// May Corvo assassinate it (`DisItemContext_Assassinate`'s `m_AssassinateOnAwareness`, as
/// roshan's r614/blink research decoded it): unaware, surprised, suspicious or begging, from any
/// side; not fighting him, fearful or choked, and not running (`m_bCanAssassinateRunners` off).
fn assassinable(npc: &Npc) -> bool {
    let fighting = npc.mode == Mode::Combat && npc.alert == Alert::Combat;
    let running = npc.velocity.with_y(0.0).length() > (npc.gait[1] + npc.gait[2]) * 0.5;
    !fighting && !running && !matches!(npc.mode, Mode::Flee | Mode::Choked)
}

fn nearest_target<'a>(
    eye: Vec3,
    fwd: Vec3,
    range: f32,
    min_dot: f32,
    npcs: impl Iterator<Item = (Entity, &'a Npc, &'a Transform)>,
) -> Option<(Entity, f32)> {
    targets_within(eye, fwd, range, min_dot, npcs).first().copied()
}

/// Everyone standing within `range` of the eye and before it (`min_dot` off the view), the
/// nearest first.
fn targets_within<'a>(eye: Vec3, fwd: Vec3, range: f32, min_dot: f32, npcs: impl Iterator<Item = (Entity, &'a Npc, &'a Transform)>) -> Vec<(Entity, f32)> {
    let f = fwd.with_y(0.0).normalize_or_zero();
    let mut out: Vec<(Entity, f32)> = npcs
        .filter(|(_, npc, _)| !npc.is_down())
        .filter_map(|(e, _, t)| {
            let to = t.translation + Vec3::Y * 0.3 - eye;
            let d = to.length();
            (d <= range && (to.with_y(0.0).normalize_or_zero().dot(f) >= min_dot || d <= 0.8)).then_some((e, d))
        })
        .collect();
    out.sort_by(|a, b| a.1.total_cmp(&b.1));
    out
}

/// A finisher's pair for a blow from before the victim (`DisTweaks_Fatality`): its own dramatic
/// death for one who has it (the targets, Daud), else one of the front finishers, the small
/// ones on the side the blade swings (`Sword_Ready_Fatality_*_Master` with
/// `Generic_Fatality_*_Slave`). (The beheadings, `m_fBeheadRandomChance`, wait for the heads
/// to come off: the original swaps in a severed head the bodies here don't have.)
/// The finishers' chance of a beheading (`m_fBeheadRandomChance`; `DH_BEHEAD` for tests: always).
fn behead_chance(data: &crate::gamedata::Data) -> f32 {
    if std::env::var("DH_BEHEAD").is_ok() {
        1.0
    } else {
        data.pawn("fatality.m_fBeheadRandomChance", 0.25)
    }
}

fn fatality_pair(lib: &crate::anim::CharAnims, full: bool, behind: bool, behead: f32) -> Option<(String, String)> {
    if let Some(slave) = lib.find_like("DramaticDeath_Front_", "_Slave") {
        return Some((format!("Sword_{}", slave.replace("_Slave", "_Master")), slave));
    }
    // a tallboy's own, before it or behind it (`Generic_Fatality_Tallboy_*_Slave`)
    let tallboy = format!("Generic_Fatality_Tallboy_{}_Slave", if behind { "Back" } else { "Front" });
    if lib.find(&tallboy).is_some() {
        return Some((format!("Sword_Ready_Fatality_Tallboy_{}_Master", if behind { "Back" } else { "Front" }), tallboy));
    }
    // a beheading now and then (`m_HeadChop` by `m_fBeheadRandomChance`: Behead A or B), where
    // the victim has its part
    if rand::random::<f32>() < behead {
        let pick = if rand::random::<bool>() { "Behead_A" } else { "Behead_B" };
        let slave = format!("Generic_Fatality_{pick}_Slave");
        if lib.find(&slave).is_some() {
            return Some((format!("Sword_Ready_Fatality_{pick}_Master"), slave));
        }
    }
    // Corvo's sword's set (`m_Front` and `m_Front_Fast`: his other sides have none): a full
    // finisher or a fast one, else the other kind where the victim has no part in it
    let has = |set: &[&str]| -> Vec<String> { set.iter().map(|k| k.to_string()).filter(|k| lib.find(&format!("Generic_Fatality_{k}_Slave")).is_some()).collect() };
    let (fulls, fasts) = (has(&["Front_A", "Front_B"]), has(&["FastFront_A", "FastFront_B", "FastFront_C", "FastFront_D"]));
    let (want, other) = if full { (fulls, fasts) } else { (fasts, fulls) };
    let options = if want.is_empty() { other } else { want };
    let pick = options.get(rand::random_range(0..options.len().max(1)))?;
    Some((format!("Sword_Ready_Fatality_{pick}_Master"), format!("Generic_Fatality_{pick}_Slave")))
}

/// Corvo's finishers' pacing (`DisItemContext_Finisher`, as roshan's r614/blink research
/// decoded it): the fast ones left before a full one (`m_NumFinishersBeforeSlow`, 4-8), the
/// time after which a full one is due anyway once a fast one has played (`m_fTimeBeforeSlow`,
/// 90-120 s), when the last full one played and when the last in slow motion.
#[derive(Resource, Default)]
pub struct FinisherPace {
    fast_left: i32,
    full_after: f32,
    last_full: f32,
    seen_fast: bool,
    last_slomo: Option<f32>,
    primed: bool,
}

/// A blow's finisher rolls: a slow-motion finisher (`m_fSlomoFinisherRandomChance`, not within
/// `m_fMinTimeBetweenRandomSlomoFinisher` of the last, and only with the one foe on Corvo; never
/// for Blood Thirst's), and whether one would play at normal speed after all
/// (`m_fChanceSlomoFinisherWillBeNormalSpeed`).
#[derive(Clone, Copy)]
struct FinisherRoll {
    slomo: bool,
    realtime: bool,
}

impl FinisherPace {
    fn reset(&mut self, data: &crate::gamedata::Data, now: f32) {
        let (lo, hi) = (data.pawn("finisher.m_NumFinishersBeforeSlow_Min", 4.0), data.pawn("finisher.m_NumFinishersBeforeSlow_Max", 8.0));
        self.fast_left = (lo + rand::random::<f32>() * (hi - lo)) as i32;
        let (lo, hi) = (data.pawn("finisher.m_fTimeBeforeSlow_Min", 90.0), data.pawn("finisher.m_fTimeBeforeSlow_Max", 120.0));
        self.full_after = lo + rand::random::<f32>() * (hi - lo);
        self.last_full = now;
        self.seen_fast = false;
        self.primed = true;
    }

    fn roll(&mut self, data: &crate::gamedata::Data, now: f32, one_foe: bool, blood_thirst: bool) -> FinisherRoll {
        if !self.primed {
            self.reset(data, now);
        }
        let recent = self.last_slomo.is_some_and(|t| now - t < data.pawn("fatality.m_fMinTimeBetweenRandomSlomoFinisher", 30.0));
        let slomo = !blood_thirst && !recent && one_foe && rand::random::<f32>() <= data.pawn("fatality.m_fSlomoFinisherRandomChance", 0.75);
        FinisherRoll { slomo, realtime: rand::random::<f32>() <= data.pawn("fatality.m_fChanceSlomoFinisherWillBeNormalSpeed", 0.9) }
    }

    /// A full finisher, not a fast one: no more than the one foe on Corvo, no Bend Time, and
    /// the fast ones spent or the time come after one.
    fn full(&self, now: f32, one_foe: bool, bending: bool) -> bool {
        one_foe && !bending && (self.fast_left <= 0 || (self.seen_fast && now - self.last_full >= self.full_after))
    }

    fn played(&mut self, data: &crate::gamedata::Data, now: f32, full: bool, slomo: bool) {
        if full {
            self.reset(data, now);
        } else {
            self.fast_left -= 1;
            self.seen_fast = true;
        }
        if slomo {
            self.last_slomo = Some(now);
        }
    }
}

/// Whether a slow-motion finisher's clip slows time (`PSI_Gameplay_KillCamMode`): Off never,
/// Normal unless it rolled to play at normal speed, Frequent always.
fn finisher_dilates(mode: u8, roll: FinisherRoll) -> bool {
    roll.slomo
        && match mode {
            0 => false,
            1 => !roll.realtime,
            _ => true,
        }
}

/// A synchronized kill begun (an assassination, a finisher): the victim's clip given it, Corvo's
/// to his arms, and him held for it.
#[allow(clippy::too_many_arguments)]
fn begin_sync_kill(commands: &mut Commands, pe: Entity, sword: &mut Sword, p: &mut Player, e: Entity, master: String, slave: String, (slomo, adrenaline): (bool, bool)) {
    commands.entity(e).try_insert(crate::npc::AssassinClip(slave.clone()));
    sword.assassin_seq += 1;
    sword.assassin_clip = master.clone();
    sword.swing = 0.0;
    p.locked = true;
    p.velocity = Vec3::ZERO;
    commands.entity(pe).insert(Assassinating { npc: e, t: 0.0, master, slave, len: None, slomo, adrenaline, real: 0.0 });
}

/// Whether a finisher plays in slow motion (`PSI_Gameplay_KillCamMode`, the fatality tweak's
/// `m_fSlomoFinisherRandomChance`): never, on the last foe fighting Corvo (else one in three),
/// or every time.
pub(crate) fn kill_cam(mode: u8, last_foe: bool) -> bool {
    match mode {
        0 => false,
        1 => last_foe || rand::random::<f32>() < 0.33,
        _ => true,
    }
}

#[allow(clippy::too_many_arguments)]
fn sword_input(
    time: Res<Time>,
    mouse: Res<ButtonInput<MouseButton>>,
    (keys, bind): (Res<ButtonInput<KeyCode>>, Res<crate::bindings::Bindings>),
    cursor: Single<&CursorOptions>,
    scripted: Option<Res<crate::script::Scripted>>,
    (mut stats, attrs, possession, paused): (ResMut<PlayerStats>, Res<crate::gamedata::Attrs>, Res<crate::possession::Possession>, Res<crate::hud::Paused>),
    mut msgs: ResMut<HudMessages>,
    (mut hits, mut noise, mut sfx, mut rats): (MessageWriter<NpcHit>, MessageWriter<Noise>, MessageWriter<crate::audio::PostEvent>, MessageWriter<crate::swarm::KillRats>),
    mut player: Query<(Entity, &Transform, &mut Player, &mut Sword), Without<Npc>>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    mut npcs: Query<(Entity, &mut Npc, &Transform), Without<Player>>,
    (mut grabbed_q, carry, mut commands): (Query<&mut crate::npc::Grabbed>, Res<crate::carry::Carry>, Commands),
    (strikeables, mut struck, held, mut world_hits): (Query<(&crate::gameplay::Strikeable, &GlobalTransform)>, MessageWriter<crate::gameplay::Struck>, Res<crate::props::Held>, MessageWriter<crate::worlddamage::WorldDamage>),
    (mut versus, rigs, settings, data, mut fx, level): (
        ResMut<Versus>,
        Query<&crate::anim::Animator, With<Npc>>,
        Res<crate::settings::Settings>,
        Res<crate::gamedata::Data>,
        MessageWriter<crate::particles::SpawnEffect>,
        Option<Res<crate::level::LevelInfo>>,
    ),
    (mut pace, clock, rapier, surfaces, mut shake): (ResMut<FinisherPace>, Res<crate::gameplay::TimeControl>, ReadRapierContext, Query<&crate::footsteps::ColliderSurfaces>, MessageWriter<crate::player::ShakeCamera>),
) {
    if paused.0 { return; }
    let Ok((pe, pt, mut p, mut sword)) = player.single_mut() else { return };
    if std::env::var("DH_ATTACK_LOG").is_ok() && mouse.just_pressed(MouseButton::Left) {
        info!("attack: versus {} busy {} carrying {} dead {} locked {} host {} unarmed {} sheathed {}", versus.npc.is_some(), held.busy(), carry.carrying(), stats.dead, p.locked, possession.host.is_some(), stats.unarmed, stats.sheathed);
    }
    // locked blades: the presses are the lock's
    if versus.npc.is_some() {
        return;
    }
    // (with a body on the shoulder only an assassination is possible)
    let carrying = carry.carrying();
    if held.busy() || (carrying && !mouse.just_pressed(MouseButton::Left)) {
        sword.blocking = false;
        return;
    }
    let dt = time.delta_secs();
    sword.parry_flash = (sword.parry_flash - dt).max(0.0);
    sword.forced_block = (sword.forced_block - dt).max(0.0);
    sword.recoil = (sword.recoil - dt).max(0.0);
    // held by a weeper: attacks are spent struggling free
    if let Ok(mut g) = grabbed_q.single_mut() {
        if mouse.just_pressed(MouseButton::Left) && !stats.dead {
            g.presses += 1;
            sfx.write(crate::audio::PostEvent::named("Snd_P_Clothes_Grab_01_Cue_ak", Some(pt.translation)));
        }
        sword.blocking = false;
        return;
    }
    if stats.dead || p.locked || possession.host.is_some() || stats.unarmed {
        sword.blocking = false;
        return;
    }
    let grabbed = cursor.grab_mode != CursorGrabMode::None || scripted.is_some();
    // empty hands: attacking or blocking draws the sword (and does nothing more)
    if stats.sheathed {
        sword.blocking = false;
        if grabbed && (mouse.just_pressed(MouseButton::Left) || keys.just_pressed(bind.key(crate::bindings::Act::Block))) {
            stats.sheathed = false;
        }
        return;
    }
    let Ok(cg) = cam.single() else { return };
    let eye = cg.translation();
    let fwd = cg.forward().as_vec3();
    // an assassination: an unaware victim in reach, from behind (or unaware of him at all);
    // its side as the victim sees Corvo, quick unless he's sneaking, the carried body's
    // variant with one on his shoulder
    if grabbed && p.grounded && mouse.just_pressed(MouseButton::Left) && !keys.pressed(bind.key(crate::bindings::Act::Block)) {
        let target = nearest_target(eye, fwd, 2.75, 0.55, npcs.iter().map(|(e, n, t)| (e, n, t)));
        if let Some((e, _)) = target {
            let (_, mut npc, nt) = npcs.get_mut(e).unwrap();
            let from = (pt.translation - nt.translation).with_y(0.0).normalize_or_zero();
            let f = npc.forward();
            // (the targets and nobles die their own dramatic deaths: `DramaticDeath_*_Slave`
            // with Corvo's `Sword_DramaticDeath_*_Master`, before him or behind him)
            let lib = rigs.get(e).ok().map(|a| a.lib.clone());
            let front = from.dot(f) >= 0.0;
            let dramatic = lib.as_ref().and_then(|l| {
                let slave = if front {
                    l.find_like("DramaticDeath_Front_", "_Slave").or_else(|| l.find_like("Generic_DramaticDeath_Front_", "_Slave"))
                } else {
                    l.find_like("DramaticDeath_Back_", "_Slave").or_else(|| l.find_like("Generic_DramaticDeath_Back_", "_Slave"))
                }?;
                let master = format!("Sword_{}", slave.trim_start_matches("Generic_").replace("_Slave", "_Master"));
                Some((master, slave))
            });
            let human = npc.kind != crate::npc::Kind::Creature && (npc.kind != crate::npc::Kind::Story || dramatic.is_some());
            if human && assassinable(&npc) {
                let right = f.cross(Vec3::Y);
                let side = if from.dot(f) > 0.707 {
                    "Front"
                } else if from.dot(f) < -0.707 {
                    "Back"
                } else if from.dot(right) < 0.0 {
                    "Left"
                } else {
                    "Right"
                };
                let pace = if p.crouched { "" } else { "Fast" };
                let carried = if carrying { "_CarryCorpse" } else { "" };
                let (master, slave) = dramatic.unwrap_or_else(|| (format!("Sword_Ready_Assassination_{pace}{side}{carried}_Master"), format!("Generic_Assassination_{pace}{side}{carried}_Slave")));
                // (one with no part in it, a tallboy: the blow lands as it always did)
                if lib.as_ref().is_some_and(|l| l.find(&slave).is_some()) {
                    hits.write(NpcHit { npc: e, damage: 999.0, kind: HitKind::Assassinate, from: pt.translation });
                    npc.attack_t = None;
                    let last = !others_fighting(&npcs, e, pt.translation);
                    begin_sync_kill(&mut commands, pe, &mut sword, &mut p, e, master, slave, (kill_cam(settings.kill_cam, last), false));
                    return;
                }
            }
        }
    }
    if carrying {
        sword.blocking = false;
        return;
    }
    // a drop assassination: attacking while falling onto someone
    if grabbed && mouse.pressed(MouseButton::Left) && !p.grounded && p.velocity.y < 0.0 && sword.swing == 0.0 {
        let feet = pt.translation - Vec3::Y * (crate::player::STAND_HALF + crate::player::RADIUS);
        let below = npcs
            .iter()
            .filter(|(_, n, _)| !n.is_down() && n.kind != crate::npc::Kind::Creature)
            .map(|(e, n, nt)| (e, n, nt, nt.translation + Vec3::Y * crate::npc::NPC_CENTER - feet))
            .filter(|(_, _, _, d)| d.with_y(0.0).length() < 1.6 && d.y < 0.6 && d.y > -3.5)
            .min_by(|a, b| a.3.length().total_cmp(&b.3.length()));
        if let Some((e, n, nt, _)) = below {
            // the side Corvo came from, as the victim faces
            let from = (pt.translation - nt.translation).with_y(0.0).normalize_or_zero();
            let f = n.forward();
            let right = f.cross(Vec3::Y);
            let dir = if from.dot(f) > 0.707 {
                0
            } else if from.dot(f) < -0.707 {
                1
            } else if from.dot(right) < 0.0 {
                2
            } else {
                3
            };
            hits.write(NpcHit { npc: e, damage: 999.0, kind: HitKind::Assassinate, from: pt.translation });
            commands.entity(e).try_insert((crate::npc::DropKilled(dir), crate::npc::DropHeight((p.air_peak - pt.translation.y).max(0.0))));
            sfx.write(crate::audio::PostEvent::named("Snd_Imp_Sword_on_Body_cue_ak", Some(nt.translation + Vec3::Y)));
            sword.drop_seq += 1;
            sword.drop_dir = dir;
            // Falling Star: a little mana
            if attrs.drop_mana > 0.0 {
                stats.mana = (stats.mana + attrs.drop_mana).min(stats.max_mana);
            }
            msgs.push("Drop Assassination");
            // the body breaks the fall
            p.velocity.y = p.velocity.y.max(-3.0);
            p.fall_speed = 0.0;
            return;
        }
    }
    // Blood Thirst: with adrenaline full, Block + Primary is an instant fatality
    if grabbed && stats.power("BloodThirsty") > 0 && stats.adrenaline >= attrs.adrenaline_max && keys.pressed(bind.key(crate::bindings::Act::Block)) && mouse.just_pressed(MouseButton::Left) {
        if let Some((e, _)) = nearest_target(eye, fwd, 2.6, 0.5, npcs.iter()) {
            hits.write(NpcHit { npc: e, damage: 999.0, kind: HitKind::Fatality, from: pt.translation });
            stats.adrenaline = 0.0;
            msgs.push("Fatality");
            // (the finisher played out, as the counters' are)
            let behind = npcs.get(e).is_ok_and(|(_, n, t)| n.forward().dot((pt.translation - t.translation).with_y(0.0).normalize_or_zero()) < -0.2);
            let (now, one_foe) = (time.elapsed_secs(), !others_fighting(&npcs, e, pt.translation));
            let roll = pace.roll(&data, now, one_foe, true);
            let full = pace.full(now, one_foe, clock.bend_remaining > 0.0);
            let pair = rigs.get(e).ok().filter(|_| npcs.get(e).is_ok_and(|(_, n, _)| n.kind != crate::npc::Kind::Creature)).and_then(|a| fatality_pair(&a.lib, full, behind, behead_chance(&data)));
            if let Some((master, slave)) = pair {
                // (its clip's adrenaline slow motion plays whatever the rolls)
                pace.played(&data, now, full, roll.slomo);
                begin_sync_kill(&mut commands, pe, &mut sword, &mut p, e, master, slave, (finisher_dilates(settings.kill_cam, roll), true));
            } else {
                sfx.write(crate::audio::PostEvent::named("Snd_Imp_Sword_on_Body_cue_ak", Some(pt.translation + Vec3::Y)));
                sword.swing = 0.001;
                sword.hit_done = true;
            }
            return;
        }
    }
    stats.adrenaline = stats.adrenaline.min(attrs.adrenaline_max);
    if sword.swing > 0.0 {
        sword.swing += dt * attrs.melee_rate;
        if !sword.hit_done && sword.swing > PLAYER_HIT {
            sword.hit_done = true;
            rats.write(crate::swarm::KillRats { at: eye + fwd * 1.5, radius: 0.9, by_player: true, source: eye });
            // the blade's reach: 210 uu, growing with his forward speed to 283.5 at a sprint (as
            // roshan's r614/blink research measured), from the eye to their middle
            let ahead = p.velocity.with_y(0.0).dot(fwd.with_y(0.0).normalize_or_zero()).max(0.0);
            let reach = 2.1 * (1.0 + 0.35 * (ahead / attrs.sprint.max(0.1)).clamp(0.0, 1.0));
            let reach = (reach * reach + 0.25).sqrt();
            // (the blade's sweep meets everyone in its reach, each once a swing: the nearest takes
            // the blow as chosen, the others its cut, `m_AlreadyHitActors`, as roshan's
            // r614/blink research decoded it)
            let swept = targets_within(eye, fwd, reach, 0.55, npcs.iter());
            if let Some(&(e, _)) = swept.first() {
                // (a finisher's rolls as the blow begins: one that kills is a finisher only in
                // slow motion, one after Corvo's parry always)
                let (now, one_foe) = (time.elapsed_secs(), !others_fighting(&npcs, e, pt.translation));
                let roll = pace.roll(&data, now, one_foe, false);
                let (_, mut npc, nt) = npcs.get_mut(e).unwrap();
                let to_player = (pt.translation - nt.translation).with_y(0.0).normalize_or_zero();
                let behind = npc.forward().dot(to_player) < -0.2;
                if assassinable(&npc) {
                    hits.write(NpcHit { npc: e, damage: 999.0, kind: HitKind::Assassinate, from: pt.translation });
                } else if npc.stagger > 1.0 || (roll.slomo && attrs.sword_damage >= npc.health && npc.min_health <= 0.0 && npc.kind != crate::npc::Kind::Creature) {
                    // off balance after Corvo's parry (or dazed), or a blow that kills as a
                    // slow-motion finisher: a finisher (`DisTweaks_Fatality`), its pair played
                    // out where it can be
                    hits.write(NpcHit { npc: e, damage: 999.0, kind: HitKind::Fatality, from: pt.translation });
                    let human = npc.kind != crate::npc::Kind::Creature;
                    npc.attack_t = None;
                    let full = pace.full(now, one_foe, clock.bend_remaining > 0.0);
                    if let Some((master, slave)) = rigs.get(e).ok().filter(|_| human).and_then(|a| fatality_pair(&a.lib, full, behind, behead_chance(&data))) {
                        pace.played(&data, now, full, roll.slomo);
                        begin_sync_kill(&mut commands, pe, &mut sword, &mut p, e, master, slave, (finisher_dilates(settings.kill_cam, roll), false));
                        return;
                    }
                } else if can_lock(&npc, nt, pt.translation, fwd, |d| (-0.02..VERSUS_ZONE).contains(&d)) {
                    // its blow is about to land too: the blades lock
                    let at = nt.translation + Vec3::Y;
                    lock_blades(&mut versus, e, &mut npc, at, &mut sfx, &mut noise);
                } else if npc.mode == Mode::Combat && npc.sees_player && npc.attack_t.is_none() && npc.stagger <= 0.0 && rand::random::<f32>() < npc.parry_chance() {
                    // the guard parries (`m_ParryChanceOfStarting`, then `..OfChaining` up to
                    // `m_ParryChainsAllowed` in a row); a parry may break its guard, leaving it
                    // open
                    let broke = rand::random::<f32>() < npc.block_break_chance();
                    npc.parries += 1;
                    npc.gesture(1);
                    // and strikes back (`DisTweaks_NPCRiposte`)
                    if npc.has_move("Riposte") && !broke {
                        npc.riposte = 0.35;
                    }
                    if broke {
                        npc.parries = 0;
                        npc.stagger = 0.9;
                    }
                    noise.write(Noise { pos: nt.translation, radius: 8.0, kind: NoiseKind::Threatening });
                    sfx.write(crate::audio::PostEvent::named("Snd_Imp_Sword_vs_Sword_ak", Some(nt.translation + Vec3::Y)));
                    // (the blades' sparks where they meet, `Dis_ContactSystem`'s sword on sword)
                    if let Some(level) = level.as_ref() {
                        let to = (nt.translation - pt.translation).with_y(0.0).normalize_or_zero();
                        crate::impacts::spawn(&mut fx, &level.scene, crate::impacts::SWORD, "Sword", pt.translation + Vec3::Y * 1.45 + to * 0.7, -to);
                    }
                    sword.recoil = 0.3;
                    // (Corvo's swing bounces off its blade: `BigHit`, the view jolted)
                    sword.bounce_seq += 1;
                    sword.bounce_wall = false;
                    shake.write(crate::player::ShakeCamera(CAM_SHAKE_PARRIED));
                    msgs.push("Blocked");
                } else {
                    npc.parries = 0;
                    hits.write(NpcHit { npc: e, damage: attrs.sword_damage, kind: HitKind::Sword, from: pt.translation });
                }
                for &(o, _) in swept.iter().skip(1) {
                    let Ok((_, mut other, ot)) = npcs.get_mut(o) else { continue };
                    if other.mode == Mode::Combat && other.sees_player && other.attack_t.is_none() && other.stagger <= 0.0 && rand::random::<f32>() < other.parry_chance() {
                        other.parries += 1;
                        other.gesture(1);
                        sfx.write(crate::audio::PostEvent::named("Snd_Imp_Sword_vs_Sword_ak", Some(ot.translation + Vec3::Y)));
                        continue;
                    }
                    other.parries = 0;
                    hits.write(NpcHit { npc: o, damage: attrs.sword_damage, kind: HitKind::Sword, from: pt.translation });
                }
            } else if let Some(e) = npcs
                .iter()
                .filter(|(_, n, _)| n.mode == Mode::Unconscious)
                .map(|(e, _, t)| (e, t.translation + Vec3::Y * 0.3))
                .filter(|(_, at)| (*at - eye).length() < 2.4 && (*at - eye).normalize_or_zero().dot(fwd) > 0.6)
                .min_by(|a, b| a.1.distance(eye).total_cmp(&b.1.distance(eye)))
                .map(|(e, _)| e)
            {
                // one lying knocked out: the blade kills them where they lie
                hits.write(NpcHit { npc: e, damage: attrs.sword_damage, kind: HitKind::Sword, from: pt.translation });
            } else if let Some((s, at)) = strikeables
                .iter()
                .map(|(s, g)| (s.0, g.translation()))
                .filter(|(_, at)| (*at - eye).length() < 2.4 && (*at - eye).normalize_or_zero().dot(fwd) > 0.5)
                .min_by(|a, b| a.1.distance(eye).total_cmp(&b.1.distance(eye)))
            {
                // a krust's shell
                struck.write(crate::gameplay::Struck { target: s, damage: attrs.sword_damage, kind: HitKind::Sword, at });
            } else {
                // the world: what the blade meets (a speaker, a crate the scripts listen to)
                use crate::worlddamage::{Reach, WorldDamage};
                world_hits.write(WorldDamage::player(Reach::Ray { from: eye, dir: fwd, len: 2.2 }, attrs.sword_damage, "DisDamageType_FastHit_Right"));
                // a wall in the blade's way: it strikes it and bounces off (`HitEnvironment`), with
                // the contact system's sparks and sound for the surface
                let world = QueryFilter::default().exclude_collider(pe).groups(CollisionGroups::new(Group::ALL, crate::level::GROUP_WORLD | crate::level::GROUP_PROP));
                if let (Ok(ctx), Some(level)) = (rapier.single(), level.as_ref()) {
                    if let Some((_, toi, surface, normal)) = crate::footsteps::ray_surface_normal(&ctx, &surfaces, eye, fwd, BLADE_WALL_REACH, world) {
                        let at = eye + fwd * toi;
                        let what = crate::impacts::struck(surface);
                        crate::impacts::spawn(&mut fx, &level.scene, crate::impacts::SWORD, what, at, normal);
                        if let Some(sound) = sword_impact_sound(&level.scene, what) {
                            sfx.write(crate::audio::PostEvent::named(sound, Some(at)));
                        }
                        noise.write(Noise { pos: at, radius: 8.0, kind: NoiseKind::Threatening });
                        shake.write(crate::player::ShakeCamera(CAM_SHAKE_ON_HIT_ENV));
                        sword.bounce_seq += 1;
                        sword.bounce_wall = true;
                        info!("sword: strikes {what} {toi:.2} m ahead");
                    }
                }
            }
        }
        if sword.swing > 0.48 {
            sword.swing = 0.0;
        }
    }
    if !grabbed {
        return;
    }
    if mouse.just_pressed(MouseButton::Left) && sword.swing == 0.0 && !sword.blocking {
        sword.swing = 0.001;
        sword.hit_done = false;
        sword.swing_dir = if sword.swing_dir > 0.0 { -1.0 } else { 1.0 };
        sword.hold = 0.0;
    }
    // block: hold GBA_Block (Ctrl) (not while reeling from a parried blow or a kick)
    if keys.pressed(bind.key(crate::bindings::Act::Block)) && sword.swing == 0.0 && sword.recoil <= 0.0 {
        sword.hold += dt;
        if !sword.blocking {
            sword.block_t = 0.0;
        }
        sword.blocking = true;
    } else {
        sword.blocking = false;
        sword.hold = 0.0;
    }
    if sword.blocking {
        sword.block_t += dt;
    }
}

#[cfg(test)]
mod finisher_tests {
    use super::*;
    use crate::gamedata::Data;

    #[test]
    fn fast_finishers_run_out_before_a_full_one() {
        let data = Data::default();
        let mut pace = FinisherPace::default();
        pace.roll(&data, 0.0, true, false);
        let fasts = pace.fast_left;
        assert!((4..=8).contains(&fasts));
        for i in 0..fasts {
            assert!(!pace.full(i as f32, true, false));
            pace.played(&data, i as f32, false, false);
        }
        assert!(pace.full(10.0, true, false));
        // not with another foe on Corvo, nor in Bend Time
        assert!(!pace.full(10.0, false, false));
        assert!(!pace.full(10.0, true, true));
        pace.played(&data, 10.0, true, false);
        assert!(!pace.full(11.0, true, false));
        // a full one is due anyway once its time has come after a fast one
        pace.played(&data, 12.0, false, false);
        assert!(pace.full(131.0, true, false));
    }

    #[test]
    fn slow_motion_wants_the_one_foe_and_waits_its_time() {
        let data = Data::default();
        let mut pace = FinisherPace::default();
        assert!(!(0..50).any(|_| pace.roll(&data, 0.0, false, false).slomo));
        assert!(!(0..50).any(|_| pace.roll(&data, 0.0, true, true).slomo));
        assert!((0..50).any(|_| pace.roll(&data, 0.0, true, false).slomo));
        pace.played(&data, 0.0, false, true);
        assert!(!(0..50).any(|_| pace.roll(&data, 20.0, true, false).slomo));
        assert!((0..50).any(|_| pace.roll(&data, 31.0, true, false).slomo));
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
mod side_tests {
    use super::*;

    #[test]
    fn a_blow_comes_from_ahead_behind_left_or_right() {
        // facing -Z (yaw 0)
        assert_eq!(side_of(Vec3::new(0.2, 1.0, -1.0), Vec3::ZERO, 0.0), 0);
        assert_eq!(side_of(Vec3::new(0.0, 0.0, 1.0), Vec3::ZERO, 0.0), 1);
        assert_eq!(side_of(Vec3::new(-1.0, 0.0, 0.3), Vec3::ZERO, 0.0), 2);
        assert_eq!(side_of(Vec3::new(1.0, 0.0, 0.0), Vec3::ZERO, 0.0), 3);
        // facing +X (a quarter turn right): what was to the right is ahead
        assert_eq!(side_of(Vec3::new(1.0, 0.0, 0.0), Vec3::ZERO, -std::f32::consts::FRAC_PI_2), 0);
    }
}

#[cfg(test)]
mod pause_tests {
    use super::*;

    #[test]
    fn releasing_block_in_a_menu_does_not_end_a_chokehold() {
        let mut app = App::new();
        app.init_resource::<Time>().init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<crate::bindings::Bindings>().init_resource::<crate::kismet::ScriptSwitches>()
            .init_resource::<PlayerStats>().init_resource::<crate::gamedata::Attrs>()
            .init_resource::<crate::props::Held>().init_resource::<crate::carry::Carry>()
            .init_resource::<crate::possession::Possession>().init_resource::<HudMessages>()
            .insert_resource(crate::hud::Paused(true)).add_message::<NpcHit>()
            .add_systems(Update, chokehold);
        let target = app.world_mut().spawn_empty().id();
        let player = app.world_mut().spawn((Player {
            velocity: Vec3::ZERO, yaw: 0.0, pitch: 0.0, crouched: false, sprinting: false,
            grounded: true, lean: 0.0, noclip: false, eye_height: crate::player::STAND_EYE,
            locked: true, air_time: 0.0, spawn: Vec3::ZERO, mantle: None, step_timer: 0.0,
            fall_speed: 0.0, power_jump: 0.0, pull: Vec3::ZERO, air_peak: 0.0, speed_cap: 0.0,
        }, Transform::IDENTITY, Choking { npc: target, t: 0.5, duration: 3.0 })).id();
        app.update();
        assert_eq!(app.world().get::<Choking>(player).map(|ch| ch.t), Some(0.5));
        assert!(app.world().get::<Player>(player).unwrap().locked);
        app.world_mut().resource_mut::<crate::hud::Paused>().0 = false;
        app.update();
        assert!(app.world().get::<Choking>(player).is_none());
        assert!(!app.world().get::<Player>(player).unwrap().locked);
    }
}

fn chokehold(
    mut commands: Commands,
    paused: Res<crate::hud::Paused>,
    switches: Res<crate::kismet::ScriptSwitches>,
    time: Res<Time>,
    (keys, bind): (Res<ButtonInput<KeyCode>>, Res<crate::bindings::Bindings>),
    stats: Res<PlayerStats>,
    (attrs, held, carry, possession): (Res<crate::gamedata::Attrs>, Res<crate::props::Held>, Res<crate::carry::Carry>, Res<crate::possession::Possession>),
    mut msgs: ResMut<HudMessages>,
    mut hits: MessageWriter<NpcHit>,
    mut player: Query<(Entity, &Transform, &mut Player, Option<&mut Choking>), Without<Npc>>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    mut npcs: Query<(Entity, &mut Npc, &mut Transform), Without<Player>>,
) {
    if paused.0 { return; }
    let Ok((pe, pt, mut p, choking)) = player.single_mut() else { return };
    if stats.dead {
        return;
    }
    // (the scripts forbid it: `DisSeqAct_ToggleChoke`)
    if switches.0.choke_off && choking.is_none() {
        return;
    }
    if let Some(mut ch) = choking {
        ch.t += time.delta_secs();
        // the hold must be kept through its loop: let go then and the victim breaks free
        let in_loop = ch.t > ch.duration - CHOKE_TIME;
        if in_loop && !keys.pressed(bind.key(crate::bindings::Act::Block)) && ch.t < ch.duration {
            if let Ok((_, mut npc, _)) = npcs.get_mut(ch.npc) {
                npc.mode = Mode::Combat;
                npc.alert = Alert::Combat;
                npc.awareness = 1.0;
                npc.stagger = 1.0;
                npc.last_seen = Some(pt.translation);
            }
            msgs.push("The victim broke free");
            p.locked = false;
            commands.entity(pe).remove::<Choking>();
            return;
        }
        let target = ch.npc;
        let fwd = Quat::from_rotation_y(p.yaw) * Vec3::NEG_Z;
        if let Ok((_, mut npc, mut nt)) = npcs.get_mut(target) {
            // hold the victim in front of Corvo
            let want = pt.translation + fwd * 0.75;
            nt.translation = nt.translation.lerp(Vec3::new(want.x, nt.translation.y, want.z), 0.3);
            npc.yaw = p.yaw;
            npc.mode = Mode::Choked;
            if ch.t >= ch.duration {
                hits.write(NpcHit { npc: target, damage: 0.0, kind: HitKind::Choke, from: pt.translation });
                p.locked = false;
                commands.entity(pe).remove::<Choking>();
            }
        } else {
            p.locked = false;
            commands.entity(pe).remove::<Choking>();
        }
        return;
    }
    // a nonlethal takedown: hold GBA_Block (Ctrl) behind an unaware enemy (the pickpocketing
    // prompt on Use stands beside it, as the original's do)
    if p.locked || held.busy() || carry.carrying() || possession.host.is_some() || !keys.just_pressed(bind.key(crate::bindings::Act::Block)) {
        return;
    }
    let Ok(cg) = cam.single() else { return };
    let eye = cg.translation();
    let fwd = cg.forward().as_vec3();
    let ro: Vec<(Entity, &Npc, &Transform)> = npcs.iter().map(|(e, n, t)| (e, n, t)).collect();
    let Some((e, d)) = nearest_target(eye, fwd, 1.7, 0.6, ro.iter().map(|(e, n, t)| (*e, *n, *t))) else { return };
    let _ = d;
    let Ok((_, mut npc, nt)) = npcs.get_mut(e) else { return };
    // from behind, or off balance; otherwise Ctrl just blocks
    let to_player = (pt.translation - nt.translation).with_y(0.0).normalize_or_zero();
    let behind = npc.forward().dot(to_player) < 0.0;
    let fighting = npc.mode == Mode::Combat && npc.sees_player;
    if npc.is_down() || npc.kind == crate::npc::Kind::Story || (fighting && npc.stagger <= 0.0) || (!behind && !fighting && npc.alert != crate::npc::Alert::Unaware) {
        return;
    }
    npc.mode = Mode::Choked;
    npc.attack_t = None;
    p.locked = true;
    // its start (`ChokeTime`: Strong Arms shortens it), then the loop
    commands.entity(pe).insert(Choking { npc: e, t: 0.0, duration: attrs.choke_time.max(0.0) + CHOKE_TIME });
    msgs.push(format!("Choking {}...", npc.name));
}

/// The assassination under way: the victim set where its clip has Corvo (its `anchor_jnt`
/// at the start: behind it, before it or at its side), Corvo held for his own clip.
#[allow(clippy::type_complexity)]
fn assassinating(
    mut commands: Commands,
    time: Res<Time>,
    mut player: Query<(Entity, &Transform, &mut Player, &mut Assassinating), Without<Npc>>,
    mut npcs: Query<(&mut Npc, &mut Transform, &crate::anim::Animator), Without<Player>>,
    mut arms: Query<&mut crate::anim::Animator, (With<crate::arms::ArmsRoot>, Without<Npc>)>,
    (mut tc, level, mut severs): (ResMut<crate::gameplay::TimeControl>, Option<Res<crate::level::LevelInfo>>, MessageWriter<crate::gore::SeverLimb>),
) {
    let Ok((pe, pt, mut p, mut a)) = player.single_mut() else {
        if tc.finisher != 1.0 {
            tc.finisher = 1.0;
            for mut anim in &mut arms {
                anim.time_scale = 1.0;
            }
        }
        return;
    };
    if a.len.is_none() {
        let len = arms.iter().next().and_then(|anim| anim.lib.find(&a.master).map(|c| anim.lib.duration(c))).unwrap_or(1.6);
        a.len = Some(len);
        if let Ok((mut npc, mut nt, anim)) = npcs.get_mut(a.npc) {
            // Corvo's place in the victim's frame (x right, z behind), turned so that place
            // lies where Corvo is, then the victim moved to put him there
            let anchor = anim.lib.find(&a.slave).and_then(|c| anim.lib.bone_start(c, "anchor_jnt"));
            if let Some(an) = anchor {
                let local = Vec3::new(an.z, 0.0, an.y);
                let to = (pt.translation - nt.translation).with_y(0.0);
                if local.length() > 0.05 && to.length() > 0.01 {
                    let ang = |v: Vec3| v.x.atan2(v.z);
                    let yaw = ang(to) - ang(local);
                    npc.yaw = yaw;
                    let at = pt.translation - Quat::from_rotation_y(yaw) * local;
                    debug!("assassination: anchor {an:?} corvo {:?} victim {:?} -> {:?} yaw {:.2}", pt.translation, nt.translation, at, yaw);
                    nt.translation = Vec3::new(at.x, nt.translation.y, at.z);
                    nt.rotation = Quat::from_rotation_y(yaw);
                }
            }
        }
        info!("assassination: {} ({len:.2} s)", a.master);
    }
    // the clip's slow motion where it has some: its `DisNotify_BendTime_Ranged` stretches (the
    // dramatic deaths') always, its `..AdrenalineBendTime..` ones (the finishers') for a Blood
    // Thirst kill or when the kill cam takes it; eased in and out
    let (mut player_k, mut world_k) = (1.0f32, 1.0f32);
    if let Some(level) = level.as_ref() {
        for b in level.scene.arm_bend.iter().filter(|b| b.clip.eq_ignore_ascii_case(&a.master) && (!b.adrenaline || a.adrenaline || a.slomo)) {
            let t = a.t - b.time;
            let w = if t < 0.0 || t > b.duration + b.fade_out {
                0.0
            } else if t < b.fade_in {
                t / b.fade_in.max(1e-3)
            } else if t > b.duration {
                1.0 - (t - b.duration) / b.fade_out.max(1e-3)
            } else {
                1.0
            };
            player_k = player_k.min(1.0 + (b.player - 1.0) * w);
            world_k = world_k.min(1.0 + (b.world - 1.0) * w);
        }
    }
    tc.finisher = world_k.max(0.01);
    for mut anim in &mut arms {
        anim.time_scale = player_k.max(0.01);
    }
    let t0 = a.t;
    a.t += time.delta_secs() * player_k.max(0.01);
    a.real += time.delta_secs();
    // the victim's clip severs a limb as it passes its notify (the beheadings')
    if let Some(level) = level.as_ref() {
        for s in level.scene.npc_severs.iter().filter(|s| s.clip.eq_ignore_ascii_case(&a.slave) && s.time > t0 && s.time <= a.t) {
            severs.write(crate::gore::SeverLimb { npc: a.npc, bone: s.bone.clone(), impulse: s.impulse, eaten: false });
        }
    }
    p.velocity = Vec3::ZERO;
    if a.t >= a.len.unwrap_or(1.6) {
        info!("{} done: {:.2} s of clip in {:.2} s", a.master, a.t, a.real);
        p.locked = false;
        tc.finisher = 1.0;
        for mut anim in &mut arms {
            anim.time_scale = 1.0;
        }
        commands.entity(pe).remove::<Assassinating>();
    }
}

/// Whether anyone else is fighting Corvo nearby (the kill cam favours the last foe).
fn others_fighting(npcs: &Query<(Entity, &mut Npc, &Transform), Without<Player>>, except: Entity, at: Vec3) -> bool {
    npcs.iter().any(|(e, n, t)| e != except && !n.is_down() && n.mode == Mode::Combat && t.translation.distance(at) < 25.0)
}

#[allow(clippy::too_many_arguments)]
fn receive_hits(
    mut hits: MessageReader<PlayerHit>,
    mut stagger: MessageWriter<NpcStagger>,
    mut noise: MessageWriter<Noise>,
    mut stats: ResMut<PlayerStats>,
    mut msgs: ResMut<HudMessages>,
    mut player: Query<(&Transform, &mut Player, &mut Sword)>,
    mut sfx: MessageWriter<crate::audio::PostEvent>,
    (mut versus, mut npcs, cam): (ResMut<Versus>, Query<(&mut Npc, &Transform), Without<Player>>, Query<&GlobalTransform, With<PlayerCamera>>),
    attrs: Res<crate::gamedata::Attrs>,
    (mut fx, level, mut shake): (MessageWriter<crate::particles::SpawnEffect>, Option<Res<crate::level::LevelInfo>>, MessageWriter<crate::player::ShakeCamera>),
) {
    use crate::audio::PostEvent;
    let Ok((pt, mut p, mut sword)) = player.single_mut() else { return };
    let fwd = cam.single().map(|c| c.forward().as_vec3()).unwrap_or(Vec3::NEG_Z);
    for h in hits.read() {
        if stats.dead {
            continue;
        }
        // locked with another: its blows wait
        if versus.npc.is_some() && versus.npc != Some(h.npc) {
            continue;
        }
        // Corvo's own blow about to land: the blades lock
        if !h.kick && !h.push && !h.unblockable && versus.npc.is_none() && sword.swing > 0.0 && !sword.hit_done && PLAYER_HIT - sword.swing < VERSUS_ZONE {
            if let Ok((mut npc, nt)) = npcs.get_mut(h.npc) {
                if can_lock(&npc, nt, pt.translation, fwd, |d| d.abs() < 0.1) {
                    sword.hit_done = true;
                    let at = nt.translation + Vec3::Y;
                    lock_blades(&mut versus, h.npc, &mut npc, at, &mut sfx, &mut noise);
                    continue;
                }
            }
        }
        // a shove out of someone's way
        if h.push {
            p.velocity += (pt.translation - h.from).with_y(0.0).normalize_or_zero() * 3.5;
            continue;
        }
        // a kick: the block broken, Corvo shoved back
        if h.kick {
            let away = (pt.translation - h.from).with_y(0.0).normalize_or_zero();
            p.velocity += away * 5.0;
            sword.blocking = false;
            sword.recoil = 0.8;
            stats.hit_from = Some(h.from);
            stats.blow = (stats.blow.0.wrapping_add(1), side_of(h.from, pt.translation, p.yaw));
            sfx.write(PostEvent::named("Snd_Imp_Fist_on_Body_cue_ak", None));
            noise.write(Noise { pos: pt.translation, radius: 15.0, kind: NoiseKind::Help });
            continue;
        }
        let fwd = Quat::from_rotation_y(p.yaw) * Vec3::NEG_Z;
        let to_npc = (h.from - pt.translation).with_y(0.0).normalize_or_zero();
        // the block's reach (`Twk_Inv_SwordCorvo`'s `m_fBlockAngle` 50 degrees); a perfect parry
        // is a block begun within `m_fTapForPerfectParryWindow` (0.2 s) of the blow
        let facing = fwd.dot(to_npc) > 50f32.to_radians().cos();
        if sword.blocking && facing && !h.unblockable {
            noise.write(Noise { pos: pt.translation, radius: 8.0, kind: NoiseKind::Threatening });
            shake.write(crate::player::ShakeCamera(h.damage * CAM_SHAKE_BLOCK_SCALE));
            sfx.write(PostEvent::named("Snd_Imp_Sword_vs_Sword_ak", None));
            // (the blades' sparks where they meet)
            if let Some(level) = level.as_ref() {
                crate::impacts::spawn(&mut fx, &level.scene, crate::impacts::SWORD, "Sword", pt.translation + Vec3::Y * 1.45 + to_npc * 0.7, -to_npc);
            }
            // (a tap parry forces a plain block for 0.3 s after: `DishonoredPlayerPawn`'s
            // block handler, as roshan's r614/blink research reads it)
            if sword.block_t < 0.2 && sword.forced_block <= 0.0 {
                stagger.write(NpcStagger { npc: h.npc, secs: 1.8, parried: true });
                sword.parry_flash = 0.25;
                sword.forced_block = 0.3;
                msgs.push("Parried!");
            } else {
                stagger.write(NpcStagger { npc: h.npc, secs: 0.5, parried: false });
                sword.recoil = 0.25;
            }
            continue;
        }
        let plague = npcs.get(h.npc).is_ok_and(|(n, _)| n.pawn.to_ascii_lowercase().contains("weeper"));
        let mut damage = if plague { (h.damage - attrs.plague_damage_reduction).max(0.0) } else { h.damage };
        // a blow from behind his view halves (`DishonoredPlayerPawn.m_fOffscreenMeleeDamageReduction`
        // 0.5 on every difficulty; at least 1: found by roshan's r614/blink research)
        if damage > 0.0 && fwd.dot(h.from - pt.translation) < 0.0 {
            damage = (damage * (1.0 - 0.5)).round().max(1.0);
        }
        if damage <= 0.0 {
            continue;
        }
        if plague {
            stats.mana = (stats.mana + attrs.plague_mana).min(stats.max_mana);
            stats.mana_cap = stats.mana_cap.max(stats.mana);
        } else {
            damage = attrs.armored(damage);
        }
        stats.take_damage(damage);
        stats.damage_flash = 1.0;
        stats.hit_from = Some(h.from);
        stats.blow = (stats.blow.0.wrapping_add(1), side_of(h.from, pt.translation, p.yaw));
        sfx.write(PostEvent::named("Snd_Imp_Sword_on_Body_cue_ak", None));
        // a big blow leaves him reeling (`m_fVulnerableTime_BigHit_Dizzy` 0.9 s: no blocking),
        // knocked back a step
        if h.big {
            sword.recoil = sword.recoil.max(0.9);
            sword.blocking = false;
            p.velocity += (pt.translation - h.from).with_y(0.0).normalize_or_zero() * 3.0;
        }
        if stats.health <= 0.0 {
            stats.dead = true;
            msgs.push("You died");
            sfx.write(PostEvent::named("Snd_VO_Bark_P_Death_cue_ak", None));
            sfx.write(PostEvent::named("VS_Death_Jingle", None));
        } else {
            let bark = if h.damage >= 30.0 { "Snd_VO_Bark_Player_Pain_Big" } else { "Snd_VO_Bark_Player_Pain_low_cue_ak" };
            sfx.write(PostEvent::named(bark, None));
        }
    }
}

pub fn animate_view_model(
    time: Res<Time>,
    stats: Res<PlayerStats>,
    possession: Res<crate::possession::Possession>,
    player: Query<(&Player, &Sword, Option<&Choking>)>,
    mut vm: Query<(&mut Transform, &mut Visibility), With<ViewModel>>,
    peek: Res<crate::keyhole::Peek>,
    cine: Res<crate::script_world::Cinematic>,
) {
    let Ok((p, sword, choking)) = player.single() else { return };
    let Ok((mut t, mut vis)) = vm.single_mut() else { return };
    *vis = if stats.dead || p.noclip || choking.is_some() || possession.host.is_some() || peek.at.is_some() || cine.hides_player() { Visibility::Hidden } else { Visibility::Inherited };
    let base = Vec3::new(0.24, -0.21, -0.42);
    let bob_speed = Vec3::new(p.velocity.x, 0.0, p.velocity.z).length();
    let tsec = time.elapsed_secs();
    let bob = Vec3::new((tsec * 7.0).sin() * 0.008, ((tsec * 14.0).sin() * 0.006).abs(), 0.0) * (bob_speed / 3.0).min(1.5);
    let mut rot = Quat::IDENTITY;
    let mut pos = base + bob;
    if sword.swing > 0.0 {
        let s = sword.swing;
        let (yaw, pitch, fwd) = if s < 0.08 {
            let k = s / 0.08;
            (0.7 * k, 0.3 * k, 0.0)
        } else {
            let k = ((s - 0.08) / 0.2).min(1.0);
            let k = 1.0 - (1.0 - k) * (1.0 - k);
            (0.7 - 1.9 * k, 0.3 - 0.7 * k, -0.12 * (1.0 - (k - 0.5).abs() * 2.0))
        };
        rot = Quat::from_rotation_y(yaw * sword.swing_dir) * Quat::from_rotation_z(pitch * sword.swing_dir);
        pos += Vec3::new(-0.05, 0.03, fwd);
    } else if sword.blocking {
        rot = Quat::from_rotation_z(1.2) * Quat::from_rotation_y(0.3);
        pos = Vec3::new(0.05, -0.12, -0.40);
    }
    if sword.recoil > 0.0 {
        pos += Vec3::new(0.0, 0.02, 0.06) * (sword.recoil / 0.3);
    }
    t.translation = t.translation.lerp(pos, 0.5);
    t.rotation = t.rotation.slerp(rot, 0.45);
}
