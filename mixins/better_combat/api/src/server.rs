//! The server half: it counts the combo, sweeps the arc and decides every roll.
//!
//! A combo: the game calls `attack_entity` when a player's left click lands on
//! an entity (in reach, before the hit). The server reads the player's own
//! held item, advances their combo for that weapon, and remembers which entity
//! this step belongs to. The hit itself is then scaled by the step's
//! multiplier in `living_incoming_damage_v2`, the hook that hears a player's
//! melee, sweep and spear hits on mobs just before the damage is dealt, so
//! armour, enchantments and the game's own attack cooldown keep their part.
//!
//! The arc: after the clicked target is scaled, `Host::entities_in` lists what
//! stands around the player and every living thing inside the weapon's reach
//! and arc takes a share of the hit through `Host::hurt_entity` (an item in
//! the game's sword tag is left to vanilla's own sweep, which this mod only
//! scales). Arms are told with the game's sweep particle and sound.
//!
//! A roll: the client asks with a direction (the keys it holds); the server
//! reads the player's state and cooldown, opens a short window in which
//! attacks miss them, and answers. Only then does the client push itself (the
//! game's own prediction path, `Host::push_player`) for the granted ticks.
//!
//! What the real API still does not give a trait mod, and this mod therefore
//! does not do: reading attack attributes or setting a food level, and any
//! first-person swing animation. See the README.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use horizon_client::mod_api::{
    AttackEntityEvent, Flow, Host, LivingIncomingDamageEvent, LivingIncomingDamageV2Event,
    LivingIncomingDamageV3Event, NativeServerPlayer, NetFromClientEvent, ParticleOptions,
    ParticleType, ServerTickEvent, ServerTickPhase,
};

use crate::core::{self, Combo, Deny, RollCheck, tuning::*};
use crate::net;
use crate::weapons::{self, Category};

/// The combo step a player's current attack uses.
#[derive(Clone, Copy, Debug)]
struct Step {
    attack: weapons::Attack,
    index: usize,
    len: usize,
    reach: f32,
    /// The held stack is a sword: the game's own sweep already hits the
    /// bystanders, so this mod scales it instead of hitting them again.
    vanilla_sweep: bool,
}

#[derive(Default)]
struct PlayerState {
    entity_id: i32,
    /// Where the player was when the swing started, for the arc's box.
    dimension: String,
    yaw: f32,
    combo: Combo,
    /// Tick of the last attack counted, for the click-spam guard.
    last_attack: Option<i64>,
    /// The step that tick's attack used, for the sweep of the same swing.
    last_step: Option<Step>,
    /// The entity the game is about to hit with the step that hit it: the
    /// damage hook that follows belongs to the same swing.
    direct: Option<(i32, i64, Step)>,
    /// The next roll is allowed from this tick on.
    roll_ready_at: i64,
}

#[derive(Default)]
struct State {
    players: HashMap<u128, PlayerState>,
    /// Entity id of a rolling player -> the last tick attacks miss them.
    iframes: HashMap<i32, i64>,
}

static TICK: AtomicI64 = AtomicI64::new(0);

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE
        .get_or_init(|| Mutex::new(State::default()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn now() -> i64 {
    TICK.load(Ordering::Relaxed)
}

/// Whether a damage type is a player's own weapon hit.
fn player_hit(damage_type: &str) -> bool {
    damage_type
        .strip_prefix("minecraft:")
        .unwrap_or(damage_type)
        == "player_attack"
}

/// Forget everything: a server that stops keeps no player's combo.
pub fn clear() {
    let mut st = state();
    st.players.clear();
    st.iframes.clear();
}

pub fn server_tick(host: &mut dyn Host, event: &ServerTickEvent<'_>) {
    if !matches!(event.phase, ServerTickPhase::ServerTickStart { .. }) {
        return;
    }
    let now = TICK.fetch_add(1, Ordering::Relaxed) + 1;
    let mut idle: Vec<u128> = Vec::new();
    {
        let mut st = state();
        // Once in a while, forget the windows that closed.
        if now % 200 == 0 {
            st.iframes.retain(|_, until| *until >= now);
        }
        // A combo that ran out tells its player, so their pips go away.
        for (uuid, p) in st.players.iter_mut() {
            if p.combo.expired(now) {
                p.combo.reset();
                idle.push(*uuid);
            }
        }
    }
    for uuid in idle {
        let _ =
            host.net_send_to_player(uuid, net::COMBO, &net::encode_combo(net::ComboMsg::HIDDEN));
    }
}

pub fn player_logged_out(player: u128) {
    let mut st = state();
    if let Some(p) = st.players.remove(&player) {
        st.iframes.remove(&p.entity_id);
    }
}

/// The profile of the stack in hand, or None when the game keeps the item.
fn held_profile(host: &dyn Host) -> Option<(String, &'static weapons::Profile)> {
    let (item, count, _plain) = host.held()?;
    if count == 0 {
        // An empty hand is the fist's own short combo.
        return Some((String::new(), &weapons::FIST));
    }
    let profile = weapons::classify(&item)?;
    Some((item, profile))
}

/// Whether the game's own sword sweep will take the bystanders of this swing.
fn vanilla_sweep(host: &dyn Host, uuid: u128, profile: &'static weapons::Profile) -> bool {
    let fallback = profile.category == Category::Sword;
    match host.held_stack(uuid) {
        Some(stack) => host
            .server_item_in_tag(&stack, "minecraft:swords")
            .unwrap_or(fallback),
        None => fallback,
    }
}

fn begin_attack(
    st: &mut PlayerState,
    now: i64,
    item: &str,
    profile: &'static weapons::Profile,
) -> Step {
    let index = st.combo.advance(now, item, profile.attacks.len());
    st.last_attack = Some(now);
    Step {
        attack: profile.attacks[index],
        index,
        len: profile.attacks.len(),
        reach: profile.reach,
        vanilla_sweep: false,
    }
}

/// The game is about to land a player's own hit on `event.entity`: the combo
/// advances here, so that the damage hook below scales this very hit.
pub fn attack_entity(host: &mut dyn Host, event: &AttackEntityEvent<'_>) -> Flow {
    let Some(player) = host.acting_player() else {
        return Flow::Continue;
    };
    if player.game_mode == 3 {
        return Flow::Continue;
    }
    let Some((item, profile)) = held_profile(host) else {
        return Flow::Continue;
    };
    let sweeps = vanilla_sweep(host, player.uuid, profile);
    let now = now();
    let mut state = state();
    let st = state.players.entry(player.uuid).or_default();
    st.entity_id = player.entity_id;
    st.dimension.clone_from(&player.dimension);
    st.yaw = player.yaw;
    // Two attack packets in one tick are one swing (a client must not run
    // through the combo).
    if st.last_attack == Some(now) {
        if let Some(step) = st.last_step {
            st.direct = Some((event.entity, now, step));
        }
        return Flow::Continue;
    }
    let mut step = begin_attack(st, now, &item, profile);
    step.vanilla_sweep = sweeps;
    st.last_step = Some(step);
    st.direct = Some((event.entity, now, step));
    Flow::Continue
}

/// A player's weapon hit before it lands: the clicked target takes the combo
/// step's multiplier, and the swing's arc takes the other targets.
pub fn living_incoming_damage_v2(
    host: &mut dyn Host,
    event: &mut LivingIncomingDamageV2Event<'_>,
) -> Flow {
    if event.causing == 0 || !player_hit(event.damage_type) {
        return Flow::Continue;
    }
    let now = now();
    let found = {
        let state = state();
        state
            .players
            .iter()
            .find(|(_, p)| p.entity_id == event.causing)
            .and_then(|(uuid, p)| {
                let step = p.last_step.filter(|_| p.last_attack == Some(now))?;
                let primary = p
                    .direct
                    .is_some_and(|(target, tick, _)| target == event.entity && tick == now);
                // Vanilla's own sweep hits the bystanders of a sword swing:
                // it follows the combo's multiplier instead of being replaced.
                (primary || step.vanilla_sweep).then_some((*uuid, step, primary))
            })
    };
    let Some((uuid, step, primary)) = found else {
        return Flow::Continue;
    };
    let amount = event.amount * step.attack.damage;
    if amount.is_finite() && amount >= 0.0 {
        event.amount = amount;
    }
    if primary {
        let msg = net::ComboMsg {
            index: step.index as u8,
            len: step.len as u8,
        };
        let _ = host.net_send_to_player(uuid, net::COMBO, &net::encode_combo(msg));
        if !step.vanilla_sweep {
            sweep(
                host,
                uuid,
                event.causing,
                event.entity,
                &step,
                amount * SWEEP_SHARE,
            );
        }
    }
    Flow::Continue
}

/// Every other living thing inside the swing's reach and arc takes a share of
/// the hit. The clicked target and the player are left out; the game's own
/// sword sweep is for swords (`Step::vanilla_sweep`), so nothing is hit twice.
fn sweep(host: &mut dyn Host, uuid: u128, attacker: i32, target: i32, step: &Step, amount: f32) {
    if !(amount.is_finite() && amount > 0.0) {
        return;
    }
    let Ok(attacker_id) = u32::try_from(attacker) else {
        return;
    };
    let Ok(target_id) = u32::try_from(target) else {
        return;
    };
    let (dimension, yaw) = {
        let st = state();
        match st.players.get(&uuid) {
            Some(p) => (p.dimension.clone(), p.yaw),
            None => return,
        }
    };
    let Some(entity) = host.entity(attacker_id) else {
        return;
    };
    let Some(feet) = host.entity_position(entity) else {
        return;
    };
    let view = core::view_vector(yaw, 0.0);
    let forward = [view[0], view[2]];
    let reach = f64::from(step.reach);
    let near = [feet[0] - reach, feet[1] - 1.0, feet[2] - reach];
    let far = [feet[0] + reach, feet[1] + 2.0, feet[2] + reach];
    let ids = host.entities_in(&dimension, near, far);
    let mut hit_any = false;
    for id in ids {
        if id == attacker_id || id == target_id {
            continue;
        }
        let Some(victim) = host.entity(id) else {
            continue;
        };
        if host.living(victim).is_none() {
            continue;
        }
        // Another player is a duel, not a sweep (and `hurt_entity` may not
        // reach a player who is not this game's own); the v3 hook scales a
        // player's hit on a player on its own.
        if host.entity_kind(victim).as_deref() == Some("minecraft:player") {
            continue;
        }
        let Some(pos) = host.entity_position(victim) else {
            continue;
        };
        let (dx, dz) = (pos[0] - feet[0], pos[2] - feet[2]);
        if dx * dx + dz * dz > reach * reach {
            continue;
        }
        if !core::in_arc(forward, [dx, dz], step.attack.arc) {
            continue;
        }
        if !host.has_line_of_sight(attacker_id, id) {
            continue;
        }
        if host.hurt_entity(
            &dimension,
            id,
            amount,
            "minecraft:player_attack",
            Some(attacker_id),
        ) {
            hit_any = true;
        }
    }
    if hit_any {
        let at = [feet[0], feet[1] + 1.0, feet[2]];
        let _ = host.play_sound_to(
            uuid,
            "minecraft:entity.player.attack.sweep",
            "players",
            Some(at),
            0.7,
            1.2,
        );
        let arc = ParticleOptions::simple(ParticleType::SweepAttack);
        let _ = host.send_particles(&arc, None, at, [0.6, 0.2, 0.6], 0.0, 1);
    }
}

/// A player's weapon hit on another player: `living_incoming_damage_v2` only
/// hears mob victims, so the combo's multiplier is applied here instead.
pub fn living_incoming_damage_v3(event: &mut LivingIncomingDamageV3Event<'_>) -> Flow {
    if event.kind != "minecraft:player" || !player_hit(event.damage_type) {
        return Flow::Continue;
    }
    let Some(uuid) = event.attacker_uuid else {
        return Flow::Continue;
    };
    let Ok(victim) = i32::try_from(event.victim) else {
        return Flow::Continue;
    };
    let now = now();
    let factor = {
        let state = state();
        state.players.get(&uuid).and_then(|p| {
            p.direct
                .filter(|(target, tick, _)| *target == victim && *tick == now)
                .map(|(_, _, step)| step.attack.damage)
        })
    };
    if let Some(factor) = factor {
        let amount = event.amount * factor;
        if amount.is_finite() && amount >= 0.0 {
            event.amount = amount;
        }
    }
    Flow::Continue
}

/// A hit on anything, before it lands: attacks miss a rolling player.
pub fn living_incoming_damage(event: &LivingIncomingDamageEvent<'_>) -> Flow {
    if event.kind != "minecraft:player" {
        return Flow::Continue;
    }
    let until = state().iframes.get(&event.entity).copied();
    if until.is_some_and(|until| now() <= until)
        && core::dodges(event.damage_type, event.causing, event.direct)
    {
        return Flow::Cancel;
    }
    Flow::Continue
}

pub fn net_from_client(host: &mut dyn Host, event: &NetFromClientEvent<'_>) {
    if event.channel != net::ROLL {
        return;
    }
    let Some((dx, dz)) = net::decode_roll(event.data) else {
        return;
    };
    roll(host, event.player, dx, dz);
}

/// The roll's answer: granted (the client pushes itself and the server misses
/// the player for a few ticks) or refused with the reason and the wait left.
fn roll(host: &mut dyn Host, uuid: u128, dx: f32, dz: f32) {
    let Some(player) = server_player(host, uuid) else {
        return;
    };
    let entity = u32::try_from(player.entity_id).ok().filter(|id| *id != 0);
    let in_water = entity
        .and_then(|id| host.entity(id))
        .and_then(|er| host.entity_is_in_water(er))
        .unwrap_or(false);
    let in_vehicle = entity.and_then(|id| host.vehicle_of(id)).is_some();
    let now = now();
    let check = RollCheck {
        alive: player.health > 0.0,
        spectator: player.game_mode == 3,
        on_ground: player.on_ground,
        flying: player.flying,
        in_water,
        in_vehicle,
    };
    let verdict = {
        let mut state = state();
        let st = state.players.entry(uuid).or_default();
        st.entity_id = player.entity_id;
        match core::check_roll(now, st.roll_ready_at, &check) {
            Ok(()) => {
                st.roll_ready_at = now + ROLL_COOLDOWN;
                if player.entity_id > 0 {
                    state.iframes.insert(player.entity_id, now + ROLL_IFRAMES);
                }
                Ok(())
            }
            Err(deny) => Err((
                deny,
                (st.roll_ready_at - now).clamp(0, i64::from(u16::MAX)) as u16,
            )),
        }
    };
    match verdict {
        Ok(()) => {
            let ok = net::RollOk {
                dir: core::roll_direction(dx, dz, player.yaw),
                speed: ROLL_SPEED,
                ticks: ROLL_TICKS,
                cooldown: ROLL_COOLDOWN as u16,
            };
            let _ = host.net_send_to_player(uuid, net::ROLL_OK, &net::encode_roll_ok(ok));
        }
        Err((deny, remaining)) => {
            let remaining = if deny == Deny::Cooldown { remaining } else { 0 };
            let _ = host.net_send_to_player(
                uuid,
                net::ROLL_NO,
                &net::encode_roll_no(deny as u8, remaining),
            );
        }
    }
}

fn server_player(host: &dyn Host, uuid: u128) -> Option<NativeServerPlayer> {
    host.server_players().into_iter().find(|p| p.uuid == uuid)
}
