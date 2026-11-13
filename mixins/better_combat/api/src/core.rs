//! The rules, free of any game type so they are tested on their own: the
//! combo counter, the swing's arc, attack strength, and the roll's checks.

/// Tuning. All times are server ticks (20 a second).
pub mod tuning {
    /// A combo starts over after this long without an attack.
    pub const COMBO_RESET_TICKS: i64 = 25;
    /// Attack requests closer together than this are dropped (click spam).
    pub const MIN_ATTACK_INTERVAL: i64 = 3;
    /// A direct hit and the attack request that goes with it arrive at most
    /// this far apart (the request is heard the tick after it arrives).
    pub const DIRECT_PAIR_TICKS: i64 = 4;
    /// At most this many extra targets per swing.
    pub const MAX_TARGETS: usize = 8;
    /// Whether a swing's arc hurts other players.
    pub const PVP_SWEEP: bool = false;
    /// Eye height of a standing player.
    pub const EYE_HEIGHT: f64 = 1.62;

    /// Ticks between two rolls.
    pub const ROLL_COOLDOWN: i64 = 80;
    /// Ticks after a roll starts during which attacks miss the player.
    pub const ROLL_IFRAMES: i64 = 8;
    /// Ticks the client keeps pushing (and ignores the movement keys).
    pub const ROLL_TICKS: u8 = 6;
    /// First push, blocks a tick; the following ticks push a quarter of it.
    pub const ROLL_SPEED: f32 = 0.6;
    /// Food exhaustion a roll costs (a jump while sprinting costs 0.2).
    pub const ROLL_EXHAUSTION: f32 = 0.6;
    /// A survival player needs more food than this to roll (as for sprinting).
    pub const ROLL_MIN_FOOD: i32 = 6;
}
use tuning::*;

/// Where a player is in their weapon's combo.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Combo {
    index: usize,
    last_tick: Option<i64>,
    weapon: String,
}

impl Combo {
    /// Count an attack at tick `now` with `weapon`, whose combo has `len`
    /// attacks; returns the index of the attack to use. The combo starts
    /// over after a pause or when the weapon changed.
    pub fn advance(&mut self, now: i64, weapon: &str, len: usize) -> usize {
        let len = len.max(1);
        let chained = self
            .last_tick
            .is_some_and(|t| now >= t && now - t <= COMBO_RESET_TICKS)
            && self.weapon == weapon;
        self.index = if chained { (self.index + 1) % len } else { 0 };
        self.last_tick = Some(now);
        if self.weapon != weapon {
            self.weapon = weapon.to_owned();
        }
        self.index
    }
}

/// Vanilla's attack strength: how charged a swing is `ticks_since` ticks
/// after the last one, for a weapon whose full charge takes
/// `cooldown_ticks` (20 / attack speed). 0 to 1.
pub fn strength(ticks_since: i64, cooldown_ticks: f32) -> f32 {
    if !cooldown_ticks.is_finite() || cooldown_ticks <= 0.0 {
        return 1.0;
    }
    ((ticks_since.max(0) as f32 + 0.5) / cooldown_ticks).clamp(0.0, 1.0)
}

/// Vanilla's damage scale for a strength: 20 % uncharged, 100 % charged.
pub fn damage_scale(strength: f32) -> f32 {
    let s = strength.clamp(0.0, 1.0);
    0.2 + 0.8 * s * s
}

/// The damage a swing deals to each target.
pub fn swing_damage(attack_damage: f32, multiplier: f32, strength: f32) -> f32 {
    let base = if attack_damage.is_finite() { attack_damage.max(1.0) } else { 1.0 };
    base * multiplier * damage_scale(strength)
}

/// The unit vector a player looks along (`Entity.calculateViewVector`):
/// yaw 0 faces south (+z), yaw 90 west (-x); pitch 90 looks down.
pub fn view_vector(yaw: f32, pitch: f32) -> [f64; 3] {
    let (yaw, pitch) = (f64::from(yaw).to_radians(), f64::from(pitch).to_radians());
    [-yaw.sin() * pitch.cos(), -pitch.sin(), yaw.cos() * pitch.cos()]
}

fn angle_diff(a: f64, b: f64) -> f64 {
    let mut d = (a - b) % std::f64::consts::TAU;
    if d > std::f64::consts::PI {
        d -= std::f64::consts::TAU;
    } else if d < -std::f64::consts::PI {
        d += std::f64::consts::TAU;
    }
    d.abs()
}

/// Whether a swing from `eye` along (`yaw`, `pitch`) reaches the box `bbox`
/// (`[min, max]`): the box's nearest point is within `range` blocks, the box
/// overlaps the horizontal arc `arc` degrees wide, and it is not far above
/// or below where the player looks. Returns the distance to the box.
pub fn swing_reaches(
    eye: [f64; 3],
    yaw: f32,
    pitch: f32,
    bbox: [[f64; 3]; 2],
    range: f64,
    arc: f32,
) -> Option<f64> {
    let [min, max] = bbox;
    if !(0..3).all(|i| min[i].is_finite() && max[i].is_finite() && min[i] <= max[i]) {
        return None;
    }
    let near: [f64; 3] = std::array::from_fn(|i| eye[i].clamp(min[i], max[i]));
    let d: [f64; 3] = std::array::from_fn(|i| near[i] - eye[i]);
    let dist = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    if dist > range {
        return None;
    }
    // Standing in the box: hit.
    if dist < 0.25 {
        return Some(dist);
    }
    let center = [(min[0] + max[0]) / 2.0, (min[2] + max[2]) / 2.0];
    let (cx, cz) = (center[0] - eye[0], center[1] - eye[2]);
    let horizontal = (cx * cx + cz * cz).sqrt();
    let half_width = ((max[0] - min[0]).max(max[2] - min[2])) / 2.0;
    let pitch_abs = f64::from(pitch).abs();
    // Looking straight up or down there is no horizontal direction: only the
    // range and the vertical test count.
    if pitch_abs < 80.0 && horizontal > half_width {
        let look = f64::from(yaw).to_radians();
        // atan2 of the view vector's (x, z): the same convention for both.
        let look_angle = (-look.sin()).atan2(look.cos());
        let to_target = cx.atan2(cz);
        // The box is this wide seen from the eye.
        let subtended = (half_width / horizontal).atan();
        let half_arc = f64::from(arc.clamp(0.0, 360.0)).to_radians() / 2.0;
        if angle_diff(look_angle, to_target) > half_arc + subtended {
            return None;
        }
    }
    // Vertically: the look's elevation must come within a margin of the box.
    let run = horizontal.max(0.05);
    let (low, high) = (((min[1] - eye[1]) / run).atan(), ((max[1] - eye[1]) / run).atan());
    let look_elev = -f64::from(pitch).to_radians();
    let margin = f64::from((arc / 4.0).clamp(20.0, 45.0)).to_radians();
    if look_elev < low - margin || look_elev > high + margin {
        return None;
    }
    Some(dist)
}

/// The horizontal unit direction of a roll: what the client asked for when
/// it is a usable vector, else where the player faces.
pub fn roll_direction(dx: f32, dz: f32, yaw: f32) -> [f32; 2] {
    let len = (dx * dx + dz * dz).sqrt();
    if dx.is_finite() && dz.is_finite() && len.is_finite() && len > 1.0e-3 {
        return [dx / len, dz / len];
    }
    let v = view_vector(yaw, 0.0);
    [v[0] as f32, v[2] as f32]
}

/// The direction the movement keys ask for, in world space: `forward` and
/// `strafe` as the game reads them (positive strafe is left), `yaw` the
/// player's. Zero when no key is held.
pub fn input_direction(forward: f32, strafe: f32, yaw: f32) -> [f32; 2] {
    let yaw = yaw.to_radians();
    let (fx, fz) = (-yaw.sin(), yaw.cos());
    let (lx, lz) = (yaw.cos(), yaw.sin());
    [fx * forward + lx * strafe, fz * forward + lz * strafe]
}

/// What the server knows of a player who asks to roll.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RollCheck {
    pub alive: bool,
    pub spectator: bool,
    pub creative: bool,
    pub on_ground: bool,
    pub flying: bool,
    pub riding: bool,
    pub in_water: bool,
    pub food: i32,
}

/// Why a roll was refused; the number goes over the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Deny {
    Cooldown = 1,
    NotOnGround = 2,
    Riding = 3,
    InWater = 4,
    Hungry = 5,
    State = 6,
}

/// Whether a roll may start at tick `now` when the last one allows the next
/// at `ready_at`.
pub fn check_roll(now: i64, ready_at: i64, c: &RollCheck) -> Result<(), Deny> {
    if !c.alive || c.spectator {
        return Err(Deny::State);
    }
    if now < ready_at {
        return Err(Deny::Cooldown);
    }
    if c.riding {
        return Err(Deny::Riding);
    }
    if c.in_water {
        return Err(Deny::InWater);
    }
    if c.flying || !c.on_ground {
        return Err(Deny::NotOnGround);
    }
    if !c.creative && c.food <= ROLL_MIN_FOOD {
        return Err(Deny::Hungry);
    }
    Ok(())
}

/// Whether a rolling player dodges a hit: attacks, projectiles and blasts
/// are dodged; what the world does to them (falling, fire, drowning, the
/// void, hunger, magic over time, `/kill`) is not.
pub fn dodges(damage_type: &str, causing: i32, direct: i32) -> bool {
    let kind = damage_type.strip_prefix("minecraft:").unwrap_or(damage_type);
    const NEVER: &[&str] = &[
        "out_of_world", "generic_kill", "outside_border", "starve", "drown", "in_wall", "cramming",
        "fall", "fly_into_wall", "stalagmite", "lava", "in_fire", "on_fire", "campfire", "hot_floor",
        "freeze", "wither", "magic", "dry_out", "cactus", "sweet_berry_bush",
    ];
    if NEVER.contains(&kind) {
        return false;
    }
    const ATTACKS: &[&str] = &[
        "mob_attack", "mob_attack_no_aggro", "player_attack", "arrow", "trident", "mob_projectile",
        "thrown", "fireball", "unattributed_fireball", "wither_skull", "explosion", "player_explosion",
        "sonic_boom", "sting", "spit", "wind_charge", "mace_smash", "spear", "indirect_magic",
        "falling_anvil", "falling_block", "falling_stalactite", "fireworks", "thorns",
    ];
    causing != 0 || direct != 0 || ATTACKS.contains(&kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combo_cycles_and_resets() {
        let mut c = Combo::default();
        assert_eq!(c.advance(100, "sword", 3), 0);
        assert_eq!(c.advance(110, "sword", 3), 1);
        assert_eq!(c.advance(120, "sword", 3), 2);
        assert_eq!(c.advance(130, "sword", 3), 0);
        assert_eq!(c.advance(140, "sword", 3), 1);
        // A pause starts over.
        assert_eq!(c.advance(140 + COMBO_RESET_TICKS + 1, "sword", 3), 0);
        assert_eq!(c.advance(140 + COMBO_RESET_TICKS + 5, "sword", 3), 1);
        // So does another weapon.
        assert_eq!(c.advance(140 + COMBO_RESET_TICKS + 6, "axe", 2), 0);
        assert_eq!(c.advance(140 + COMBO_RESET_TICKS + 7, "axe", 2), 1);
        assert_eq!(c.advance(140 + COMBO_RESET_TICKS + 8, "axe", 2), 0);
        // A zero length never divides by zero; a clock going back starts over.
        assert_eq!(c.advance(10, "axe", 0), 0);
    }

    #[test]
    fn strength_and_damage() {
        // A sword: attack speed 1.6, 12.5 ticks.
        assert!(strength(0, 12.5) < 0.05);
        assert_eq!(strength(13, 12.5), 1.0);
        assert_eq!(strength(-5, 12.5), strength(0, 12.5));
        assert_eq!(strength(3, 0.0), 1.0);
        assert!((damage_scale(0.0) - 0.2).abs() < 1e-6);
        assert!((damage_scale(1.0) - 1.0).abs() < 1e-6);
        assert!((swing_damage(7.0, 1.2, 1.0) - 8.4).abs() < 1e-5);
        // A broken attribute still hurts like a fist.
        assert!((swing_damage(f32::NAN, 1.0, 1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn view_vector_follows_minecraft() {
        let close = |a: [f64; 3], b: [f64; 3]| (0..3).all(|i| (a[i] - b[i]).abs() < 1e-6);
        assert!(close(view_vector(0.0, 0.0), [0.0, 0.0, 1.0]));
        assert!(close(view_vector(90.0, 0.0), [-1.0, 0.0, 0.0]));
        assert!(close(view_vector(-90.0, 0.0), [1.0, 0.0, 0.0]));
        assert!(close(view_vector(0.0, 90.0), [0.0, -1.0, 0.0]));
    }

    /// A zombie-sized box standing at (x, z), feet at y 0.
    fn mob(x: f64, z: f64) -> [[f64; 3]; 2] {
        [[x - 0.3, 0.0, z - 0.3], [x + 0.3, 1.95, z + 0.3]]
    }
    const EYE: [f64; 3] = [0.0, 1.62, 0.0];

    #[test]
    fn slash_hits_in_front_and_to_the_sides() {
        // Facing south (+z), a 120 degree slash, range 3.
        assert!(swing_reaches(EYE, 0.0, 0.0, mob(0.0, 2.0), 3.0, 120.0).is_some());
        assert!(swing_reaches(EYE, 0.0, 0.0, mob(1.5, 1.5), 3.0, 120.0).is_some());
        assert!(swing_reaches(EYE, 0.0, 0.0, mob(-1.5, 1.5), 3.0, 120.0).is_some());
        // Straight to the side is outside 60 degrees a side.
        assert!(swing_reaches(EYE, 0.0, 0.0, mob(2.5, 0.0), 3.0, 120.0).is_none());
        // Behind.
        assert!(swing_reaches(EYE, 0.0, 0.0, mob(0.0, -2.0), 3.0, 120.0).is_none());
        // Too far.
        assert!(swing_reaches(EYE, 0.0, 0.0, mob(0.0, 4.0), 3.0, 120.0).is_none());
    }

    #[test]
    fn stab_is_narrow() {
        assert!(swing_reaches(EYE, 0.0, 0.0, mob(0.0, 2.5), 3.0, 30.0).is_some());
        assert!(swing_reaches(EYE, 0.0, 0.0, mob(1.5, 1.5), 3.0, 30.0).is_none());
    }

    #[test]
    fn the_arc_turns_with_the_player() {
        // Facing west (-x).
        assert!(swing_reaches(EYE, 90.0, 0.0, mob(-2.0, 0.0), 3.0, 90.0).is_some());
        assert!(swing_reaches(EYE, 90.0, 0.0, mob(2.0, 0.0), 3.0, 90.0).is_none());
        // Yaw wraps: 450 is 90.
        assert!(swing_reaches(EYE, 450.0, 0.0, mob(-2.0, 0.0), 3.0, 90.0).is_some());
        assert!(swing_reaches(EYE, -270.0, 0.0, mob(-2.0, 0.0), 3.0, 90.0).is_some());
    }

    #[test]
    fn height_matters() {
        // A mob 6 blocks up is out of range; one on a ledge 2 up but looked
        // away from (looking down) is not hit.
        let ledge = [[-0.3, 3.2, 1.7], [0.3, 5.15, 2.3]];
        assert!(swing_reaches(EYE, 0.0, 60.0, ledge, 3.0, 120.0).is_none());
        assert!(swing_reaches(EYE, 0.0, -30.0, ledge, 3.0, 120.0).is_some());
        // Standing inside the box always hits.
        assert!(swing_reaches(EYE, 180.0, 0.0, mob(0.0, 0.1), 3.0, 30.0).is_some());
        // Garbage boxes are refused.
        assert!(swing_reaches(EYE, 0.0, 0.0, [[f64::NAN; 3]; 2], 3.0, 120.0).is_none());
    }

    #[test]
    fn roll_direction_sanitises() {
        let d = roll_direction(3.0, 4.0, 0.0);
        assert!((d[0] - 0.6).abs() < 1e-6 && (d[1] - 0.8).abs() < 1e-6);
        // No usable request: where the player faces (south, then west).
        for bad in [(0.0, 0.0), (f32::NAN, 1.0), (f32::INFINITY, 0.0)] {
            let d = roll_direction(bad.0, bad.1, 0.0);
            assert!(d[0].abs() < 1e-6 && (d[1] - 1.0).abs() < 1e-6);
        }
        let d = roll_direction(0.0, 0.0, 90.0);
        assert!((d[0] + 1.0).abs() < 1e-6 && d[1].abs() < 1e-6);
    }

    #[test]
    fn input_direction_follows_the_keys() {
        let close = |a: [f32; 2], b: [f32; 2]| (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6;
        // Facing south: forward is +z, left is +x (east).
        assert!(close(input_direction(1.0, 0.0, 0.0), [0.0, 1.0]));
        assert!(close(input_direction(0.0, 1.0, 0.0), [1.0, 0.0]));
        assert!(close(input_direction(-1.0, 0.0, 0.0), [0.0, -1.0]));
        // Facing west: forward is -x, left is +z (south).
        assert!(close(input_direction(1.0, 0.0, 90.0), [-1.0, 0.0]));
        assert!(close(input_direction(0.0, 1.0, 90.0), [0.0, 1.0]));
        assert!(close(input_direction(0.0, 0.0, 33.0), [0.0, 0.0]));
    }

    #[test]
    fn roll_checks() {
        let ok = RollCheck {
            alive: true,
            spectator: false,
            creative: false,
            on_ground: true,
            flying: false,
            riding: false,
            in_water: false,
            food: 20,
        };
        assert_eq!(check_roll(100, 100, &ok), Ok(()));
        assert_eq!(check_roll(99, 100, &ok), Err(Deny::Cooldown));
        assert_eq!(check_roll(100, 0, &RollCheck { on_ground: false, ..ok }), Err(Deny::NotOnGround));
        assert_eq!(check_roll(100, 0, &RollCheck { flying: true, ..ok }), Err(Deny::NotOnGround));
        assert_eq!(check_roll(100, 0, &RollCheck { riding: true, ..ok }), Err(Deny::Riding));
        assert_eq!(check_roll(100, 0, &RollCheck { in_water: true, ..ok }), Err(Deny::InWater));
        assert_eq!(check_roll(100, 0, &RollCheck { food: 6, ..ok }), Err(Deny::Hungry));
        assert_eq!(check_roll(100, 0, &RollCheck { food: 0, creative: true, ..ok }), Ok(()));
        assert_eq!(check_roll(100, 0, &RollCheck { alive: false, ..ok }), Err(Deny::State));
        assert_eq!(check_roll(100, 0, &RollCheck { spectator: true, ..ok }), Err(Deny::State));
    }

    #[test]
    fn what_a_roll_dodges() {
        assert!(dodges("minecraft:mob_attack", 12, 12));
        assert!(dodges("minecraft:arrow", 0, 0));
        assert!(dodges("minecraft:explosion", 0, 0));
        assert!(dodges("somemod:laser", 7, 0));
        assert!(!dodges("minecraft:fall", 0, 0));
        assert!(!dodges("minecraft:out_of_world", 0, 0));
        assert!(!dodges("minecraft:generic_kill", 5, 5));
        assert!(!dodges("minecraft:starve", 0, 0));
        assert!(!dodges("minecraft:lava", 0, 0));
        // Unknown and caused by nothing: not dodged.
        assert!(!dodges("somemod:radiation", 0, 0));
    }
}
