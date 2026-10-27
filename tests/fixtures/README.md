# Test fixtures

Small demo files used by the tests and CI:

- `tests/golden.rs`: snapshots `--json` output and a digest of the embedded
  HTML tables for **every demo in this directory** (see `docs/OUTPUT.md`).
- `tests/cli_contract.rs`, `scripts/check-viewer.sh`, `scripts/check-wasm.mjs`
  and `demoscope --check tests/fixtures` (CI) all read this directory.

Anything added here is published with the repository, forever, in git history.

## Eligibility rule

A demo is a fixture only if all of these hold:

1. The recorder is `Sierra` (the maintainer). The maintainer's own SteamID in
   the file is accepted.
2. Every other player is a bot. No other real person's name, SteamID or chat.
3. It fits the budget: about 2 MB per file and about 15 MB for the directory.
   Short clips are fine if they still decode as `PASS`.
4. It is not a known-failing demo.

One documented exception: `hl2_2004_trainstation.dem` (below).

Total size of the fixtures in this directory: **8,641,903 bytes**.

## Coverage

| Engine family | Fixture |
|---------------|---------|
| Source 1, demo protocol 3, multi-player | `css_bots_cs_office.dem` (CS:S), `hl2dm_bots_dm_overwatch_clip.dem` (HL2 Deathmatch) |
| Source 1, demo protocol 3, single-player | `portal_testchmb_a_10.dem` (Portal) |
| Source 1, demo protocol 4 | `portal2_sp_a3_01.dem` (Portal 2) |
| Source 1, CS:GO (protobuf path, GOTV) | `csgo_gotv_dust2_clip.dem` |
| Source 1, old engine (net protocol 7) | `hl2_2004_trainstation.dem` |
| Source 2, CS2 | `cs2_dust2_solo.dem` |
| Source 2, Deadlock | `deadlock_bots_clip.dem` |

**No fixture, covered only by the maintainer's private corpus:** Quake 1,
QuakeWorld (`.mvd` and `.qwd`), Quake 2, Quake 3 Arena, Quake Live, GoldSrc
(Half-Life 1, CS 1.6, Condition Zero, DoD), TF2, Day of Defeat: Source,
Half-Life: Source, HL2 retail and episodes, Left 4 Dead 1 and 2, The Stanley
Parable, Garry's Mod (Classic and 13) and Dota 2. CI does not exercise
these decoders.

## css_bots_cs_office.dem

| | |
|---|---|
| Original name | `bots.dem` |
| Size | 917,408 bytes |
| SHA-256 | `cb64c695aee488077c87597c279b2bc558c014bab15a0c8b267a13b22664f3d0` |
| Engine / game | Source 1, Counter-Strike: Source, demo protocol 3, net protocol 24 |
| Map | `cs_office`, 46.8 s |
| Covers | multi-player entity decode (14 tracks, 7,489 samples, 18 named), game events (`player_death`), `--csv` / `--json` (3,091 usercmds) |

**Source.** Recorded by the maintainer on a local server (`localhost:27015`)
against bots.
**License.** Recorded by the maintainer.
**Personal data.** Recorder `Sierra` and the maintainer's SteamID. The other 17
players are CS:S bots (SteamID `BOT`, default bot names). No chat, no IP
addresses, no file paths.

## cs2_dust2_solo.dem

| | |
|---|---|
| Original name | `d 2.dem` |
| Size | 870,268 bytes |
| SHA-256 | `9d04083079dff3429a79fdb865115df5557f7fd5127480191e4d600653961222` |
| Engine / game | Source 2, CS2, build 10772, network protocol 14165 |
| Map | `de_dust2`, 105.2 s |
| Covers | CS2 client recording: 1 player track (4,178 samples, 0 failed packets), `ENTITY_BUTTONS` (203 samples), 3 events, the Source 2 `--summary`, `--json` / `--csv` rejection |
| Not covered | the CS2 map overlay (no `.vpk` is shipped), kill and round events |

**Source.** Recorded by the maintainer on a local server (`loopback:1`), alone.
**License.** Recorded by the maintainer.
**Personal data.** Recorder `Sierra` and the maintainer's SteamID (the only
SteamID in the file; checked by decompressing every frame). The header holds
the maintainer's local game path
`D:\SteamLibrary\steamapps\common\Counter-Strike Global Offensive\game\csgo`
(accepted). No other player, no chat, no IP addresses.

## csgo_gotv_dust2_clip.dem

| | |
|---|---|
| Original name | first 1,200,000 bytes of `testinggotv.dem` |
| Size | 1,200,000 bytes |
| SHA-256 | `f890bd4b1f89447e2654c00b44a5e57594e5979233f5640d44b4f5fb0b6bbc5c` |
| Engine / game | Source 1, CS:GO (GOTV), demo protocol 4, net protocol 13881 |
| Map | `de_dust2` |
| Covers | the CS:GO protobuf path: 11 tracks, 3,672 samples, 11 named, 130 events (spawns, hurt, death, round start / end), life states, weapons |

The clip is the start of the file, cut mid-stream: it has no end-of-demo marker
and the header reports 0 ticks and 0 length. It decodes cleanly up to the last
complete packet.

**Source.** Recorded by the maintainer: a GOTV recording of a local server
named `Sierra`, with bots.
**License.** Recorded by the maintainer.
**Personal data.** Recorder `Sierra` and the maintainer's SteamID
(`STEAM_1:0:594013346`, the only SteamID in the file). The other players are 9
CS:GO bots (SteamID `BOT`) and the GOTV bot. The server name is `Sierra`. No
chat, no IP addresses, no file paths.

## portal2_sp_a3_01.dem

| | |
|---|---|
| Original name | `testingportal2.dem` |
| Size | 1,369,055 bytes |
| SHA-256 | `dd842ab801ee23a4ff265541b597df5a6957800d165613567bf2e5e492b5d41a` |
| Engine / game | Source 1, Portal 2, demo protocol 4, net protocol 2001 |
| Map | `sp_a3_01`, 96.9 s, single-player |
| Covers | the proto-4 path: 1 native track (2,449 samples), 5,844 usercmds (`--csv` / `--json`), 915 console-command events |

**Source.** Recorded by the maintainer on a local single-player session.
**License.** Recorded by the maintainer.
**Personal data.** Recorder `Sierra` and the maintainer's SteamID
(`STEAM_1:0:594013346`). The only player. No chat, no IP addresses, no file
paths.

## portal_testchmb_a_10.dem

| | |
|---|---|
| Original name | `d.dem` |
| Size | 481,691 bytes |
| SHA-256 | `d112ac781d804cfd99667f2c42c93c33a985e7ed85db03652037268e9526590a` |
| Engine / game | Source 1, Portal, demo protocol 3, net protocol 24 |
| Map | `testchmb_a_10`, 22.4 s, single-player |
| Covers | the proto-3 single-player path: 1 native track (950 samples), 1,548 usercmds (`--csv` / `--json`), 135 console-command events |

**Source.** Recorded by the maintainer on a local single-player session.
**License.** Recorded by the maintainer.
**Personal data.** Recorder `Sierra`. No SteamID in the file (the decoder shows
`STEAM_ID_PENDING`). The only player. No chat, no IP addresses, no file paths.

## deadlock_bots_clip.dem

| | |
|---|---|
| Original name | first 1,500,000 bytes of `testingdeadlock.dem` |
| Size | 1,500,000 bytes |
| SHA-256 | `8026b83dc04513ff09ff82a4eb765bd4b53cc7f4bd11e174d2bd9724a5878be2` |
| Engine / game | Source 2, Deadlock, build 10725, network protocol 48 |
| Map | `new_player_basics` (the guided bot match), cut short |
| Covers | Deadlock entity decode: 5 tracks, 12,770 samples, 5 named, 2,554 packets, 0 failed; the Source 2 `--summary`; `--json` / `--csv` rejection |
| Not covered | game events (none in the clip), held weapon, the map overlay (no `.vpk` is shipped) |

The clip is the start of the file, cut mid-stream: the header's end-of-demo
information is missing, so the length and tick count read 0.

**Source.** Recorded by the maintainer on a local guided bot match
(`127.0.0.1:27015`).
**License.** Recorded by the maintainer.
**Personal data.** Recorder `Sierra` and the maintainer's SteamID (the only
SteamID in the file; checked by decompressing every frame). The other four
players are `Bot8` to `Bot11`. The header holds the maintainer's local game path
`D:\SteamLibrary\steamapps\common\Deadlock\game\citadel` (accepted) and the
file also holds a private LAN address (`192.168.1.8`) next to `127.0.0.1`. No
chat.

## hl2dm_bots_dm_overwatch_clip.dem

| | |
|---|---|
| Original name | first 1,500,000 bytes of `testingdm.dem` |
| Size | 1,500,000 bytes |
| SHA-256 | `244923b6dbd73cdd190fe7da78dcd65c74ae66fc8dbd938ef260433599f98c0b` |
| Engine / game | Source 1, Half-Life 2: Deathmatch, demo protocol 3, net protocol 24 |
| Map | `dm_overwatch` |
| Covers | multi-player entity decode (17 tracks, 10,863 samples, 17 named), `player_death` events, `--csv` / `--json` (4,679 usercmds) |

The clip is the start of the file, cut mid-stream: it has no end-of-demo marker.
The header is intact, so it still reports the full 434.7 s length.

**Source.** Recorded by the maintainer on a local server (`localhost:27015`)
against bots.
**License.** Recorded by the maintainer.
**Personal data.** Recorder `Sierra` and the maintainer's SteamID. The other 16
players are bots (SteamID `BOT`, all named `Bot Name`). No chat, no IP addresses,
no file paths.

## hl2_2004_trainstation.dem

| | |
|---|---|
| Original name | `gmancheapledemo.dem` |
| Size | 803,481 bytes |
| SHA-256 | `d26928be0be1864a84d6b84ab7368dfb746fd96e3efdfaf3bb2bb5f0fb7c7cbd` |
| Engine / game | Source 1, Half-Life 2 (2004 engine), demo protocol 2, net protocol 7 |
| Map | `d1_trainstation_05`, 54.8 s, single-player |
| Covers | the old-engine path: 1 native track (1,976 samples), 3,770 usercmds (`--csv` / `--json`), 162 console-command events |

**Source.** Demo Submissions, posted publicly (a channel where demos were
shared for use in a video). Not recorded by the maintainer.
**License.** None stated. There is no explicit redistribution license.
**Personal data.** None found. The client name is `unnamed`, the server is
`localhost:0`, no player names are decoded, and a scan of the file found no
SteamIDs, e-mail addresses, URLs, IP addresses or file paths. The console
commands are movement keys only.
**Will be removed on request.** This is the one fixture that does not follow
the "recorded by the maintainer" rule.

## Adding a fixture

1. Check the [eligibility rule](#eligibility-rule), the size budget, and that
   the demo decodes as `PASS`. A truncated Quake, QuakeWorld, Source 1
   (proto 3) or Source 2 file still decodes; GoldSrc cannot be truncated (its
   directory is at the end of the file), and some proto-4 and CS:GO files fail
   when cut.
2. Check for personal data before committing: every player name and SteamID
   (`--html`, then `ENTITY_NAMES`), chat and console text, server addresses and
   file paths. For a Source 2 file, decompress the frames first: the raw bytes
   are Snappy-compressed, so a plain `strings` scan misses most of them.
3. Add an entry here: source, license, personal data and SHA-256.
4. Generate the golden files and review them:
   `UPDATE_GOLDEN=1 cargo test --test golden`, then `git diff tests/golden/`.
5. Run `demoscope --check tests/fixtures`, `./scripts/check-viewer.sh` and
   `node scripts/check-wasm.mjs`.
