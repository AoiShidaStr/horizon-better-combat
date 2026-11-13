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
  1.0 / 1.0 / 1.2, axe 1.0 / 1.25, dagger 0.9 / 0.9 / 1.2). The step multiplies the damage of the
  hit the player made (`living_incoming_damage_v2`), so armour, enchantments, the game's own
  attack cooldown and death handling keep working. A pause of 25 ticks or a weapon change starts
  the combo over; two clicks in one tick count once.
- **Dodge roll**, server-authoritative: the client only asks (`better_combat:roll`), the server
  checks the player's state and cooldown (4 s, 80 ticks), opens 8 ticks in which attacks, arrows and blasts
  miss them (`living_incoming_damage`, with a list of what a roll must *not* dodge: fall, fire,
  lava, hunger, the void, `/kill`…), and answers. Only on a grant does the client push its own
  player (`Host::push_player`, the prediction path), play a sweep of air, and start its wait.
- **Client half**: a rebindable key (default `R`, listed in Options > Mods), `/roll`, the combo
  pips above the hotbar and a refusal line ("Roll cooling down", "Cannot roll in the air").
- **Protocol** (`api/src/net.rs`): version byte + exact length on every message, both halves
  validating what they receive; 15 unit tests over the rules and the wire format
  (`cargo test -p mod-better_combat`).
- **Packing**: `python tools/pack.py` writes `dist/better_combat-0.1.0.hnmod`.

## Missing / not done

- **The swing arc and its extra targets are not applied.** Upstream Better Combat hits every
  entity in a wide arc; a Horizon trait mod can neither list the entities around a player
  (`Host` has `entity(id)`, `server_players()`, but no query) nor deal damage of its own (no
  `hurt` helper: `native_hurt_entity` is a game-internal function, not a mod helper). So the
  arc, its range bonus and the sweep's reach exist only as tuning data and tested rules
  (`Attack::arc`), not in play: a click still hits the one target the game picked, with the
  combo's multiplier, and vanilla's own sword sweep is left untouched. This is the one big gap
  and the reason the mod is not "Better Combat complete". It needs a hook point that lists
  nearby entities and one that applies damage — both would be new `Host` helpers.
- **No hunger cost, no water/vehicle refusal** on a roll: no helper reads a player's food level,
  their vehicle or whether they are in water, so a roll works while swimming or riding and costs
  nothing. The checks that do run: alive, not a spectator, on the ground, not flying, cooldown.
- **PvP is not scaled.** `living_incoming_damage_v2` only fires for a player's melee, sweep and
  spear hits on a mob; a hit on another player keeps vanilla's damage, so the combo's multiplier
  applies to mobs only.
- **The HUD is approximate.** A combo pip row is drawn only from the combo's second step (`index
  0` doubles as the "combo is over" message that hides the pips), and the refusal line stays on
  screen until the next granted roll instead of fading on its own: no client tick hook is used.
- **Range and attack speed are the game's.** Weapon ranges (`range_bonus` upstream) and attack
  cooldown cannot be read or changed through the API, so a claymore has no extra reach and a
  dagger is not faster — only the combo's multipliers differ per weapon.
- **The roll goes where the player looks.** A client mod cannot read the WASD state, so the
  direction sent is empty and the server rolls along the player's yaw. The protocol already
  carries a direction for the day a movement-input hook exists.
- **The combo is shown, not animated.** No first-person swing animation or attack sound of its
  own: `Attack::arc` is not drawn, and the client draws only the pips. No hand model change, no
  attack indicator for the combo.
- **`/roll` is a client command**, so it works only through this mod's own chat handling. There
  is no server command, no permission and no config file.
- **Not played.** Everything compiles against the real game sources and the rules are unit
  tested, but nobody has fought a mob or rolled on a live client: the HUD, the key and the roll's
  feel are unverified in game.

## Files

```
mods/better_combat/
  horizon-native.json               native manifest packed at the .hnmod's root
  mixins/better_combat/
    horizon-mixin.toml              [mod] + [compatibility] + [api] path
    api/Cargo.toml                  the trait-mod package (horizon-client only)
    api/src/lib.rs                  Hooks: the wiring
    api/src/server.rs               the combo, the damage step, the roll, its iframes
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
