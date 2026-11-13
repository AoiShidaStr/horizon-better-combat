//! Combat Combos & Roll, for Horizon: a clean-room port of the *behaviour* of
//! Better Combat + Combat Roll.
//!
//! Upstream Better Combat and Combat Roll are All-Rights-Reserved: none of
//! their code, data or assets is used here. Everything below is original work
//! written from the public description of what the mods do (weapons chain a
//! short combo with different swings; a key rolls the player to dodge).
//!
//! Hook points used (docs/native-mods.md, "Trait mods"): a server tick
//! (`server_tick`), a player's attack on an entity (`attack_entity`), a melee
//! hit on a mob before it lands (`living_incoming_damage_v2`), a hit on
//! anything (`living_incoming_damage`), the two halves' messages
//! (`net_from_client`, `net_from_server`), the client's key (`key`, `mods`),
//! its own commands (`commands`, `command`) and the screen it draws on
//! (`screen_render_post`).
#![forbid(unsafe_code)]

use horizon_client::mod_api::*;

mod client;
mod core;
mod net;
mod server;
mod weapons;

/// The mod's id: its settings key, its channels and its point in a panic.
pub const MOD_ID: &str = "better_combat";
/// The key's name in Options > Mods, and the key it starts as.
pub const KEY_NAME: &str = "roll";
pub const KEY_DEFAULT: &str = "key.keyboard.r";

/// The launcher calls this for the built game.
pub fn hooks() -> Box<dyn Hooks> {
    Box::new(Combat::default())
}

/// One instance serves the window's game and the server thread alike, so what
/// it keeps lives behind locks (`Hooks` is `&self` and `Send + Sync`).
#[derive(Default)]
struct Combat {
    client: client::Client,
}

impl Hooks for Combat {
    fn server_tick(&self, host: &mut dyn Host, event: &mut ServerTickEvent<'_>) {
        server::server_tick(host, event);
    }

    fn attack_entity(&self, host: &mut dyn Host, event: &mut AttackEntityEvent<'_>) -> Flow {
        server::attack_entity(host, event)
    }

    fn living_incoming_damage_v2(&self, host: &mut dyn Host, event: &mut LivingIncomingDamageV2Event<'_>) -> Flow {
        server::living_incoming_damage_v2(host, event)
    }

    fn living_incoming_damage(&self, event: &mut LivingIncomingDamageEvent<'_>) -> Flow {
        server::living_incoming_damage(event)
    }

    fn net_from_client(&self, host: &mut dyn Host, event: &mut NetFromClientEvent<'_>) {
        server::net_from_client(host, event);
    }

    fn net_from_server(&self, host: &mut dyn Host, event: &mut NetFromServerEvent<'_>) {
        self.client.message(host, event.channel, event.data);
    }

    fn player_logged_out(&self, host: &mut dyn Host, event: &mut PlayerLoggedOutEvent) {
        let _ = host;
        server::player_logged_out(event.player);
    }

    fn mods(&self, host: &mut dyn Host, event: &mut ModsEvent<'_>) {
        let _ = host;
        self.client.mods(event.mods);
    }

    fn key(&self, host: &mut dyn Host, event: &mut KeyEvent<'_>) {
        self.client.key(host, event.key, event.pressed);
    }

    fn commands(&self, host: &mut dyn Host, event: &mut CommandsEvent<'_>) {
        let _ = host;
        self.client.commands(event.commands);
    }

    fn command(&self, host: &mut dyn Host, event: &mut CommandEvent<'_>) -> Flow {
        if self.client.command(host, event.command) {
            Flow::Cancel
        } else {
            Flow::Continue
        }
    }

    fn screen_render_post(&self, host: &dyn Host, event: &mut ScreenRenderPostEvent<'_>) {
        self.client.render(host, event.batch);
    }
}
