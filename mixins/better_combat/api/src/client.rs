//! The client half: the roll key and `/roll`, the server's answers, the push
//! the server granted, and the combo pips above the hotbar.
//!
//! The server owns every decision. This side only asks, applies what was
//! granted to its own player (the game's own prediction path), and draws.

use std::sync::Mutex;

use horizon_client::mod_api::*;

use crate::net;
use crate::{KEY_DEFAULT, KEY_NAME, MOD_ID};

/// What the last combo message said, for the pips.
#[derive(Default)]
pub struct Client {
    view: Mutex<View>,
}

#[derive(Default)]
struct View {
    /// The combo step in use and the combo's length; `None` hides the pips.
    combo: Option<(u8, u8)>,
    /// The last refusal, drawn briefly in place of the pips.
    notice: Option<&'static str>,
}

/// `SoundSource.PLAYERS` is the server's; a local sound is centred on the
/// player who rolled.
const ROLL_SOUND: &str = "minecraft:entity.player.attack.sweep";

impl Client {
    /// Options > Mods lists the mod and its key.
    pub fn mods(&self, mods: &mut Vec<NativeMod>) {
        mods.push(NativeMod {
            id: MOD_ID.to_owned(),
            title: "Combat Combos & Roll".to_owned(),
            description: "Weapons chain a short combo of heavier blows. Press the roll key (or type \
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

    /// The request: no direction, so the server rolls the player where they
    /// look (a client cannot read the movement keys from a trait mod).
    fn ask(&self, host: &mut dyn Host) {
        let _ = host.net_send_to_server(net::ROLL, &net::encode_roll(0.0, 0.0));
    }

    pub fn message(&self, host: &mut dyn Host, channel: &str, data: &[u8]) {
        match channel {
            net::COMBO => {
                if let Some(m) = net::decode_combo(data) {
                    self.view.lock().unwrap_or_else(|e| e.into_inner()).combo =
                        (m.index > 0).then_some((m.index, m.len));
                }
            }
            net::ROLL_OK => {
                let Some(ok) = net::decode_roll_ok(data) else { return };
                self.view.lock().unwrap_or_else(|e| e.into_inner()).notice = None;
                // The push the server granted, added to the player's own
                // velocity as the client moves itself (vanilla's prediction).
                host.push_player([ok.dir[0] * ok.speed, 0.0, ok.dir[1] * ok.speed]);
                let at = host.client_state().map_or([0.0; 3], |s| s.eye_position);
                host.play_local_sound(ROLL_SOUND, at, 0.6, 1.6, 0);
            }
            net::ROLL_NO => {
                let Some((reason, _)) = net::decode_roll_no(data) else { return };
                let text = match reason {
                    1 => "Roll cooling down",
                    2 => "Cannot roll in the air",
                    _ => "Cannot roll now",
                };
                self.view.lock().unwrap_or_else(|e| e.into_inner()).notice = Some(text);
            }
            _ => {}
        }
    }

    /// The pips above the hotbar: one per combo step, the current one gold.
    pub fn render(&self, host: &dyn Host, batch: &mut HudBatch) {
        let Some(state) = host.client_state() else { return };
        if !host.playing() {
            return;
        }
        let [w, h] = state.gui_size;
        let view = self.view.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(notice) = view.notice {
            let text = notice;
            let width = host.text_width(text);
            host.text(batch, text, [(w as f32 - width) / 2.0, h as f32 - 62.0], 0xFF5555, true);
            return;
        }
        let Some((index, len)) = view.combo else { return };
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
