# Test fixtures

Small demo files used by the tests and CI:

- `tests/golden.rs`: snapshots `--json` output and a digest of the embedded
  HTML tables for **every file in this directory** (see `docs/OUTPUT.md`).
- `tests/cli_contract.rs`, `scripts/check-viewer.sh`, `scripts/check-wasm.mjs`
  and `demoscope --check tests/fixtures` (CI) all read this directory.

Anything added here is published with the repository, forever, in git history.
Read [Adding a fixture](#adding-a-fixture) first.

The origin of these files was not recorded when they were added (the history
shows one commit, `b179439`, adding both). The entries below say what can be
read from the files themselves. Lines marked **To confirm** need the
repository owner; they are open questions, not facts.

## backgrounddem.dem

| | |
|---|---|
| Size | 477,737 bytes |
| SHA-256 | `726ef4d5e0d685b964109eb275151a2a7cec08aa74edb21d01571decc326c932` |
| Engine / game | Source 1, Portal, demo protocol 3, net protocol 24 |
| Map | `background1`, 22.5 s, scripted single-player menu demo |
| Covers | Source 1 header and packet walk, `--csv` / `--json` usercmds, the native single-player entity path (1 track), console-command events, map-less HTML |
| Not covered | multi-player entities, game events, any other game |

**Source.** The file looks like one of Portal's stock main-menu background
demos, not a recording made by the project: the map is `background1`, the
server is `localhost:0`, and the client name `cge7-193` also appears in a
Half-Life: Source `background02` demo (`game_dir` `hl1`). That points to a
Valve-authored file shipped in the game's data, but it is not proven.

**License / permission.** **To confirm.** If it is a stock Valve asset, it is
Valve's copyrighted game content and the repository has no license to
redistribute it. If so, replace it with a demo recorded by the project
(a short Portal session on a small map) before 1.0.

**Personal data.** None found. No SteamID (the decoder reports
`STEAM_ID_PENDING`), no chat, no file paths. The client name `cge7-193`
looks like a machine name, not a person.

## hi.dem

| | |
|---|---|
| Size | 322,071 bytes |
| SHA-256 | `3878ef2060eaae405ed4f9f0ca2a5a51df5ca4d2536507af71aa27472801f8d4` |
| Engine / game | Source 2, Deadlock, build 10725, network protocol 48 |
| Map | `dl_hideout`, 21.7 s, 3 player entities, 0 game events |
| Covers | the Source 2 container, entity decode (3 tracks, 1,179 samples, 457 packets, 0 failed), Source 2 `--summary`, `--json` / `--csv` rejection |
| Not covered | game events, held weapon, map overlay (no `.vpk` is shipped), CS2 and Dota 2 |

**Source.** Unknown. The recorder's client name is `A-6-hawk`, not a name used
in the owner's other demos, and the server is `127.0.0.1:27015` (a local
session). **To confirm:** who recorded it and under what terms.

**License / permission.** **To confirm.** It is a recording of Valve's game by
someone, and the project has no stated permission from that person.

**Personal data. This file contains 3 real players** (per the repository
owner's note). What can be read from the file:

- Player names: `Bot0`, `Bot1` and the recorder `A-6-hawk`. The first two read
  like bot names, but the file does not say so. Treat all three names as
  personal data until confirmed.
- SteamIDs: the decoder does not expose them (`steam_id` is empty in the HTML),
  but the raw file contains 12 byte sequences shaped like 64-bit SteamIDs.
  They have not been decoded; assume they are real.
- A local file path in the header: `G:\SteamLibrary\steamapps\common\Deadlock\game\citadel`
  (a drive letter and library folder). The HTML output keeps only `citadel`;
  the file itself, and `--summary`, show the full path.
- No chat. The file's events are empty.

If any of that is not acceptable, the file has to be replaced and removed from
history (`git filter-repo`); deleting it in a later commit does not remove it.

## Adding a fixture

1. Size: about 2 MB per file, about 15 MB for the directory. Prefer a short
   clip. A truncated Quake, QuakeWorld, Source 1 (proto 3) or Source 2 file
   still decodes; GoldSrc cannot be truncated (its directory is at the end of
   the file), and some proto-4 and CS:GO files fail when cut.
2. Record who made it and the terms in this file, before committing. Prefer a
   demo you recorded yourself, alone or against bots, on a stock map.
3. Check for personal data: player names, SteamIDs (`--html`, then look at
   `ENTITY_NAMES`; also run `strings` on the file), chat, server addresses and
   file paths. Record each in this file. A SteamID of the recorder is personal
   data too.
4. Generate the golden files and review them:
   `UPDATE_GOLDEN=1 cargo test --test golden`, then `git diff tests/golden/`.
5. Run `demoscope --check tests/fixtures`, `./scripts/check-viewer.sh` and
   `node scripts/check-wasm.mjs`.
