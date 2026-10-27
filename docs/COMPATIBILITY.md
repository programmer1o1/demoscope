# Compatibility

## At a glance

| Engine family | Games | Status | Map overlay | Details |
|---------------|-------|:-:|---|---------|
| Source 1 (`HL2DEMO`, `GMODEMO`) | TF2, HL2 family, CS:S, CS:GO, DoD:S, Portal, Portal 2, Stanley Parable, L4D1/2, GMod, Half-Life: Source | ✅ / 🟡 by game | VBSP `.bsp` (incl. LZMA lumps) | [Source-family formats](#source-family-formats) |
| Source 2 (`PBDEMS2`) | CS2 ✅; Dota 2, Deadlock 🟡 | ✅ / 🟡 by game | CS2 / Deadlock `.vpk` collision mesh | [Source-family formats](#source-family-formats) |
| GoldSrc (`HLDEMO`) | CS 1.6 ✅; Condition Zero, Half-Life 1, DoD 🟡 | ✅ / 🟡 by game | v30 `.bsp`, multiple maps | [GoldSrc](#goldsrc-hldemo) |
| Quake (`.dem`, `.mvd`, `.dm2`, `.dm_66`-`.dm_68`) | Quake 1, QuakeWorld (MVD), Quake 2, Quake 3 Arena | ✅ | Q1 / Q2 / Q3 `.bsp` | [Quake family](#quake-family) |
| QuakeWorld client demos (`.qwd`) | QuakeWorld | ⚠ synthetic tests only | | [Quake family](#quake-family) |
| Quake Live (`.dm_71`+) | | ⚠ attempted with a warning; correctness unverified | | [Quake family](#quake-family) |
| SFM (DMX), Titanfall (`R1DEMO`) | | ✗ rejected on magic | | [Source-family formats](#source-family-formats) |

Use `demoscope --check` (see [Verification](#verification)) to test your own
demos. How each decoder was built is covered in [Decoder notes](#decoder-notes).

## Status legend

- ✅ Supported: native decode verified on a named demo, no documented feature gaps.
- 🟡 Partial: decodes, with documented gaps.
- ⚠ Detected / experimental.
- ✗ Rejected.

Status reflects feature coverage. Individual failing demos are listed under
[Known issues](#known-issues) and don't change a game's status.

## Source-family formats

"Native" below means entity data was decoded from the demo's network stream,
with no fallback to the recorder camera.

| Game | Status | Verified on |
|------|--------|-------------|
| Team Fortress 2 | ✅ Events, map (incl. LZMA), life breaks, teleport arcs, multi-player tracks | `mvm_rottenburg_c2`: 48 tracks, 1.92M samples. Three more demos, 3-8 tracks each |
| Counter-Strike: Source | ✅ Full | `de_valley`: 24 tracks, 100k samples. `cs_office`: three demos, 14-19 tracks each |
| Half-Life 2 Deathmatch | ✅ Full | `dm_overwatch`: 17 tracks, 65k samples |
| Half-Life 2, Episode One / Two, Lost Coast | 🟡 Decodes through the generic Source 1 path; no sample to confirm | not yet verified on a named demo |
| Half-Life: Source | 🟡 Recorder camera only. DataTables are not parsed, so there are no per-player tracks | Half-Life: Source `background02` menu demo (`game_dir` `hl1`): 1 track, 1,497 samples, camera fallback |
| Day of Defeat: Source | 🟡 Decodes through the generic Source 1 path; no sample to confirm | not yet verified on a named demo |
| Portal | ✅ Native player track, camera. Single-player demos carry no networked events, so the event log is empty | Portal `testchmb_a_10` and a scripted `background1` menu demo: 1 native track each |
| HL2 old engine (2004) | 🟡 Native player track, camera, inputs. Player names are not decoded | HL2 2004-engine demo: 1,976 native samples, 0 names |
| Garry's Mod 9 / 10 Classic | 🟡 Camera, inputs, player track (falls back to the camera path where entity deltas are sparse). Player names are not decoded | GMod Classic demo: camera fallback, 28 native vs 3,142 camera samples |
| Garry's Mod 13+ (`GMODEMO`) | 🟡 Inputs, events, camera, per-player tracks. Player names are not decoded on the sampled demos. 2024-era builds decode almost no entity data | `gm_flatgrass` (2021 build): 446 native samples, 0 names. 2024 builds: see [Known issues](#known-issues) |
| Portal 2 | ✅ Positions, eye-yaw, inputs, incl. mid-game / SAR speedrun recordings. Single-player demos have no networked events, so the event log is empty | Three single-player demos, native, incl. a SAR full-game recording (finale) |
| Aperture Tag, Portal Stories, Portal Reloaded | 🟡 Same engine path as Portal 2 | not yet verified on a named demo |
| The Stanley Parable | 🟡 Same engine path as Portal 2 | not yet verified on a named demo |
| Left 4 Dead 2 | 🟡 Positions, eye-yaw, weapons, names, life states, inputs, camera. Sparse on the one sample: 4 named players, 2 tracks (one with 9 samples) | `c8m2_subway`: 2 native tracks |
| Left 4 Dead 1 | 🟡 Same engine path as Left 4 Dead 2 | not yet verified on a named demo |
| CS:GO (Source 1, net protocol 13xxx, pre-CS2) | ✅ Positions, angles, life states, weapons, inputs, names, events, grenades, economy, scoreboard | Four demos, 7-11 native tracks each, incl. an `ar_monastery` GOTV match |
| CS2 | ✅ Positions, names, deaths, events, kill attribution, rounds, grenades, inputs (client recordings only), held weapon, economy, scoreboard, map overlay from `.vpk` | `de_nuke` match (build 10924): 10 tracks, 779k samples, 0 failed packets, 1,544 events. Three more recordings, one on build 10772: 1-11 tracks, 0 failed packets |
| Deadlock | 🟡 Positions, names. Events: sparse (observed on 2 of 3 sample demos). No held weapon, no recorded inputs (the viewer infers them from movement). Map overlay loads from a `.vpk` | Three demos: 6, 3 and 12 tracks. `new_player_basics`: 6 tracks, all 132,776 samples inside the `.vpk` geometry bounds (107,887 verts), median distance to the nearest vertex 18 units. The other two maps had no `.vpk` available |
| Dota 2 | 🟡 Positions, names, life states. Events: none observed on the sample demo. No held weapon, no recorded inputs (the viewer infers them from movement). Map overlay not verified (no `.vpk` available) | One demo: 10 tracks, 147k samples, 0 failed packets, 65 death transitions |
| SFM (DMX) / Titanfall (`R1DEMO`) | ✗ Different file format, rejected on magic | n/a: rejected on magic |

Source 1 decodes player entities natively in every game: any entity with index
1-64 whose class name contains `Player` is tracked, and there is no per-game
list. The recorder camera is used instead only per demo, when no entity
produced a position sample. Camera-only demos open in the viewer with a single
track.

## Quake family

Quake demos record the server's network stream, so positions are read directly
from the file. Files are routed by extension, after the Source and GoldSrc magic
checks, so a renamed file can be misdetected: any `.dem` that isn't a Source or
GoldSrc demo is read as NetQuake, and any `.dm_NN` as Quake 3 (a `.dm2` renamed
to `.dem` is misread as NetQuake). For the map overlay, place the matching `.bsp`
beside the demo (Quake 3 maps ship inside a `.pk3`: extract `maps/<name>.bsp`).

| Game | Status | Verified on |
|------|--------|-------------|
| Quake 1 (NetQuake `.dem`, protocol 15) | ✅ Player tracks, POV angles, names, map | Two demos: 12 players each, 31 and 74 tracks |
| QuakeWorld MVD (`.mvd`, protocol 28 + FTE / MVDSV extensions) | ✅ Player tracks, view angles, names, deaths, spectators, map | 4on4 KTX match on `dm3`: 8 tracks, 255,272 samples, whole file decoded |
| QuakeWorld client demo (`.qwd`) | ⚠ Implemented; covered by synthetic unit tests only: player tracks, recorder view angles, names, deaths | not yet verified on a named demo |
| Quake 2 (`.dm2`, protocol 34) | ✅ Player tracks, angles, names, map, recorder death-hiding | One DM demo on `base64`: 114 tracks, 5 named players |
| Quake 3 Arena (`.dm_66`-`.dm_68`) | ✅ Player tracks, angles, names, map, death and spectator hiding | Demos on `q3tourney6`, `cpm4` and `pro-q3dm6` |
| Quake Live (`.dm_71`, `.dm_73`, `.dm_90`+) | ⚠ Attempted with a warning ("will probably desync"); correctness unverified | `.dm_71`: 4 tracks, 3,760 samples. `.dm_91`: 2 tracks, 17,763 samples, no player names |

## GoldSrc (HLDEMO)

Demo protocol 5, net protocol 48.

| Game | Status | Verified on |
|------|--------|-------------|
| Counter-Strike 1.6 | ✅ Recorder camera, per-player tracks, names, map overlay | `de_nuke`: 10 tracks, ~150k samples. `de_tuscan`: 10 tracks, ~139k samples |
| Condition Zero | 🟡 Recorder camera, per-player tracks, map overlay. Only the recorder is named; other players show as `player N` | `cs_assault`: 12 tracks, 5,082 samples, 1 of 12 players named |
| Half-Life 1, Day of Defeat | 🟡 Same decoder as CS 1.6 | not yet verified on a named demo |

GoldSrc viewers can load several maps at once with repeatable `--map` flags or
multiple browser drops. When consecutive maps share an `info_landmark`, they are
aligned into one campaign space automatically, and the camera and player tracks
switch spaces at the changelevel. Maps without a shared landmark keep their
native coordinates.

## Known issues

Described by game, build and symptom. These are individual recordings or
builds; they don't change a game's status above.

- **CS2:** one recording (build 10772) fails entirely: no packet decodes and
  `--check` reports `WARN`. Other recordings on the same build decode cleanly.
- **Deadlock:** one recording loses about 5% of its packets (`WARN`).
- **CS:GO POV recordings:** the local player's entity track can be sparse. One
  sampled POV demo had 23 entity samples against 10,878 camera samples.
- **Garry's Mod 13, 2024-era builds:** almost no entity data decodes (1-46
  samples, or a fall back to the camera). The 2021 sample works.
- **Half-Life: Source:** DataTables are not parsed, so only the recorder camera
  is available.
- **Old engine (HL2 2004, GMod Classic) and the GMod 2021 sample:** player
  names are not decoded.
- **Condition Zero:** only the recorder's name decodes on the sample; other
  players are labelled `player N`. Counter-Strike 1.6 names decode.
- **Quake Live:** not rejected, so decoding is attempted and may desync. The
  `.dm_91` sample decoded no player names.

## Verification

The release workflow builds and tests the Rust library and CLI before producing
platform archives. The regression workflow also generates HTML and validates
its embedded JavaScript when a demo corpus is available. For a local corpus,
run:

```bash
demoscope --check /path/to/demos
```

`PASS` means native tracks were decoded: player tracks that come from the
demo's entity data. A demo whose only track is the recorder camera copied in as
a fallback is `WARN`, and the `decode` column shows `fallback` (or `mixed` when
other players decoded natively). `WARN` also means a usable result with partial
packet loss. `FAIL` means the decoder errored or produced nothing, and makes the
command exit non-zero. `PASS` still says nothing about how complete the decode
is: a demo with a single native sample passes.

A directory sweep checks `.dem`, `.dm2`, `.mvd`, `.qwd` and every `.dm_NN`
file. Quake 3 files outside `.dm_66`-`.dm_68` (Quake Live) are attempted and
reported as `WARN` ("experimental") when they decode, never `PASS`. Historical
corpus counts are kept in the changelog alongside the version they describe
rather than presented here as current results.

## Decoder notes

Technical background for each decoder, for contributors. Users can skip this.

### Source 1

- **HL2 old engine / GMod Classic (net protocol 7).** Entity and field gaps use
  the November 2004 `engine.dll`'s unsigned Exp-Golomb reader, followed by a
  post-hierarchy `SPROP_CHANGES_OFTEN` reorder. Packets apply transactionally,
  so one bad delta can't corrupt later state. Old view angles are 16-bit
  bitangles; GMod Classic's usercmd carries a byte-aligned button mask.
  `gmancheapledemo` gives a 1,976-sample native player track. On `zombieslayer`,
  later mod-specific deltas are sparse, so the recorder uses the dense camera
  path (3,142 samples) while decoded remote entities are kept.
- **GMod 13+.** A modified Source 2013 stream with three GMod-only encodings
  (gated on `garrysmod`): 13-bit edict indices (8,192 edicts), a 24-bit
  `svc_PacketEntities` length, and a hybrid `svc_CreateStringTable`. High-index
  sandbox props that arrive without an Enter-PVS are skipped; players are
  unaffected.
- **Portal 2 / Stanley / L4D (demo protocol 4).** See [PROTO4.md](PROTO4.md).
  Portal 2 decodes 306 tables / 235 classes, Stanley 237 classes, L4D1 222 and
  L4D2 278. L4D shares the Portal 2 net-message numbering but keeps an 11-bit
  `svc_UserMessage` length (Portal 2 uses 12).
- **CS:GO.** A separate protobuf-based path in `src/source/csgo/` (messages,
  SendTables, PacketEntities, game events, live `userinfo` table).

### Source 2

- A from-scratch pipeline in `src/source2/`: Snappy, the `PBDEMS2` container,
  flattened serializers linked by (name, version), the field-path Huffman tree,
  all field decoders, string tables, baselines and the entity delta walk.
  Routed by the `PBDEMS2\0` magic.
- **CS2 map overlay.** CS2 maps are VPK-packed Source 2 resources, decoded by a
  VPK v2 reader, resource container, KV3 v5 decoder and physics-shape walker
  (`src/source2/{vpk,resource,kv3,vphys,map}.rs`). The overlay is the
  `world_physics.vmdl_c` collision mesh; textured render meshes are out of
  scope. `de_nuke.vpk` gives 115,738 verts / 191,628 triangles.
- **CS2 gameplay.** Inputs come from `m_nButtonDownMaskPrev`, the held weapon
  from `m_hActiveWeapon`, and kill attribution from remapped `player_death` pawn
  handles. Rounds are synthesized from `m_totalRoundsPlayed` (CS2 sends no
  `round_end`). Molotov fire is attributed through `CInferno.m_hOwnerEntity`.

### Quake

- **Map formats.** Quake 1 (v29, shared with GoldSrc), Quake 2 (`IBSP` v38) and
  Quake 3 (`IBSP` v46; bezier patches skipped, tool and sky shaders dropped),
  routed through `bsp::extract_any_bsp`.
- **Death and spectator hiding.** Quake 3 uses entity `EF_DEAD`, the recorder's
  `PM_DEAD` and the spectator team; Quake 2 uses the recorder's `PM_DEAD` /
  `PM_GIB`. The recorder is not hidden for `PM_SPECTATOR`, since POV demos sit
  there the whole time.
- **Quake 3.** The Huffman tree and field tables are ported from GPL ioquake3
  (`qcommon/msg.c`, `qcommon/huffman.c`). Quake Live extends the field tables,
  which is why it desyncs.
- **Quake 1.** Player names are converted from the Quake colour charset so clan
  tags read correctly.
- **QuakeWorld** (`src/quake/qw.rs`). `.mvd` and `.qwd` share one message
  parser with different framing: MVD frames carry a millisecond delta and a
  target-player byte, QWD frames an absolute time and the recorder's usercmd.
  Players come from `svc_playerinfo` (delta-coded per player in MVDs); entity
  updates are consumed but not drawn. `FTE_PEXT_FLOATCOORDS` switches coords to
  floats and angles to 16 bits, and MVDSV hidden-message frames (`dem_multiple`
  sent to nobody) are skipped. Layouts follow the ezQuake / MVDSV sources.
  Run with `DUMP_QUAKE=1` to print where decoding stopped and per-player frags
  against decoded deaths.

### GoldSrc

- The 544-byte header, directory and frame stream are parsed following the
  open-source `hldemo` parser. The camera comes from each NetMsg's `RefParams`.
  The walker resyncs past occasional mis-sized frames.
- Other players come from the bit-packed delta protocol inside each NetMsg
  (`svc_deltadescription`, `svc_packetentities`, `svc_deltapacketentities`,
  names from `svc_updateuserinfo`), mirroring the LGPL `khanghugo/dem`.
- The map overlay reads v30 / v29 BSPs, dropping sky and tool textures by name.
  `cs_assault.bsp` gives 2,805 verts / 5,547 triangles.
