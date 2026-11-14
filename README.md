# Combat Combos & Roll — Horizon port

Weapons chain a short combo of heavier blows, and a key rolls the player out of danger. A
clean-room port of the *behaviour* of Better Combat + Combat Roll to
[Horizon](https://github.com/horizon-rust/horizon) (Minecraft Java 26.3 rewritten in Rust).

- Mod id: `better_combat`
- Kind: **native mod** (`.hnmod`, a trait mod — no content mod, no mixin injection)
- Side: **both** — one bundle, the server half and the client half of the same package
- Built against Horizon commit `a85af177a82504f357ff10d5d40d0b7230e2ac32` (hook API 415)

## Why a clean-room rewrite

Upstream *Better Combat* and *Combat Roll* are All-Rights-Reserved. Nothing of their code, data
or assets is used here: the combos, the weapon profiles, the roll, the protocol and the HUD are
original code written from the public description of what those mods do. This port's own code is
MIT (`LICENSE`); see `NOTICE.md`.

## Done

- **Per-weapon combos, server-authoritative.** The server hears a player's attack
  (`attack_entity`), reads the item actually in their hand and advances that weapon's combo.
  Ten profiles (`api/src/weapons.rs`): sword, claymore, dagger, katana, axe, mace, polearm,
  scythe, tool, fist — each with its own sequence of damage multipliers (e.g. sword
  1.0 / 1.0 / 1.2, axe 1.0 / 1.25, dagger 0.9 / 0.9 / 1.2), arc width and reach. The step
  multiplies the damage of the hit the player made (`living_incoming_damage_v2`), so armour,
  enchantments, the game's own attack cooldown and death handling keep working. A pause of 25
  ticks or a weapon change starts the combo over; two clicks in one tick count once. An empty
  hand has the fist's own short combo.
- **The swing arc really hits.** After the clicked target is scaled, the server lists what
  stands around the player (`Host::entities_in`), keeps the living things inside the weapon's
  reach and inside its arc (`core::in_arc`), with line of sight, and hurts each for a share of
  the hit (`Host::hurt_entity`, `SWEEP_SHARE` = 0.6) with the sweep particle and sound. An item
  in the game's `minecraft:swords` tag is left to vanilla's own sweep, which the mod only
  scales to the combo's multiplier — so nothing is ever hit twice. Other players are not swept:
  a duel is one target, and `living_incoming_damage_v3` scales that hit instead.
- **PvP follows the combo.** `living_incoming_damage_v2` only hears mob victims, so a player's
  hit on another player is scaled in `living_incoming_damage_v3` by the same step.
- **Dodge roll**, server-authoritative: the client only asks (`better_combat:roll`), sending
  the direction of the movement keys it holds; the server checks the player's state and
  cooldown (4 s, 80 ticks), refuses in water, while riding, in the air or flying, opens 8 ticks
  in which attacks, arrows and blasts miss them (`living_incoming_damage`, with a list of what
  a roll must *not* dodge: fall, fire, lava, hunger, the void, `/kill`…), and answers. Only on
  a grant does the client push its own player (`Host::push_player`, the prediction path) for
  the granted ticks, play a sweep of air, and start its cooldown.
- **Client half**: a rebindable key (default `R`, listed in Options > Mods), `/roll`, the combo
  pips above the hotbar (from the first step), and a refusal line ("Roll cooling down",
  "Cannot roll in the air", "Cannot roll in water", "Cannot roll while riding") that fades
  after two seconds through the client tick hook.
- **Protocol** (`api/src/net.rs`): version byte + exact length on every message, both halves
  validating what they receive; 15 unit tests over the rules and the wire format
  (`cargo test -p mod-better_combat`).
- **Packing**: `python tools/pack.py` writes `dist/better_combat-0.1.0.hnmod`.

## Missing / not done

- **No hunger cost** on a roll: the food level is readable (`Host::player_food`) but there is
  no helper that sets it, so a roll costs nothing. The other checks do run: alive, not a
  spectator, on the ground, not flying, not in water, not riding, cooldown.
- **The sweep spares other players.** The arc skips player victims (a player's hit on a player
  is only scaled, not swept); a true PvP sweep would need to know teams/allies, which no helper
  exposes.
- **Attack speed is the game's.** Weapon attack cooldown cannot be read or changed through the
  API, so a dagger is not faster than a claymore — only the combo's multipliers, arc and reach
  differ per weapon. Reach is this mod's own (2.6 to 4.2 blocks), not the game's click range.
- **The combo is shown, not animated.** No first-person swing animation, hand model change or
  attack indicator of its own: the client draws the pips, and a sweep is the game's particle
  and sound.
- **`/roll` is a client command**, so it works only through this mod's own chat handling. There
  is no server command, no permission and no config file.
- **The HUD is plain.** Pips are a row of squares above the hotbar, the refusal line is a
  single centred line; no icons, no cooldown bar.
- **Not played.** Everything compiles against the real game sources and the rules are unit
  tested, but nobody has fought a mob or rolled on a live client: the HUD, the key, the arc's
  feel and the roll's push are unverified in game.

## Files

```
mods/better_combat/
  horizon-native.json               native manifest packed at the .hnmod's root
  mixins/better_combat/
    horizon-mixin.toml              [mod] + [compatibility] + [api] path
    api/Cargo.toml                  the trait-mod package (horizon-client only)
    api/src/lib.rs                  Hooks: the wiring
    api/src/server.rs               the combo, the arc, the damage steps, the roll, its iframes
    api/src/client.rs               key, /roll, the grant's push, the pips
    api/src/net.rs                  the two halves' messages
    api/src/core.rs                 the rules, free of game types
    api/src/weapons.rs              the weapon profiles and their classification
  tools/pack.py                     packs dist/better_combat-<version>.hnmod
```

## Building and checking (VPS)

The bundle is a trait mod, so it is built into a game the way the launcher builds one: the
`api/` package is staged as a workspace member beside `horizon-client`, and a generated
`crates/native-game` binary links `hooks()`. On the build host:

```sh
cd /opt/build/horizon && git worktree add --detach /opt/build/wt/better_combat
scp -r mods/better_combat root@host:/opt/build/mods/
bash /opt/build/bc-stage.sh              # stages api/ + crates/native-game (copy of ps-stage.sh)
export CARGO_TARGET_DIR=/opt/build/target CARGO_BUILD_JOBS=4
cargo check -p mod-better_combat         # the mod against the real game sources
cargo test  -p mod-better_combat         # the unit tests
```

## Installing

- **Server**: the `.hnmod` in `horizon-native-mods/`, `mod.better_combat:true` in
  `horizon-mods.txt`, then the launcher builds the server engine with it (`--server-output`).
- **Client**: the same `.hnmod` in `horizon-native-mods/` and the same line; the launcher builds
  the game with it. Both halves come from the same bundle, so a server and its players must run
  the same version.

## Licence notices

- This port: MIT (`LICENSE`). Original code, no upstream file copied.
- Upstream Better Combat (by daedelus) and Combat Roll (by wefcd): All-Rights-Reserved, **nothing**
  from them is included; their names say only what this reimplements.
- Horizon: this mod only links against `horizon-client`'s public mod API.
