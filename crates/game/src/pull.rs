//! Daud's Pull (The Brigmore Witches: `DLC07_Twk_Pwr_Pull`, `DisDLC07Tweaks_Pull`): held, he
//! reaches for what he looks at (`m_fMaxDistance`, 20 m; within `m_fCheckExtent`, 1 m, of the
//! look, in sight), its rim showing; let go, it comes to him: a pickup flies to his hand at
//! `m_fPickupSpeed` (30 m/s) and is taken, a loose prop into his hands as if grabbed, and from
//! the second level a person (`m_Level2Params`) torn off its feet and flung to him, reeling,
//! to stop `m_fMinDistance` (2 m) short. Each costs as its class does (`m_ManaCosts`: a person
//! 40, a pickup or a prop 20). The arms reach (`Powers_Cast_Pull_In`, `_Loop`), cast (`_Out`)
//! or give up (`_Abort`: Use, the power put away, nothing there). The scripts can bar a
//! person (`DisDLC07SeqAct_SetPullOverride`: Lizzy Stride) and hear of one taken
//! (`DisDLC07SeqEvent_NPCPulled`: begun, over).
//!
//! Our reading where the tweak is silent: the person's flight is a throw toward him (across at
//! up to 14 m/s, up at 3), as Wind Blast's push in reverse; the rats, fish and river krusts
//! its list also names are not taken.

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
            (aim, fly_pickups, pulled_people).chain().after(crate::interact::FocusSet).before(crate::interact::use_focus).run_if(in_state(GameState::InGame)),
        );
    }
}

/// What Pull reaches for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Pickup(Entity),
    Prop(Entity),
    Person(Entity),
}

impl Target {
    /// Its mana (`m_ManaCosts`: a person's 40, a pickup's or a prop's 20).
    fn cost(self) -> f32 {
        match self {
            Target::Person(_) => 40.0,
            _ => 20.0,
        }
    }
    fn entity(self) -> Entity {
        match self {
            Target::Pickup(e) | Target::Prop(e) | Target::Person(e) => e,
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
    /// people taken: seconds before the scripts hear it is over
    people: Vec<f32>,
}

/// The tweak's numbers (`DisDLC07Tweaks_Pull`), metres.
struct Reach {
    max: f32,
    extent: f32,
    pickup_speed: f32,
    min_distance: f32,
}

impl Reach {
    fn of(data: &crate::gamedata::Data) -> Reach {
        let p = |k: &str, d: f32| data.active("Pull").and_then(|a| a.params.get(k)).copied().unwrap_or(d);
        Reach {
            max: p("m_fMaxDistance", 2000.0) * 0.01,
            extent: p("m_fCheckExtent", 100.0) * 0.01,
            pickup_speed: p("m_fPickupSpeed", 3000.0) * 0.01,
            min_distance: p("m_Level1Params.m_fMinDistance", 200.0) * 0.01,
        }
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

/// A person's flight to him: across toward him (to stop short), up a little.
pub fn fling(from: Vec3, to: Vec3, stop: f32) -> Vec3 {
    let flat = (to - from).with_y(0.0);
    let d = (flat.length() - stop).max(0.0);
    flat.normalize_or_zero() * (d * 1.6).clamp(4.0, 14.0) + Vec3::Y * 3.0
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn aim(
    (mouse, keys, bind, paused, cursor, scripted): (Res<ButtonInput<MouseButton>>, Res<ButtonInput<KeyCode>>, Res<crate::bindings::Bindings>, Res<crate::hud::Paused>, Single<&bevy::window::CursorOptions>, Option<Res<crate::script::Scripted>>),
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
) {
    if paused.0 {
        return;
    }
    let level_n = Power::Pull.level(&stats);
    let Ok((pe, pt, p)) = player.single() else { return };
    let grabbed = cursor.grab_mode != bevy::window::CursorGrabMode::None || scripted.is_some();
    let can_act = grabbed && !stats.dead && !p.locked && !held.busy() && powers.blink.is_none() && tc.wheel >= 1.0 && possession.host.is_none();
    let selected = powers.selected == Power::Pull;
    if st.swallow && !mouse.pressed(MouseButton::Right) {
        st.swallow = false;
    }
    let sound = |key: &str| data.power_sound("Pull", level_n, key).map(str::to_string);
    let end = |st: &mut PullAim, powers: &mut crate::powers::Powers| {
        st.on = false;
        st.target = None;
        powers.aiming = false;
    };
    if st.on {
        let cancel = !selected || !can_act || !powers.aiming || keys.just_pressed(bind.key(crate::bindings::Act::Use));
        if cancel {
            st.cancel_seq = st.cancel_seq.wrapping_add(1);
            st.swallow = mouse.pressed(MouseButton::Right);
            end(&mut st, &mut powers);
        } else if !mouse.pressed(MouseButton::Right) {
            // let go: it comes, if there is one and the mana for it
            match st.target {
                Some(t) if stats.mana >= t.cost() => {
                    stats.spend_mana(t.cost(), &attrs);
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
                                let vel = fling(nt.translation, pt.translation, Reach::of(&data).min_distance);
                                npc.velocity = vel;
                                npc.knock = Some((vel, crate::npc::Throw::Blast));
                                stagger.write(NpcStagger { npc: e, secs: 2.5, parried: false });
                                st.people.push(1.5);
                                if let Some(vm) = vm.as_mut() {
                                    vm.npc_pulled.push(0);
                                }
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
    } else if selected && can_act && !st.swallow && mouse.just_pressed(MouseButton::Right) && powers.cooldown <= 0.0 {
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
            let r = Reach::of(&data);
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
                    if !n.is_down() && !n.floored && !blocked.contains(&from.0) {
                        things.push((Target::Person(e), t.translation));
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
    let step = Reach::of(&data).pickup_speed * time.delta_secs();
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

/// People taken: the scripts hear it is over once they have landed.
fn pulled_people(time: Res<Time>, mut st: ResMut<PullAim>, mut vm: Option<ResMut<crate::kismet::Vm>>) {
    let dt = time.delta_secs();
    let mut over = 0;
    st.people.retain_mut(|t| {
        *t -= dt;
        if *t <= 0.0 {
            over += 1;
            false
        } else {
            true
        }
    });
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
    fn a_person_is_flung_toward_him_to_stop_short() {
        let v = fling(Vec3::new(10.0, 0.0, 0.0), Vec3::ZERO, 2.0);
        assert!(v.x < -4.0 && v.y > 0.0, "{v}");
        let near = fling(Vec3::new(2.5, 0.0, 0.0), Vec3::ZERO, 2.0);
        assert!(near.x >= -4.0 - 1e-3, "{near}");
    }
}
