//! The rules, free of any game type so they are tested on their own: the
//! combo counter, the swing's arc, the roll's direction, the roll's checks and
//! which hits a roll dodges.
//!
//! The server applies the arc with two `Host` helpers that exist at the pinned
//! revision: `entities_in` lists what stands nearby and `hurt_entity` deals the
//! hit, so a sweep is the combo's multiplier on the clicked target plus a share
//! of it to every living thing inside the weapon's reach and arc. Attack
//! attributes, food and speed are still the game's, and arm/weapon animations
//! are not reachable from a trait mod.

/// Tuning. All times are server ticks (20 a second).
pub mod tuning {
    /// A combo starts over after this long without an attack.
    pub const COMBO_RESET_TICKS: i64 = 25;
    /// Attack requests closer together than this are dropped (click spam).
    pub const MIN_ATTACK_INTERVAL: i64 = 3;

    /// The share of the swing's damage the other targets in the arc take
    /// (vanilla's own sword sweep is left to the game and only scaled).
    pub const SWEEP_SHARE: f32 = 0.6;

    /// Ticks between two rolls.
    pub const ROLL_COOLDOWN: i64 = 80;
    /// Ticks after a roll starts during which attacks miss the player.
    pub const ROLL_IFRAMES: i64 = 8;
    /// Ticks the client keeps pushing (and ignores the movement keys).
    pub const ROLL_TICKS: u8 = 6;
    /// First push, in blocks a tick.
    pub const ROLL_SPEED: f32 = 0.6;
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

    /// Whether the counter has gone stale by `now` (a pause ends the combo).
    pub fn expired(&self, now: i64) -> bool {
        self.last_tick.is_some_and(|t| now - t > COMBO_RESET_TICKS)
    }

    /// Forget what was counted: the next attack starts at the combo's first
    /// step again.
    pub fn reset(&mut self) {
        self.index = 0;
        self.last_tick = None;
    }
}

/// The unit vector a player looks along (`Entity.calculateViewVector`):
/// yaw 0 faces south (+z), yaw 90 west (-x); pitch 90 looks down.
pub fn view_vector(yaw: f32, pitch: f32) -> [f64; 3] {
    let (yaw, pitch) = (f64::from(yaw).to_radians(), f64::from(pitch).to_radians());
    [
        -yaw.sin() * pitch.cos(),
        -pitch.sin(),
        yaw.cos() * pitch.cos(),
    ]
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

/// What the server knows of a player who asks to roll.
///
/// The player's health and game mode, whether they stand on the ground or fly,
/// and whether they swim or ride: a roll on the ground only, as upstream. Food
/// is readable but has no setter, so a roll costs no hunger; see the README.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RollCheck {
    pub alive: bool,
    pub spectator: bool,
    pub on_ground: bool,
    pub flying: bool,
    pub in_water: bool,
    pub in_vehicle: bool,
}

/// Why a roll was refused; the number goes over the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Deny {
    Cooldown = 1,
    NotOnGround = 2,
    State = 3,
    InWater = 4,
    InVehicle = 5,
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
    if c.in_water {
        return Err(Deny::InWater);
    }
    if c.in_vehicle {
        return Err(Deny::InVehicle);
    }
    if c.flying || !c.on_ground {
        return Err(Deny::NotOnGround);
    }
    Ok(())
}

/// Whether a target at `to` (a horizontal offset from the player) lies inside
/// the swing's arc, whose full width is `arc` degrees around `forward`.
pub fn in_arc(forward: [f64; 2], to: [f64; 2], arc: f32) -> bool {
    let flen = (forward[0] * forward[0] + forward[1] * forward[1]).sqrt();
    let tlen = (to[0] * to[0] + to[1] * to[1]).sqrt();
    if !(flen > 1.0e-6 && tlen > 1.0e-6) {
        return false;
    }
    let cos = (forward[0] * to[0] + forward[1] * to[1]) / (flen * tlen);
    let half = (f64::from(arc) * 0.5).clamp(0.0, 179.0).to_radians().cos();
    cos >= half
}

/// Whether a rolling player dodges a hit: attacks, projectiles and blasts
/// are dodged; what the world does to them (falling, fire, drowning, the
/// void, hunger, magic over time, `/kill`) is not.
pub fn dodges(damage_type: &str, causing: i32, direct: i32) -> bool {
    let kind = damage_type
        .strip_prefix("minecraft:")
        .unwrap_or(damage_type);
    const NEVER: &[&str] = &[
        "out_of_world",
        "generic_kill",
        "outside_border",
        "starve",
        "drown",
        "in_wall",
        "cramming",
        "fall",
        "fly_into_wall",
        "stalagmite",
        "lava",
        "in_fire",
        "on_fire",
        "campfire",
        "hot_floor",
        "freeze",
        "wither",
        "magic",
        "dry_out",
        "cactus",
        "sweet_berry_bush",
    ];
    if NEVER.contains(&kind) {
        return false;
    }
    const ATTACKS: &[&str] = &[
        "mob_attack",
        "mob_attack_no_aggro",
        "player_attack",
        "arrow",
        "trident",
        "mob_projectile",
        "thrown",
        "fireball",
        "unattributed_fireball",
        "wither_skull",
        "explosion",
        "player_explosion",
        "sonic_boom",
        "sting",
        "spit",
        "wind_charge",
        "mace_smash",
        "spear",
        "indirect_magic",
        "falling_anvil",
        "falling_block",
        "falling_stalactite",
        "fireworks",
        "thorns",
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
    fn combo_expiry_is_a_pause_only() {
        let mut c = Combo::default();
        assert!(!c.expired(0));
        c.advance(100, "sword", 3);
        assert!(!c.expired(100 + COMBO_RESET_TICKS));
        assert!(c.expired(100 + COMBO_RESET_TICKS + 1));
        // A reset is not an expiry: nothing is counted any more.
        c.reset();
        assert!(!c.expired(10_000));
        assert_eq!(c.advance(10_001, "sword", 3), 0);
    }

    #[test]
    fn view_vector_follows_minecraft() {
        let close = |a: [f64; 3], b: [f64; 3]| (0..3).all(|i| (a[i] - b[i]).abs() < 1e-6);
        assert!(close(view_vector(0.0, 0.0), [0.0, 0.0, 1.0]));
        assert!(close(view_vector(90.0, 0.0), [-1.0, 0.0, 0.0]));
        assert!(close(view_vector(-90.0, 0.0), [1.0, 0.0, 0.0]));
        assert!(close(view_vector(0.0, 90.0), [0.0, -1.0, 0.0]));
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
    fn roll_checks() {
        let ok = RollCheck {
            alive: true,
            spectator: false,
            on_ground: true,
            flying: false,
            in_water: false,
            in_vehicle: false,
        };
        assert_eq!(check_roll(100, 100, &ok), Ok(()));
        assert_eq!(check_roll(101, 100, &ok), Ok(()));
        assert_eq!(check_roll(99, 100, &ok), Err(Deny::Cooldown));
        assert_eq!(
            check_roll(
                100,
                0,
                &RollCheck {
                    on_ground: false,
                    ..ok
                }
            ),
            Err(Deny::NotOnGround)
        );
        assert_eq!(
            check_roll(100, 0, &RollCheck { flying: true, ..ok }),
            Err(Deny::NotOnGround)
        );
        assert_eq!(
            check_roll(
                100,
                0,
                &RollCheck {
                    in_water: true,
                    ..ok
                }
            ),
            Err(Deny::InWater)
        );
        assert_eq!(
            check_roll(
                100,
                0,
                &RollCheck {
                    in_vehicle: true,
                    ..ok
                }
            ),
            Err(Deny::InVehicle)
        );
        assert_eq!(
            check_roll(100, 0, &RollCheck { alive: false, ..ok }),
            Err(Deny::State)
        );
        assert_eq!(
            check_roll(
                100,
                0,
                &RollCheck {
                    spectator: true,
                    ..ok
                }
            ),
            Err(Deny::State)
        );
    }

    #[test]
    fn the_arc_follows_the_weapon() {
        // Facing south (+z): a target south is inside, one north is not.
        assert!(in_arc([0.0, 1.0], [0.2, 2.0], 120.0));
        assert!(!in_arc([0.0, 1.0], [0.0, -1.0], 120.0));
        // A 120 degree swing takes 60 degrees either side, not 70.
        let edge = |deg: f64| [deg.to_radians().sin(), deg.to_radians().cos()];
        assert!(in_arc([0.0, 1.0], edge(59.0), 120.0));
        assert!(!in_arc([0.0, 1.0], edge(61.0), 120.0));
        // A narrow stab spares what a wide slash would take.
        assert!(!in_arc([0.0, 1.0], [1.0, 1.0], 20.0));
        assert!(in_arc([0.0, 1.0], [1.0, 1.0], 180.0));
        // Degenerate vectors never match.
        assert!(!in_arc([0.0, 0.0], [0.0, 1.0], 120.0));
        assert!(!in_arc([0.0, 1.0], [0.0, 0.0], 120.0));
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
