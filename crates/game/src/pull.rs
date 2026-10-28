//! Daud's Pull (The Brigmore Witches: `DLC07_Twk_Pwr_Pull`, `DisDLC07Tweaks_Pull`): held, he
//! reaches for what he looks at (`m_fMaxDistance`, 20 m; within `m_fCheckExtent`, 1 m, of the
//! look, in sight), its rim showing; let go, it comes to him: a pickup flies to his hand at
//! `m_fPickupSpeed` (30 m/s) and is taken, a loose prop into his hands as if grabbed. From the
//! second level, living things and bodies: a person is torn off its feet
//! (`DisDLC07StateNPCMasterPulled`) and drawn to him at `m_Level2Params.m_fSpeed` (5 m/s) to stop
//! `m_fMinDistance` (2 m) before him; within `m_fLiftAnimDistance` (3 m) it is held up
//! (`m_bLifted`), helpless, `m_fLiftMinHeight`..`m_fLiftMaxHeight` (0.5-1.5 m) off the floor, his
//! arm holding it (`Powers_Cast_Pull_Lift_In`, `_Lift_Loop`). Held, it can be killed or choked
//! out from any side, and it is his shield: the blows and shots that would reach him strike it
//! (`ePlayerStat_DamageTakenByPullLiftShield`). He lets it drop with the power again
//! (`_Lift_Out`) or Use (`_Lift_Abort`); a witch blinks out of it after `m_TeleportDelay`
//! (2-12 s). A body (`DisDLC07StateNPCMasterDead_Pulled`) is drawn in by its pawn's bone
//! (`m_PullBoneName`) at `m_fPullForce`, held up by `m_PullUpForce`, and taken onto his shoulder
//! as it reaches him. Each costs as its class does (`m_ManaCosts`: a pawn, living or dead, 40, a
//! pickup or a prop 20). The arms reach (`Powers_Cast_Pull_In`, `_Loop`), cast (`_Out`) or give
//! up (`_Abort`: Use, the power put away, nothing there). The scripts can bar a person
//! (`DisDLC07SeqAct_SetPullOverride`: Lizzy Stride) and hear of one taken
//! (`DisDLC07SeqEvent_NPCPulled`: begun, over).
//!
//! Our reading where the tweak is silent: a person drawn in goes straight to him (it was in his
//! sight), and dropped falls limp (`m_fCancelImpulse`, `m_fCancelHeightImpulse` as its push);
//! the hold lasts until he lets go; a body drawn in goes no faster than a person; the rats, fish
//! and river krusts its list also names are not taken.

use bevy::prelude::*;
use bevy_rapier3d::prelude::{CollisionGroups, Group, QueryFilter, ReadRapierContext};

use crate::audio::PostEvent;
use crate::gameplay::{NpcStagger, PlayerStats};
use crate::interact::Pickup;
use crate::level::{LevelInfo, GROUP_WORLD};
use crate::npc::{FromSpawner, Npc};
use crate::powers::Power;
use crate::GameState;

pub struct PullPlugin;

impl Plugin for PullPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PullAim>().add_systems(
            Update,
            (hold_lift, aim, fly_pickups, pulled_bodies).chain().after(crate::interact::FocusSet).before(crate::interact::use_focus).run_if(in_state(GameState::InGame)),
        );
    }
}

/// What Pull reaches for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Pickup(Entity),
    Prop(Entity),
    Person(Entity),
    /// the dead or unconscious
    Body(Entity),
    /// a river krust, torn off its wall
    Krust(Entity),
}

impl Target {
    /// Its mana (`m_ManaCosts`: a pawn's 40, living or dead; a pickup's or a prop's 20).
    fn cost(self) -> f32 {
        match self {
            Target::Person(_) | Target::Body(_) | Target::Krust(_) => 40.0,
            _ => 20.0,
        }
    }
    fn entity(self) -> Entity {
        match self {
            Target::Pickup(e) | Target::Prop(e) | Target::Person(e) | Target::Body(e) | Target::Krust(e) => e,
        }
    }
}

/// The aim, and what is on its way.
#[derive(Resource, Default)]
pub struct PullAim {
    on: bool,
    target: Option<Target>,
    /// aims given up (the arms play `_Abort`)
    pub cancel_seq: u32,
    swallow: bool,
    /// pickups flying in
    flying: Vec<Entity>,
    /// the person drawn in and held (the arms hold it up while there is one), and how long a
    /// witch has before she blinks out of it
    pub lift: Option<Entity>,
    blink_out: Option<f32>,
    /// holds ended: let go (the arms play `_Lift_Out`), given up (`_Lift_Abort`)
    pub lift_out_seq: u32,
    pub lift_abort_seq: u32,
    /// bodies drawn in: each, and seconds left to reach him
    bodies: Vec<(Entity, f32)>,
}

impl PullAim {
    /// The one held up before him as a shield, if any.
    pub fn shield(&self) -> Option<Entity> {
        self.lift
    }
}

/// The tweak's numbers (`DisDLC07Tweaks_Pull`), metres.
struct Reach {
    max: f32,
    extent: f32,
    pickup_speed: f32,
    /// a living thing's pull (`m_Level2Params`): its speed, where it stops, within what it is
    /// held up, at what height off the floor, how near counts as there (`m_fCutOffThreshold`)
    speed: f32,
    stop: f32,
    lift_distance: f32,
    lift: [f32; 2],
    cutoff: f32,
    /// a witch's time before she blinks out of it (`m_TeleportDelay`)
    teleport: [f32; 2],
    /// a hold let go: its push along and up (`m_fCancelImpulse`, `m_fCancelHeightImpulse`)
    cancel: [f32; 2],
}

impl Reach {
    fn of(data: &crate::gamedata::Data) -> Reach {
        let p = |k: &str, d: f32| data.active("Pull").and_then(|a| a.params.get(k)).copied().unwrap_or(d);
        Reach {
            max: p("m_fMaxDistance", 2000.0) * 0.01,
            extent: p("m_fCheckExtent", 100.0) * 0.01,
            pickup_speed: p("m_fPickupSpeed", 3000.0) * 0.01,
            speed: p("m_Level2Params.m_fSpeed", 500.0) * 0.01,
            stop: p("m_Level2Params.m_fMinDistance", 200.0) * 0.01,
            lift_distance: p("m_fLiftAnimDistance", 300.0) * 0.01,
            lift: [p("m_fLiftMinHeight", 50.0) * 0.01, p("m_fLiftMaxHeight", 150.0) * 0.01],
            cutoff: p("m_fCutOffThreshold", 50.0) * 0.01,
            teleport: [p("m_TeleportDelay.m_fMinValue", 2.0), p("m_TeleportDelay.m_fMaxValue", 12.0)],
            cancel: [p("m_fCancelImpulse", 10.0) * 0.01, p("m_fCancelHeightImpulse", 200.0) * 0.01],
        }
    }
}

/// Whether the one held up before him stands between him and a blow or a shot from `from`: ahead
/// of him on its way (no further than its source), within half a body's width of its line (0.6 m:
/// our reading of the shield's reach, the held one's body as it hangs).
pub fn shields(daud: Vec3, from: Vec3, held: Vec3) -> bool {
    let to_attacker = (from - daud).with_y(0.0);
    let dir = to_attacker.normalize_or_zero();
    let to_held = (held - daud).with_y(0.0);
    let along = to_held.dot(dir);
    let off = (to_held - dir * along).length();
    along > 0.0 && along < to_attacker.length() + 0.5 && off < 0.6
}

/// Where a person held by Pull is drawn to: before him at `stop` along his look (level), its feet
/// `height` off his floor once held up; on its way, at its own height until it is within the lift
/// distance.
pub fn lift_goal(daud: Vec3, look: Vec3, at: Vec3, stop: f32, height: f32, lift_distance: f32) -> (Vec3, bool) {
    let flat = look.with_y(0.0).normalize_or(Vec3::NEG_Z);
    let feet = daud.y - (crate::player::STAND_HALF + crate::player::RADIUS);
    let near = (at - daud).with_y(0.0).length() <= lift_distance;
    let held = daud.with_y(feet + height + crate::npc::NPC_CENTER) + flat * stop;
    if near {
        (held, true)
    } else {
        (held.with_y(at.y.max(held.y - 1.0)), false)
    }
}

/// The nearest thing to the look: ahead within `max`, off the look by no more than `extent`
/// (or a twentieth of its distance, farther off), the least off (and nearer) first.
pub fn pick(eye: Vec3, fwd: Vec3, things: &[(Target, Vec3)], max: f32, extent: f32) -> Option<(Target, Vec3)> {
    let mut best: Option<(f32, Target, Vec3)> = None;
    for &(t, at) in things {
        let v = at - eye;
        let along = v.dot(fwd);
        if !(0.5..=max).contains(&along) {
            continue;
        }
        let off = (v - fwd * along).length();
        let allowed = extent.max(along * 0.05);
        if off > allowed {
            continue;
        }
        let score = off / allowed + along / max * 0.25;
        if best.as_ref().is_none_or(|b| score < b.0) {
            best = Some((score, t, at));
        }
    }
    best.map(|(_, t, at)| (t, at))
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn aim(
    (bind, paused, cursor, scripted): (Res<crate::bindings::Bindings>, Res<crate::hud::Paused>, Single<&bevy::window::CursorOptions>, Option<Res<crate::script::Scripted>>),
    (mut st, mut powers, mut stats, data, attrs, tune, settings, tc, held, possession): (
        ResMut<PullAim>,
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
    player: Query<(Entity, &Transform, &crate::player::Player)>,
    cam: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    (pickups, props, bodies): (Query<(Entity, &Pickup, &GlobalTransform)>, Query<(Entity, &crate::props::Prop)>, Query<&GlobalTransform, Without<Pickup>>),
    mut npcs: Query<(Entity, &mut Npc, &Transform, &FromSpawner), Without<crate::player::Player>>,
    level: Option<Res<LevelInfo>>,
    mut vm: Option<ResMut<crate::kismet::Vm>>,
    (mut sfx, mut used, mut stagger, mut grab, mut lit): (MessageWriter<PostEvent>, MessageWriter<crate::powers::PowerUsed>, MessageWriter<NpcStagger>, MessageWriter<crate::props::PullProp>, ResMut<crate::highlight::ExtraHighlight>),
    (mut commands, mut carry, ragdolls, consumed): (Commands, ResMut<crate::carry::Carry>, Query<(), With<crate::ragdoll::Ragdoll>>, Query<(), Or<(With<crate::npc::ConsumedBody>, With<crate::npc::Ashes>, With<crate::npc::Immolated>)>>),
    mut krusts: Query<(Entity, &mut crate::krust::Krust), Without<crate::possession::Possessed>>,
) {
    if paused.0 {
        return;
    }
    let level_n = Power::Pull.level(&stats);
    let Ok((pe, pt, p)) = player.single() else { return };
    let grabbed = cursor.grab_mode != bevy::window::CursorGrabMode::None || scripted.is_some();
    let can_act = grabbed && !stats.dead && !p.locked && !held.busy() && powers.blink.is_none() && tc.wheel >= 1.0 && possession.host.is_none();
    let selected = powers.selected == Power::Pull;
    if st.swallow && !bind.pressed(crate::bindings::Act::Secondary) {
        st.swallow = false;
    }
    let sound = |key: &str| data.power_sound("Pull", level_n, key).map(str::to_string);
    let end = |st: &mut PullAim, powers: &mut crate::powers::Powers| {
        st.on = false;
        st.target = None;
        powers.aiming = false;
    };
    if st.on {
        let cancel = !selected || !can_act || !powers.aiming || bind.just_pressed(crate::bindings::Act::Use);
        if cancel {
            st.cancel_seq = st.cancel_seq.wrapping_add(1);
            st.swallow = bind.pressed(crate::bindings::Act::Secondary);
            end(&mut st, &mut powers);
        } else if !bind.pressed(crate::bindings::Act::Secondary) {
            // let go: it comes, if there is one and the mana for it
            match st.target {
                // (Leverage's `PullManaCostMultiplier`)
                Some(t) if stats.mana >= t.cost() * attrs.pull_cost => {
                    stats.spend_mana(t.cost() * attrs.pull_cost, &attrs);
                    powers.cast_seq = powers.cast_seq.wrapping_add(1);
                    powers.cast_power = Power::Pull;
                    powers.cooldown = 0.8;
                    used.write(crate::powers::PowerUsed(Power::Pull));
                    if let Some(s) = sound("m_pAcquiredSoundEvent") {
                        sfx.write(PostEvent::named(&s, None));
                    }
                    match t {
                        Target::Pickup(e) => st.flying.push(e),
                        Target::Prop(e) => {
                            grab.write(crate::props::PullProp(e));
                        }
                        Target::Person(e) => {
                            if let Ok((_, mut npc, nt, _)) = npcs.get_mut(e) {
                                // torn off its feet by the side it is taken from, drawn to him
                                // (Leverage's `PullSpeedMultiplier`: faster), knowing him now
                                let r = Reach::of(&data);
                                let from = (pt.translation - nt.translation).with_y(0.0).normalize_or_zero();
                                let f = npc.forward();
                                let side = Vec3::new(-f.z, 0.0, f.x);
                                let (ahead, right) = (from.dot(f), from.dot(side));
                                let dir = if ahead.abs() >= right.abs() { if ahead >= 0.0 { 0 } else { 3 } } else if right > 0.0 { 2 } else { 1 };
                                let height = r.lift[0] + rand::random::<f32>() * (r.lift[1] - r.lift[0]).max(0.0);
                                npc.pulled = Some(crate::npc::Pulled { goal: nt.translation, speed: r.speed * attrs.pull_speed, lifted: false, from: dir, shown: 0, height, face: None });
                                npc.attack_t = None;
                                npc.knock = None;
                                npc.last_seen = Some(pt.translation);
                                stagger.write(NpcStagger { npc: e, secs: 0.5, parried: false });
                                // (a witch blinks out of it, `m_TeleportDelay`)
                                st.blink_out = npc.arms.witch.is_some().then(|| r.teleport[0] + rand::random::<f32>() * (r.teleport[1] - r.teleport[0]).max(0.0));
                                st.lift = Some(e);
                                if let Some(vm) = vm.as_mut() {
                                    vm.npc_pulled.push(0);
                                }
                            }
                        }
                        Target::Krust(e) => {
                            // (torn at, then off its wall: thrown at him along the pull,
                            // `m_RiverKrustImpulse` -750 along it, 500 up, in the pull's frame: our
                            // reading of the frame)
                            if let (Ok((_, mut k)), Ok(g)) = (krusts.get_mut(e), bodies.get(e)) {
                                let p = |key: &str, d: f32| data.active("Pull").and_then(|a| a.params.get(key)).copied().unwrap_or(d);
                                let toward = (pt.translation - g.translation()).with_y(0.0).normalize_or_zero();
                                let throw = toward * -p("m_RiverKrustImpulse.x", -750.0) * 0.01 + Vec3::Y * p("m_RiverKrustImpulse.z", 500.0) * 0.01;
                                let secs = level.as_ref().and_then(|l| l.scene.krusts.get(k.index() as usize)).map_or(1.3, |d| if d.pull_duration > 0.0 { d.pull_duration } else { 1.3 });
                                k.pull(secs, throw);
                            }
                        }
                        Target::Body(e) => {
                            // drawn in by its bone where it has a body to draw (else onto his
                            // shoulder at once)
                            let pull = npcs.get(e).ok().and_then(|(_, n, _, _)| n.npc_type).and_then(|i| level.as_ref()?.scene.npc_types.get(i as usize)?.stats.as_ref()?.pull.clone());
                            if ragdolls.contains(e) {
                                let p = pull.unwrap_or_default();
                                let r = Reach::of(&data);
                                commands.entity(e).try_insert(crate::ragdoll::PulledBody {
                                    to: pt.translation,
                                    force: if p.force > 0.0 { p.force } else { 20.0 },
                                    up: if p.force > 0.0 { p.up } else { [[-10.0, 15.0], [0.0, 10.0], [10.0, 0.0]] },
                                    bone: if p.bone.is_empty() { "neck_jnt".into() } else { p.bone },
                                    speed: r.speed * attrs.pull_speed,
                                    at: None,
                                });
                                st.bodies.push((e, 5.0));
                            } else {
                                carry.take = Some(e);
                            }
                            if let Some(vm) = vm.as_mut() {
                                vm.npc_pulled.push(0);
                            }
                        }
                    }
                }
                Some(_) => {
                    crate::powers::out_of_mana(&mut powers, &mut sfx);
                    st.cancel_seq = st.cancel_seq.wrapping_add(1);
                }
                None => st.cancel_seq = st.cancel_seq.wrapping_add(1),
            }
            end(&mut st, &mut powers);
        }
    } else if selected && can_act && !st.swallow && st.lift.is_none() && !carry.carrying() && bind.just_pressed(crate::bindings::Act::Secondary) && powers.cooldown <= 0.0 {
        if tune.inhibited {
            sfx.write(PostEvent::named("Snd_Power_Empty", None));
            powers.cooldown = 0.5;
        } else {
            // (short of the least it costs: a remedy drunk first, if the option says so)
            if stats.mana < 20.0 && settings.auto_mana_elixir && stats.mana_elixirs > 0 {
                stats.mana_elixirs -= 1;
                stats.mana = (stats.mana + attrs.mana_elixir_amount(stats.max_mana, rand::random::<f32>())).min(stats.max_mana);
                stats.mana_cap = stats.mana_cap.max(stats.mana);
                sfx.write(PostEvent::named("Snd_UI_Ingame_Wheel_Mana", None));
            }
            if stats.mana < 20.0 {
                crate::powers::out_of_mana(&mut powers, &mut sfx);
            } else {
                st.on = true;
                powers.aiming = true;
                if let Some(s) = sound("m_pStartTargetingSoundEvent") {
                    sfx.write(PostEvent::named(&s, None));
                }
            }
        }
    }
    // what it reaches for, while held
    if st.on {
        if let (Ok(ctx), Ok(cg), Some(level)) = (rapier.single(), cam.single(), level.as_ref()) {
            // (Leverage's `PullDistanceMultiplier`: farther)
            let mut r = Reach::of(&data);
            r.max *= attrs.pull_reach;
            let (eye, fwd) = (cg.translation(), cg.forward().as_vec3());
            let mut things: Vec<(Target, Vec3)> = pickups.iter().map(|(e, _, g)| (Target::Pickup(e), g.translation())).collect();
            for (e, prop) in &props {
                let (Some(b), _) = prop.body() else { continue };
                if crate::props::takeable(level, prop) {
                    if let Ok(g) = bodies.get(b) {
                        things.push((Target::Prop(e), g.translation()));
                    }
                }
            }
            if level_n >= 2 {
                let blocked = vm.as_ref().map(|v| v.pull_blocked.clone()).unwrap_or_default();
                for (e, n, t, from) in &npcs {
                    if !n.is_down() && !n.floored && n.pulled.is_none() && !blocked.contains(&from.0) {
                        things.push((Target::Person(e), t.translation));
                    } else if n.is_down() && !consumed.contains(e) && !st.bodies.iter().any(|b| b.0 == e) {
                        // (the dead and the unconscious, where they lie: their middle above the
                        // floor)
                        things.push((Target::Body(e), t.translation - Vec3::Y * 0.6));
                    }
                }
            }
            // (the river krusts, living, on their walls)
            if level_n >= 2 {
                for (e, k) in &krusts {
                    if k.alive() && !k.pulled() {
                        if let Ok(g) = bodies.get(k.collider()) {
                            things.push((Target::Krust(e), g.translation()));
                        }
                    }
                }
            }
            let filter = QueryFilter::default().exclude_collider(pe).groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
            // (in sight: nothing of the world between)
            things.retain(|(_, at)| {
                let to = *at - eye;
                let d = to.length();
                ctx.cast_ray(eye, to / d.max(1e-3), (d - 0.3).max(0.0), true, filter).is_none()
            });
            st.target = pick(eye, fwd, &things, r.max, r.extent).map(|(t, _)| t);
        }
    }
    let want: Vec<Entity> = st.target.filter(|_| st.on).map(|t| vec![t.entity()]).unwrap_or_default();
    if lit.0 != want {
        lit.0 = want;
    }
}

/// Pickups drawn in, at `m_fPickupSpeed`: taken at his hand.
fn fly_pickups(
    time: Res<Time>,
    data: Res<crate::gamedata::Data>,
    attrs: Option<Res<crate::gamedata::Attrs>>,
    mut st: ResMut<PullAim>,
    cam: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    pickups: Query<&Pickup>,
    mut transforms: Query<&mut Transform>,
    mut force: ResMut<crate::interact::ForceUse>,
) {
    if st.flying.is_empty() {
        return;
    }
    let Ok(cg) = cam.single() else { return };
    let hand = cg.translation() + cg.forward().as_vec3() * 0.6 - Vec3::Y * 0.2;
    let step = Reach::of(&data).pickup_speed * attrs.as_ref().map_or(1.0, |a| a.pull_speed) * time.delta_secs();
    let mut done = Vec::new();
    for &e in &st.flying {
        let Ok(p) = pickups.get(e) else {
            done.push(e);
            continue;
        };
        let Ok(mut t) = transforms.get_mut(e) else {
            done.push(e);
            continue;
        };
        let to = hand - t.translation;
        let d = to.length();
        let delta = if d <= step { to } else { to / d * step };
        t.translation += delta;
        for &x in &p.entities {
            if let Ok(mut xt) = transforms.get_mut(x) {
                xt.translation += delta;
            }
        }
        // (at his hand: taken, one a frame)
        if d <= step + 0.3 && force.0.is_none() {
            force.0 = Some(e);
            done.push(e);
        }
    }
    st.flying.retain(|e| !done.contains(e));
}

/// The person held: drawn to him and held up before him while he keeps it; let go with the power
/// again (`_Lift_Out`) or Use (`_Lift_Abort`), the power put away or he down, it falls; killed or
/// taken down, it is no longer his to hold; a witch blinks out of it. The scripts hear it is over.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn hold_lift(
    time: Res<Time>,
    (bind, paused, data, attrs, stats, powers): (Res<crate::bindings::Bindings>, Res<crate::hud::Paused>, Res<crate::gamedata::Data>, Res<crate::gamedata::Attrs>, Res<PlayerStats>, Res<crate::powers::Powers>),
    mut st: ResMut<PullAim>,
    player: Query<(&Transform, &crate::player::Player)>,
    cam: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    mut npcs: Query<(&mut Npc, &Transform), Without<crate::player::Player>>,
    taken: Query<(Option<&crate::combat::Choking>, Option<&crate::combat::Assassinating>), With<crate::player::Player>>,
    mut vm: Option<ResMut<crate::kismet::Vm>>,
    mut sfx: MessageWriter<PostEvent>,
    mut witches: Query<&mut crate::witch::Witch>,
) {
    let Some(e) = st.lift else { return };
    if paused.0 {
        return;
    }
    let r = Reach::of(&data);
    let dt = time.delta_secs();
    let taken_down = taken.single().is_ok_and(|(c, a)| c.is_some_and(|c| c.npc == e) || a.is_some_and(|a| a.npc == e));
    let Ok((mut npc, nt)) = npcs.get_mut(e) else {
        st.lift = None;
        return;
    };
    // (choked or killed by him: his takedown has it now)
    if taken_down || npc.is_down() || npc.mode == crate::npc::Mode::Choked {
        npc.pulled = None;
        st.lift = None;
        if npc.is_down() && !taken_down {
            st.lift_out_seq = st.lift_out_seq.wrapping_add(1);
        }
        if let Some(vm) = vm.as_mut() {
            vm.npc_pulled.push(1);
        }
        return;
    }
    let Ok((pt, p)) = player.single() else { return };
    let look = cam.single().map(|c| c.forward().as_vec3()).unwrap_or(Vec3::NEG_Z);
    let witch_out = st.blink_out.as_mut().is_some_and(|t| {
        *t -= dt;
        *t <= 0.0
    });
    let let_go = bind.just_pressed(crate::bindings::Act::Secondary);
    let give_up = bind.just_pressed(crate::bindings::Act::Use) || powers.selected != Power::Pull || stats.dead || p.locked;
    if let_go || give_up || witch_out || npc.pulled.is_none() {
        if give_up {
            st.lift_abort_seq = st.lift_abort_seq.wrapping_add(1);
        } else {
            st.lift_out_seq = st.lift_out_seq.wrapping_add(1);
        }
        st.swallow = let_go;
        st.lift = None;
        st.blink_out = None;
        npc.pulled = None;
        if witch_out {
            // (she blinks out of it, out of his reach: away from him, where she can stand)
            if let Ok(mut w) = witches.get_mut(e) {
                w.escape = true;
            }
        } else {
            // (dropped: it falls limp, a little push along and up)
            let along = (nt.translation - pt.translation).with_y(0.0).normalize_or_zero();
            npc.knock = Some((along * r.cancel[0] + Vec3::Y * r.cancel[1], crate::npc::Throw::Limp));
        }
        if let Some(s) = data.power_sound("Pull", 2, "m_pStopSoundEvent").map(str::to_string) {
            sfx.write(PostEvent::named(&s, None));
        }
        if let Some(vm) = vm.as_mut() {
            vm.npc_pulled.push(1);
        }
        return;
    }
    // drawn on: before him, held up once near (its height kept), helpless
    let Some(pl) = npc.pulled.as_mut() else { return };
    let (goal, near) = lift_goal(pt.translation, look, nt.translation, r.stop, pl.height, r.lift_distance + r.cutoff);
    if near && !pl.lifted {
        pl.lifted = true;
        sfx.write(PostEvent::named("Snd_Pwr_Pull_Lift", Some(nt.translation)));
    }
    pl.goal = goal;
    // (held: it keeps up with him as he moves)
    if pl.lifted {
        pl.speed = pl.speed.max(r.speed * attrs.pull_speed * 3.0);
    }
    npc.stagger = npc.stagger.max(0.3);
    npc.attack_t = None;
    npc.aim = None;
}

/// Bodies drawn in: toward his chest; at him, onto his shoulder (`carry`); not there in time
/// (caught on something), let be.
#[allow(clippy::type_complexity)]
fn pulled_bodies(
    mut commands: Commands,
    time: Res<Time>,
    mut st: ResMut<PullAim>,
    mut carry: ResMut<crate::carry::Carry>,
    player: Query<&Transform, With<crate::player::Player>>,
    mut bodies: Query<&mut crate::ragdoll::PulledBody>,
    mut vm: Option<ResMut<crate::kismet::Vm>>,
) {
    if st.bodies.is_empty() {
        return;
    }
    let Ok(pt) = player.single() else { return };
    let dt = time.delta_secs();
    let chest = pt.translation + Vec3::Y * 0.2;
    let mut over = 0;
    let free = !carry.carrying() && carry.take.is_none();
    let mut taking = None;
    st.bodies.retain_mut(|(e, left)| {
        *left -= dt;
        let Ok(mut pb) = bodies.get_mut(*e) else {
            over += 1;
            return false;
        };
        pb.to = chest;
        // (at his feet or before him: within reach of his arms, as Use would take it up)
        let arrived = pb.at.is_some_and(|a| (a - chest).with_y(0.0).length() < 1.8 && (a.y - chest.y).abs() < 2.2);
        if (arrived && free && taking.is_none()) || *left <= 0.0 {
            commands.entity(*e).try_remove::<crate::ragdoll::PulledBody>();
            if arrived {
                taking = Some(*e);
            }
            over += 1;
            return false;
        }
        true
    });
    if let Some(e) = taking {
        carry.take = Some(e);
    }
    if let Some(vm) = vm.as_mut() {
        vm.npc_pulled.extend(std::iter::repeat_n(1, over));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_thing_least_off_the_look_is_taken_within_reach() {
        let e = |i: u32| Entity::from_raw_u32(i).unwrap();
        let things = [(Target::Pickup(e(1)), Vec3::new(0.3, 0.0, -5.0)), (Target::Prop(e(2)), Vec3::new(0.05, 0.0, -8.0)), (Target::Person(e(3)), Vec3::new(0.0, 0.0, -25.0)), (Target::Pickup(e(4)), Vec3::new(3.0, 0.0, -5.0))];
        let got = pick(Vec3::ZERO, Vec3::NEG_Z, &things, 20.0, 1.0).map(|t| t.0);
        assert_eq!(got, Some(Target::Prop(e(2))), "the nearest to the look, not the nearest");
        // beyond reach or well off the look: nothing
        assert_eq!(pick(Vec3::ZERO, Vec3::NEG_Z, &things[2..], 20.0, 1.0), None);
    }

    #[test]
    fn a_person_is_drawn_before_him_and_held_up_once_near() {
        let daud = Vec3::new(0.0, 1.0, 0.0);
        let feet = daud.y - (crate::player::STAND_HALF + crate::player::RADIUS);
        // far: toward the place before him, at its own height
        let (far, held) = lift_goal(daud, Vec3::NEG_Z, Vec3::new(0.0, feet + 0.97, -10.0), 2.0, 1.0, 3.5);
        assert!(!held);
        assert!((far - Vec3::new(0.0, feet + 0.97, -2.0)).length() < 1e-3, "{far}");
        // near: held up before him, its feet its height off his floor
        let (up, held) = lift_goal(daud, Vec3::new(0.0, -0.5, -1.0), Vec3::new(0.3, feet + 0.97, -3.0), 2.0, 1.0, 3.5);
        assert!(held);
        assert!((up - Vec3::new(0.0, feet + 1.0 + crate::npc::NPC_CENTER, -2.0)).length() < 1e-3, "{up}");
    }

    #[test]
    fn the_one_held_up_takes_what_comes_from_its_side() {
        let daud = Vec3::ZERO;
        let held = Vec3::new(0.0, 0.5, -2.0);
        // a guard before him, beyond it: struck instead; one beside him, or behind: not
        assert!(shields(daud, Vec3::new(0.3, 0.0, -6.0), held));
        assert!(shields(daud, Vec3::new(0.0, 0.0, -1.8), held));
        assert!(!shields(daud, Vec3::new(4.0, 0.0, 0.0), held));
        assert!(!shields(daud, Vec3::new(0.0, 0.0, 5.0), held));
    }

    #[test]
    fn a_body_is_held_up_more_the_further_below_him_it_lies() {
        let up = [[-10.0, 15.0], [0.0, 10.0], [10.0, 0.0]];
        assert_eq!(crate::ragdoll::pull_lift(&up, -20.0), 15.0);
        assert_eq!(crate::ragdoll::pull_lift(&up, 0.0), 10.0);
        assert!((crate::ragdoll::pull_lift(&up, -5.0) - 12.5).abs() < 1e-4);
        assert!((crate::ragdoll::pull_lift(&up, 5.0) - 5.0).abs() < 1e-4);
        assert_eq!(crate::ragdoll::pull_lift(&up, 30.0), 0.0);
    }
}
