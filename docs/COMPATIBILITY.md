# Compatibility

## At a glance

| Engine family | Games | Per-player tracks | Map overlay | Details |
|---------------|-------|:-:|---|---------|
| Source 1 (`HL2DEMO`, `GMODEMO`) | TF2, HL2 family, CS:S, CS:GO, DoD:S, Portal, Portal 2, Stanley Parable, L4D1/2, GMod | ✅ | VBSP `.bsp` (incl. LZMA lumps) | [Source-family formats](#source-family-formats) |
| Source 2 (`PBDEMS2`) | CS2, Dota 2, Deadlock | ✅ | CS2 `.vpk` collision mesh | [Source-family formats](#source-family-formats) |
| GoldSrc (`HLDEMO`) | Half-Life 1, CS 1.6, DoD, Condition Zero | ✅ | v30 `.bsp`, multiple maps | [GoldSrc](#goldsrc-hldemo) |
| Quake (`.dem`, `.mvd`, `.qwd`, `.dm2`, `.dm_66`-`.dm_68`) | Quake 1, QuakeWorld, Quake 2, Quake 3 Arena | ✅ | Q1 / Q2 / Q3 `.bsp` | [Quake family](#quake-family) |
| Quake Live (`.dm_71`+) | | ⚠ detected, not decoded | | [Quake family](#quake-family) |
| SFM (DMX), Titanfall (`R1DEMO`) | | ✗ rejected on magic | | [Source-family formats](#source-family-formats) |

Use `demoscope --check` (see [Verification](#verification)) to test your own
demos. How each decoder was built is covered in [Decoder notes](#decoder-notes).

## Source-family formats

| Game | Status | Verified on |
|------|--------|-------------|
| Team Fortress 2 | ✅ Events, map (incl. LZMA), life breaks, teleport arcs, multi-player tracks | |
| Half-Life 2, Episode One / Two, Lost Coast, HL2 Deathmatch | ✅ Full | |
| Counter-Strike: Source | ✅ Full | |
| Day of Defeat: Source | ✅ Full | |
| Portal | ✅ Full | |
| HL2 old engine (2004) | ✅ Native player track, camera, inputs | `gmancheapledemo.dem` |
| Garry's Mod 9 / 10 Classic | ✅ Camera, inputs, player track (falls back to the camera path where entity deltas are sparse) | `zombieslayer.dem` |
| Garry's Mod 13+ (`GMODEMO`) | ✅ Inputs, events, names, camera, per-player tracks | `type6interloperedgmod`, `garrythirteen`, `gm_flatgrass` |
| Portal 2, Aperture Tag, Portal Stories, Portal Reloaded | ✅ Positions, eye-yaw, inputs, incl. mid-game / SAR speedrun recordings. Single-player demos have no networked events, so the event log is empty | `testingportal2`, SAR speedrun |
| The Stanley Parable | ✅ Same path as Portal 2 | sample demo, 2,226 samples |
| L4D1 / L4D2 | ✅ Positions, eye-yaw, weapons, names, life states, inputs, camera | `demogaty.dem` (L4D2, `c8m2_subway`) |
| CS:GO (Source 1, ≤ 2023) | ✅ Positions, angles, life states, weapons, inputs, names, events, grenades, economy, scoreboard | GOTV and POV recordings |
| CS2 | ✅ Positions, names, deaths, events, kill attribution, rounds, grenades, inputs, held weapon, economy, scoreboard, map overlay from `.vpk` | full `de_nuke` match: 10 players, 0 failed packets |
| Dota 2 / Deadlock | ✅ Positions, names, events. No held weapon; inputs are estimated from movement | |
| SFM (DMX) / Titanfall (`R1DEMO`) | ✗ Different file format, rejected on magic | |

Multi-player entity decode is native on TF2, CS:S, CS:GO, Portal 2, The Stanley
Parable, L4D1/2, GMod 13 and Source 2. Other Source 1 games fall back to the
recorder camera when compatible player properties are unavailable.

## Quake family

Quake demos record the server's network stream, so positions are read directly
from the file. Files are routed by extension, after the Source and GoldSrc magic
checks. For the map overlay, place the matching `.bsp` beside the demo (Quake 3
maps ship inside a `.pk3`: extract `maps/<name>.bsp`).

| Game | Status | Verified on |
|------|--------|-------------|
| Quake 1 (NetQuake `.dem`, protocol 15) | ✅ Player tracks, POV angles, names, map | `boodwand2`, `1134_rd1` |
| QuakeWorld MVD (`.mvd`, protocol 28 + FTE / MVDSV extensions) | ✅ Player tracks, view angles, names, deaths, spectators, map | 4on4 KTX match on `dm3`: 8 players, whole file decoded |
| QuakeWorld client demo (`.qwd`) | ⚠ Implemented; covered by synthetic unit tests only (no real sample yet): player tracks, recorder view angles, names, deaths | |
| Quake 2 (`.dm2`, protocol 34) | ✅ Player tracks, angles, names, map, recorder death-hiding | 5-player DM demo, `base64.bsp` |
| Quake 3 Arena (`.dm_66`-`.dm_68`) | ✅ Player tracks, angles, names, map, death and spectator hiding | `q3tourney6`, `cpm4`, `pro-q3dm6` |
| Quake Live (`.dm_71`, `.dm_73`, `.dm_90`+) | ⚠ Detected and warned; decode is not supported | |

## GoldSrc (HLDEMO)

| Game | Status | Verified on |
|------|--------|-------------|
| Half-Life 1, CS 1.6, DoD, Condition Zero (demo proto 5, net proto 48) | ✅ Recorder camera, per-player tracks, names, map overlay | `cs_assault` (CZ): 13 player tracks, all inside the map |

GoldSrc viewers can load several maps at once with repeatable `--map` flags or
multiple browser drops. When consecutive maps share an `info_landmark`, they are
aligned into one campaign space automatically, and the camera and player tracks
switch spaces at the changelevel. Maps without a shared landmark keep their
native coordinates.

## Verification

The release workflow builds and tests the Rust library and CLI before producing
platform archives. The regression workflow also generates HTML and validates
its embedded JavaScript when a demo corpus is available. For a local corpus,
run:

```bash
demoscope --check /path/to/demos
```

`PASS` means the format-specific decoder produced usable tracks, `WARN` means
the result is usable with partial packet loss, and `FAIL` makes the command exit
non-zero. Historical corpus counts are kept in the changelog alongside the
version they describe rather than presented here as current results.

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
