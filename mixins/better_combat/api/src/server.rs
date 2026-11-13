//! The server half: it decides every hit and every roll.
//!
//! A swing: the client only says "I swung at something that is not a block"
//! (`better_combat:attack`, no payload). The server reads the player's own
//! position, look, held item and attributes, advances their combo, and hurts
//! what stands in the swing's arc through the game's damage path
//! (`Host::hurt_entity`: armour, invulnerability time, knockback, death, and
//! the other mods' damage hooks). The target the player clicked on is hurt by
//! the game itself; its damage is scaled by the combo step.
//!
//! A roll: the client asks with a direction; the server checks the player's
//! state and cooldown, opens a short window in which attacks miss them, and
//! answers. Only then does the client push itself.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use horizon_client::mod_api::{
    AttackEntityEvent, Flow, Host, LivingIncomingDamageEvent, LivingIncomingDamageV2Event, NativeServerPlayer,
    NetFromClientEvent, ParticleOptions, ParticleType, PlayerArmSwingEvent, ServerTickEvent, ServerTickPhase,
};

use crate::core::{self, Combo, Deny, RollCheck, tuning::*};
use crate::net;
use crate::weapons::{self, Attack, Category};

/// `SoundSource.PLAYERS`.
const SOUND_PLAYERS: usize = 7;

/// The combo step a player's current attack uses.
#[derive(Clone, Copy, Debug)]
struct Step {
    attack: Attack,
    index: usize,
    len: usize,
    range_bonus: f64,
    category: Category,
    strength: f32,
}

#[derive(Default)]
struct PlayerState {
    entity_id: i32,
    combo: Combo,
    /// Tick of the last attack counted, for the strength of the next.
    last_attack: Option<i64>,
    /// The entity the game's own hit landed on, the tick it did, and the
    /// combo step that hit used: the attack request that follows belongs to
    /// the same swing.
    direct: Option<(i32, i64, Step)>,
    /// An attack request waits for the player's next tick.
    sweep_pending: bool,
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
/// Set while this mod deals its own swing damage, so its damage hook leaves
/// those hits alone.
static OWN_SWING: AtomicBool = AtomicBool::new(false);

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

/// `BETTER_COMBAT_SWING_ATTACKS=1` on the server: every arm swing counts as
/// an attack request, for clients without this mod's client half (and the
/// headless test bots). Off by default because a swing is also sent while
/// mining and placing.
fn swings_attack() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("BETTER_COMBAT_SWING_ATTACKS").is_ok_and(|v| v == "1"))
}

pub fn clear() {
    let mut st = state();
    st.players.clear();
    st.iframes.clear();
}

pub fn server_tick(event: &ServerTickEvent<'_>) {
    if !matches!(event.phase, ServerTickPhase::ServerTickStart { .. }) {
        return;
    }
    let now = TICK.fetch_add(1, Ordering::Relaxed) + 1;
    // Once in a while, forget the windows that closed.
    if now % 200 == 0 {
        state().iframes.retain(|_, until| *until >= now);
    }
}

pub fn player_logged_out(player: u128) {
    let mut st = state();
    if let Some(p) = st.players.remove(&player) {
        st.iframes.remove(&p.entity_id);
    }
}

/// The held item's id (empty for an empty hand) and its profile.
fn held_profile(host: &mut dyn Host, uuid: u128) -> (String, Option<&'static weapons::Profile>) {
    let stack = host.held_stack(uuid).filter(|s| s.count > 0);
    let item = stack.as_ref().map(|s| s.item.to_string()).unwrap_or_default();
    let profile = weapons::classify(&item, |tag| {
        stack.as_ref().is_some_and(|s| host.server_item_in_tag(s, tag).unwrap_or(false))
    });
    (item, profile)
}

/// Ticks a full charge of the player's weapon takes.
fn cooldown_ticks(host: &mut dyn Host, uuid: u128) -> f32 {
    let speed = host.attribute(uuid, "minecraft:attack_speed").map_or(4.0, |(_, v)| v);
    if speed.is_finite() && speed > 0.0 { (20.0 / speed) as f32 } else { 5.0 }
}

/// Count an attack of `player` at `now` and return its combo step.
fn begin_attack(
    st: &mut PlayerState,
    now: i64,
    item: &str,
    profile: &'static weapons::Profile,
    cooldown: f32,
) -> Step {
    let strength = st.last_attack.map_or(1.0, |t| core::strength(now - t, cooldown));
    let index = st.combo.advance(now, item, profile.attacks.len());
    st.last_attack = Some(now);
    Step {
        attack: profile.attacks[index],
        index,
        len: profile.attacks.len(),
        range_bonus: profile.range_bonus,
        category: profile.category,
        strength,
    }
}

/// The game is about to land a player's own hit on `event.entity`: the
/// combo advances here, so that the damage hook below scales this very hit.
pub fn attack_entity(host: &mut dyn Host, event: &AttackEntityEvent<'_>) {
    let Some(player) = host.acting_player() else { return };
    if player.game_mode == 3 {
        return;
    }
    let (item, Some(profile)) = held_profile(host, player.uuid) else { return };
    let cooldown = cooldown_ticks(host, player.uuid);
    let now = now();
    let mut state = state();
    let st = state.players.entry(player.uuid).or_default();
    st.entity_id = player.entity_id;
    // Two attack packets in one tick are one swing (a multi-target client
    // must not run through the combo).
    if st.direct.is_some_and(|(_, t, _)| t == now) {
        return;
    }
    let step = begin_attack(st, now, &item, profile, cooldown);
    st.direct = Some((event.entity, now, step));
}

/// A player's melee on a mob, before it lands. The clicked target takes the
/// combo step's multiplier; the game's own sword sweep on the others is
/// dropped, since the swing's arc replaces it.
pub fn living_incoming_damage_v2(event: &mut LivingIncomingDamageV2Event<'_>) -> Flow {
    if OWN_SWING.load(Ordering::Relaxed) || event.causing == 0 {
        return Flow::Continue;
    }
    if event.damage_type != "minecraft:player_attack" {
        return Flow::Continue;
    }
    let now = now();
    let state = state();
    let Some(st) = state.players.values().find(|p| p.entity_id == event.causing) else {
        return Flow::Continue;
    };
    let Some((target, tick, step)) = st.direct else { return Flow::Continue };
    if tick != now {
        return Flow::Continue;
    }
    if event.entity == target {
        event.amount *= step.attack.damage;
        Flow::Continue
    } else if step.category == Category::Sword {
        // Vanilla's sweep (swords only) on a bystander.
        Flow::Cancel
    } else {
        Flow::Continue
    }
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
    match event.channel {
        net::ATTACK => {
            if net::decode_attack(event.data).is_some() {
                state().players.entry(event.player).or_default().sweep_pending = true;
            }
        }
        net::ROLL => {
            if let Some((dx, dz)) = net::decode_roll(event.data) {
                roll(host, event.player, dx, dz);
            }
        }
        _ => {}
    }
}

pub fn player_arm_swing(event: &PlayerArmSwingEvent<'_>) {
    if swings_attack() {
        state().players.entry(event.player).or_default().sweep_pending = true;
    }
}

/// The end of a player's own tick, with their level in place: a pending
/// attack request is carried out here.
pub fn player_tick(host: &mut dyn Host) {
    // Cheap first: most ticks nothing waits.
    if !state().players.values().any(|p| p.sweep_pending) {
        return;
    }
    let Some(player) = host.acting_player() else { return };
    let pending = state()
        .players
        .get_mut(&player.uuid)
        .is_some_and(|st| std::mem::take(&mut st.sweep_pending));
    if pending {
        swing(host, &player);
    }
}

/// Kinds a swing never hurts, whatever `Host::living` says of them.
fn spared(kind: &str) -> bool {
    matches!(kind, "minecraft:armor_stand" | "minecraft:item" | "minecraft:experience_orb")
        || (!PVP_SWEEP && kind == "minecraft:player")
}

fn swing(host: &mut dyn Host, player: &NativeServerPlayer) {
    if player.game_mode == 3 || player.health <= 0.0 || player.entity_id <= 0 {
        return;
    }
    let (item, Some(profile)) = held_profile(host, player.uuid) else { return };
    let cooldown = cooldown_ticks(host, player.uuid);
    let now = now();
    // The step: the one the game's direct hit already used, else a new one.
    let (step, direct) = {
        let mut state = state();
        let st = state.players.entry(player.uuid).or_default();
        st.entity_id = player.entity_id;
        match st.direct.take() {
            Some((target, tick, step)) if now - tick <= DIRECT_PAIR_TICKS => (step, Some(target)),
            _ => {
                if st.last_attack.is_some_and(|t| now - t < MIN_ATTACK_INTERVAL) {
                    return;
                }
                (begin_attack(st, now, &item, profile, cooldown), None)
            }
        }
    };
    let me = player.entity_id as u32;
    let attack_damage = host.attribute(player.uuid, "minecraft:attack_damage").map_or(1.0, |(_, v)| v as f32);
    let amount = core::swing_damage(attack_damage, step.attack.damage, step.strength);
    let reach = host
        .attribute(player.uuid, "minecraft:entity_interaction_range")
        .map_or(3.0, |(_, v)| v)
        .clamp(0.0, 16.0);
    let range = (reach + step.range_bonus).max(1.0);
    let eye = host
        .entity(me)
        .and_then(|e| host.entity_eye_position(e))
        .unwrap_or([player.position[0], player.position[1] + EYE_HEIGHT, player.position[2]]);

    // Everything alive in the arc, nearest first.
    let mut targets: Vec<(f64, u32)> = Vec::new();
    for (id, kind, pos) in host.level_entities(&player.dimension) {
        if id == me || direct.is_some_and(|d| d as u32 == id) || spared(&kind) {
            continue;
        }
        // A cheap cut before the handles: nothing this far is in reach.
        let far = range + 8.0;
        if (0..3).any(|i| (pos[i] - eye[i]).abs() > far) {
            continue;
        }
        let Some(entity) = host.entity(id) else { continue };
        if host.living(entity).is_none() {
            continue;
        }
        let Some(bbox) = host.entity_bounding_box(entity) else { continue };
        let Some(distance) = core::swing_reaches(eye, player.yaw, player.pitch, bbox, range, step.attack.arc)
        else {
            continue;
        };
        // Somebody's pet (a tamed wolf, cat, horse...) is spared.
        if host.entity_nbt(entity).is_some_and(|n| n.contains_key(&"Owner".into())) {
            continue;
        }
        if !host.has_line_of_sight(me, id) {
            continue;
        }
        targets.push((distance, id));
    }
    targets.sort_by(|a, b| a.0.total_cmp(&b.0));
    targets.truncate(MAX_TARGETS);

    let mut hits = 0u8;
    OWN_SWING.store(true, Ordering::Relaxed);
    for (_, id) in &targets {
        if host.hurt_entity(&player.dimension, *id, amount, "minecraft:player_attack", Some(me)) {
            hits = hits.saturating_add(1);
        }
    }
    OWN_SWING.store(false, Ordering::Relaxed);

    // What the others see and hear of a swing that cut through something.
    if hits > 0
        && let Some(level) = host.level(&player.dimension)
    {
        let view = core::view_vector(player.yaw, 0.0);
        let at = [eye[0] + view[0] * 1.2, eye[1] - 0.5, eye[2] + view[2] * 1.2];
        let pitch = if step.attack.arc < 45.0 { 1.3 } else { 1.0 };
        host.level_play_sound(level, "minecraft:entity.player.attack.sweep", at, SOUND_PLAYERS, 0.8, pitch);
        let particle = ParticleOptions::simple(ParticleType::SweepAttack);
        host.level_send_particles(level, &particle, None, at, [0.0; 3], 0.0, 1);
    }
    let msg = net::ComboMsg { index: step.index as u8, len: step.len as u8, hits };
    let _ = host.net_send_to_player(player.uuid, net::COMBO, &net::encode_combo(msg));
}

fn roll(host: &mut dyn Host, uuid: u128, dx: f32, dz: f32) {
    let Some(player) = host.server_players().into_iter().find(|p| p.uuid == uuid) else { return };
    let now = now();
    let me = player.entity_id.max(0) as u32;
    let food = host.player_food(uuid);
    let check = RollCheck {
        alive: player.health > 0.0,
        spectator: player.game_mode == 3,
        creative: player.game_mode == 1,
        on_ground: player.on_ground,
        flying: player.flying,
        riding: host.vehicle_of(me).is_some(),
        in_water: host.entity(me).and_then(|e| host.entity_is_in_water(e)).unwrap_or(false),
        food: food.map_or(20, |f| f.0),
    };
    let verdict = {
        let mut state = state();
        let st = state.players.entry(uuid).or_default();
        st.entity_id = player.entity_id;
        let verdict = core::check_roll(now, st.roll_ready_at, &check);
        match verdict {
            Ok(()) => {
                st.roll_ready_at = now + ROLL_COOLDOWN;
                state.iframes.insert(player.entity_id, now + ROLL_IFRAMES);
                Ok(())
            }
            Err(deny) => Err((deny, (st.roll_ready_at - now).clamp(0, i64::from(u16::MAX)) as u16)),
        }
    };
    match verdict {
        Ok(()) => {
            if !check.creative
                && let Some((level, saturation, exhaustion)) = food
            {
                let food = Some((level, saturation, exhaustion + ROLL_EXHAUSTION));
                host.set_player_status(uuid, None, food, None);
            }
            if let Some(level) = host.level(&player.dimension) {
                host.level_play_sound(
                    level,
                    "minecraft:item.armor.equip_leather",
                    player.position,
                    SOUND_PLAYERS,
                    0.9,
                    0.7,
                );
            }
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
