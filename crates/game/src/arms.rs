//! Corvo's first-person arms: the original `Ply_Player.Skm_Player` rig played with the
//! game's first-person animation sets. The right arm (sword) runs on the base layer
//! (idle, locomotion, attacks, parries, hits, chokes); the left arm runs on the overlay
//! layer (powers, crossbow, pistol). The sword sits in `handAttachment_R_jnt`, ranged
//! weapons in `handAttachment_L_jnt`. If the animations are missing, the right hand is
//! posed with two-bone IK onto the procedural sword of the view model instead.

use crate::anim::{AnimPose, Animator, CharAnims, ClipId};
use crate::combat::{animate_view_model, spawn_view_model, Choking, Sword, SwordGrip, SwordModel, ViewModel, VIEW_LAYER};
use crate::gameplay::PlayerStats;
use crate::level::{GameAssets, PartMat};
use crate::player::{Player, PlayerCamera};
use crate::powers::{Power, Powers};
use crate::world_light::LitActor;
use crate::GameState;
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::light::NotShadowCaster;
use bevy::mesh::skinning::SkinnedMesh;
use bevy::mesh::MeshTag;
use bevy::prelude::*;
use std::f32::consts::FRAC_PI_2;
use std::sync::Arc;

pub struct ArmsPlugin;

impl Plugin for ArmsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DeathFall>()
            .init_resource::<ArmSteps>().init_resource::<ParryZone>()
            .add_systems(OnEnter(GameState::InGame), spawn_arms.after(spawn_view_model))
            .add_systems(Update, (animate_arms.in_set(ArmsAnimSet), arm_steps.after(ArmsAnimSet), arm_effects.after(ArmsAnimSet), possess_hand.after(ArmsAnimSet), (animate_held, animate_sword, show_bolts).after(ArmsAnimSet), show_upgrades, pose_arms_ik, mark_skin, swap_arms).after(animate_view_model).run_if(in_state(GameState::InGame)))
            .add_systems(PostUpdate, (align_arms.in_set(ArmsAlign), arm_pitch).after(AnimPose).before(TransformSystems::Propagate))
            .add_systems(Last, scene_align_log.run_if(|| std::env::var_os("DH_SCENE_ALIGN").is_some()));
    }
}

/// The arms' clips are chosen.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArmsAnimSet;

/// Corvo's footfalls as his arms' locomotion loop cues them (`DishonoredNotify_Footfall`, the
/// pawn's footfall 0x00B66ED0): whether the loop playing has any, and how many came since the
/// player's last look (`player`).
#[derive(Resource, Default)]
pub struct ArmSteps {
    pub cued: bool,
    pub due: u32,
}

/// While Corvo's block parries, from its start (s): his block clip's `DisNotify_BlockParryZone`
/// (`Sword_Ready_ParryIdle_In`, to 0.431 s), as roshan's r614/blink research decoded the block
/// handler; none: the clip has none.
#[derive(Resource, Default)]
pub struct ParryZone(pub Option<f32>);

/// The footfalls the arms' loop passed since the last frame (its time wrapping as it loops).
fn arm_steps(
    level: Option<Res<crate::level::LevelInfo>>,
    arms: Query<&Animator, With<ArmsRoot>>,
    mut steps: ResMut<ArmSteps>,
    mut zone: ResMut<ParryZone>,
    mut marks: Local<Option<(String, std::collections::HashMap<ClipId, Vec<f32>>)>>,
    mut was: Local<Option<(ClipId, f32)>>,
) {
    let (Some(level), Ok(anim)) = (level, arms.single()) else { return };
    if marks.as_ref().is_none_or(|(n, _)| *n != level.scene.name) {
        let all = anim.lib.marks(&level.scene);
        zone.0 = anim.lib.find("Sword_Ready_ParryIdle_In").and_then(|c| all.get(&c)).and_then(|m| m.parry_zone).map(|z| z.1);
        let by = all.into_iter().filter(|(_, m)| !m.footfalls.is_empty()).map(|(id, m)| (id, m.footfalls)).collect();
        *marks = Some((level.scene.name.clone(), by));
    }
    let now = anim.current().filter(|c| c.looping).map(|c| (c.clip, c.t));
    let feet = now.and_then(|(clip, _)| marks.as_ref().and_then(|(_, m)| m.get(&clip)));
    steps.cued = feet.is_some();
    if let (Some(feet), Some((clip, t)), Some((before, t0))) = (feet, now, *was) {
        if clip == before {
            let len = anim.lib.duration(clip).max(1e-3);
            let passed = |a: f32, b: f32| feet.iter().filter(|&&f| f > a && f <= b).count() as u32;
            let n = if t >= t0 { passed(t0, t) } else { passed(t0, len) + passed(-1.0, t) };
            if n > 0 && std::env::var_os("DH_STEP_LOG").is_some() {
                info!("arm steps: {} at {t:.3} of {}", anim.lib.clip(clip).name, n);
            }
            steps.due += n;
        }
    }
    *was = now;
}

/// Corvo dead and falling with his death's clip (the view on his camera bone, not sunk to the
/// floor: `player`).
#[derive(Resource, Default)]
pub struct DeathFall(pub bool);

/// The arms' root is placed on the camera.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArmsAlign;

/// Rig space faces +X with +Y up; the camera looks down -Z.
fn rig_to_camera() -> Quat {
    Quat::from_rotation_y(FRAC_PI_2)
}

/// Shift of the procedural (IK) rig relative to the eye: a little forward so the sword
/// target sits comfortably within arm's reach.
const IK_OFFSET: Vec3 = Vec3::new(0.0, 0.0, -0.08);

#[derive(Component)]
pub struct ArmsRoot;

/// A gun's muzzle in Corvo's hand (the pistol's `Spitfire`, the crossbow's
/// `Ps_Muzzle_Socket`): where its flash plays, its tracer leaves from.
#[derive(Component)]
pub struct Muzzle(pub Power);

/// A held item's own rig (its prop's name): it moves with the hand's clips.
#[derive(Component)]
struct HeldRig(&'static str);

/// A bolt on the crossbow's rig: the one loaded, or the next brought in as it reloads.
#[derive(Component)]
struct LoadedBolt {
    reload: bool,
}

/// The crossbow's bolt shows while it's loaded (not from the shot until the reload brings the
/// next, nor with none left); the reload's bolt while the reload plays.
fn show_bolts(arms: Query<&Animator, With<ArmsRoot>>, mut bolts: Query<(&LoadedBolt, &mut Visibility)>, (powers, stats): (Res<Powers>, Res<PlayerStats>)) {
    let Ok(arms) = arms.single() else { return };
    let arm = arms.overlay_state().0.map(|c| arms.lib.clip(c).name.as_str()).unwrap_or("");
    let t = arms.overlay_time().unwrap_or(0.0);
    let reloading = matches!(arm, "Crossbow_Reload_In" | "Crossbow_RapidReload") || (arm == "Crossbow_Fire_FastReload" && t > 0.25);
    let shot = matches!(arm, "Crossbow_Fire" | "Crossbow_Spyglass_Fire") || (arm == "Crossbow_Fire_FastReload" && t <= 0.25);
    let none = powers.selected.ammo(&stats).unwrap_or(1) == 0;
    for (b, mut v) in &mut bolts {
        let show = if b.reload { reloading } else { !reloading && !shot && !none };
        let want = if show { Visibility::Inherited } else { Visibility::Hidden };
        if *v != want {
            *v = want;
        }
    }
}

/// An upgrade as it shows on a gun (`DisTweaks_Upgrade.m_pAttachMesh`), and whether that gun is
/// the one loaded with explosive rounds.
#[derive(Component)]
struct UpgradePart {
    id: String,
    explosive: bool,
}

/// The pistol's upgrades that don't show with explosive rounds loaded
/// (`Twk_Inv_PistolCorvo.m_RevertedUpgradesForExplosive`: its cylinder and the magazines' drums).
const EXPLOSIVE_REVERTS: &[&str] = &["Twk_Upgrade_Pistol", "Twk_Upgrade_PistolMagazine1", "Twk_Upgrade_PistolMagazine2", "Twk_Upgrade_PistolMagazine3"];

/// The guns' upgrades show on them while owned and not given way to a later one
/// (`m_RevertedUpgrades`: the magazines' drums in place of the cylinder, each the next's);
/// the cylinder is Corvo's pistol's own (`Twk_Upgrade_Pistol`).
fn show_upgrades(stats: Res<PlayerStats>, data: Res<crate::gamedata::Data>, mut parts: Query<(&UpgradePart, &mut Visibility)>) {
    let owned = |id: &str| id == "Twk_Upgrade_Pistol" || stats.upgrades.iter().any(|u| u == id);
    for (p, mut v) in &mut parts {
        let replaced = data.0.upgrades.iter().any(|u| owned(&u.id) && u.replaces.iter().any(|r| *r == p.id));
        let show = owned(&p.id) && !replaced && !(p.explosive && EXPLOSIVE_REVERTS.contains(&p.id.as_str()));
        let want = if show { Visibility::Inherited } else { Visibility::Hidden };
        if *v != want {
            *v = want;
        }
    }
}

#[derive(Component)]
struct ArmRig {
    joints: Vec<Entity>,
    bind_t: Vec<Vec3>,
    bind_local: Vec<Quat>,
    /// bind pose in rig space
    bind_rot: Vec<Quat>,
    bind_pos: Vec<Vec3>,
    right: Chain,
    left: Chain,
    attach: usize,
    camera: usize,
    /// the collarbones (right, left): the view's look controls turn them
    collar: [Option<usize>; 2],
    /// right-hand digit joints with their fist curl (local axis, angle)
    curl: Vec<(usize, Vec3, f32)>,
}

#[derive(Clone, Copy)]
struct Chain {
    upper: usize,
    lower: usize,
    hand: usize,
}

/// The first-person clips, by situation.
struct ArmClips {
    idle: Option<ClipId>,
    sneak_idle: Option<ClipId>,
    walk: Option<ClipId>,
    run: Option<ClipId>,
    sprint: Option<ClipId>,
    sneak_walk: Option<ClipId>,
    sneak_run: Option<ClipId>,
    jump: Option<ClipId>,
    /// leaving the ground in a jump (`Sword_Ready_JumpIn`); the left hand's, a power's or a
    /// gadget's in it, its take-off and its landing (`Powers_JumpIn`, `_JumpLandSmall`,
    /// `Gadgets_JumpIn`...)
    jump_in: Option<ClipId>,
    left_jump: [[Option<ClipId>; 2]; 2],
    /// landing (`DisTweaks_PlayerPawn_NavigationActions`: `m_Action_JumpLandSmall_Ready` /
    /// `_Sneak`, and `m_Action_JumpLandBig` past `m_fFall_Stun_Vel`)
    land: Option<ClipId>,
    land_sneak: Option<ClipId>,
    land_big: Option<ClipId>,
    /// dying, struck from ahead, behind, the left, the right (`Generic_Death_Impact`, `_Back`,
    /// `_Left`, `_Right`: named, as the characters' deaths are, for the side the blow came from),
    /// from a fall (`Generic_Death_FallImpact_Back`), in the water (`Generic_Death_Water`),
    /// drowned (`Generic_Death_Water_OutOfBreath`)
    death: [Option<ClipId>; 4],
    death_fall: Option<ClipId>,
    death_water: Option<ClipId>,
    death_drowned: Option<ClipId>,
    /// killed in a wolfhound's teeth (`Sword_ArmAttack_TransToDeath_Minigame`, then
    /// `Sword_WolfhoundDeath_Minigame`: its arm attack's linked clips)
    death_pinned: [Option<ClipId>; 2],
    /// killed by a wolfhound's jump attack, knocked down (`Sword_JumpAttack_TransToKill_Minigame`,
    /// then `Sword_WolfhoundDeath_Minigame`)
    death_jumped: Option<ClipId>,
    /// knocked down from ahead, behind, the left, the right (`Sword_KnockdownFront`...; empty
    /// handed `Empty_Knockdown*`)
    knockdown: [[Option<ClipId>; 4]; 2],
    /// a mantle (`Ply_Player_Mantle_at`): empty-handed or sword in hand, standing or sneaking,
    /// low, medium or high
    mantle: [[[Option<ClipId>; 3]; 2]; 2],
    /// a slide (`Ply_Player_Slide_at`), empty-handed or sword in hand: in, its loop, out to a
    /// sneak or a sprint
    slide: [[Option<ClipId>; 4]; 2],
    attack_right: Vec<ClipId>,
    attack_left: Vec<ClipId>,
    sneak_attack: Option<ClipId>,
    /// the swings stopped short: each ready swing's bounce (`_Big`), the sneak swing's off a
    /// body and off a wall (`Sword_Sneak_Attack_BigNpc`, `_BigWall`)
    attack_right_big: Vec<ClipId>,
    attack_left_big: Vec<ClipId>,
    sneak_big: [Option<ClipId>; 2],
    parry_in: Option<ClipId>,
    parry_idle: Option<ClipId>,
    parry_out: Option<ClipId>,
    parry: Vec<ClipId>,
    /// thrown by a blow, by its side: ahead, behind, left, right (`Sword_BigHit_*`)
    hit: [Option<ClipId>; 4],
    choke_in: Option<ClipId>,
    choke_loop: Option<ClipId>,
    choke_win: Option<ClipId>,
    choke_lose: Option<ClipId>,
    choke_cancel: Option<ClipId>,
    // held by a weeper; pinned by a wolfhound, its teeth in his arm (`Ply_WolfhoundAttacks_as`:
    // `Sword_ArmAttack_GrabArm_Minigame`, `_GrabLoop_`, `_GrabOut_`, its arm attack's linked
    // clips), where the level has them
    grabbed_in: Option<ClipId>,
    grabbed_loop: Option<ClipId>,
    grabbed_out: Option<ClipId>,
    pinned: [Option<ClipId>; 3],
    /// a witch's tentacle has his arm (`Ply_ArmGrab_In`, `_Loop`, `_Out`)
    tentacle: [Option<ClipId>; 3],
    // left arm
    powers_idle: Option<ClipId>,
    /// the left hand on the move with a power, with a gadget: walking, sprinting, in the air
    /// (`Powers_Walk`, `Powers_Jump`, `Gadgets_Walk`, `Gadgets_Sprint`, `Gadgets_Jump`)
    left_loco: [[Option<ClipId>; 3]; 2],
    /// the left hand with a power, with a gadget, as the sword slashes from the right, from
    /// the left (`Powers_SwordSlash_Right`, `Gadgets_SwordSlash_Left`)
    left_slash: [[Option<ClipId>; 2]; 2],
    gadgets_idle: Option<ClipId>,
    /// the Heart's grip: its fingers about it (`Ply_Gadgets_Heart_as`, on the left hand's
    /// fingers over the gadgets' hold)
    heart_fingers: Option<ClipId>,
    blink: Option<ClipId>,
    /// Blink held: the hand comes up and holds (`Powers_Cast_Blink_In`, `_Loop`)
    blink_aim: [Option<ClipId>; 2],
    /// a cast without the mana for it: the hand's empty gesture (`Powers_OutOfMana`)
    out_of_mana: Option<ClipId>,
    windblast: Option<ClipId>,
    bend_time: Option<ClipId>,
    switch: Option<ClipId>,
    /// the left hand taking up and putting away a power or a gadget (`Powers_Ready_Equip`,
    /// `_Unequip`; `Gadgets_Ready_Equip`, `_Unequip`)
    left_equip: [[Option<ClipId>; 2]; 2],
    crossbow_fire: Option<ClipId>,
    /// the crossbow's shot reloading as it goes, with its reload upgrade
    /// (`m_FireReloadFastCrossbow`: `Crossbow_Fire_FastReload`, at 1.5)
    crossbow_fire_fast: Option<ClipId>,
    /// Daud's: summoning (aimed: `Powers_Cast_SummonAssassin_In`, `_Loop`; let go `_Out`, given
    /// up `_Cancel`), Void Gaze opening, his wristbow raised
    summon: Option<ClipId>,
    summon_aim: [Option<ClipId>; 2],
    summon_cancel: Option<ClipId>,
    /// his Pull (The Brigmore Witches: Powers_Cast_Pull_In, _Loop aimed; _Out let go,
    /// _Abort given up)
    pull: Option<ClipId>,
    pull_aim: [Option<ClipId>; 2],
    pull_cancel: Option<ClipId>,
    /// a person held up (`_Lift_In`, `_Lift_Loop`), let drop (`_Lift_Out`), given up (`_Lift_Abort`)
    pull_lift: [Option<ClipId>; 4],
    dark_vision: Option<ClipId>,
    wristbow_idle: Option<ClipId>,
    pistol_fire: Option<ClipId>,
    /// the pistol's shot with its accuracy upgrade (`m_FireAccurateAction`: `Pistol_Fire_Accurate`)
    pistol_fire_accurate: Option<ClipId>,
    /// a gun raised to the eye through the mask's optics, pistol and crossbow: in, held, out,
    /// its shot, its accurate shot (`Pistol_Spyglass_*`, `Crossbow_Spyglass_*`)
    spyglass: [[Option<ClipId>; 5]; 2],
    /// Possession: aimed (`Powers_Cast_Possession_In`, `_Loop`), cast (`_OutUU`), given up
    /// (`_Cancel`)
    possess: Option<ClipId>,
    possess_aim: [Option<ClipId>; 2],
    possess_cancel: Option<ClipId>,
    swarm: Option<ClipId>,
    grenade_throw: Option<ClipId>,
    /// a grenade cooking in the hand (`Gadgets_Grenade_Cooking_In`, `_Idle`), the next one
    /// taken after a throw (`Gadgets_Grenade_Reload`), or none left (`_OutOfAmmo`)
    grenade_cook: [Option<ClipId>; 2],
    grenade_reload: Option<ClipId>,
    grenade_empty: Option<ClipId>,
    razor_place: Option<ClipId>,
    /// the guns' reload stages (`Pistol_Reload_In`...), by name
    reload: Vec<(&'static str, ClipId)>,
    // drop assassinations, by side (front, back, left, right)
    drop_kill: [Option<ClipId>; 4],
    // swimming (both hands, the sword away)
    swim_idle: Option<ClipId>,
    swim: [Option<ClipId>; 4],
    // empty-handed (no sword): idle, sneaking idle and walk
    empty_idle: Option<ClipId>,
    /// thrown by a blow empty-handed, by its side (`Empty_BigHit_Front`...)
    empty_hit: [Option<ClipId>; 4],
    /// empty-handed in the air and landing (`Empty_Sneak_Jump`, `_JumpLandSmall`: the empty set's
    /// only jump)
    empty_jump: [Option<ClipId>; 2],
    empty_sneak: [Option<ClipId>; 2],
    // drawing the sword (standing, sneaking)
    equip: [Option<ClipId>; 2],
    // a sword lock: the struggle, then won big, won, lost
    versus: [Option<ClipId>; 4],
    // the head's bob, by pace (walk, run, sprint, sneak) and way (forward, back, left, right):
    // `Ply_Head_Locomotion_as`' additive `ADD_Head_<pace><N|S|W|E>`
    bob: [[Option<ClipId>; 4]; 4],
    // the head through a mantle and on a chain (`ADD_Head_Mantle_Low`, `ADD_Head_Rope_Climb`)
    bob_mantle: Option<ClipId>,
    bob_climb: Option<ClipId>,
    // and winded (`ADD_Head_OutOfbreath`: `breath`)
    bob_breath: Option<ClipId>,
}

impl ArmClips {
    fn resolve(lib: &CharAnims) -> Option<ArmClips> {
        let c = ArmClips {
            idle: lib.find("Sword_Ready_Idle"),
            sneak_idle: lib.find("Sword_Sneak_Idle"),
            walk: lib.find("Sword_Ready_Walk"),
            bob: ["Walk", "Run", "Sprint", "Sneak"].map(|pace| ["N", "S", "W", "E"].map(|way| lib.find(&format!("ADD_Head_{pace}{way}")))),
            bob_mantle: lib.find("ADD_Head_Mantle_Low"),
            bob_climb: lib.find("ADD_Head_Rope_Climb"),
            bob_breath: lib.find("ADD_Head_OutOfbreath"),
            run: lib.find("Sword_Ready_Run"),
            sprint: lib.find("Sword_Ready_Sprint"),
            sneak_walk: lib.find("Sword_Sneak_Walk"),
            sneak_run: lib.find("Sword_Sneak_Run"),
            jump: lib.find("Sword_Ready_Jump"),
            jump_in: lib.find("Sword_Ready_JumpIn"),
            left_jump: ["Powers", "Gadgets"].map(|s| ["JumpIn", "JumpLandSmall"].map(|w| lib.find(&format!("{s}_{w}")))),
            land: lib.find("Sword_Ready_JumpLandSmall"),
            land_sneak: lib.find("Sword_Sneak_JumpLandSmall"),
            land_big: lib.find("Generic_JumpLandBig"),
            death: [lib.find("Generic_Death_Impact"), lib.find("Generic_Death_Back"), lib.find("Generic_Death_Left"), lib.find("Generic_Death_Right")],
            death_fall: lib.find("Generic_Death_FallImpact_Back"),
            death_water: lib.find("Generic_Death_Water"),
            death_drowned: lib.find("Generic_Death_Water_OutOfBreath"),
            death_pinned: [lib.find("Sword_ArmAttack_TransToDeath_Minigame"), lib.find("Sword_WolfhoundDeath_Minigame")],
            death_jumped: lib.find("Sword_JumpAttack_TransToKill_Minigame"),
            knockdown: ["Sword_", "Empty_"].map(|p| ["Front", "Back", "Left", "Right"].map(|s| lib.find(&format!("{p}Knockdown{s}")))),
            mantle: [["Empty_Mantle", "Empty_Sneak_Mantle"], ["Sword_Ready_Mantle", "Sword_Sneak_Mantle"]]
                .map(|ways| ways.map(|w| ["Low", "Medium", "High"].map(|h| lib.find(&format!("{w}{h}"))))),
            slide: ["Empty", "Sword"].map(|s| ["SlideIn", "SlideLoop", "SlideOutSneak", "SlideOutSprint"].map(|n| lib.find(&format!("{s}_{n}")))),
            // (a swing is the `_Small` clip, as roshan's r614/blink research captured the
            // original playing; its `_Big` the bounce when it's stopped short)
            attack_right: lib.all(&["Sword_Ready_AttackRight_A_Small", "Sword_Ready_AttackRight_B_Small"]),
            attack_left: lib.all(&["Sword_Ready_AttackLeft_A_Small", "Sword_Ready_AttackLeft_B_Small"]),
            sneak_attack: lib.find("Sword_Sneak_Attack_Small"),
            attack_right_big: lib.all(&["Sword_Ready_AttackRight_A_Big", "Sword_Ready_AttackRight_B_Big"]),
            attack_left_big: lib.all(&["Sword_Ready_AttackLeft_A_Big", "Sword_Ready_AttackLeft_B_Big"]),
            sneak_big: [lib.find("Sword_Sneak_Attack_BigNpc"), lib.find("Sword_Sneak_Attack_BigWall")],
            parry_in: lib.find("Sword_Ready_ParryIdle_In"),
            parry_idle: lib.find("Sword_Ready_ParryIdle"),
            parry_out: lib.find("Sword_Ready_ParryIdle_Out"),
            parry: lib.all(&["Sword_Ready_Parry_A", "Sword_Ready_Parry_B", "Sword_Ready_Parry_C"]),
            hit: ["Front", "Back", "Left", "Right"].map(|s| lib.find(&format!("Sword_BigHit_{s}"))),
            choke_in: lib.find("Sword_Choke_In_Master"),
            choke_loop: lib.find("Sword_Choke_Loop_Master"),
            choke_win: lib.find("Sword_Choke_Win_Master"),
            choke_lose: lib.find("Sword_Choke_Lose_Master"),
            choke_cancel: lib.find("Sword_Choke_Cancel_Master"),
            grabbed_in: lib.find("Sword_Weeper_ArmGrab_In"),
            grabbed_loop: lib.find("Sword_Weeper_ArmGrab_Loop"),
            grabbed_out: lib.find("Sword_Weeper_ArmGrab_Out"),
            pinned: ["GrabArm", "GrabLoop", "GrabOut"].map(|s| lib.find(&format!("Sword_ArmAttack_{s}_Minigame"))),
            tentacle: ["In", "Loop", "Out"].map(|s| lib.find(&format!("Ply_ArmGrab_{s}"))),
            drop_kill: [
                lib.find("Sword_Ready_Assassination_DropFront_Master"),
                lib.find("Sword_Ready_Assassination_DropBack_Master"),
                lib.find("Sword_Ready_Assassination_DropLeft_Master"),
                lib.find("Sword_Ready_Assassination_DropRight_Master"),
            ],
            swim_idle: lib.find("Empty_SwimIdle"),
            empty_idle: lib.find("Empty_Idle"),
            empty_hit: ["Front", "Back", "Left", "Right"].map(|s| lib.find(&format!("Empty_BigHit_{s}"))),
            empty_jump: [lib.find("Empty_Sneak_Jump"), lib.find("Empty_Sneak_JumpLandSmall")],
            empty_sneak: [lib.find("Empty_Sneak_Idle"), lib.find("Empty_Sneak_Walk")],
            equip: [lib.find("Sword_Ready_Equip"), lib.find("Sword_Sneak_Equip")],
            versus: [
                lib.find("Sword_Ready_Versus_MinigameUU"),
                lib.find("Sword_Ready_Versus_BigWin"),
                lib.find("Sword_Ready_Versus_MediumWin"),
                lib.find("Sword_Ready_Versus_MediumLose"),
            ],
            swim: [lib.find("Empty_SwimN"), lib.find("Empty_SwimE"), lib.find("Empty_SwimS"), lib.find("Empty_SwimW")],
            powers_idle: lib.find("Powers_Idle"),
            left_loco: ["Powers", "Gadgets"].map(|s| ["Walk", "Sprint", "Jump"].map(|w| lib.find(&format!("{s}_{w}")))),
            left_slash: ["Powers", "Gadgets"].map(|s| ["Right", "Left"].map(|w| lib.find(&format!("{s}_SwordSlash_{w}")))),
            gadgets_idle: lib.find("Gadgets_Idle"),
            heart_fingers: lib.first(&["Gadgets_Heart_Fingers_Idle", "Gadgets_Heart_Fingers_Default"]),
            blink: lib.find("Powers_Cast_Blink_Out"),
            blink_aim: [lib.find("Powers_Cast_Blink_In"), lib.find("Powers_Cast_Blink_Loop")],
            out_of_mana: lib.find("Powers_OutOfMana"),
            windblast: lib.find("Powers_Cast_Windblast"),
            bend_time: lib.find("Powers_Cast_BendTime"),
            switch: lib.find("Powers_Switch"),
            left_equip: ["Powers", "Gadgets"].map(|s| ["Equip", "Unequip"].map(|e| lib.find(&format!("{s}_Ready_{e}")))),
            crossbow_fire: lib.find("Crossbow_Fire").or_else(|| lib.find("Wristbow_Fire")),
            crossbow_fire_fast: lib.find("Crossbow_Fire_FastReload"),
            summon: lib.find("Powers_Cast_SummonAssassin_Out"),
            summon_aim: [lib.find("Powers_Cast_SummonAssassin_In"), lib.find("Powers_Cast_SummonAssassin_Loop")],
            summon_cancel: lib.find("Powers_Cast_SummonAssassin_Cancel"),
            pull: lib.find("Powers_Cast_Pull_Out"),
            pull_aim: [lib.find("Powers_Cast_Pull_In"), lib.find("Powers_Cast_Pull_Loop")],
            pull_cancel: lib.find("Powers_Cast_Pull_Abort"),
            pull_lift: ["In", "Loop", "Out", "Abort"].map(|k| lib.find(&format!("Powers_Cast_Pull_Lift_{k}"))),
            dark_vision: lib.find("Powers_Cast_DarkVision_In"),
            wristbow_idle: lib.find("Wristbow_Idle"),
            pistol_fire: lib.find("Pistol_Fire"),
            pistol_fire_accurate: lib.find("Pistol_Fire_Accurate"),
            spyglass: ["Pistol", "Crossbow"].map(|g| ["In", "Idle", "Out", "Fire", "FireAccurate"].map(|s| lib.find(&format!("{g}_Spyglass_{s}")))),
            possess: lib.find("Powers_Cast_Possession_OutUU").or_else(|| lib.find("Powers_Cast_Possession_In")),
            possess_aim: [lib.find("Powers_Cast_Possession_In"), lib.find("Powers_Cast_Possession_Loop")],
            possess_cancel: lib.find("Powers_Cast_Possession_Cancel"),
            swarm: lib.find("Powers_Cast_Swarm"),
            grenade_throw: lib.find("Gadgets_Grenade_Deploy"),
            grenade_cook: [lib.find("Gadgets_Grenade_Cooking_In"), lib.find("Gadgets_Grenade_Cooking_Idle")],
            grenade_reload: lib.find("Gadgets_Grenade_Reload"),
            grenade_empty: lib.find("Gadgets_Grenade_OutOfAmmo"),
            reload: ["Pistol_Reload_In", "Pistol_Reload_Out", "Pistol_FastReload", "Crossbow_Reload_In", "Crossbow_Reload_Out", "Crossbow_RapidReload"].into_iter().filter_map(|n| lib.find(n).map(|c| (n, c))).collect(),
            razor_place: lib.find("Gadgets_SpringRazor_Deploy"),
        };
        (c.idle.is_some() && c.walk.is_some()).then_some(c)
    }
}

/// Playback state of the animated arms.
#[derive(Component)]
struct ArmsAnim {
    clips: Arc<ArmClips>,
    /// the left hand's finger joints (a held item's grip)
    fingers_l: Arc<Vec<usize>>,
    /// the left arm (`Collarbone_L_Jnt` and all below it): what the left hand's clips move
    left_arm: Arc<Vec<bool>>,
    /// camera_jnt's bind transform relative to the camera
    eye: Transform,
    /// the head's bob: its cycle (0-1) and weight (`align_arms`), and the breath's clip time
    bob_phase: f32,
    bob_weight: f32,
    breath_t: f32,
    swinging: bool,
    next_attack: usize,
    /// the swing playing (0 right, 1 left, 2 sneak; its clip in its list), and the bounces seen
    swing_clip: (u8, usize),
    /// a slash just begun (its side: 0 from the right, 1 the left) for the left hand to go with
    slash: Option<u8>,
    bounce_seen: u32,
    blocking: bool,
    /// aiming Blink, a summoning or a possession (as last seen); their cancels seen
    blink_aiming: bool,
    summon_cancels: u32,
    pull_cancels: u32,
    pull_lifting: bool,
    pull_lift_ends: (u32, u32),
    possess_cancels: u32,
    /// a grenade cooking (as last seen)
    cooking: bool,
    choking: bool,
    /// the chokehold's part playing: 1 the hold, 2 its Win, 3 its Lose
    choke_phase: u8,
    grabbed: bool,
    /// (by a wolfhound)
    pinned: bool,
    tentacle_grab: bool,
    oneshot: bool,
    /// mantling (as last seen)
    mantling: bool,
    /// sliding (as last seen)
    sliding: bool,
    /// the gun raised to the eye, zoomed (as last seen: which)
    raised: Option<usize>,
    /// in the air long enough that touching down is a landing
    airborne: bool,
    /// on the ground last frame (a jump's take-off)
    grounded: bool,
    parry_flash: f32,
    damage_flash: f32,
    /// the blows seen (`PlayerStats::blow`)
    blow_seen: u32,
    knock_seen: u32,
    cast_seq: u32,
    /// the reload clips seen asked for
    reload_seq: u32,
    /// the casts without mana seen
    no_mana_seq: u32,
    selected: Power,
    /// what the left hand shows (behind `selected` while it puts the old away), and what it
    /// takes up once the old is down
    shown: Power,
    swap: Option<Power>,
    attach_l: Option<(Entity, Transform)>,
    /// ranged weapon meshes held in the left hand: (power, parent entity)
    held: Vec<(Power, Entity)>,
    /// the sword (put away while swimming or carrying a body)
    sword: Option<Entity>,
    swimming: bool,
    carry_seq: u32,
    drop_seq: u32,
    assassin_seq: u32,
    /// the sword locks seen begun and settled
    versus_seq: (u32, u32),
    /// the hands emptied by the scripts, and the sword being drawn again
    sheathed: bool,
    equipping: bool,
    /// the scene key the arms play (`matinee::SceneArms`)
    scene_key: Option<(u32, usize)>,
    /// his death's clip started
    died: bool,
    /// the arms' sockets effects play at (by name), the clips just started whose effects
    /// are due, and those effects: seconds to wait, socket, particle system
    sockets: Vec<(String, Entity)>,
    /// the lens effects due (seconds to wait, particle system)
    lens_fx: Vec<(f32, u32)>,
    started: Vec<String>,
    fx: Vec<(f32, Entity, u32)>,
}

fn spawn_arms(
    mut commands: Commands,
    assets: Option<Res<GameAssets>>,
    powers: Option<Res<Powers>>,
    cam: Query<Entity, With<PlayerCamera>>,
    player: Query<&LitActor, With<Player>>,
    sword: Query<Entity, With<SwordModel>>,
    level: Option<Res<crate::level::LevelInfo>>,
) {
    let Ok(cam) = cam.single() else { return };
    let Some(vis) = assets.as_ref().and_then(|a| a.npc_types.iter().flatten().find(|v| v.name == "player_arms")) else {
        warn!("no first-person arms in this map's cache");
        return;
    };
    let bones = &vis.skeleton.bones;
    let find = |n: &str| bones.iter().position(|b| b.name.eq_ignore_ascii_case(n));
    let (Some(cam_j), Some(attach)) = (find("camera_jnt"), find("handAttachment_R_jnt")) else { return };
    let chain = |side: &str| -> Option<Chain> {
        Some(Chain {
            upper: find(&format!("upper_arm_{side}_jnt"))?,
            lower: find(&format!("lower_arm_{side}_jnt"))?,
            hand: find(&format!("hand_{side}_jnt"))?,
        })
    };
    let (Some(right), Some(left)) = (chain("R"), chain("L")) else { return };

    // bind pose in rig space
    let mut bind_rot: Vec<Quat> = Vec::with_capacity(bones.len());
    let mut bind_pos: Vec<Vec3> = Vec::with_capacity(bones.len());
    for b in bones {
        let q = Quat::from_array(b.rotation).normalize();
        let t = Vec3::from(b.translation);
        if b.parent >= 0 && (b.parent as usize) < bind_rot.len() {
            let p = b.parent as usize;
            bind_pos.push(bind_pos[p] + bind_rot[p] * t);
            bind_rot.push(bind_rot[p] * q);
        } else {
            bind_pos.push(t);
            bind_rot.push(q);
        }
    }
    let animated = vis.anims.as_ref().and_then(|lib| ArmClips::resolve(lib).map(|c| (lib.clone(), c)));
    let rot = rig_to_camera();
    let offset = if animated.is_some() { Vec3::ZERO } else { IK_OFFSET };
    let root_t = Transform::from_translation(-(rot * bind_pos[cam_j]) + offset).with_rotation(rot);
    let root = commands.spawn((ArmsRoot, root_t, Visibility::default(), RenderLayers::layer(VIEW_LAYER))).id();
    commands.entity(cam).add_child(root);

    let mut joints = Vec::with_capacity(bones.len());
    for b in bones {
        let e = commands
            .spawn((
                Transform::from_translation(Vec3::from(b.translation)).with_rotation(Quat::from_array(b.rotation).normalize()),
                Visibility::default(),
                Name::new(b.name.clone()),
            ))
            .id();
        joints.push(e);
    }
    for (i, b) in bones.iter().enumerate() {
        let parent = if b.parent >= 0 && (b.parent as usize) < i { joints[b.parent as usize] } else { root };
        commands.entity(parent).add_child(joints[i]);
    }
    let slot = player.single().map(|l| l.slot).unwrap_or(0);
    let marked: Vec<PartMat> = vis.parts.view_parts.iter().map(|p| p.1.clone()).collect();
    let unmarked: Option<Vec<PartMat>> = vis.no_mark.as_ref().map(|v| v.view_parts.iter().map(|p| p.1.clone()).collect());
    for (k, (mesh, mat)) in vis.parts.view_parts.iter().enumerate() {
        let mut m = commands.spawn_empty();
        mat.apply(&mut m);
        let m = m
            .insert((
                Mesh3d(mesh.clone()),
                MeshTag(slot),
                SkinnedMesh { inverse_bindposes: vis.inverse_bindposes.clone(), joints: joints.clone() },
                NotShadowCaster,
                NoFrustumCulling,
                RenderLayers::layer(VIEW_LAYER),
                crate::fxlight::LitPart,
                ArmsPart(k),
            ))
            .id();
        if let Some(u) = unmarked.as_ref().and_then(|u| u.get(k)) {
            commands.entity(m).insert(MarkSkin { marked: marked[k].clone(), unmarked: u.clone(), shown: None });
        }
        commands.entity(root).add_child(m);
    }
    // weapons go to the mesh's sockets
    let socket = |name: &str, fallback: usize| -> (Entity, Transform) {
        match vis.skeleton.sockets.iter().find(|s| s.name.eq_ignore_ascii_case(name)).and_then(|s| find(&s.bone).map(|b| (b, s))) {
            Some((b, s)) => (joints[b], Transform::from_translation(Vec3::from(s.translation)).with_rotation(Quat::from_array(s.rotation).normalize())),
            None => (joints[fallback], Transform::IDENTITY),
        }
    };
    if let Ok(s) = sword.single() {
        let (bone, at) = socket("RightHandWpn", attach);
        commands.entity(s).insert(at);
        commands.entity(bone).add_child(s);
        // (its swings trail, by the arms' clips)
        if let Some(blade) = level.as_ref().and_then(|l| crate::trails::blade_of(&l.scene, "player_sword")) {
            commands.entity(s).insert(crate::trails::BladeTrail::new(blade, root, true));
        }
    }

    if let Some((lib, clips)) = animated {
        let attach_l = find("handAttachment_L_jnt").map(|i| socket("LeftHandWpn", i));
        // (Daud's wristbow on his forearm: the arms' `Wristbow` socket)
        let wrist = attach_l.and_then(|_| find("handAttachment_L_jnt")).filter(|_| vis.skeleton.sockets.iter().any(|s| s.name.eq_ignore_ascii_case("Wristbow"))).map(|i| socket("Wristbow", i));
        // crossbow and pistol in the left hand, shown while selected
        let mut held = Vec::new();
        if let (Some((hand, at)), Some(a)) = (attach_l, assets.as_ref()) {
            for (power, prop) in [
                (Power::Crossbow, "crossbow"),
                (Power::SleepDart, "crossbow"),
                (Power::IncendiaryBolt, "crossbow"),
                (Power::ExplosiveBolt, "crossbow"),
                (Power::Pistol, "pistol"),
                (Power::ExplosiveBullet, "pistol"),
                (Power::Heart, "heart"),
                (Power::Grenade, "grenade"),
                (Power::StickyGrenade, "grenade"),
                (Power::SpringRazor, "springrazor"),
                (Power::ArcMine, "arcmine"),
                (Power::StunMine, "stunmine"),
                (Power::Chokedust, "chokedust"),
            ] {
                let Some(parts) = a.props.get(prop) else { continue };
                let (hand, at) = if prop == "crossbow" { wrist.unwrap_or((hand, at)) } else { (hand, at) };
                let holder = commands.spawn((at, Visibility::Hidden, RenderLayers::layer(VIEW_LAYER))).id();
                // its own rig where the map has it cooked (its parts move with the hand's
                // clips), else the item at rest
                let rig = level.as_ref().and_then(|l| l.scene.prop_rigs.get(prop)).and_then(|t| a.npc_types.get(*t as usize)).and_then(|v| v.as_ref());
                if let Some(rv) = rig {
                    let bones = &rv.skeleton.bones;
                    let rj: Vec<Entity> = bones
                        .iter()
                        .map(|b| commands.spawn((Transform::from_translation(Vec3::from(b.translation)).with_rotation(Quat::from_array(b.rotation).normalize()), Visibility::Inherited)).id())
                        .collect();
                    for (i, b) in bones.iter().enumerate() {
                        let parent = if b.parent >= 0 && (b.parent as usize) < i { rj[b.parent as usize] } else { holder };
                        commands.entity(parent).add_child(rj[i]);
                    }
                    for (mesh, mat) in &rv.parts.view_parts {
                        let mut m = commands.spawn((
                            Mesh3d(mesh.clone()),
                            MeshTag(slot),
                            SkinnedMesh { inverse_bindposes: rv.inverse_bindposes.clone(), joints: rj.clone() },
                            NotShadowCaster,
                            NoFrustumCulling,
                            RenderLayers::layer(VIEW_LAYER),
                        ));
                        mat.apply(&mut m);
                        let m = m.id();
                        commands.entity(holder).add_child(m);
                    }
                    // the crossbow's bolt of the ammunition it shoots, loaded on its `Bolt_Socket`
                    // (`m_ArrowSocketName`) and brought in on its `Bolt_Reload_Socket` as it reloads
                    let bolt = match power {
                        Power::SleepDart => "bolt_sleep",
                        Power::IncendiaryBolt => "bolt_flare",
                        Power::ExplosiveBolt => "bolt_explosive",
                        _ => "bolt",
                    };
                    if let (true, Some(bp)) = (prop == "crossbow", a.props.get(bolt)) {
                        for (socket, reload) in [("Bolt_Socket", false), ("Bolt_Reload_Socket", true)] {
                            let Some(s) = rv.skeleton.sockets.iter().find(|s| s.name.eq_ignore_ascii_case(socket)) else { continue };
                            let Some(bi) = bones.iter().position(|b| b.name.eq_ignore_ascii_case(&s.bone)) else { continue };
                            let at = Transform::from_translation(Vec3::from(s.translation)).with_rotation(Quat::from_array(s.rotation).normalize());
                            let part = commands.spawn((at, Visibility::Hidden, RenderLayers::layer(VIEW_LAYER), LoadedBolt { reload })).id();
                            for (mesh, mat) in &bp.view_parts {
                                let mut m = commands.spawn((Mesh3d(mesh.clone()), MeshTag(slot), NotShadowCaster, RenderLayers::layer(VIEW_LAYER)));
                                mat.apply(&mut m);
                                let m = m.id();
                                commands.entity(part).add_child(m);
                            }
                            commands.entity(rj[bi]).add_child(part);
                        }
                    }
                    if let Some(lib) = rv.anims.clone() {
                        let mut anim = Animator::new(lib, &rv.skeleton, rj);
                        anim.sounds = false;
                        commands.entity(holder).insert((HeldRig(prop), anim));
                    }
                } else {
                    for (mesh, mat) in &parts.view_parts {
                        let mut m = commands.spawn((Mesh3d(mesh.clone()), MeshTag(slot), NotShadowCaster, RenderLayers::layer(VIEW_LAYER)));
                        mat.apply(&mut m);
                        let m = m.id();
                        commands.entity(holder).add_child(m);
                    }
                }
                if let Some(at) = parts.muzzle {
                    let m = commands.spawn((Muzzle(power), at, Visibility::Inherited)).id();
                    commands.entity(holder).add_child(m);
                }
                // (a gun's upgrades on it, each shown while owned: `show_upgrades`)
                for (key, up) in a.props.iter().filter(|(k, _)| k.strip_prefix("upgrade:").and_then(|r| r.split_once(':')).is_some_and(|(g, _)| g == prop)) {
                    let id = key.rsplit(':').next().unwrap_or_default().to_string();
                    let part = commands.spawn((Transform::IDENTITY, Visibility::Hidden, RenderLayers::layer(VIEW_LAYER), UpgradePart { id, explosive: power == Power::ExplosiveBullet })).id();
                    for (mesh, mat) in &up.view_parts {
                        let mut m = commands.spawn((Mesh3d(mesh.clone()), MeshTag(slot), NotShadowCaster, RenderLayers::layer(VIEW_LAYER)));
                        mat.apply(&mut m);
                        let m = m.id();
                        commands.entity(part).add_child(m);
                    }
                    commands.entity(holder).add_child(part);
                }
                commands.entity(hand).add_child(holder);
                held.push((power, holder));
            }
        }
        // the sockets the clips' effects play at (the Mark: `Tattoo`), and Possession's glow
        // (`LeftHand_PowerCastA`)
        let mut sockets = Vec::new();
        if let Some(level) = level.as_ref() {
            let mut names: Vec<&str> = level.scene.arm_fx.iter().map(|f| f.socket.as_str()).collect();
            names.push(POSSESS_HAND_SOCKET);
            names.sort_unstable();
            names.dedup();
            for name in names {
                let Some(s) = vis.skeleton.sockets.iter().find(|s| s.name.eq_ignore_ascii_case(name)) else { continue };
                let Some(b) = find(&s.bone) else { continue };
                let at = Transform::from_translation(Vec3::from(s.translation)).with_rotation(Quat::from_array(s.rotation).normalize());
                let e = commands.spawn((at, Visibility::default(), Name::new(format!("socket {name}")))).id();
                commands.entity(joints[b]).add_child(e);
                sockets.push((name.to_string(), e));
            }
        }
        let animator = Animator::new(lib, &vis.skeleton, joints.clone());
        let eye = root_t * animator.bind_model(cam_j);
        commands.entity(root).insert((
            animator,
            ArmsAnim {
                clips: Arc::new(clips),
                fingers_l: Arc::new(
                    bones
                        .iter()
                        .enumerate()
                        .filter(|(_, b)| {
                            let n = b.name.to_ascii_lowercase();
                            n.ends_with("_l_jnt") && ["thumb_", "index_", "middle_", "ring_", "pinky_"].iter().any(|f| n.starts_with(f))
                        })
                        .map(|(i, _)| i)
                        .collect(),
                ),
                left_arm: Arc::new({
                    let mut on = vec![false; bones.len()];
                    let root = find("Collarbone_L_Jnt");
                    for (i, b) in bones.iter().enumerate() {
                        // (parents come before their children)
                        on[i] = Some(i) == root || (b.parent >= 0 && on.get(b.parent as usize).copied().unwrap_or(false));
                    }
                    on
                }),
                eye,
                bob_phase: 0.0,
                bob_weight: 0.0,
                breath_t: 0.0,
                swinging: false,
                next_attack: 0,
                swing_clip: (0, 0),
                slash: None,
                bounce_seen: 0,
                blocking: false,
                blink_aiming: false,
                summon_cancels: 0,
            pull_cancels: 0,
            pull_lifting: false,
            pull_lift_ends: (0, 0),
                possess_cancels: 0,
                cooking: false,
                choking: false,
                choke_phase: 0,
                grabbed: false,
                pinned: false,
                tentacle_grab: false,
                oneshot: false,
                airborne: false,
                grounded: true,
                mantling: false,
                sliding: false,
                raised: None,
                parry_flash: 0.0,
                damage_flash: 0.0,
                blow_seen: 0,
                knock_seen: 0,
                cast_seq: powers.as_ref().map(|p| p.cast_seq).unwrap_or(0),
                reload_seq: powers.as_ref().map(|p| p.reload_anim.0).unwrap_or(0),
                no_mana_seq: powers.as_ref().map(|p| p.no_mana_seq).unwrap_or(0),
                selected: powers.as_ref().map(|p| p.selected).unwrap_or(Power::Pistol),
                shown: powers.as_ref().map(|p| p.selected).unwrap_or(Power::Pistol),
                swap: None,
                attach_l,
                held,
                sword: sword.single().ok(),
                swimming: false,
                sheathed: false,
                equipping: false,
                scene_key: None,
                died: false,
                carry_seq: 0,
                drop_seq: 0,
                assassin_seq: 0,
                versus_seq: (0, 0),
                sockets,
                started: Vec::new(),
                lens_fx: Vec::new(),
                fx: Vec::new(),
            },
        ));
    }

    // A fist for the procedural fallback: every digit joint bends towards the palm.
    let mut curl = Vec::new();
    if let (Some(hand), Some(i0), Some(m0), Some(p0)) = (find("hand_R_jnt"), find("index_0_R_jnt"), find("middle_0_R_jnt"), find("pinky_0_R_jnt")) {
        let fingers = (bind_pos[m0] - bind_pos[hand]).normalize_or_zero();
        let knuckles = (bind_pos[p0] - bind_pos[i0]).normalize_or_zero();
        let palm = fingers.cross(knuckles).normalize_or_zero();
        for (digit, angles) in [
            ("index", [1.1, 1.3, 0.9]),
            ("middle", [1.2, 1.35, 0.9]),
            ("ring", [1.25, 1.35, 0.9]),
            ("pinky", [1.3, 1.3, 0.9]),
            ("thumb", [0.2, 0.5, 0.5]),
        ] {
            for k in 0..3 {
                let (Some(j), Some(c)) = (find(&format!("{digit}_{k}_R_jnt")), find(&format!("{digit}_{}_R_jnt", k + 1)).or(find(&format!("{digit}_end_R_jnt")))) else { continue };
                let dir = (bind_pos[c] - bind_pos[j]).normalize_or_zero();
                let axis = dir.cross(palm).normalize_or_zero();
                if axis != Vec3::ZERO {
                    curl.push((j, bind_rot[j].inverse() * axis, angles[k]));
                }
            }
        }
    }
    commands.entity(root).insert(ArmRig {
        curl,
        joints,
        bind_t: bones.iter().map(|b| Vec3::from(b.translation)).collect(),
        bind_local: bones.iter().map(|b| Quat::from_array(b.rotation).normalize()).collect(),
        bind_rot,
        bind_pos,
        right,
        left,
        attach,
        camera: cam_j,
        collar: [find("Collarbone_R_Jnt"), find("Collarbone_L_Jnt")],
    });
}

fn pick(v: &[ClipId], i: usize) -> Option<ClipId> {
    if v.is_empty() {
        None
    } else {
        Some(v[i % v.len()])
    }
}

/// Choose the arms' clips from the player's state.
#[allow(clippy::type_complexity)]
fn animate_arms(
    stats: Res<PlayerStats>,
    attrs: Res<crate::gamedata::Attrs>,
    powers: Res<Powers>,
    possession: Res<crate::possession::Possession>,
    player: Query<(&Player, &Sword, Option<&Choking>, Option<&crate::npc::Grabbed>)>,
    mut arms: Query<(&mut Animator, &mut ArmsAnim, &mut Visibility), With<ArmsRoot>>,
    mut held: Query<&mut Visibility, Without<ArmsRoot>>,
    (swim, mut carry, peek, cine, scene_arms): (Res<crate::swim::Swim>, ResMut<crate::carry::Carry>, Res<crate::keyhole::Peek>, Res<crate::script_world::Cinematic>, Res<crate::matinee::SceneArms>),
    (versus, slide, edge, zoom): (Res<crate::combat::Versus>, Res<crate::player::Slide>, Res<crate::player::MantleEdge>, Res<crate::zoom::Zoom>),
    mut death_fall: ResMut<DeathFall>,
    (summon_aim, possess_aim, pull_aim): (Option<Res<crate::summon::SummonAim>>, Option<Res<crate::possession::PossessAim>>, Option<Res<crate::pull::PullAim>>),
) {
    let (Ok((p, sword, choking, grabbed)), Ok((mut anim, mut st, mut vis))) = (player.single(), arms.single_mut()) else { return };
    let st = &mut *st;
    let c = st.clips.clone();
    // dead: he falls as the original's death has him, his hands and the view going with his
    // body (by the side the last blow came from: `Generic_Death_Impact` ahead, `_Back`, `_Left`,
    // `_Right`; `Generic_Death_FallImpact_Back` from a fall, `Generic_Death_Water` drowned)
    let dying = stats.dead && c.death[1].is_some() && possession.host.is_none() && !cine.hides_player();
    if dying {
        if !st.died {
            st.died = true;
            // (pinned by a wolfhound as he died)
            let pinned = (std::mem::take(&mut st.grabbed) && st.pinned) || grabbed.is_some_and(|g| g.hound);
            let cl = if pinned && c.death_pinned[0].is_some() {
                c.death_pinned[0]
            } else if stats.knocked > 0.0 && c.death_jumped.is_some() {
                // (killed by a wolfhound's jump attack)
                c.death_jumped
            } else if stats.died_drowning {
                c.death_drowned.or(c.death_water).or(c.death[1])
            } else if swim.swimming() {
                c.death_water.or(c.death[1])
            } else if stats.died_falling {
                c.death_fall.or(c.death[1])
            } else {
                c.death[(stats.blow.1 as usize).min(3)].or(c.death[1])
            };
            if let Some(cl) = cl {
                anim.restart(cl, false, 1.0, 0.15);
                anim.stop_overlay(0.15);
                anim.stop_upper(0.15);
                death_fall.0 = true;
                if std::env::var_os("DH_ARMS_LOG").is_some() {
                    info!("arms: death {}", anim.lib.clip(cl).name);
                }
            }
        } else if let Some(cl) = c.death_pinned[1].filter(|_| anim.finished() && anim.current().is_some_and(|p| Some(p.clip) == c.death_pinned[0] || Some(p.clip) == c.death_jumped)) {
            anim.restart(cl, false, 1.0, 0.1);
            if std::env::var_os("DH_ARMS_LOG").is_some() {
                info!("arms: death {}", anim.lib.clip(cl).name);
            }
        }
        if *vis != Visibility::Inherited {
            *vis = Visibility::Inherited;
        }
        return;
    }
    st.died = false;
    death_fall.0 = false;
    // inside a possessed host there are no hands of Corvo's to see, nor with his eye to a keyhole
    let hidden = stats.dead || p.noclip || possession.host.is_some() || peek.at.is_some() || cine.hides_player();
    let want = if hidden { Visibility::Hidden } else { Visibility::Inherited };
    if *vis != want {
        *vis = want;
    }

    // ---- right arm / body (base layer)
    let mut base_busy = false;
    // a scene's own animation of Corvo's arms
    match scene_arms.playing(&anim.lib) {
        Some((seq, t, looping, rate, key)) => {
            if let Some(cl) = anim.lib.find(seq) {
                if st.scene_key != Some(*key) {
                    st.scene_key = Some(*key);
                    anim.restart(cl, *looping, *rate, 0.2);
                    anim.seek(*t);
                } else if !*looping {
                    // (kept to the key's place: a stretched key's pace, its loops starting over)
                    anim.play(cl, false, *rate, 0.2);
                    if anim.current().is_some_and(|c| (c.t - *t).abs() > 0.08) {
                        anim.seek(*t);
                    }
                }
                base_busy = true;
            }
        }
        None => st.scene_key = None,
    }
    // swimming: both arms stroke (`Empty_Swim*` by the direction swum), the sword away
    let empty = stats.unarmed || stats.sheathed;
    let full_body = scene_arms.playing(&anim.lib).is_some()
        || swim.swimming() || carry.carrying();
    let away = full_body || empty;
    if away != st.swimming {
        st.swimming = away;
        if let Some(mut v) = st.sword.and_then(|s| held.get_mut(s).ok()) {
            *v = if away { Visibility::Hidden } else { Visibility::Inherited };
        }
    }
    // a body on the shoulder: the original's carry clips (lift and drop play through)
    if carry.carrying() {
        let moving = Vec2::new(p.velocity.x, p.velocity.z).length() > 0.4;
        if let Some(cl) = crate::carry::master_clip(carry.phase, moving).and_then(|n| anim.lib.find(n)) {
            let oneshot = !matches!(carry.phase, crate::carry::CarryPhase::Hold);
            if carry.resume_animation {
                carry.resume_animation = false;
                st.carry_seq = carry.seq;
                anim.restart(cl, !oneshot, 1.0, 0.0);
                if let Some((name, time)) = &carry.animation {
                    if anim.lib.clip(cl).name == *name {
                        anim.seek(*time);
                        if std::env::var_os("DH_CARRY_LOG").is_some() {
                            info!("carry: resumed {name} at {time:.3}s");
                        }
                    }
                }
            } else if oneshot {
                if carry.seq != st.carry_seq {
                    st.carry_seq = carry.seq;
                    anim.restart(cl, false, 1.0, 0.15);
                }
            } else {
                anim.play(cl, true, 1.0, 0.25);
            }
        }
        base_busy = true;
    } else if swim.swimming() {
        // (only in the water: empty hands on land are the empty idle's, below)
        let local = Quat::from_rotation_y(-p.yaw) * p.velocity;
        let speed = local.length();
        let (cl, rate) = if speed < 0.5 {
            (c.swim_idle, 1.0)
        } else {
            let k = if local.z.abs() >= local.x.abs() {
                if local.z < 0.0 { 0 } else { 2 }
            } else if local.x > 0.0 {
                1
            } else {
                3
            };
            (c.swim[k].or(c.swim[0]), speed / 3.0)
        };
        if let Some(cl) = cl.or(c.idle) {
            anim.play(cl, true, rate.clamp(0.6, 1.8), 0.3);
        }
        base_busy = true;
    }
    // a mantle: the arms reach for the edge and haul him over (`Ply_Player_Mantle_at`, in the
    // full-body picker: low, medium or high by the edge's height, to 1.2 and 1.7 m; the sneaking
    // ones into a space too low to stand; empty-handed or sword in hand), at Acrobat's rate
    // (`MantleAnimRate`): each lasts as long as its climb
    match p.mantle.filter(|_| !base_busy) {
        Some(_) => {
            if !st.mantling {
                let ty = if edge.h <= 1.2 { 0 } else if edge.h <= 1.7 { 1 } else { 2 };
                if let Some(cl) = c.mantle[!empty as usize][p.crouched as usize][ty] {
                    anim.restart(cl, false, attrs.mantle_rate.max(0.01), 0.08);
                    st.oneshot = true;
                }
            }
            // (no landing after it, though it began in the air)
            st.airborne = false;
            st.mantling = true;
            base_busy = true;
        }
        None => st.mantling = false,
    }
    // a slide (`Ply_Player_Slide_at`'s in, loop and out): held through it, then out to the
    // sprint it may go back to or to a sneak (`SlideOutSprint`, `SlideOutSneak`)
    let [s_in, s_loop, s_sneak, s_sprint] = c.slide[!empty as usize];
    match slide.0.filter(|_| !base_busy) {
        Some(_) => {
            if !st.sliding {
                if let Some(cl) = s_in.or(s_loop) {
                    anim.restart(cl, s_in.is_none(), 1.0, 0.08);
                }
            } else if let Some(cl) = s_loop.filter(|l| anim.finished() && anim.current().map(|q| q.clip) != Some(*l)) {
                anim.play(cl, true, 1.0, 0.05);
            }
            st.sliding = true;
            base_busy = true;
        }
        None if st.sliding => {
            st.sliding = false;
            // (empty hands: their idle takes over)
            if let Some(cl) = (if p.sprinting { s_sprint } else { s_sneak }).filter(|_| !base_busy && !empty) {
                anim.restart(cl, false, 1.0, 0.1);
                st.oneshot = true;
            }
        }
        None => {}
    }
    // the sword drawn again: its equip clip plays through
    if stats.sheathed != st.sheathed {
        st.sheathed = stats.sheathed;
        if !stats.sheathed && !base_busy {
            if let Some(cl) = c.equip[p.crouched as usize].or(c.equip[0]) {
                anim.restart(cl, false, 1.0, 0.1);
                st.equipping = true;
            }
        }
    }
    if st.equipping {
        if anim.finished() || base_busy {
            st.equipping = false;
        } else {
            base_busy = true;
        }
    }
    // no sword: the empty hands, low; struck, thrown from the blow's side or knocked down
    // (`Empty_BigHit_*`, `Empty_Knockdown*`), as the sword's
    if !base_busy && empty {
        let side = |s: u8| (s as usize).min(3);
        let struck = if stats.knockdown.0 != st.knock_seen {
            c.knockdown[1][side(stats.knockdown.1)].map(|cl| (cl, 1.0))
        } else if stats.blow.0 != st.blow_seen || stats.damage_flash > st.damage_flash + 1e-3 {
            let s = if stats.blow.0 != st.blow_seen { side(stats.blow.1) } else { 0 };
            c.empty_hit[s].map(|cl| (cl, 1.4))
        } else if st.airborne && p.grounded {
            // (touching down: its landing, at Agility's rate as the sword's)
            c.empty_jump[1].map(|cl| (cl, attrs.land_rate.max(0.1)))
        } else {
            None
        };
        if let Some((cl, rate)) = struck {
            anim.restart(cl, false, rate, 0.05);
            st.oneshot = true;
            if std::env::var("DH_ARMS_LOG").is_ok() {
                info!("arms: struck empty-handed {}", anim.lib.clip(cl).name);
            }
        }
        if st.oneshot && !anim.finished() {
            // (let it play out)
        } else {
            st.oneshot = false;
            let moving = Vec2::new(p.velocity.x, p.velocity.z).length() > 0.4;
            let cl = if !p.grounded && p.air_time > 0.25 {
                c.empty_jump[0]
            } else if p.crouched {
                if moving { c.empty_sneak[1] } else { c.empty_sneak[0] }
            } else {
                c.empty_idle
            };
            if let Some(cl) = cl.or(c.empty_idle).or(c.idle) {
                anim.play(cl, true, 1.0, 0.3);
            }
        }
        base_busy = true;
    }
    // a weeper holds Corvo's arm, or a wolfhound has it in its teeth
    let [grab_in, grab_loop, _] = match grabbed {
        Some(g) if g.hound && c.pinned[1].is_some() => c.pinned,
        Some(g) if g.tentacle && c.tentacle[1].is_some() => c.tentacle,
        _ => [c.grabbed_in, c.grabbed_loop, c.grabbed_out],
    };
    if grabbed.is_some() {
        if !st.grabbed {
            st.grabbed = true;
            st.pinned = grabbed.is_some_and(|g| g.hound) && c.pinned[1].is_some();
            st.tentacle_grab = grabbed.is_some_and(|g| g.tentacle) && c.tentacle[1].is_some();
            if let Some(cl) = grab_in.or(grab_loop) {
                anim.restart(cl, false, 1.0, 0.1);
            }
        } else if anim.finished() {
            if let Some(cl) = grab_loop {
                anim.play(cl, true, 1.0, 0.1);
            }
        }
        base_busy = true;
    } else if st.grabbed {
        st.grabbed = false;
        if let Some(cl) = if st.pinned { c.pinned[2] } else if st.tentacle_grab { c.tentacle[2] } else { c.grabbed_out } {
            anim.restart(cl, false, 1.0, 0.1);
            st.oneshot = true;
        }
    }
    if base_busy {
    } else if let Some(ch) = choking {
        // the hold (its In, then its loop), then its Win or Lose chain played through
        let phase = match ch.end {
            None => 1,
            Some(crate::combat::ChokeEnd::Win(_)) => 2,
            Some(crate::combat::ChokeEnd::Lose(_)) => 3,
        };
        if !st.choking || st.choke_phase != phase {
            st.choking = true;
            st.choke_phase = phase;
            let cl = match phase {
                1 => c.choke_in.or(c.choke_loop),
                2 => c.choke_win,
                _ => c.choke_lose,
            };
            if let Some(cl) = cl {
                anim.restart(cl, false, 1.0, 0.15);
            }
        } else if phase == 1 && anim.finished() {
            if let Some(cl) = c.choke_loop {
                anim.play(cl, true, 1.0, 0.1);
            }
        }
        base_busy = true;
    } else if st.choking {
        st.choking = false;
        // let go during the hold (the victim gone): its Cancel
        if st.choke_phase == 1 && !carry.carrying() {
            if let Some(cl) = c.choke_cancel {
                anim.restart(cl, false, 1.0, 0.1);
                st.oneshot = true;
            }
        }
        st.choke_phase = 0;
    }
    // a sword lock: the struggle, then how it went
    if !base_busy && versus.seq != st.versus_seq.0 {
        st.versus_seq.0 = versus.seq;
        if let Some(cl) = c.versus[0] {
            anim.restart(cl, true, 1.0, 0.08);
        }
    }
    if versus.npc.is_some() {
        base_busy = true;
    } else if versus.done_seq != st.versus_seq.1 {
        st.versus_seq.1 = versus.done_seq;
        let k = match versus.outcome {
            1 => 1,
            2 | 3 => 2,
            _ => 3,
        };
        if let Some(cl) = c.versus[k] {
            anim.restart(cl, false, 1.0, 0.08);
            st.oneshot = true;
            base_busy = true;
        }
    }
    // a ground assassination: Corvo's side of it
    if !base_busy && sword.assassin_seq != st.assassin_seq {
        st.assassin_seq = sword.assassin_seq;
        if let Some(cl) = anim.lib.find(&sword.assassin_clip).or(c.sneak_attack) {
            anim.restart(cl, false, 1.0, 0.05);
            st.oneshot = true;
            base_busy = true;
            st.started.push(sword.assassin_clip.clone());
        }
    }
    if !base_busy && sword.drop_seq != st.drop_seq {
        st.drop_seq = sword.drop_seq;
        if let Some(cl) = c.drop_kill[sword.drop_dir as usize % 4].or(c.sneak_attack) {
            anim.restart(cl, false, 1.0, 0.05);
            st.oneshot = true;
        }
    }
    if !base_busy {
        if sword.swing > 0.0 && !st.swinging {
            st.swinging = true;
            st.bounce_seen = sword.bounce_seq;
            let (kind, i) = if p.crouched && c.sneak_attack.is_some() {
                (2, 0)
            } else if sword.swing_dir > 0.0 {
                (0, st.next_attack / 2)
            } else {
                (1, st.next_attack / 2)
            };
            st.swing_clip = (kind, i);
            st.slash = Some((kind == 1) as u8);
            let cl = match kind {
                2 => c.sneak_attack,
                0 => pick(&c.attack_right, i),
                _ => pick(&c.attack_left, i),
            };
            st.next_attack += 1;
            if let Some(cl) = cl {
                anim.restart(cl, false, attrs.melee_rate, 0.06);
                st.oneshot = true;
            }
        } else if sword.swing == 0.0 {
            st.swinging = false;
        }
        // stopped short (on a blade, on a wall): its bounce (the `_Big` clips' rate 1.25)
        if sword.bounce_seq != st.bounce_seen {
            st.bounce_seen = sword.bounce_seq;
            let (kind, i) = st.swing_clip;
            let cl = match kind {
                2 => c.sneak_big[sword.bounce_wall as usize].or(c.sneak_big[0]),
                0 => pick(&c.attack_right_big, i),
                _ => pick(&c.attack_left_big, i),
            };
            if let Some(cl) = cl {
                anim.restart(cl, false, 1.25 * attrs.melee_rate, 0.05);
                st.oneshot = true;
            }
        }
        if sword.parry_flash > st.parry_flash + 1e-3 {
            if let Some(cl) = pick(&c.parry, st.next_attack) {
                anim.restart(cl, false, 1.3, 0.05);
                st.oneshot = true;
            }
        }
        // knocked down (a wolfhound's jump attack): its clip from its side, over the blow's
        if stats.knockdown.0 != st.knock_seen {
            st.knock_seen = stats.knockdown.0;
            let side = (stats.knockdown.1 as usize).min(3);
            if let Some(cl) = c.knockdown[empty as usize][side].or(c.knockdown[0][side]) {
                anim.restart(cl, false, 1.0, 0.05);
                st.oneshot = true;
                st.blow_seen = stats.blow.0;
                st.damage_flash = stats.damage_flash;
                if std::env::var("DH_ARMS_LOG").is_ok() {
                    info!("arms: knocked down {}", anim.lib.clip(cl).name);
                }
            }
        }
        // a blow: the arms thrown from its side (a kick too, harmless as it is); other harm, as
        // from ahead
        let blow = stats.blow.0 != st.blow_seen;
        if blow || stats.damage_flash > st.damage_flash + 1e-3 {
            let side = if blow { stats.blow.1 as usize } else { 0 };
            if std::env::var("DH_ARMS_LOG").is_ok() {
                info!("arms: hit from side {side} (blow {blow})");
            }
            if let Some(cl) = c.hit[side.min(3)].or(c.hit[0]) {
                anim.restart(cl, false, 1.4, 0.05);
                st.oneshot = true;
            }
        }
        if sword.blocking && !st.oneshot {
            if !st.blocking {
                st.blocking = true;
                if let Some(cl) = c.parry_in.or(c.parry_idle) {
                    anim.restart(cl, false, 1.5, 0.08);
                }
            } else if anim.finished() {
                if let Some(cl) = c.parry_idle {
                    anim.play(cl, true, 1.0, 0.1);
                }
            }
            base_busy = true;
        } else if st.blocking {
            st.blocking = false;
            if let Some(cl) = c.parry_out {
                anim.restart(cl, false, 1.2, 0.08);
                st.oneshot = true;
            }
        }
    }
    st.parry_flash = sword.parry_flash;
    st.damage_flash = stats.damage_flash;
    st.blow_seen = stats.blow.0;
    st.knock_seen = stats.knockdown.0;
    // touching down: the landing for how hard and how he stands (a hard one past
    // `StatePlayerMasterFalling.m_fFall_Stun_Vel`, 2200 uu/s), as roshan's r614/blink research
    // decoded the landing's actions
    // (leaving the ground rising: a jump, not a fall)
    let took_off = std::mem::replace(&mut st.grounded, p.grounded) && !p.grounded && p.velocity.y > 1.0;
    if !base_busy && took_off {
        if let Some(cl) = c.jump_in {
            anim.restart(cl, false, 1.0, 0.08);
            st.oneshot = true;
        }
    }
    let mut landed = false;
    if !p.grounded && p.air_time > 0.25 {
        st.airborne = true;
    } else if p.grounded && st.airborne {
        st.airborne = false;
        landed = true;
    }
    if !base_busy && landed {
        let cl = if p.fall_speed > FALL_STUN { c.land_big.or(c.land) } else if p.crouched { c.land_sneak.or(c.land) } else { c.land };
        if let Some(cl) = cl {
            if std::env::var("DH_BOB_LOG").is_ok() {
                info!("arms: landing at {:.1} m/s ({})", p.fall_speed, if p.fall_speed > FALL_STUN { "hard" } else if p.crouched { "sneaking" } else { "soft" });
            }
            // (Agility's quicker recovery: `LandAnimRate`)
            anim.restart(cl, false, attrs.land_rate.max(0.1), 0.08);
            st.oneshot = true;
        }
    }
    if !base_busy {
        if st.oneshot && !anim.finished() {
            // let the one-shot play out
        } else {
            st.oneshot = false;
            let speed = Vec2::new(p.velocity.x, p.velocity.z).length();
            let (cl, rate) = if !p.grounded && p.air_time > 0.25 {
                (c.jump.or(c.idle), 1.0)
            } else if speed < 0.4 {
                (if p.crouched { c.sneak_idle.or(c.idle) } else { c.idle }, 1.0)
            } else if p.crouched {
                if speed > 2.5 { (c.sneak_run.or(c.sneak_walk), speed / 3.0) } else { (c.sneak_walk.or(c.walk), speed / 1.9) }
            } else if p.sprinting && speed > 4.5 {
                (c.sprint.or(c.run), speed / 6.2)
            } else if speed > 2.6 {
                (c.run.or(c.walk), speed / 4.5)
            } else {
                (c.walk, speed / 2.5)
            };
            if let Some(cl) = cl {
                anim.play(cl, true, rate.clamp(0.6, 1.6), 0.25);
            }
        }
    }

    // ---- left arm (overlay): powers and ranged weapons
    for (power, e) in &st.held {
        if let Ok(mut v) = held.get_mut(*e) {
            // (a grenade thrown: out of the hand from its release, half way through the throw,
            // until the next one is taken, or for good with none left)
            let thrown = matches!(power, Power::Grenade | Power::StickyGrenade | Power::Chokedust)
                && (anim.overlay_is(st.clips.grenade_empty)
                    || anim.overlay_is(st.clips.grenade_throw) && anim.overlay_state().0.zip(anim.overlay_time()).is_some_and(|(cl, t)| t > anim.lib.duration(cl) * 0.45));
            let want = if !full_body && *power == st.shown && !thrown { Visibility::Inherited } else { Visibility::Hidden };
            if *v != want { *v = want; }
        }
    }
    if full_body {
        if std::env::var("DH_ARMS_LOG").is_ok() {
            info!("arms: full body {:?} base {}", scene_arms.0.as_ref().map(|s| s.0.clone()), anim.current().map(|p| anim.lib.clip(p.clip).name.clone()).unwrap_or_default());
        }
        anim.stop_overlay(0.1);
        // Do not replay casts or switches made before the full-body sequence
        // when normal gameplay resumes.
        st.cast_seq = powers.cast_seq;
        st.reload_seq = powers.reload_anim.0;
        st.selected = powers.selected;
        return;
    }
    let ranged = powers.selected.is_gadget();
    // a gun's reload: its stages' clips, the gun held open between them
    let reloading = crate::aim::Weapon::of(powers.selected).is_some_and(|w| powers.reload[w as usize] > 0.0);
    if powers.reload_anim.0 != st.reload_seq {
        st.reload_seq = powers.reload_anim.0;
        // (the quick shot reloads as it goes: its rapid reload isn't played over it)
        let quick = anim.overlay_is(c.crossbow_fire_fast) && !anim.overlay_finished();
        if let Some(&(name, cl)) = c.reload.iter().find(|(n, _)| *n == powers.reload_anim.1).filter(|(n, _)| !(quick && *n == "Crossbow_RapidReload")) {
            anim.restart_overlay(cl, false, 1.0, 0.08);
            st.started.push(name.to_string());
        }
    }
    // the sword slashing: the left hand with a power or gadget goes with it
    if let Some(side) = st.slash.take() {
        let set = (powers.selected == Power::Heart || ranged) as usize;
        let busy = powers.aiming || powers.cooking.is_some() || zoom.level > 0 || !(anim.overlay_finished() || anim.overlay_is(c.powers_idle) || anim.overlay_is(c.gadgets_idle) || c.left_loco.iter().flatten().any(|l| anim.overlay_is(*l)));
        if let Some(cl) = c.left_slash[set][side as usize].filter(|_| powers.selected != Power::Empty && !busy) {
            anim.restart_overlay(cl, false, 1.0, 0.08);
        }
    }
    // a cast without the mana for it: the hand's empty gesture
    if powers.no_mana_seq != st.no_mana_seq {
        st.no_mana_seq = powers.no_mana_seq;
        if let Some(cl) = c.out_of_mana.filter(|_| !ranged) {
            anim.restart_overlay(cl, false, 1.0, 0.08);
        }
    }
    let fast_bow = stats.upgrades.iter().any(|u| u == "Twk_Upgrade_Crossbow_ReloadSpeed");
    // (a gun raised to the eye through the optics: its own clips, `FirePistolZoom`,
    // `CrossbowFireZoom`; the pistol's accurate shot with `Twk_Upgrade_Pistol_Accuracy2`)
    let gun = crate::aim::Weapon::of(powers.selected).map(|w| w as usize);
    let zoomed = zoom.level > 0 && gun.is_some_and(|g| c.spyglass[g][1].is_some());
    let accurate = stats.upgrades.iter().any(|u| u == "Twk_Upgrade_Pistol_Accuracy2");
    if powers.cast_seq != st.cast_seq {
        st.cast_seq = powers.cast_seq;
        let gun_shot = |g: usize| {
            let sg = c.spyglass[g];
            if zoomed {
                if accurate && g == 0 { sg[4].or(sg[3]) } else { sg[3] }
            } else if accurate && g == 0 {
                c.pistol_fire_accurate
            } else {
                None
            }
        };
        let cl = match powers.cast_power {
            Power::Blink => c.blink,
            Power::Windblast => c.windblast,
            Power::SummonAssassin => c.summon.or(c.windblast),
            Power::Pull => c.pull.or(c.windblast),
            Power::BendTime => c.bend_time,
            Power::DarkVision => c.dark_vision.or(c.switch),
            Power::Empty | Power::Heart => c.switch,
            Power::Possess => c.possess,
            Power::DevouringSwarm => c.swarm,
            Power::Crossbow | Power::SleepDart | Power::IncendiaryBolt | Power::ExplosiveBolt => gun_shot(1).or(c.crossbow_fire_fast.filter(|_| fast_bow)).or(c.crossbow_fire),
            Power::Pistol | Power::ExplosiveBullet => gun_shot(0).or(c.pistol_fire),
            Power::Grenade | Power::StickyGrenade | Power::Chokedust => c.grenade_throw,
            Power::SpringRazor | Power::ArcMine | Power::StunMine => c.razor_place,
        };
        if let Some(cl) = cl {
            // (`FireReloadFastCrossbow`'s `m_fPlayRateOverride` 1.5)
            let rate = if Some(cl) == c.crossbow_fire_fast { 1.5 } else { 1.0 };
            anim.restart_overlay(cl, false, rate, 0.08);
            st.started.push(cast_clip(powers.cast_power).to_string());
        }
    } else if powers.selected != st.selected {
        // from a power to a power, the mark's switch (`Powers_Switch`); a gadget taken up or put
        // away (or a power from or to the empty hand), the hand goes down with the old
        // (`*_Ready_Unequip`) and comes up with the new (`*_Ready_Equip`) once it is down
        let set = |p: Power| if p == Power::Empty { None } else { Some(p.is_gadget() as usize) };
        let (old, new) = (set(st.shown), set(powers.selected));
        if old == Some(0) && new == Some(0) && st.swap.is_none() {
            st.shown = powers.selected;
            if let Some(cl) = c.switch {
                anim.restart_overlay(cl, false, 1.3, 0.1);
                st.started.push("Powers_Switch".to_string());
            }
        } else if st.swap.is_some() {
            // (already going down: it comes up with the newest)
            st.swap = Some(powers.selected);
        } else if let Some(cl) = old.and_then(|o| c.left_equip[o][1]) {
            anim.restart_overlay(cl, false, 1.0, 0.08);
            st.swap = Some(powers.selected);
            st.started.push(anim.lib.clip(cl).name.clone());
        } else {
            st.shown = powers.selected;
            if let Some(cl) = new.and_then(|n| c.left_equip[n][0]) {
                anim.restart_overlay(cl, false, 1.0, 0.08);
                st.started.push(anim.lib.clip(cl).name.clone());
            }
        }
    }
    // the old put away: the new taken up
    if let Some(to) = st.swap {
        let down = c.left_equip.iter().any(|e| anim.overlay_is(e[1]));
        if !down || anim.overlay_finished() {
            st.swap = None;
            st.shown = to;
            let set = (to != Power::Empty).then(|| to.is_gadget() as usize);
            if let Some(cl) = set.and_then(|n| c.left_equip[n][0]) {
                anim.restart_overlay(cl, false, 1.0, 0.08);
                st.started.push(anim.lib.clip(cl).name.clone());
            }
        }
    }
    if powers.selected != st.selected {
        st.selected = powers.selected;
    }
    let bow = matches!(powers.selected, Power::Crossbow | Power::SleepDart | Power::IncendiaryBolt | Power::ExplosiveBolt);
    // the left hand's clips move the left arm alone (`Ply_Player_at`'s `LeftArmFilter`, a
    // blend per bone from `Collarbone_L_Jnt`: its casts, throws and guns, while the sword arm
    // keeps its own); possession takes the whole body (its `FullBodyStatePicker`)
    anim.overlay_mask = if anim.overlay_is(c.possess) || st.left_arm.iter().all(|on| !on) { None } else { Some(st.left_arm.clone()) };
    // (the Heart held up as the gadgets are, its fingers about it: not the open hand of a power)
    let heart = powers.selected == Power::Heart;
    anim.masked = c.heart_fingers.filter(|_| heart && !st.fingers_l.is_empty()).map(|f| (f, 0.0, st.fingers_l.clone()));
    // (nothing in the left hand: it rests as the body's clip has it)
    let idle = if powers.selected == Power::Empty {
        None
    } else if heart {
        c.gadgets_idle.or(c.powers_idle)
    } else if ranged {
        if bow { c.wristbow_idle.or(c.gadgets_idle) } else { c.gadgets_idle }
    } else {
        c.powers_idle
    };
    // on the move with a power or gadget in hand: the left hand's own walk, sprint or leap,
    // in step with the body's clip (UE3's sync group: the same part of its cycle); crouched,
    // or with the wrist bow, the idle
    let speed = Vec2::new(p.velocity.x, p.velocity.z).length();
    let set = (heart || ranged) as usize;
    let loco = if idle.is_none() || bow || st.blocking || st.choking || swim.swimming() {
        None
    } else if !p.grounded {
        c.left_loco[set][2]
    } else if p.crouched || speed < 0.6 {
        None
    } else if p.sprinting && speed > 4.5 {
        c.left_loco[set][1].or(c.left_loco[set][0])
    } else {
        c.left_loco[set][0]
    };
    let idle = loco.or(idle);
    // (a power or a gadget in hand, the hand leaves the ground with him and lands with him)
    let left_jump = c.left_jump[set];
    if loco.is_some() && took_off {
        if let Some(cl) = left_jump[0] {
            anim.restart_overlay(cl, false, 1.0, 0.1);
        }
    } else if landed && idle.is_some() && !bow && !st.blocking && !st.choking && (anim.overlay_finished() || anim.overlay_is(c.left_loco[set][2]) || anim.overlay_is(idle)) {
        if let Some(cl) = left_jump[1] {
            anim.restart_overlay(cl, false, attrs.land_rate.max(0.1), 0.08);
        }
    }
    let leaping = left_jump.iter().flatten().any(|j| anim.overlay_is(Some(*j))) && !anim.overlay_finished();
    // aiming Blink (or Daud's summoning): the hand comes up (`Powers_Cast_Blink_In`) and holds
    // (`_Loop`) until it's let go and the cast plays; a summoning given up plays its `_Cancel`
    let [aim_in, aim_loop] = match powers.selected {
        Power::SummonAssassin => c.summon_aim,
        Power::Pull => c.pull_aim,
        Power::Possess => c.possess_aim,
        _ => c.blink_aim,
    };
    let aiming = powers.aiming && matches!(powers.selected, Power::Blink | Power::SummonAssassin | Power::Pull | Power::Possess) && aim_loop.is_some();
    if let Some(a) = summon_aim.as_ref().filter(|a| a.cancel_seq != st.summon_cancels) {
        st.summon_cancels = a.cancel_seq;
        if let Some(cl) = c.summon_cancel {
            anim.restart_overlay(cl, false, 1.0, 0.1);
        }
    }
    if let Some(a) = pull_aim.as_ref().filter(|a| a.cancel_seq != st.pull_cancels) {
        st.pull_cancels = a.cancel_seq;
        if let Some(cl) = c.pull_cancel {
            anim.restart_overlay(cl, false, 1.0, 0.1);
        }
    }
    // a person held up by Pull: the arm holds it (`_Lift_In`, `_Lift_Loop`) till it is let drop
    // (`_Lift_Out`) or given up (`_Lift_Abort`)
    if let Some(a) = pull_aim.as_ref() {
        let [l_in, l_loop, l_out, l_abort] = c.pull_lift;
        if (a.lift_out_seq, a.lift_abort_seq) != st.pull_lift_ends {
            let abort = a.lift_abort_seq != st.pull_lift_ends.1;
            st.pull_lift_ends = (a.lift_out_seq, a.lift_abort_seq);
            if let Some(cl) = if abort { l_abort.or(l_out) } else { l_out } {
                anim.restart_overlay(cl, false, 1.0, 0.1);
                st.started.push(anim.lib.clip(cl).name.clone());
            }
        }
        let lifting = a.lift.is_some();
        if lifting && !st.pull_lifting {
            if let Some(cl) = l_in.or(l_loop) {
                anim.restart_overlay(cl, l_in.is_none(), 1.0, 0.15);
                st.started.push(anim.lib.clip(cl).name.clone());
            }
        } else if lifting && (anim.overlay_finished() || !(anim.overlay_is(l_in) || anim.overlay_is(l_loop))) {
            if let Some(cl) = l_loop {
                anim.play_overlay(cl, true, 1.0, 0.15);
            }
        }
        st.pull_lifting = lifting;
    }
    if let Some(a) = possess_aim.as_ref().filter(|a| a.cancel_seq != st.possess_cancels) {
        st.possess_cancels = a.cancel_seq;
        if let Some(cl) = c.possess_cancel {
            anim.restart_overlay(cl, false, 1.0, 0.1);
        }
    }
    if aiming {
        if !st.blink_aiming {
            if let Some(cl) = aim_in.or(aim_loop) {
                anim.restart_overlay(cl, aim_in.is_none(), 1.0, 0.15);
                st.started.push(anim.lib.clip(cl).name.clone());
            }
        } else if anim.overlay_finished() || !(anim.overlay_is(aim_in) || anim.overlay_is(aim_loop)) {
            if let Some(cl) = aim_loop {
                anim.play_overlay(cl, true, 1.0, 0.15);
            }
        }
    }
    st.blink_aiming = aiming;
    // a grenade cooking: the hand draws it back (`Gadgets_Grenade_Cooking_In`) and holds
    // (`_Idle`) until it's thrown
    let [cook_in, cook_loop] = c.grenade_cook;
    let cooking = powers.cooking.is_some() && cook_loop.is_some();
    if cooking {
        if !st.cooking {
            if let Some(cl) = cook_in.or(cook_loop) {
                anim.restart_overlay(cl, cook_in.is_none(), 1.0, 0.1);
            }
        } else if anim.overlay_finished() || !(anim.overlay_is(cook_in) || anim.overlay_is(cook_loop)) {
            if let Some(cl) = cook_loop {
                anim.play_overlay(cl, true, 1.0, 0.1);
            }
        }
    }
    st.cooking = cooking;
    // thrown: the next grenade taken from the belt, or the empty hand with none left
    if anim.overlay_is(c.grenade_throw) && anim.overlay_finished() {
        let left = powers.selected.ammo(&stats).unwrap_or(0);
        if let Some(cl) = if left > 0 { c.grenade_reload } else { c.grenade_empty } {
            anim.restart_overlay(cl, false, 1.0, 0.1);
        }
    }
    // zoomed with a gun: it comes up to the eye (`*_Spyglass_In`) and is held there (`_Idle`)
    // between shots; the optics put away, it comes down (`_Out`)
    let in_reload = c.reload.iter().any(|(_, cl)| anim.overlay_is(Some(*cl)));
    let raised = zoomed && !(reloading && in_reload);
    if let (true, Some(g)) = (raised, gun) {
        let [s_in, s_idle, _, s_fire, s_acc] = c.spyglass[g];
        let shot = (anim.overlay_is(s_fire) || anim.overlay_is(s_acc)) && !anim.overlay_finished();
        if st.raised != Some(g) {
            if let Some(cl) = s_in.or(s_idle) {
                anim.restart_overlay(cl, s_in.is_none(), 1.0, 0.1);
            }
        } else if !shot && (anim.overlay_finished() || !(anim.overlay_is(s_in) || anim.overlay_is(s_idle))) {
            if let Some(cl) = s_idle {
                anim.play_overlay(cl, true, 1.0, 0.1);
            }
        }
        st.raised = Some(g);
    } else if let Some(g) = st.raised.take() {
        if let Some(cl) = c.spyglass[g][2].filter(|_| !in_reload) {
            anim.restart_overlay(cl, false, 1.0, 0.1);
        }
    }
    let aiming = aiming || cooking || raised;
    let spyglass_busy = c.spyglass.iter().flatten().any(|cl| anim.overlay_is(*cl)) || anim.overlay_is(c.pistol_fire_accurate);
    if !aiming && !leaping && !(reloading && in_reload) && (anim.overlay_finished() || !(in_reload || anim.overlay_is(c.blink) || anim.overlay_is(c.windblast) || anim.overlay_is(c.bend_time) || anim.overlay_is(c.switch) || c.left_equip.iter().flatten().any(|e| anim.overlay_is(*e)) || anim.overlay_is(c.crossbow_fire) || anim.overlay_is(c.crossbow_fire_fast) || anim.overlay_is(c.pistol_fire) || anim.overlay_is(c.possess) || anim.overlay_is(c.swarm) || anim.overlay_is(c.grenade_throw) || anim.overlay_is(c.grenade_reload) || anim.overlay_is(c.razor_place) || anim.overlay_is(c.summon) || anim.overlay_is(c.summon_cancel) || anim.overlay_is(c.possess_cancel) || anim.overlay_is(c.dark_vision) || anim.overlay_is(c.out_of_mana) || c.left_slash.iter().flatten().any(|s| anim.overlay_is(*s)) || spyglass_busy)) {
        match idle {
            Some(cl) if !st.choking && !st.blocking => {
                let leap = loco.is_some() && loco == c.left_loco[set][2];
                anim.play_overlay(cl, !leap, 1.0, 0.25);
                // (walking or sprinting: at the body's place in its cycle)
                if let (Some(_), false, Some(b)) = (loco, leap, anim.current()) {
                    if b.looping && anim.overlay_is(Some(cl)) {
                        let at = (b.t / anim.lib.duration(b.clip)).fract() * anim.lib.duration(cl);
                        anim.seek_overlay(at);
                    }
                }
            }
            _ => anim.stop_overlay(0.2),
        }
    }
    if std::env::var("DH_ARMS_LOG").is_ok() {
        let name = |c: Option<ClipId>| c.map(|c| anim.lib.clip(c).name.clone()).unwrap_or_default();
        let (ov, w) = anim.overlay_state();
        info!("arms: base {} overlay {} ({w:.2}) selected {:?} empty {empty} vis {:?}", name(anim.current().map(|p| p.clip)), name(ov), powers.selected, *vis);
    }
    let _ = st.attach_l;
}

/// The held guns' rigs move with the hand: the pistol's hammer falls as it fires and its fast
/// reload plays (`CorvoGun_Fire_Hammer`, `CorvoGun_FastReload`); the crossbow plays the hand's clip of
/// its own name (its shot, reloads, the shot through the optics), else rests loaded or, with no
/// bolts left, empty (`Crossbow_Idle`, `Crossbow_OutOfAmmo_Idle`); the Heart beats.
fn animate_held(
    arms: Query<&Animator, With<ArmsRoot>>,
    mut held: Query<(&HeldRig, &mut Animator, &Visibility), Without<ArmsRoot>>,
    (powers, stats): (Res<Powers>, Res<PlayerStats>),
) {
    let Ok(arms) = arms.single() else { return };
    let arm = arms.overlay_state().0.map(|c| arms.lib.clip(c).name.clone()).unwrap_or_default();
    let t = arms.overlay_time().unwrap_or(0.0);
    for (rig, mut anim, vis) in &mut held {
        if *vis == Visibility::Hidden {
            continue;
        }
        let lib = anim.lib.clone();
        let (clip, synced) = match rig.0 {
            // (the Heart beats in his hand, its own tree's looping `Heart_Beat`: `Heart_at`)
            "heart" => ("Heart_Beat", false),
            "pistol" => match arm.as_str() {
                "Pistol_Fire" | "Pistol_Fire_Accurate" | "Pistol_Spyglass_Fire" | "Pistol_Spyglass_FireAccurate" => ("CorvoGun_Fire_Hammer", true),
                "Pistol_FastReload" => ("CorvoGun_FastReload", true),
                _ => ("CorvoGun_Idle", false),
            },
            _ => match arm.as_str() {
                a @ ("Crossbow_Fire" | "Crossbow_Fire_FastReload" | "Crossbow_RapidReload" | "Crossbow_Reload_In" | "Crossbow_Spyglass_Fire") => (a, true),
                _ if powers.selected.ammo(&stats).unwrap_or(1) == 0 => ("Crossbow_OutOfAmmo_Idle", false),
                _ => ("Crossbow_Idle", false),
            },
        };
        let Some(c) = lib.find(clip) else { continue };
        if anim.current().map(|q| q.clip) != Some(c) {
            anim.restart(c, !synced, 1.0, 0.05);
        }
        if synced {
            anim.seek(t);
        }
    }
}

/// Corvo's sword folds and unfolds with his arms: the arms' clip of the sword's own name (its
/// draws and sheathings, quick or not, standing or sneaking, and the first time he takes it up:
/// `PlySword_Anim_as`), at the arms' time; else open in his hand (`Sword_Ready_Idle`).
fn animate_sword(arms: Query<&Animator, With<ArmsRoot>>, mut sword: Query<&mut Animator, (With<crate::combat::SwordRig>, Without<ArmsRoot>)>) {
    let (Ok(arms), Ok(mut anim)) = (arms.single(), sword.single_mut()) else { return };
    let lib = anim.lib.clone();
    let now = arms.current().and_then(|p| lib.find(&arms.lib.clip(p.clip).name).map(|c| (c, p.t)));
    match now {
        Some((c, t)) => {
            if anim.current().map(|q| q.clip) != Some(c) {
                anim.restart(c, false, 1.0, 0.05);
            }
            anim.seek(t);
        }
        None => {
            if let Some(c) = lib.find("Sword_Ready_Idle").filter(|c| anim.current().map(|q| q.clip) != Some(*c)) {
                anim.restart(c, true, 1.0, 0.1);
            }
        }
    }
}

/// The clip a cast plays on the left arm (`ArmClips::resolve`).
fn cast_clip(p: Power) -> &'static str {
    match p {
        Power::Blink => "Powers_Cast_Blink_Out",
        Power::Windblast => "Powers_Cast_Windblast",
        Power::SummonAssassin => "Powers_Cast_SummonAssassin_Out",
        Power::Pull => "Powers_Cast_Pull_Out",
        Power::BendTime => "Powers_Cast_BendTime",
        Power::DarkVision | Power::Empty | Power::Heart => "Powers_Switch",
        Power::Possess => "Powers_Cast_Possession_In",
        Power::DevouringSwarm => "Powers_Cast_Swarm",
        Power::Crossbow | Power::SleepDart | Power::IncendiaryBolt | Power::ExplosiveBolt => "Crossbow_Fire",
        Power::Pistol | Power::ExplosiveBullet => "Pistol_Fire",
        Power::Grenade | Power::StickyGrenade | Power::Chokedust => "Gadgets_Grenade_Deploy",
        Power::SpringRazor | Power::ArcMine | Power::StunMine => "Gadgets_SpringRazor_Deploy",
    }
}

/// Where Possession's glow sits on the left hand (`DisTweaks_Possess`'s
/// `m_HandCastParticleAttachSocket`).
const POSSESS_HAND_SOCKET: &str = "LeftHand_PowerCastA";

/// Possession's glow on the left hand while it is aimed (`DisTweaks_Possess.m_pHandCastParticle`,
/// `Possession_HandCast_01`): the power's tick (0x00BF37E0, decoded for this rewrite from the
/// user's own Dishonored.exe) keeps it on while the power is aimed and Corvo is neither swimming
/// nor on a chain (ladder physics), and puts it out otherwise.
fn possess_hand(
    (aim, swim, climb): (Res<crate::possession::PossessAim>, Res<crate::swim::Swim>, Res<crate::climb::Climb>),
    arms: Query<&ArmsAnim>,
    mut tagged: Query<(&crate::particles::EffectTag, &mut crate::particles::ParticleEmitter)>,
    mut fx: MessageWriter<crate::particles::SpawnEffect>,
    mut shown: Local<Option<Entity>>,
) {
    let want = aim.on && !swim.swimming() && climb.on.is_none();
    let socket = arms.iter().find_map(|a| a.sockets.iter().find(|(n, _)| n.eq_ignore_ascii_case(POSSESS_HAND_SOCKET)).map(|(_, e)| *e));
    let target = socket.filter(|_| want);
    if *shown == target {
        return;
    }
    if let Some(old) = shown.take() {
        for (tag, mut em) in &mut tagged {
            if tag.0 == old {
                em.stop();
            }
        }
    }
    if let Some(s) = target {
        fx.write(crate::particles::SpawnEffect { follow: Some(s), tag: Some(s), turn: true, view: true, secs: 600.0, ..crate::particles::SpawnEffect::at("possess_hand", Vec3::ZERO) });
    }
    *shown = target;
}

/// The clips' effects at the arms' sockets (`scene.arm_fx`: the Mark glowing as he casts),
/// drawn with the arms.
fn arm_effects(
    time: Res<Time>,
    level: Option<Res<crate::level::LevelInfo>>,
    mut arms: Query<(&mut ArmsAnim, &Animator)>,
    mut fx: MessageWriter<crate::particles::SpawnEffect>,
    (mut vm, mut lens_n): (Option<ResMut<crate::kismet::Vm>>, Local<u32>),
    (killing, victims, pt): (Query<&crate::combat::Assassinating>, Query<&Transform, With<crate::npc::Npc>>, Query<&Transform, With<Player>>),
) {
    let Some(level) = level else { return };
    for (mut st, anim) in &mut arms {
        let st = &mut *st;
        for clip in std::mem::take(&mut st.started) {
            for f in level.scene.arm_fx.iter().filter(|f| f.clip.eq_ignore_ascii_case(&clip)) {
                // (the lens's: before the eye, not at a socket; the blade's in the victim: at it)
                if f.socket == "lens" || f.socket == "flesh" {
                    st.lens_fx.push((f.time, f.system));
                } else if let Some((_, e)) = st.sockets.iter().find(|(n, _)| n.eq_ignore_ascii_case(&f.socket)) {
                    st.fx.push((f.time, *e, f.system));
                }
            }
        }
        // (on the clip's own clock: the finishers' slow motion slows them too)
        let dt = time.delta_secs() * anim.time_scale;
        st.fx.retain_mut(|(t, e, system)| {
            *t -= dt;
            if *t > 0.0 {
                return true;
            }
            fx.write(crate::particles::SpawnEffect { follow: Some(*e), system: Some(*system), secs: 2.0, turn: true, view: true, ..crate::particles::SpawnEffect::at("", Vec3::ZERO) });
            false
        });
        st.lens_fx.retain_mut(|(t, system)| {
            *t -= dt;
            if *t > 0.0 {
                return true;
            }
            if *system == u32::MAX {
                // the blade in the victim's flesh
                if let (Some(b), Ok(k)) = (level.scene.blade_blood.as_ref(), killing.single()) {
                    if let (Ok(vt), Ok(p)) = (victims.get(k.npc), pt.single()) {
                        crate::npc::blade_blood(&mut fx, vm.as_deref_mut(), b, k.npc, vt.translation, p.translation);
                    }
                }
                return false;
            }
            if let Some(vm) = vm.as_mut() {
                *lens_n = (*lens_n + 1) % 0x80;
                vm.lens.push((crate::hudfx::GAMEPLAY_LENS + 0x200 + *lens_n, Some((*system, false, 0.0))));
            }
            false
        });
    }
}

/// (`DH_SCENE_ALIGN`) Where Corvo's hands and the characters' chests are in the world, each
/// second: how a scene's parts line up.
fn scene_align_log(time: Res<Time>, mut last: Local<f32>, rigs: Query<&ArmRig>, cam: Query<&GlobalTransform, With<crate::player::PlayerCamera>>, joints: Query<&GlobalTransform>, npcs: Query<(&crate::npc::Npc, &crate::npc::NpcRig, &GlobalTransform)>) {
    if time.elapsed_secs() - *last < 1.0 {
        return;
    }
    *last = time.elapsed_secs();
    let c = cam.single().map(|g| g.translation()).unwrap_or_default();
    for r in &rigs {
        let at = |i: usize| r.joints.get(i).and_then(|j| joints.get(*j).ok()).map(|g| g.translation());
        info!("scene align: camera {c:.2} right hand {:.2?} left {:.2?}", at(r.right.hand), at(r.left.hand));
    }
    for (n, rig, ng) in &npcs {
        if let Some(g) = rig.joint("spine_2_jnt").and_then(|j| joints.get(j).ok()) {
            if g.translation().distance(c) < 8.0 {
                info!("scene align: {} chest {:.2} yaw {:.2} at {:.2} turned {:.2}", n.name, g.translation(), n.yaw, ng.translation(), ng.rotation().to_euler(EulerRot::YXZ).0);
            }
        }
    }
}

/// The head's bob (`DisAnimNodeBlendByHeadBob`, by `PSI_Gameplay_HeadBobAmount`): as Corvo
/// walks, runs, sprints or sneaks his head's additive clip for the pace (`ADD_Head_*`, blended
/// by the way he goes as `AnimNodeBlendDirectional` does, in step with his stride) moves
/// camera_jnt, and the view goes with it; his arms keep to his body. The root is placed so the
/// animated camera joint lands where the bobbing view has it.
#[allow(clippy::type_complexity)]
fn align_arms(
    time: Res<Time>,
    settings: Res<crate::settings::Settings>,
    mut arms: Query<(&mut Transform, &Animator, &mut ArmsAnim, &ArmRig, &Visibility), With<ArmsRoot>>,
    mut cam: Query<&mut Transform, (With<crate::player::PlayerCamera>, Without<ArmsRoot>)>,
    player: Query<&Player>,
    (swim, climb, cine): (Res<crate::swim::Swim>, Res<crate::climb::Climb>, Res<crate::script_world::Cinematic>),
    (breath, data, death_fall, scene_arms): (Res<crate::breath::Breath>, Res<crate::gamedata::Data>, Res<DeathFall>, Res<crate::matinee::SceneArms>),
) {
    for (mut t, anim, mut st, rig, vis) in &mut arms {
        let a = anim.model(rig.camera);
        // (a scene playing his full-body keys: the view is its camera bone's, whole, in the
        // scene's cinematic mode too, as the soiree state's camera is: held by Daud's assassin,
        // thrown down, the Empress dying in his arms. Before, the view stayed as he had left
        // it while his arms played the scene before it)
        let scene = scene_arms.playing(&anim.lib).is_some();
        if std::env::var("DH_CAMBONE_LOG").is_ok() {
            let b = anim.bind_model(rig.camera);
            let d = b.rotation.inverse() * a.rotation;
            let (ax, ang) = d.to_axis_angle();
            info!("camera bone: turned {:.1} deg about {ax:.2}, moved {:.3}", ang.to_degrees(), a.translation.distance(b.translation));
        }
        let eye = st.eye.compute_affine();
        let p = player.single().ok();
        let on = *vis != Visibility::Hidden && (!cine.on || scene);
        // the stride: its pace and rate as the arms' locomotion has them
        let dt = time.delta_secs();
        let mut deltas = Vec::new();
        if let Some(p) = p {
            let c = st.clips.clone();
            let v = Vec2::new(p.velocity.x, p.velocity.z);
            let speed = v.length();
            let (pace, rate) = if p.crouched {
                (3, speed / if speed > 2.5 { 3.0 } else { 1.9 })
            } else if p.sprinting && speed > 4.5 {
                (2, speed / 6.2)
            } else if speed > 2.6 {
                (1, speed / 4.5)
            } else {
                (0, speed / 2.5)
            };
            let moving = p.grounded && !swim.swimming() && speed >= 0.4 && p.mantle.is_none() && climb.on.is_none();
            let target = if moving { ((speed - 0.4) / 0.8).clamp(0.0, 1.0) } else { 0.0 };
            st.bob_weight += (target - st.bob_weight).clamp(-4.0 * dt, 4.0 * dt);
            if let Some(n) = c.bob[pace][0] {
                let len = anim.lib.duration(n).max(0.1);
                if moving {
                    st.bob_phase = (st.bob_phase + dt * rate.clamp(0.6, 1.6) / len).fract();
                }
                // the way he goes, against where he faces: forward / back, left / right
                let fwd = Vec2::new(-p.yaw.sin(), -p.yaw.cos());
                let right = Vec2::new(-fwd.y, fwd.x);
                let th = v.dot(right).atan2(v.dot(fwd));
                let q = std::f32::consts::FRAC_PI_2;
                let side = if th >= 0.0 { 3 } else { 2 };
                let ways = if th.abs() <= q { [(0, 1.0 - th.abs() / q), (side, th.abs() / q)] } else { [(1, (th.abs() - q) / q), (side, 1.0 - (th.abs() - q) / q)] };
                if st.bob_weight > 0.0 {
                    for (way, w) in ways {
                        if let Some(clip) = c.bob[pace][way].filter(|_| w > 0.0) {
                            anim.lib.additive(clip, st.bob_phase * anim.lib.duration(clip), w * st.bob_weight, &mut deltas);
                        }
                    }
                }
            }
            // winded: the head heaves (`DisAnimNodeBlendByPhysicalCondition`'s additive, by how
            // winded he is times `m_fBreathlessnessAnimWeight`)
            let heave = breath.level * data.pawn("m_fBreathlessnessAnimWeight", 0.15);
            if let (Some(clip), true) = (c.bob_breath, heave > 0.001) {
                st.breath_t = (st.breath_t + dt) % anim.lib.duration(clip).max(0.1);
                anim.lib.additive(clip, st.breath_t, heave, &mut deltas);
            }
            // a mantle: the head's nod over it; a chain: its sway as he climbs
            if let (Some((_, _, k)), Some(clip)) = (p.mantle, c.bob_mantle) {
                anim.lib.additive(clip, k.clamp(0.0, 1.0) * anim.lib.duration(clip), 1.0, &mut deltas);
            } else if let (Some(_), Some(clip)) = (climb.on, c.bob_climb) {
                if p.velocity.y.abs() > 0.1 {
                    st.bob_phase = (st.bob_phase + dt / anim.lib.duration(clip).max(0.1)).fract();
                }
                anim.lib.additive(clip, st.bob_phase * anim.lib.duration(clip), 1.0, &mut deltas);
            }
        }
        let root = match cam.single_mut() {
            Ok(mut ct) if on => {
                // the view on the camera bone, as the original's camera has it (roshan's
                // r614/blink research decoded it): where the animated camera_jnt is, with
                // `DishonoredPlayerCamera.m_RollAmount` of its turn from its rest pose; the arms
                // keep to the body
                let rest = anim.bind_model(rig.camera);
                let moved = eye * rest.compute_affine().inverse() * a.compute_affine() * eye.inverse();
                let (_, mrot, mtr) = moved.to_scale_rotation_translation();
                // (falling dead, all of it: our reading of the death's view)
                let roll = if death_fall.0 || scene { 1.0 } else { ROLL_AMOUNT };
                let mut off = bevy::math::Affine3A::from_rotation_translation(Quat::IDENTITY.slerp(mrot, roll), mtr);
                if !deltas.is_empty() {
                    // the view's sway: camera_jnt with the bob against without it, in the camera's frame
                    let bobbed = anim.model_with(rig.camera, &deltas);
                    let bob = eye * a.compute_affine().inverse() * bobbed.compute_affine() * eye.inverse();
                    let (_, orot, otr) = bob.to_scale_rotation_translation();
                    let amount = settings.head_bob.clamp(0.0, 1.0);
                    if std::env::var("DH_BOB_LOG").is_ok() {
                        info!("bob: off {:.3} {:.3} {:.3} turn {:.2} deg", otr.x, otr.y, otr.z, orot.to_axis_angle().1.to_degrees());
                    }
                    off = off * bevy::math::Affine3A::from_rotation_translation(Quat::IDENTITY.slerp(orot, amount), otr.clamp_length_max(BOB_MAX) * amount);
                }
                let (_, cr, ctr) = (ct.compute_affine() * off).to_scale_rotation_translation();
                ct.rotation = cr;
                ct.translation = ctr;
                off.inverse() * eye * rest.compute_affine().inverse()
            }
            _ => eye * a.compute_affine().inverse(),
        };
        let (_, r, tr) = root.to_scale_rotation_translation();
        t.rotation = r;
        t.translation = tr;
    }
}

/// The arms turned against the view as the original's look controls turn them (as roshan's
/// r614/blink research decoded them): each collarbone by its hand's item's pitch offset for the
/// view's pitch (`m_ArmPitch_OffsetKeys`, eased between the keys by a cosine): the sword's and
/// the powers' bring the hands up 10 degrees looking straight down and 15 down looking straight
/// up, the pistol's and the crossbow's 2 and 3.
#[allow(clippy::type_complexity)]
fn arm_pitch(
    rigs: Query<(&ArmRig, &Animator, &ArmsAnim, &Visibility), With<ArmsRoot>>,
    player: Query<&Player>,
    powers: Option<Res<Powers>>,
    data: Res<crate::gamedata::Data>,
    cine: Res<crate::script_world::Cinematic>,
    mut joints: Query<&mut Transform, (Without<ArmsRoot>, Without<ViewModel>, Without<SwordGrip>, Without<crate::player::PlayerCamera>)>,
) {
    let (Ok(p), Some(powers)) = (player.single(), powers) else { return };
    if cine.on {
        return;
    }
    let left_item = match powers.selected {
        Power::Pistol => "Twk_Inv_PistolCorvo",
        Power::Crossbow => "Twk_Inv_CrossbowCorvo",
        Power::Grenade | Power::StickyGrenade => "Twk_Inv_GrenadeCorvo",
        Power::SpringRazor => "Twk_Inv_SpringRazorCorvo",
        Power::Heart => "Twk_Inv_HeartCorvo",
        _ => "Twk_Inv_Powers",
    };
    let pitch = p.pitch.to_degrees();
    for (rig, anim, st, vis) in &rigs {
        // (a pose not written this frame keeps the turn already laid on it)
        if *vis == Visibility::Hidden || anim.frozen {
            continue;
        }
        // the view's right, in the rig's frame (the camera bone at rest, less its place on the
        // camera)
        let camera = anim.bind_model(rig.camera).compute_affine() * st.eye.compute_affine().inverse();
        let (_, cam_rot, _) = camera.to_scale_rotation_translation();
        let right = cam_rot * Vec3::X;
        for (side, item) in [(0, "Twk_Inv_SwordCorvo"), (1, left_item)] {
            let Some(j) = rig.collar[side] else { continue };
            let Some(keys) = data.0.arm_pitch.get(item).or_else(|| data.0.arm_pitch.get("Twk_Inv_Powers")) else { continue };
            let off = pitch_offset(keys, pitch).to_radians();
            if off.abs() < 1e-4 {
                continue;
            }
            let parent = rig.bind_rot[j] * rig.bind_local[j].inverse();
            if let Ok(mut t) = joints.get_mut(rig.joints[j]) {
                t.rotation = Quat::from_axis_angle(parent.inverse() * right, off) * t.rotation;
            }
        }
    }
}

/// An item's arm pitch difference (degrees) for the view's pitch (degrees, up positive): the
/// keys' values, eased between the two around it by a cosine, held past the ends.
fn pitch_offset(keys: &[[f32; 2]], pitch: f32) -> f32 {
    let (Some(first), Some(last)) = (keys.first(), keys.last()) else { return 0.0 };
    if pitch <= first[0] {
        return first[1];
    }
    for w in keys.windows(2) {
        if pitch <= w[1][0] {
            let t = (pitch - w[0][0]) / (w[1][0] - w[0][0]).max(1e-3);
            let e = (1.0 - (std::f32::consts::PI * t).cos()) * 0.5;
            return w[0][1] + (w[1][1] - w[0][1]) * e;
        }
    }
    last[1]
}

/// How much of the camera bone's turn the view takes (`DishonoredPlayerCamera.m_RollAmount`).
const ROLL_AMOUNT: f32 = 0.7;

/// A landing this fast (m/s) is a hard one (`StatePlayerMasterFalling.m_fFall_Stun_Vel`, 2200 uu/s).
const FALL_STUN: f32 = 22.0;

/// The most the bob moves the view (m).
const BOB_MAX: f32 = 0.25;

/// Two-bone IK: world (rig space) rotations of the upper and lower bone so the chain end
/// reaches `target`, bending towards `pole`. Bones point along their local +X.
fn two_bone(rig: &ArmRig, c: Chain, parent_rot: Quat, target: Vec3, pole: Vec3) -> (Quat, Quat) {
    let s = rig.bind_pos[c.upper];
    let a = rig.bind_t[c.lower].length();
    let b = rig.bind_t[c.hand].length();
    let to = target - s;
    let d = to.length().clamp((a - b).abs() + 1e-3, a + b - 1e-3);
    let dir = to.normalize_or(Vec3::NEG_Y);
    let cos_s = ((a * a + d * d - b * b) / (2.0 * a * d)).clamp(-1.0, 1.0);
    let sin_s = (1.0 - cos_s * cos_s).sqrt();
    let side = (pole - dir * pole.dot(dir)).normalize_or(Vec3::NEG_Y);
    let elbow = s + dir * (cos_s * a) + side * (sin_s * a);
    let hand = s + dir * d;
    let upper0 = parent_rot * rig.bind_local[c.upper];
    let upper = Quat::from_rotation_arc(upper0 * Vec3::X, (elbow - s).normalize_or(dir)) * upper0;
    let lower0 = upper * rig.bind_local[c.lower];
    let lower = Quat::from_rotation_arc(lower0 * Vec3::X, (hand - elbow).normalize_or(dir)) * lower0;
    (upper, lower)
}

/// Procedural fallback when the arm animations are unavailable.
#[allow(clippy::type_complexity)]
fn pose_arms_ik(
    rigs: Query<(&ArmRig, &Transform), (With<ArmsRoot>, Without<Animator>)>,
    mut vis: Query<&mut Visibility, (With<ArmsRoot>, Without<Animator>)>,
    vm: Query<(&Transform, &Visibility), (With<ViewModel>, Without<ArmsRoot>)>,
    grip: Query<&Transform, (With<SwordGrip>, Without<ArmsRoot>, Without<ViewModel>)>,
    mut joints: Query<&mut Transform, (Without<ArmsRoot>, Without<ViewModel>, Without<SwordGrip>)>,
) {
    let (Ok((rig, root_t)), Ok((vm_t, vm_vis)), Ok(grip_t)) = (rigs.single(), vm.single(), grip.single()) else { return };
    if let Ok(mut v) = vis.single_mut() {
        *v = *vm_vis;
    }
    // sword target in rig space
    let target = root_t.compute_affine().inverse() * vm_t.compute_affine() * grip_t.compute_affine();
    let (_, att_rot, att_pos) = target.to_scale_rotation_translation();
    // hand joint target from the attachment socket target
    let hand_rot = att_rot * rig.bind_local[rig.attach].inverse();
    let hand_pos = att_pos - hand_rot * rig.bind_t[rig.attach];

    let parent_of = |j: usize, rig: &ArmRig| rig.bind_rot[j] * rig.bind_local[j].inverse();
    // right arm: elbow down and out to the side
    let r = rig.right;
    let pr = parent_of(r.upper, rig);
    let (up, lo) = two_bone(rig, r, pr, hand_pos, Vec3::new(-0.3, -1.0, 0.6));
    let set = |joints: &mut Query<&mut Transform, (Without<ArmsRoot>, Without<ViewModel>, Without<SwordGrip>)>, j: usize, q: Quat| {
        if let Ok(mut t) = joints.get_mut(rig.joints[j]) {
            t.rotation = q;
        }
    };
    set(&mut joints, r.upper, pr.inverse() * up);
    set(&mut joints, r.lower, up.inverse() * lo);
    set(&mut joints, r.hand, lo.inverse() * hand_rot);
    for &(j, axis, angle) in &rig.curl {
        set(&mut joints, j, rig.bind_local[j] * Quat::from_axis_angle(axis, angle));
    }

    // left arm: hanging by the side, below the view
    let l = rig.left;
    let pl = parent_of(l.upper, rig);
    let shoulder = rig.bind_pos[l.upper];
    let (up, lo) = two_bone(rig, l, pl, shoulder + Vec3::new(0.05, -0.45, -0.08), Vec3::new(-0.2, -0.3, -1.0));
    set(&mut joints, l.upper, pl.inverse() * up);
    set(&mut joints, l.lower, up.inverse() * lo);
}

/// One of the arms' meshes (its place among the player type's view parts).
#[derive(Component)]
pub struct ArmsPart(pub usize);

/// The arms the scripts put on the player (`DisDLC07SeqAct_SetPlayerArmsMesh`: Daud bloodied in
/// his dream, in an Overseer's coat at Coldridge; his own again): each part's mesh and material
/// swapped for the other arms' (cooked on the same skeleton).
fn swap_arms(
    mut commands: Commands,
    vm: Option<Res<crate::kismet::Vm>>,
    assets: Option<Res<GameAssets>>,
    mut parts: Query<(Entity, &ArmsPart, &mut Mesh3d, &mut Visibility)>,
    mut shown: Local<Option<String>>,
) {
    let (Some(vm), Some(assets)) = (vm, assets) else { return };
    let want = vm.arms_mesh.clone();
    if *shown == want {
        return;
    }
    let Some(player) = assets.npc_types.iter().flatten().find(|v| v.kind == "player") else { return };
    let alt = want.as_ref().and_then(|w| assets.arm_meshes.iter().find(|(p, _)| p.eq_ignore_ascii_case(w))).map(|(_, v)| v);
    // (an arms mesh not cooked as another: his own)
    let set = alt.unwrap_or(&player.parts);
    for (e, part, mut mesh, mut vis) in &mut parts {
        match set.view_parts.get(part.0) {
            Some((m, mat)) => {
                mesh.0 = m.clone();
                let mut ec = commands.entity(e);
                ec.remove::<(MeshMaterial3d<crate::lightmap::WorldMaterial>, MeshMaterial3d<crate::ue3mat::Ue3Material>)>();
                mat.apply(&mut ec);
                // (the mark's two skins are his own arms')
                if alt.is_some() {
                    ec.remove::<MarkSkin>();
                }
                *vis = Visibility::Inherited;
            }
            None => *vis = Visibility::Hidden,
        }
    }
    *shown = want;
}

/// An arm mesh with both skins: the Outsider's mark appears with the first gift (the dream).
#[derive(Component)]
pub struct MarkSkin {
    marked: PartMat,
    unmarked: PartMat,
    shown: Option<bool>,
}

pub fn mark_skin(mut commands: Commands, stats: Res<crate::gameplay::PlayerStats>, mut q: Query<(Entity, &mut MarkSkin)>) {
    let marked = !stats.powers.is_empty();
    for (e, mut s) in &mut q {
        if s.shown == Some(marked) {
            continue;
        }
        s.shown = Some(marked);
        let mut ec = commands.entity(e);
        ec.remove::<(MeshMaterial3d<crate::lightmap::WorldMaterial>, MeshMaterial3d<crate::ue3mat::Ue3Material>)>();
        if marked { s.marked.apply(&mut ec) } else { s.unmarked.apply(&mut ec) }
    }
}
