//! The client half: the roll key and `/roll`, the server's answers, the push
//! the server granted, and the combo pips above the hotbar.
//!
//! The server owns every decision. This side only asks (with the direction the
//! movement keys point to), applies what was granted to its own player (the
//! game's own prediction path) while the grant lasts, and draws.

use std::sync::Mutex;

use horizon_client::mod_api::*;

use crate::core;
use crate::net;
use crate::{KEY_DEFAULT, KEY_NAME, MOD_ID};

/// What the last combo message said, for the pips.
#[derive(Default)]
pub struct Client {
    view: Mutex<View>,
}

#[derive(Default)]
struct View {
    /// The combo step in use and the combo's length; `len` 0 hides the pips.
    combo: Option<(u8, u8)>,
    /// The last refusal, drawn until its ticks run out.
    notice: Option<(&'static str, u8)>,
    /// The roll the server granted, still being pushed.
    roll: Option<Roll>,
}

#[derive(Clone, Copy)]
struct Roll {
    dir: [f32; 2],
    speed: f32,
    left: u8,
    ticks: u8,
}

/// `SoundSource.PLAYERS` is the server's; a local sound is centred on the
/// player who rolled.
const ROLL_SOUND: &str = "minecraft:entity.player.attack.sweep";
/// How long a refusal line stays up, in client ticks (two seconds).
const NOTICE_TICKS: u8 = 40;

impl Client {
    /// Options > Mods lists the mod and its key.
    pub fn mods(&self, mods: &mut Vec<NativeMod>) {
        mods.push(NativeMod {
            id: MOD_ID.to_owned(),
            title: "Combat Combos & Roll".to_owned(),
            description:
                "Weapons chain a short combo of heavier blows. Press the roll key (or type \
                          /roll) to dodge: attacks, arrows and blasts miss you for a moment."
                    .to_owned(),
            keys: vec![[
                KEY_NAME.to_owned(),
                "Dodge roll".to_owned(),
                KEY_DEFAULT.to_owned(),
            ]],
        });
    }

    /// `/roll`: the same request as the key.
    pub fn commands(&self, commands: &mut Vec<&'static str>) {
        commands.push("roll");
    }

    pub fn command(&self, host: &mut dyn Host, line: &str) -> bool {
        let line = line.trim();
        if line == "roll" || line == "roll forward" {
            self.ask(host);
            return true;
        }
        false
    }

    pub fn key(&self, host: &mut dyn Host, key: &str, pressed: bool) {
        if !pressed || !host.playing() {
            return;
        }
        if key == host.mod_key(MOD_ID, KEY_NAME, KEY_DEFAULT) {
            self.ask(host);
        }
    }

    /// The request: the direction the movement keys point to, relative to
    /// where the player looks. No key held asks for where they face.
    fn ask(&self, host: &mut dyn Host) {
        let (dx, dz) = match host.client_state() {
            Some(state) => movement(host, state.yaw),
            None => (0.0, 0.0),
        };
        let _ = host.net_send_to_server(net::ROLL, &net::encode_roll(dx, dz));
    }

    pub fn message(&self, host: &mut dyn Host, channel: &str, data: &[u8]) {
        match channel {
            net::COMBO => {
                if let Some(m) = net::decode_combo(data) {
                    self.view.lock().unwrap_or_else(|e| e.into_inner()).combo =
                        (m.len > 0).then_some((m.index, m.len));
                }
            }
            net::ROLL_OK => {
                let Some(ok) = net::decode_roll_ok(data) else {
                    return;
                };
                let mut view = self.view.lock().unwrap_or_else(|e| e.into_inner());
                view.notice = None;
                view.roll = Some(Roll {
                    dir: ok.dir,
                    speed: ok.speed,
                    left: ok.ticks,
                    ticks: ok.ticks,
                });
                drop(view);
                // The push itself follows in `tick`, as the client moves
                // itself (vanilla's prediction), for the granted ticks.
                let at = host
                    .client_state()
                    .and_then(|s| s.eye_position)
                    .unwrap_or([0.0; 3]);
                host.play_local_sound(ROLL_SOUND, at, 0.6, 1.6, 0);
            }
            net::ROLL_NO => {
                let Some((reason, _)) = net::decode_roll_no(data) else {
                    return;
                };
                let text = match reason {
                    1 => "Roll cooling down",
                    2 => "Cannot roll in the air",
                    4 => "Cannot roll in water",
                    5 => "Cannot roll while riding",
                    _ => "Cannot roll now",
                };
                self.view.lock().unwrap_or_else(|e| e.into_inner()).notice =
                    Some((text, NOTICE_TICKS));
            }
            _ => {}
        }
    }

    /// One client tick: fade the refusal line and keep the roll moving.
    pub fn tick(&self, host: &mut dyn Host) {
        let mut view = self.view.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((text, left)) = view.notice {
            view.notice = (left > 1).then_some((text, left - 1));
        }
        let Some(roll) = view.roll else { return };
        if roll.left == 0 || roll.ticks == 0 {
            view.roll = None;
            return;
        }
        // Each tick is a little weaker, so the roll lands rather than slides.
        let ease = 0.4 + 0.6 * f32::from(roll.left) / f32::from(roll.ticks);
        push(host, roll.dir, roll.speed * ease, roll.left, roll.ticks);
        view.roll = if roll.left > 1 {
            Some(Roll {
                left: roll.left - 1,
                ..roll
            })
        } else {
            None
        };
    }

    /// The pips above the hotbar: one per combo step, the current one gold.
    pub fn render(&self, host: &dyn Host, batch: &mut HudBatch) {
        let Some(state) = host.client_state() else {
            return;
        };
        if !host.playing() {
            return;
        }
        let [w, h] = state.gui_size;
        let view = self.view.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((notice, _)) = view.notice {
            let width = host.text_width(notice);
            host.text(
                batch,
                notice,
                [(w as f32 - width) / 2.0, h as f32 - 62.0],
                0xFF5555,
                true,
            );
            return;
        }
        let Some((index, len)) = view.combo else {
            return;
        };
        let (pip, gap) = (6.0f32, 3.0f32);
        let total = len as f32 * pip + (len as f32 - 1.0) * gap;
        let x0 = (w as f32 - total) / 2.0;
        let y = h as f32 - 62.0;
        for i in 0..len {
            let x = x0 + i as f32 * (pip + gap);
            let argb = if i == index { 0xFFFFD24A } else { 0x80404040 };
            host.fill(batch, [x, y, pip, pip], argb);
        }
    }
}

/// Push the local player along the granted direction for one tick.
fn push(host: &mut dyn Host, dir: [f32; 2], speed: f32, left: u8, ticks: u8) {
    let scale = if ticks == 0 {
        speed
    } else {
        // The first push is the strongest, then it eases out with `left`.
        speed * (0.4 + 0.6 * f32::from(left) / f32::from(ticks))
    };
    host.push_player([dir[0] * scale, 0.0, dir[1] * scale]);
}

/// The horizontal direction the movement keys ask for, in world axes.
fn movement(host: &dyn Host, yaw: f32) -> (f32, f32) {
    let axis = |on: bool| if on { 1.0 } else { 0.0 };
    let forward = axis(host.is_key_down(KeyCode::KeyW)) - axis(host.is_key_down(KeyCode::KeyS));
    let right = axis(host.is_key_down(KeyCode::KeyD)) - axis(host.is_key_down(KeyCode::KeyA));
    if forward == 0.0 && right == 0.0 {
        return (0.0, 0.0);
    }
    let view = core::view_vector(yaw, 0.0);
    let (fx, fz) = (view[0] as f32, view[2] as f32);
    // Vanilla's right vector is the view turned 90 degrees clockwise.
    let (rx, rz) = (-fz, fx);
    let dx = fx * forward + rx * right;
    let dz = fz * forward + rz * right;
    let len = (dx * dx + dz * dz).sqrt();
    if len.is_finite() && len > 1.0e-4 {
        (dx / len, dz / len)
    } else {
        (0.0, 0.0)
    }
}
