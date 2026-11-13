//! The server half: it counts the combo and decides every roll.
//!
//! A combo: the game calls `attack_entity` when a player's left click lands on
//! an entity (in reach, before the hit). The server reads the player's own
//! held item, advances their combo for that weapon, and remembers which entity
//! this step belongs to. The hit itself is then scaled by the step's
//! multiplier in `living_incoming_damage_v2`, the hook that hears a player's
//! melee, sweep and spear hits on mobs just before the damage is dealt, so
//! armour, enchantments and the game's own attack cooldown keep their part.
//!
//! A roll: the client asks with a direction; the server reads the player's
//! state and cooldown, opens a short window in which attacks miss them, and
//! answers. Only then does the client push itself (the game's own prediction
//! path, `Host::push_player`), as a client's movement is its own in vanilla.
//!
//! What the real API does not give a trait mod, and this mod therefore does
//! not do: listing the entities around a player, dealing its own damage,
//! reading attack attributes, food, vehicles or water. See the README.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use horizon_client::mod_api::{
    AttackEntityEvent, Flow, Host, LivingIncomingDamageEvent, LivingIncomingDamageV2Event, NetFromClientEvent,
    NativeServerPlayer, ServerTickEvent, ServerTickPhase,
};

use crate::core::{self, Combo, Deny, RollCheck, tuning::*};
use crate::net;
use crate::weapons;

/// The combo step a player's current attack uses.
#[derive(Clone, Copy, Debug)]
struct Step {
    attack: weapons::Attack,
    index: usize,
    len: usize,
}

#[derive(Default)]
struct PlayerState {
    entity_id: i32,
    combo: Combo,
    /// Tick of the last attack counted, for the click-spam guard.
    last_attack: Option<i64>,
    /// The step that tick's attack used, for a second hit in the same tick.
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
        let msg = net::ComboMsg { index: 0, len: 1 };
        let _ = host.net_send_to_player(uuid, net::COMBO, &net::encode_combo(msg));
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
        return None;
    }
    let profile = weapons::classify(&item)?;
    Some((item, profile))
}

fn begin_attack(st: &mut PlayerState, now: i64, item: &str, profile: &'static weapons::Profile) -> Step {
    let index = st.combo.advance(now, item, profile.attacks.len());
    st.last_attack = Some(now);
    Step {
        attack: profile.attacks[index],
        index,
        len: profile.attacks.len(),
    }
}

/// The game is about to land a player's own hit on `event.entity`: the combo
/// advances here, so that the damage hook below scales this very hit.
pub fn attack_entity(host: &mut dyn Host, event: &AttackEntityEvent<'_>) -> Flow {
    let Some(player) = host.acting_player() else { return Flow::Continue };
    if player.game_mode == 3 {
        return Flow::Continue;
    }
    let Some((item, profile)) = held_profile(host) else { return Flow::Continue };
    let now = now();
    let mut state = state();
    let st = state.players.entry(player.uuid).or_default();
    st.entity_id = player.entity_id;
    // Two attack packets in one tick are one swing (a client must not run
    // through the combo).
    if st.last_attack == Some(now) {
        if let Some(step) = st.last_step {
            st.direct = Some((event.entity, now, step));
        }
        return Flow::Continue;
    }
    let step = begin_attack(st, now, &item, profile);
    st.last_step = Some(step);
    st.direct = Some((event.entity, now, step));
    Flow::Continue
}

/// A player's melee, sweep or spear hit on a mob, before it lands: the hit the
/// player clicked takes the combo step's multiplier. Vanilla's own sword sweep
/// on the bystanders is left alone, since this mod cannot deal its own damage
/// to replace it.
pub fn living_incoming_damage_v2(host: &mut dyn Host, event: &mut LivingIncomingDamageV2Event<'_>) -> Flow {
    if event.causing == 0 || event.damage_type != "minecraft:player_attack" {
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
                p.direct
                    .filter(|(target, tick, _)| *target == event.entity && *tick == now)
                    .map(|(_, _, step)| (*uuid, step))
            })
    };
    let Some((uuid, step)) = found else { return Flow::Continue };
    let amount = event.amount * step.attack.damage;
    if amount.is_finite() && amount >= 0.0 {
        event.amount = amount;
    }
    let msg = net::ComboMsg {
        index: step.index as u8,
        len: step.len as u8,
    };
    let _ = host.net_send_to_player(uuid, net::COMBO, &net::encode_combo(msg));
    Flow::Continue
}

/// A hit on anything, before it lands: attacks miss a rolling player.
pub fn living_incoming_damage(event: &LivingIncomingDamageEvent<'_>) -> Flow {
    if event.kind != "minecraft:player" {
        return Flow::Continue;
    }
    let until = state().iframes.get(&event.entity).copied();
    if until.is_some_and(|until| now() <= until) && core::dodges(event.damage_type, event.causing, event.direct) {
        return Flow::Cancel;
    }
    Flow::Continue
}

pub fn net_from_client(host: &mut dyn Host, event: &NetFromClientEvent<'_>) {
    if event.channel != net::ROLL {
        return;
    }
    let Some((dx, dz)) = net::decode_roll(event.data) else { return };
    roll(host, event.player, dx, dz);
}

/// The roll's answer: granted (the client pushes itself and the server misses
/// the player for a few ticks) or refused with the reason and the wait left.
fn roll(host: &mut dyn Host, uuid: u128, dx: f32, dz: f32) {
    let Some(player) = server_player(host, uuid) else { return };
    let now = now();
    let check = RollCheck {
        alive: player.health > 0.0,
        spectator: player.game_mode == 3,
        on_ground: player.on_ground,
        flying: player.flying,
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
            Err(deny) => Err((deny, (st.roll_ready_at - now).clamp(0, i64::from(u16::MAX)) as u16)),
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
            let _ = host.net_send_to_player(uuid, net::ROLL_NO, &net::encode_roll_no(deny as u8, remaining));
        }
    }
}

fn server_player(host: &dyn Host, uuid: u128) -> Option<NativeServerPlayer> {
    host.server_players().into_iter().find(|p| p.uuid == uuid)
}
