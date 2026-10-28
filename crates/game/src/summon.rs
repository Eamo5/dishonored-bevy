//! Daud's Summon Assassin (The Knife of Dunwall: `DLC06_Pwr_Assassummon`): an assassin of his
//! (the power level's `m_pAssassinTweaks`: a novice, then a master) appears where he aims, or
//! behind the enemy he aims at (`m_fDistFromTarget`) and goes for it; a threatening noise where
//! he appears (`m_bMakeNoiseOnSpawn`). As many as the level allows (`m_MaxAssassins`: one, the
//! one before goes); gone once out of a fight for a while (`m_bDisappearAfterCombat`,
//! `m_fNoCombatTimeout`). With Arcane Bond II he moves through bent time as Daud does.
//!
//! The power is aimed as its component has it (`UDisDLC06ActivePowerComponent_SummonAssassin`,
//! decoded from the user's executable for this rewrite: its tick 0x00D00C80, the spot
//! 0x00CFA380, the markers 0x00CF3FA0 / 0x00CF4070, the refusal 0x00CF44D0, the release
//! 0x00CFA1E0, the cancel 0x00D00BE0). Held, the targeting starts (`m_pStartTargetingSoundEvent`,
//! the arms' `Powers_Cast_SummonAssassin_In`, `_Loop`) and each frame finds the spot:
//! - the aim traced `m_fTargetingRange` (20 m); stopped within `m_fTargetingCloseRange` (3 m),
//!   it is traced again from the eye moved `m_fTargetingCloseRangeOffset` (30) up (as far as
//!   there is room), down, left and right, the first that reaches `m_fTargetingFarRange` (6 m)
//!   taken;
//! - an enemy the trace meets, else one within `m_fMaxAngleToTargetNPC_Deg` (10) in sight:
//!   the assassin goes `m_fDistFromTarget` (3.5 m) behind it, by its own facing, to the navmesh
//!   near there; its marker (`m_pTargetingEffect_NPC_Template`) on the enemy;
//! - else the aim drawn back `m_fTargetingPullback` (0.8 m) along the look, the ground below,
//!   on the navmesh, room for him there, and not by Daud (`m_fTargetingPlayerCheckRadius` 2 m
//!   out, `m_fTargetingPlayerCheckHeight` 1 m): the air's marker at the aim, the ground's below
//!   it (`_Air_`, `_Ground_Template`);
//! - nowhere: the refused marker (`_Disallowed_Template`), and "You cannot summon there."
//!   (`m_TargetOffNavMeshMessage`) held while it stays so.
//!
//! Let go on a spot, he is summoned (`m_pTargetingSuccessSoundEvent`, the arms' `_Out`); else,
//! or with Use ("Cancel Summon"), or the power put away, the aim ends without him
//! (`m_pTargetingFailSoundEvent`, `_Cancel`). The first assassin about starts
//! `m_StartActiveAssassinsSoundEvent`, the last gone stops it.

use crate::audio::PostEvent;
use crate::gameplay::{HudMessages, Noise, NoiseKind, PlayerStats};
use crate::level::{GameAssets, LevelInfo, GROUP_WORLD};
use crate::npc::{Mode, Npc, NPC_CENTER, NPC_HALF, NPC_RADIUS};
use crate::powers::Power;
use crate::world_light::WorldLighting;
use crate::GameState;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

pub struct SummonPlugin;

impl Plugin for SummonPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<Summon>()
            .init_resource::<SummonAim>()
            .add_systems(OnExit(GameState::InGame), |mut a: ResMut<SummonAim>| *a = SummonAim::default())
            .add_systems(Update, (aim, summon, summoned).chain().run_if(in_state(GameState::InGame)));
    }
}

/// Summon an assassin: where, the enemy aimed at, at what power level.
#[derive(Message, Clone, Copy)]
pub struct Summon {
    pub at: Vec3,
    pub target: Option<Entity>,
    pub level: u8,
}

/// How near Daud a summoned assassin that fought gives the winner's salute as it goes
/// (`DLC06_TweaksBehavior_AssnSalute`'s `m_fWinAnimMaxRange`, 2000 uu).
const WIN_SALUTE_RANGE: f32 = 20.0;

/// A summoned assassin: the enemy he was sent for (until he has it), how long he has been out
/// of a fight.
#[derive(Component)]
pub struct Summoned {
    foe: Option<Entity>,
    fresh: bool,
    idle: f32,
    /// it has fought (its departure's salute the winner's)
    fought: bool,
    /// saluting as it goes: how long before it is gone
    leaving: Option<f32>,
}

#[allow(clippy::too_many_arguments)]
fn summon(
    mut commands: Commands,
    mut requests: MessageReader<Summon>,
    level: Option<Res<LevelInfo>>,
    assets: Option<Res<GameAssets>>,
    mut wl: Option<ResMut<WorldLighting>>,
    settings: Res<crate::settings::Settings>,
    existing: Query<(Entity, &Transform), With<Summoned>>,
    player: Query<&Transform, (With<crate::player::Player>, Without<Summoned>)>,
    mut fx: MessageWriter<crate::particles::SpawnEffect>,
    mut sfx: MessageWriter<crate::audio::PostEvent>,
    mut noise: MessageWriter<Noise>,
) {
    let (Some(level), Some(assets)) = (level, assets) else {
        requests.clear();
        return;
    };
    for r in requests.read() {
        let scene = &level.scene;
        let Some(&si) = scene.summons.get(r.level.clamp(1, 2) as usize - 1).or(scene.summons.first()) else {
            warn!("summon: no assassin on this map");
            continue;
        };
        // (one at a time: the one before goes)
        for (e, t) in &existing {
            sfx.write(crate::audio::PostEvent::named("AI_Assassin_Pwr_Teleport_Start", Some(t.translation)));
            commands.entity(e).try_despawn();
        }
        let Some(e) = crate::npc::spawn_npc_as(&mut commands, &assets, scene, wl.as_deref_mut(), si as usize, settings.difficulty, None) else { continue };
        // placed, facing Daud's way
        let from = player.single().map(|p| p.translation).unwrap_or(r.at);
        let to = (r.at - from).with_y(0.0);
        let yaw = (-to.x).atan2(-to.z);
        commands.entity(e).insert((
            Transform::from_translation(r.at + Vec3::Y * (NPC_CENTER + 0.05)).with_rotation(Quat::from_rotation_y(yaw)),
            Summoned { foe: r.target, fresh: true, idle: 0.0, fought: false, leaving: None },
        ));
        fx.write(crate::particles::SpawnEffect::at("summon_trigger", r.at));
        noise.write(Noise { corvo: false, pos: r.at, radius: 4.0, kind: NoiseKind::Threatening });
        if std::env::var_os("DH_SUMMON_LOG").is_some() {
            info!("summon: assassin (level {}) at {:.1}{}", r.level, r.at, if r.target.is_some() { ", for an enemy" } else { "" });
        }
    }
}

/// The summoned go for their enemy, keep Daud's pace through bent time with Arcane Bond II,
/// and go once out of a fight long enough. The first about starts the active sound
/// (`m_StartActiveAssassinsSoundEvent`), the last gone stops it.
#[allow(clippy::too_many_arguments)]
fn summoned(
    mut commands: Commands,
    time: Res<Time>,
    data: Res<crate::gamedata::Data>,
    stats: Res<PlayerStats>,
    mut aim: ResMut<SummonAim>,
    mut q: Query<(Entity, &mut Npc, &mut Summoned, &Transform, Option<&crate::anim::Animator>)>,
    mut sfx: MessageWriter<crate::audio::PostEvent>,
    mut fx: MessageWriter<crate::particles::SpawnEffect>,
    player: Query<&Transform, (With<crate::player::Player>, Without<Summoned>)>,
) {
    let daud = player.single().ok().map(|t| t.translation);
    let timeout = data.active("SummonAssassin").and_then(|a| a.params.get("m_fNoCombatTimeout")).copied().unwrap_or(5.0);
    let bond = stats.power("ArcaneBond");
    let about = q.iter().filter(|(_, n, _, _, _)| !n.is_down()).count();
    if (about > 0) != (aim.about > 0) {
        let (key, d) = if about > 0 { ("m_StartActiveAssassinsSoundEvent", "Snd_Pwr_Summon_Active") } else { ("m_StopActiveAssassinsSoundEvent", "Snd_Pwr_Summon_Active_Stop") };
        let level = Power::SummonAssassin.level(&stats);
        sfx.write(crate::audio::PostEvent::named(data.power_sound("SummonAssassin", level, key).unwrap_or(d), None));
    }
    aim.about = about;
    for (e, mut npc, mut s, t, anim) in &mut q {
        if s.fresh {
            s.fresh = false;
            npc.foe = s.foe.take();
            npc.mode = Mode::Idle;
        }
        npc.out_of_bend = bond >= 2;
        if npc.is_down() {
            continue;
        }
        // saluting as it goes, then gone
        if let Some(left) = s.leaving.as_mut() {
            *left -= time.delta_secs();
            if *left <= 0.0 {
                sfx.write(crate::audio::PostEvent::named("AI_Assassin_Pwr_Teleport_Start", Some(t.translation)));
                fx.write(crate::particles::SpawnEffect::at("summon_trigger", t.translation - Vec3::Y * NPC_CENTER));
                commands.entity(e).try_despawn();
            }
            continue;
        }
        let fighting = npc.foe.is_some() || npc.mode == Mode::Combat;
        s.fought |= fighting;
        s.idle = if fighting { 0.0 } else { s.idle + time.delta_secs() };
        if s.idle > timeout {
            // (its salute to Daud as it goes, `DisDLC06Tweaks_AIBehavior_AssassinSalute`: the
            // winner's, `m_WinAnim` `Sword_DepartureSalute`, having fought and within
            // `m_fWinAnimMaxRange` (20 m) of him, else `m_LoseAnim`, `_NoFight`)
            let near = daud.is_some_and(|d| d.distance(t.translation) <= WIN_SALUTE_RANGE);
            let clip = if s.fought && near { "Sword_DepartureSalute" } else { "Sword_DepartureSalute_NoFight" };
            let len = anim.and_then(|a| a.lib.find(clip).map(|c| a.lib.duration(c)));
            if let (Some(_), Some(d)) = (len, daud) {
                let to = (d - t.translation).with_y(0.0);
                if to.length() > 0.1 {
                    npc.yaw = (-to.x).atan2(-to.z);
                }
                npc.target = None;
                commands.entity(e).try_insert(crate::npc::FinisherClip(clip.to_string(), false));
            }
            if std::env::var_os("DH_SUMMON_LOG").is_some() {
                info!("summon: assassin departs ({clip}, {:.1} s)", len.unwrap_or(0.0));
            }
            s.leaving = Some(len.unwrap_or(0.0));
        }
    }
}

/// The targeting under way (the arms read it: aiming while `on`, `_Cancel` as `cancel_seq`
/// moves).
#[derive(Resource, Default)]
pub struct SummonAim {
    pub on: bool,
    pub cancel_seq: u32,
    spot: Option<Spot>,
    /// the markers' entities and the effect each shows: the aim's, the ground's
    markers: [Option<Entity>; 2],
    shown: [Option<&'static str>; 2],
    refused: bool,
    /// the left hand held on after a cancel: it summons nothing until let go
    swallow: bool,
    /// assassins about last frame (the active sound's start and stop)
    about: usize,
}

/// Where an assassin would go, as the aim finds it (the component's `+0xB4`..`+0xE8`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spot {
    /// the enemy aimed at
    pub npc: Option<Entity>,
    /// the aim's marker: on the enemy, in the air over the spot, or where it was refused
    pub mark: Option<Vec3>,
    /// the ground's marker, under the aim
    pub ground: Vec3,
    /// his feet
    pub feet: Vec3,
    pub ok: bool,
}

/// The aim's numbers (`DisDLC06Tweaks_SummonAssassin`, in metres).
pub struct Reach {
    range: f32,
    close: f32,
    far: f32,
    offset: f32,
    pullback: f32,
    ground: f32,
    near_h: f32,
    near_r: f32,
    cone: f32,
    behind: f32,
}

impl Reach {
    pub fn of(data: &crate::gamedata::Data) -> Self {
        let p = |k: &str, d: f32| data.active("SummonAssassin").and_then(|a| a.params.get(k)).copied().unwrap_or(d);
        Reach {
            range: p("m_fTargetingRange", 2000.0) * 0.01,
            close: p("m_fTargetingCloseRange", 300.0) * 0.01,
            far: p("m_fTargetingFarRange", 600.0) * 0.01,
            offset: p("m_fTargetingCloseRangeOffset", 30.0) * 0.01,
            pullback: p("m_fTargetingPullback", 80.0) * 0.01,
            // (the ground looked for well below; 500 m in the tweak)
            ground: (p("m_fTargetingGroundCheckDistance", 50000.0) * 0.01).min(200.0),
            near_h: p("m_fTargetingPlayerCheckHeight", 100.0) * 0.01,
            near_r: p("m_fTargetingPlayerCheckRadius", 200.0) * 0.01,
            cone: p("m_fMaxAngleToTargetNPC_Deg", 10.0).to_radians(),
            behind: p("m_fDistFromTarget", 350.0) * 0.01,
        }
    }
}

/// An enemy as the aim sees it: its middle and facing.
pub struct Enemy {
    pub e: Entity,
    pub center: Vec3,
    pub facing: Vec3,
}

/// The enemy whose capsule a look along `fwd` from `eye` meets within `len`, nearest first.
fn met_on_the_way<'a>(eye: Vec3, fwd: Vec3, len: f32, enemies: &'a [Enemy]) -> Option<&'a Enemy> {
    enemies
        .iter()
        .filter_map(|n| {
            let along = (n.center - eye).dot(fwd);
            let off = n.center - (eye + fwd * along);
            (along > 0.0 && along < len + NPC_RADIUS && off.with_y(0.0).length() < NPC_RADIUS && off.y.abs() < NPC_HALF + NPC_RADIUS).then_some((along, n))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|m| m.1)
}

/// Behind an enemy by its own facing (`m_fDistFromTarget` along its back, the original's flag
/// for in front left unset).
fn behind(facing: Vec3) -> Vec3 {
    (-facing).with_y(0.0).normalize_or(Vec3::Z)
}

/// The spot the aim finds (0x00CFA380): `view` the camera's rotation, `daud` his middle.
#[allow(clippy::too_many_arguments)]
fn find_spot(ctx: &RapierContext, me: Entity, eye: Vec3, view: Quat, daud: Vec3, enemies: &[Enemy], nav: Option<&crate::navmesh::NavGrid>, r: &Reach) -> Spot {
    let (fwd, right, up) = (view * Vec3::NEG_Z, view * Vec3::X, view * Vec3::Y);
    let walls = QueryFilter::default().exclude_collider(me).exclude_sensors().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
    let ray = |from: Vec3, dir: Vec3, len: f32| ctx.cast_ray(from, dir, len, true, walls).map(|(_, t)| t);
    let reach = |from: Vec3| from + fwd * ray(from, fwd, r.range).unwrap_or(r.range);
    let mut hit = reach(eye);
    // an enemy the trace meets on its way
    let met = met_on_the_way(eye, fwd, (hit - eye).length(), enemies);
    // stopped short: traced again from beside the eye, the first to reach far enough taken
    if met.is_none() && (hit - eye).length() <= r.close {
        let room = ray(eye, up, r.offset).unwrap_or(r.offset);
        for from in [eye + up * room, eye - up * r.offset, eye - right * r.offset, eye + right * r.offset] {
            let h = reach(from);
            if (h - from).length() >= r.far {
                hit = h;
                break;
            }
        }
    }
    // else the enemy nearest, within its cone of the look and in sight
    let target = met.or_else(|| {
        enemies
            .iter()
            .filter_map(|n| {
                let to = n.center - eye;
                let d = to.length().max(1e-3);
                (d < r.range && (to / d).angle_between(fwd) < r.cone && ray(eye, to / d, (d - 0.4).max(0.0)).is_none()).then_some((d, n))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|m| m.1)
    });
    let ground = |q: Vec3, down: f32| ray(q, Vec3::NEG_Y, down).map(|t| q - Vec3::Y * t);
    let walkable = |p: Vec3| nav.is_none_or(|g| g.on_mesh(p));
    let refused = |mark: Option<Vec3>| Spot { npc: None, mark, ground: Vec3::ZERO, feet: Vec3::ZERO, ok: false };
    if let Some(n) = target {
        // behind it as far as the world lets, on the navmesh near there
        let back = behind(n.facing);
        let room = ray(n.center, back, r.behind).map(|t| (t - NPC_RADIUS).max(0.0)).unwrap_or(r.behind);
        let feet = [1.0, 0.75, 0.5, 0.25, 0.0].iter().find_map(|k| ground(n.center + back * room * k, NPC_CENTER + 1.0).filter(|g| walkable(*g)));
        return match feet {
            Some(feet) => Spot { npc: Some(n.e), mark: Some(n.center), ground: feet, feet, ok: true },
            // (the original's marker then stands nowhere)
            None => refused(None),
        };
    }
    // drawn back along the look, the ground below it
    let flat = fwd.with_y(0.0).normalize_or_zero();
    let p = hit - flat * r.pullback;
    let Some(g) = ground(p, r.ground) else { return refused(Some(p)) };
    if !walkable(g) {
        return refused(Some(p));
    }
    // room for him there
    let middle = g + Vec3::Y * NPC_CENTER;
    let body = Collider::capsule_y(NPC_HALF, NPC_RADIUS * 0.9);
    let mut blocked = false;
    ctx.intersect_shape(middle + Vec3::Y * 0.05, Quat::IDENTITY, &*body.raw, walls, |_| {
        blocked = true;
        false
    });
    // and not on Daud
    let daud_half = crate::player::STAND_HALF + crate::player::RADIUS;
    let near = (middle.y + r.near_h - daud.y).abs() <= r.near_h + daud_half && (daud - g).with_y(0.0).length() <= crate::player::RADIUS + r.near_r;
    if blocked || near {
        return refused(Some(p));
    }
    Spot { npc: None, mark: Some(p.with_y(p.y.max(middle.y))), ground: p.with_y(g.y), feet: g, ok: true }
}

#[derive(Component)]
struct SummonMarker;

const REFUSAL: &str = "You cannot summon there.";

/// The aim, its markers and its end (see the module's words).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn aim(
    mut commands: Commands,
    (bind, paused, cursor, scripted): (Res<crate::bindings::Bindings>, Res<crate::hud::Paused>, Single<&bevy::window::CursorOptions>, Option<Res<crate::script::Scripted>>),
    (mut st, mut powers, mut stats, data, attrs, tune, settings, tc, held, possession): (
        ResMut<SummonAim>,
        ResMut<crate::powers::Powers>,
        ResMut<PlayerStats>,
        Res<crate::gamedata::Data>,
        Res<crate::gamedata::Attrs>,
        Res<crate::musicbox::Tune>,
        Res<crate::settings::Settings>,
        Res<crate::gameplay::TimeControl>,
        Res<crate::props::Held>,
        Res<crate::possession::Possession>,
    ),
    rapier: ReadRapierContext,
    nav: Option<Res<crate::navmesh::NavGrid>>,
    player: Query<(Entity, &Transform, &crate::player::Player)>,
    cam: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    npcs: Query<(Entity, &Transform, &Npc), Without<Summoned>>,
    mut markers: Query<(&mut Transform, &mut Visibility), (With<SummonMarker>, Without<Npc>, Without<crate::player::Player>)>,
    level: Option<Res<LevelInfo>>,
    (mut msgs, mut sfx, mut fx, mut summon, mut used): (ResMut<HudMessages>, MessageWriter<PostEvent>, MessageWriter<crate::particles::SpawnEffect>, MessageWriter<Summon>, MessageWriter<crate::powers::PowerUsed>),
    mut tagged: Query<(&crate::particles::EffectTag, &mut crate::particles::ParticleEmitter)>,
) {
    if paused.0 {
        return;
    }
    let level_n = Power::SummonAssassin.level(&stats);
    let sound = |key: &str, d: &str| -> String { data.power_sound("SummonAssassin", level_n, key).unwrap_or(d).to_string() };
    let Ok((pe, pt, p)) = player.single() else { return };
    let grabbed = cursor.grab_mode != bevy::window::CursorGrabMode::None || scripted.is_some();
    let can_act = grabbed && !stats.dead && !p.locked && !held.busy() && powers.blink.is_none() && tc.wheel >= 1.0 && possession.host.is_none();
    let selected = powers.selected == Power::SummonAssassin;
    if st.swallow && !bind.pressed(crate::bindings::Act::Secondary) {
        st.swallow = false;
    }
    // (the markers, made once)
    for m in st.markers.iter_mut() {
        if m.is_none() {
            *m = Some(commands.spawn((SummonMarker, Transform::default(), Visibility::Hidden, DespawnOnExit(GameState::InGame))).id());
        }
    }
    let end = |st: &mut SummonAim, powers: &mut crate::powers::Powers, msgs: &mut HudMessages| {
        st.on = false;
        st.spot = None;
        powers.aiming = false;
        if st.refused {
            msgs.drop_tutorial(REFUSAL);
            st.refused = false;
        }
    };
    if st.on {
        // put away, the hand taken, or Use ("Cancel Summon"): the aim ends without him
        let cancel = !selected || !can_act || !powers.aiming || bind.just_pressed(crate::bindings::Act::Use);
        if cancel {
            sfx.write(PostEvent::named(&sound("m_pTargetingFailSoundEvent", "Snd_Pwr_Summon_Fail"), None));
            st.cancel_seq = st.cancel_seq.wrapping_add(1);
            st.swallow = bind.pressed(crate::bindings::Act::Secondary);
            end(&mut st, &mut powers, &mut msgs);
        } else if !bind.pressed(crate::bindings::Act::Secondary) {
            // let go (0x00CFA1E0): summoned on a spot, else as cancelled
            let cost = Power::SummonAssassin.mana(&data);
            match st.spot.filter(|s| s.ok) {
                Some(s) if stats.mana >= cost => {
                    stats.spend_mana(cost, &attrs);
                    summon.write(Summon { at: s.feet, target: s.npc, level: level_n });
                    sfx.write(PostEvent::named(&sound("m_pTargetingSuccessSoundEvent", "Snd_Pwr_Summon_Out"), None));
                    powers.cast_seq = powers.cast_seq.wrapping_add(1);
                    powers.cast_power = Power::SummonAssassin;
                    powers.cooldown = 1.0;
                    used.write(crate::powers::PowerUsed(Power::SummonAssassin));
                }
                _ => {
                    sfx.write(PostEvent::named(&sound("m_pTargetingFailSoundEvent", "Snd_Pwr_Summon_Fail"), None));
                    st.cancel_seq = st.cancel_seq.wrapping_add(1);
                }
            }
            end(&mut st, &mut powers, &mut msgs);
        }
    } else if selected && can_act && !st.swallow && bind.just_pressed(crate::bindings::Act::Secondary) && powers.cooldown <= 0.0 {
        let cost = Power::SummonAssassin.mana(&data);
        if tune.inhibited {
            sfx.write(PostEvent::named("Snd_Power_Empty", None));
            powers.cooldown = 0.5;
        } else {
            // (short of mana: a remedy drunk first, if the option says so)
            if stats.mana < cost && settings.auto_mana_elixir && stats.mana_elixirs > 0 {
                stats.mana_elixirs -= 1;
                stats.mana = (stats.mana + attrs.mana_elixir_amount(stats.max_mana, rand::random::<f32>())).min(stats.max_mana);
                stats.mana_cap = stats.mana_cap.max(stats.mana);
                sfx.write(PostEvent::named("Snd_UI_Ingame_Wheel_Mana", None));
            }
            if stats.mana < cost {
                crate::powers::out_of_mana(&mut powers, &mut sfx);
            } else {
                st.on = true;
                powers.aiming = true;
                sfx.write(PostEvent::named(&sound("m_pStartTargetingSoundEvent", "Snd_Pwr_Summon_Start"), None));
            }
        }
    }
    // the spot, while aiming
    if st.on {
        if let (Ok(ctx), Ok(cg)) = (rapier.single(), cam.single()) {
            let enemies: Vec<Enemy> = npcs.iter().filter(|(_, _, n)| !n.is_down() && n.hostile()).map(|(e, t, _)| Enemy { e, center: t.translation, facing: t.rotation * Vec3::NEG_Z }).collect();
            let spot = find_spot(&ctx, pe, cg.translation(), cg.rotation(), pt.translation, &enemies, nav.as_deref(), &Reach::of(&data));
            if std::env::var_os("DH_SUMMON_LOG").is_some() && st.spot.map(|s| (s.ok, s.npc)) != Some((spot.ok, spot.npc)) {
                info!("summon aim: {spot:?}");
            }
            st.spot = Some(spot);
        }
        // "You cannot summon there." held while it is so
        let refused = st.spot.is_some_and(|s| !s.ok);
        if refused != st.refused {
            if refused {
                msgs.tutorial(REFUSAL, f32::INFINITY);
            } else {
                msgs.drop_tutorial(REFUSAL);
            }
            st.refused = refused;
        }
    }
    // the markers: the aim's (on the enemy, in the air, or refused), the ground's under a spot
    let want: [(Option<&'static str>, Vec3); 2] = match st.spot.filter(|_| st.on) {
        Some(s) if s.npc.is_some() => [(Some("summon_target_npc"), s.mark.unwrap_or(s.feet)), (None, Vec3::ZERO)],
        Some(s) if s.ok => [(Some("summon_target_air"), s.mark.unwrap_or(s.feet)), (Some("summon_target"), s.ground)],
        Some(Spot { mark: Some(m), .. }) => [(Some("summon_target_null"), m), (None, Vec3::ZERO)],
        _ => [(None, Vec3::ZERO), (None, Vec3::ZERO)],
    };
    let has = |n: &str| level.as_ref().is_some_and(|l| l.scene.effects.iter().any(|e| e.0 == n));
    for (i, (name, at)) in want.into_iter().enumerate() {
        let Some(me) = st.markers[i] else { continue };
        let name = name.filter(|n| has(n));
        if let Ok((mut t, mut v)) = markers.get_mut(me) {
            t.translation = at;
            *v = if name.is_some() { Visibility::Inherited } else { Visibility::Hidden };
        }
        if name != st.shown[i] {
            for (tag, mut em) in &mut tagged {
                if tag.0 == me {
                    em.stop();
                }
            }
            if let Some(n) = name {
                // (carried by the marker: placed at it)
                fx.write(crate::particles::SpawnEffect { follow: Some(me), tag: Some(me), secs: 600.0, ..crate::particles::SpawnEffect::at(n, Vec3::ZERO) });
            }
            st.shown[i] = name;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_look_meets_an_enemy_in_its_path_nearest_first() {
        let e = |i: u32, z: f32, x: f32| Enemy { e: Entity::from_raw_u32(i).unwrap(), center: Vec3::new(x, 0.97, -z), facing: Vec3::Z };
        let enemies = [e(1, 8.0, 0.1), e(2, 4.0, 0.0), e(3, 2.0, 1.0)];
        let eye = Vec3::new(0.0, 1.6, 0.0);
        let fwd = Vec3::NEG_Z;
        // the one 4 m on (the one 2 m on stands a metre aside)
        assert_eq!(met_on_the_way(eye, fwd, 20.0, &enemies).map(|n| n.e), Some(Entity::from_raw_u32(2).unwrap()));
        // a wall at 3 m hides them all
        assert!(met_on_the_way(eye, fwd, 3.0, &enemies).is_none());
    }

    #[test]
    fn the_assassin_goes_behind_the_enemy_by_its_facing() {
        // facing Daud (+Z, toward him at the origin): behind it is away from him
        assert_eq!(behind(Vec3::Z), Vec3::NEG_Z);
        // turned away from him: behind it is his side
        assert_eq!(behind(Vec3::NEG_Z), Vec3::Z);
    }
}
