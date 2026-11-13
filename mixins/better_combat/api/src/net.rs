//! The mod's messages (docs/native-mods.md, "Mod messages"). Every message
//! starts with a version byte and has one exact length; anything else is
//! dropped. What a client sends is a player's own input: the server reads a
//! request out of it, never a fact.

pub const VERSION: u8 = 1;

/// Client to server: "I want to roll this way" (a horizontal direction; zero
/// means "where I face").
pub const ROLL: &str = "better_combat:roll";
/// Server to client: the combo step that was just used.
pub const COMBO: &str = "better_combat:combo";
/// Server to client: the roll is granted; push yourself.
pub const ROLL_OK: &str = "better_combat:roll_ok";
/// Server to client: the roll is refused.
pub const ROLL_NO: &str = "better_combat:roll_no";

pub fn encode_roll(dx: f32, dz: f32) -> Vec<u8> {
    let mut out = vec![VERSION];
    out.extend_from_slice(&dx.to_le_bytes());
    out.extend_from_slice(&dz.to_le_bytes());
    out
}

fn f32_at(data: &[u8], at: usize) -> Option<f32> {
    Some(f32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

/// The direction asked for; not yet checked for NaN or length
/// (`core::roll_direction` does that).
pub fn decode_roll(data: &[u8]) -> Option<(f32, f32)> {
    if data.len() != 9 || data[0] != VERSION {
        return None;
    }
    Some((f32_at(data, 1)?, f32_at(data, 5)?))
}

/// A combo step: its index and the combo's length. `index` 0 with `len` 1 is
/// also what a combo that ran out reports, which hides the client's pips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComboMsg {
    pub index: u8,
    pub len: u8,
}

pub fn encode_combo(m: ComboMsg) -> Vec<u8> {
    vec![VERSION, m.index, m.len]
}

pub fn decode_combo(data: &[u8]) -> Option<ComboMsg> {
    match data {
        &[VERSION, index, len] if len > 0 && index < len => Some(ComboMsg { index, len }),
        _ => None,
    }
}

/// A granted roll: the unit direction, the first push in blocks a tick, how
/// many ticks it lasts and the ticks until the next one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RollOk {
    pub dir: [f32; 2],
    pub speed: f32,
    pub ticks: u8,
    pub cooldown: u16,
}

pub fn encode_roll_ok(m: RollOk) -> Vec<u8> {
    let mut out = vec![VERSION];
    out.extend_from_slice(&m.dir[0].to_le_bytes());
    out.extend_from_slice(&m.dir[1].to_le_bytes());
    out.extend_from_slice(&m.speed.to_le_bytes());
    out.push(m.ticks);
    out.extend_from_slice(&m.cooldown.to_le_bytes());
    out
}

/// A client checks what its server says too: a direction that is not a unit
/// vector or a speed out of bounds is dropped.
pub fn decode_roll_ok(data: &[u8]) -> Option<RollOk> {
    if data.len() != 16 || data[0] != VERSION {
        return None;
    }
    let (dx, dz, speed) = (f32_at(data, 1)?, f32_at(data, 5)?, f32_at(data, 9)?);
    let len = (dx * dx + dz * dz).sqrt();
    if !(len.is_finite() && (0.9..=1.1).contains(&len)) || !(speed.is_finite() && (0.0..=2.0).contains(&speed)) {
        return None;
    }
    let ticks = data[13];
    if ticks > 40 {
        return None;
    }
    Some(RollOk { dir: [dx, dz], speed, ticks, cooldown: u16_at(data, 14)? })
}

/// A refused roll: the reason (`core::Deny`) and the ticks left to wait.
pub fn encode_roll_no(reason: u8, remaining: u16) -> Vec<u8> {
    let mut out = vec![VERSION, reason];
    out.extend_from_slice(&remaining.to_le_bytes());
    out
}

pub fn decode_roll_no(data: &[u8]) -> Option<(u8, u16)> {
    if data.len() != 4 || data[0] != VERSION {
        return None;
    }
    Some((data[1], u16_at(data, 2)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_are_valid_names() {
        for ch in [ROLL, COMBO, ROLL_OK, ROLL_NO] {
            let (id, name) = ch.split_once(':').unwrap();
            assert_eq!(id, "better_combat");
            assert!(!name.is_empty() && name.len() <= 64);
            assert!(name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-/".contains(&b)));
        }
    }

    #[test]
    fn roll_round_trip_and_garbage() {
        assert_eq!(decode_roll(&encode_roll(0.5, -0.25)), Some((0.5, -0.25)));
        assert_eq!(decode_roll(&[]), None);
        assert_eq!(decode_roll(&[VERSION; 8]), None);
        assert_eq!(decode_roll(&[VERSION; 10]), None);
        let mut wrong = encode_roll(1.0, 0.0);
        wrong[0] = 9;
        assert_eq!(decode_roll(&wrong), None);
        // NaN comes through: the rules clean it.
        let nan = decode_roll(&encode_roll(f32::NAN, 0.0)).unwrap();
        assert!(nan.0.is_nan());
    }

    #[test]
    fn combo_round_trip_and_garbage() {
        let m = ComboMsg { index: 2, len: 3 };
        assert_eq!(decode_combo(&encode_combo(m)), Some(m));
        assert_eq!(decode_combo(&encode_combo(ComboMsg { index: 0, len: 1 })), Some(ComboMsg { index: 0, len: 1 }));
        assert_eq!(decode_combo(&[VERSION, 3, 3]), None);
        assert_eq!(decode_combo(&[VERSION, 0, 0]), None);
        assert_eq!(decode_combo(&[VERSION, 0]), None);
    }

    #[test]
    fn roll_ok_round_trip_and_garbage() {
        let m = RollOk { dir: [0.6, 0.8], speed: 0.6, ticks: 6, cooldown: 80 };
        assert_eq!(decode_roll_ok(&encode_roll_ok(m)), Some(m));
        // Not a unit direction, too fast, too long, NaN: dropped.
        assert_eq!(decode_roll_ok(&encode_roll_ok(RollOk { dir: [3.0, 4.0], ..m })), None);
        assert_eq!(decode_roll_ok(&encode_roll_ok(RollOk { speed: 9.0, ..m })), None);
        assert_eq!(decode_roll_ok(&encode_roll_ok(RollOk { ticks: 200, ..m })), None);
        assert_eq!(decode_roll_ok(&encode_roll_ok(RollOk { dir: [f32::NAN, 0.0], ..m })), None);
        assert_eq!(decode_roll_ok(&encode_roll_ok(m)[..15]), None);
    }

    #[test]
    fn roll_no_round_trip() {
        assert_eq!(decode_roll_no(&encode_roll_no(1, 37)), Some((1, 37)));
        assert_eq!(decode_roll_no(&[VERSION, 1, 0]), None);
    }
}
