# demoscope

Fast Rust parser and interactive 3D visualiser for Source Engine demo files (`.dem`).

Supports Team Fortress 2, Half-Life 2 (all versions), Counter-Strike: Source, Day of Defeat: Source, Portal, Portal 2, and The Stanley Parable - i.e. **HL2DEMO** Source 1 games on `net_protocol ≥ 24`, where it decodes player positions, inputs, and game state into a full visualisation. Left 4 Dead 1/2 and CS:GO parse end-to-end (header, inputs, names, recorder camera path) with per-player entity tracks still in progress. Older `net_protocol = 7` titles (Garry's Mod 9, HL2 Old Engine) parse only their header + console log. See the [Compatibility](#compatibility) table and [Investigating Stanley Parable & L4D2](#investigating-stanley-parable--l4d2).

---

## What's new in v0.4.0

- **Left 4 Dead 1 & 2 now parse end-to-end** - the full demo stream walks to `DEM_STOP` (inputs, names, recorder camera path; `data23.dem` = 509 usercmds, `ellisfloor.dem` = 432). Two container quirks were peeled back: L4D ships **4 splitscreen slots** (`Split_t[4]`, 304-byte `democmdinfo`), and its newer command enum inserts `dem_customdata` at 8 and pushes `dem_stringtables` to **9** (Portal 2 / Orange Box keep it at 8). The **SendTable/DataTable format was also cracked** by sweeping flag-width × priority × `m_nBits` for a sane server-class count: L4D1 uses 16-bit TF2 flags with **`m_nBits = 6`** (222 classes); L4D2 uses 19-bit Alien-Swarm flags + priority with `m_nBits = 6` (278 classes). The shared L4D quirk is the 6-bit `m_nBits` field - everything else uses 7, and a one-bit miscount desyncs the whole table walk. The previously-conflated `portal2_engine` boolean is now decoupled into three independent axes (`portal2_extra_bits` flag format, `bit_count_bits`, and container `is_portal2_engine`). Per-player entity *tracks* still need the L4D net-message map - see [Investigating Stanley Parable & L4D2](#investigating-stanley-parable--l4d2).
- **CS:GO parses end-to-end** - the command-enum shift (stringtables at 9) and the splitscreen handling apply, so CS:GO walks to STOP with full inputs and player names. Its `player_info_s` blob has a distinct layout (16-byte `version + xuid` prefix, **128-byte name field, big-endian integers**) - the userinfo parser now detects it by blob size and reads names/SteamIDs correctly. Entity tracks need a protobuf decoder (its DataTables and PacketEntities are protobuf-encoded) - a separate, larger effort.
- **First-person POV camera now uses the real per-frame camera** - on demos with no usercmds (Portal 2, Stanley), the view angle is sourced from the dense `democmdinfo.viewAngles` stream (≈1 sample per game packet) instead of the sparse networked eye-angle SendProp (which a recorder barely networks for itself). Interpolating the dense stream gives a smooth, accurate POV that turns at the camera's real cadence - no slow "smoothing-cam" sweep, no freeze-then-snap. Exposed as `__VIEW_ANGLES__`/`viewAngleAt()`.
- **Smoother camera & timeline** - OrbitControls inertia is off and the follow-target snaps (1:1 tracking, no glide); the timeline playhead is **tick-based** and redraws every frame so it advances continuously even on sparse synthesized timelines (it used to freeze between samples); and **dragging the timeline seeks to a continuous tick** with the avatar/camera interpolated to it, so scrubbing is smooth instead of snapping to sparse cmd samples.
- **Player-name parsing across engines** - `parse_player_info_blob` now handles all three `player_info_s` layouts by blob size: proto-3 (`name[32]`@0), proto-4 (xuid + `name[32]`@8), and CS:GO (version + xuid + `name[128]`@16, big-endian). The dead-player camera-follow case is fixed too (it no longer chases a dead primary's spectated coordinates).
- **Overflow-hardened demo walkers** - every length read in the packet walkers (`main.rs` + `player_tracks.rs`) now guards a negative/garbage length and uses saturating arithmetic. This fixes debug-build panics (`attempt to add with overflow`) on desync; release builds silently wrapped before. Validated across a **450-demo corpus** (TF2, HL2 + episodes, DoD:S, CS:S, CS:GO, Portal 1/2, Stanley, L4D1/2, plus Source 2 cs2/dota2) with **zero crashes** - the 10 non-parsing files are correctly rejected Source 2 (`PBDEMS2`) and HL2-beta (`HLDEMO`) magic.

---

## What's new in v0.3.0

- **Portal 2 & The Stanley Parable now decode real entity positions** - proto-4 (`demo_protocol = 4`) entity decode works end-to-end. `youareamoron.dem` yields 242 position samples of real `sp_a2_core` coordinates; Stanley Parable yields 2226. The last bug was a priority-sort mismatch in the SendTable flatten, found by hooking `engine.dll`'s `CDeltaBitsReader::ReadNextPropIndex` with Frida and diffing the engine's per-prop bit widths against ours. Full account in **[Proto-4 (Portal 2 / Stanley Parable / L4D) decode](#proto-4-portal-2--stanley-parable--l4d-decode)**, and a reusable how-to for the remaining games in **[Investigating Stanley Parable & L4D2](#investigating-stanley-parable--l4d2)**.
- **Entity-only playback timeline** - demos with no parsed usercmds (all of proto-4) now synthesize a playback timeline + world positions from the decoded primary-entity track, so the 3D scrubber, follow-cam, and speedometer run on real data instead of throwing on an empty `CMDS` array. The first-person camera pitch comes from `m_angEyeAngles[0]` (yaw from `[1]`), so look-up/down works.
- **Proto-4 player names** - `player_info_s` on Portal 2 / Stanley prepends an 8-byte `xuid` before `name[32]`, so the TF2-layout parser read empty/garbage names. The blob parser now auto-detects the name offset (0 for proto-3, 8 for proto-4), so Portal 2 / Stanley players show their real names (`Dragonツ`, `that mf`).
- **In-browser parser (WASM)** - `src/lib.rs` exposes `parse_demo_to_html` via `wasm-bindgen`; `scripts/build-wasm.sh` produces a 600 KB `.wasm` + JS glue under `web/`. The drag-and-drop UI at `web/index.html` lets users drop a `.dem` (and optionally `.bsp`) onto the page and view it without running the CLI. See "WASM (in-browser) build" below.
- **CS:S parity work** - three concrete fixes for the Source 1 sister game:
  - **Velocity-based teleport detection** - the per-player path-line and `mpSampleAt` interpolation use `(distance / Δt) > 900 u/s` instead of a flat distance threshold. CS:S samples are 1–4 seconds apart so a flat threshold flagged normal running as teleports. Cuts false teleports on `111.dem` from 1.3 % → 0.4 % of sample-pairs.
  - **Sub-second `m_lifeState` flicker filter** - CS:S pulses `m_lifeState=1` for ~20-30 ticks on hit-flinch / weapon-drop events that aren't real deaths. Any dead gap shorter than 0.6 s in the alive-interval list is coalesced into the surrounding alive interval. Real deaths (≥ 1 s dead) survive.
  - **Spec/observer hide** - new `Hide specs ✓` toggle in the Players panel suppresses entities with < 3 position samples that aren't the current primary. Catches idle bots, the HLTV/SourceTV camera, and the recorder when they're spectating someone else (cross-referenced against the `svc_SetView` switch intervals). On by default; right-clicking a sparse-sample entity to set them as primary auto-reveals them.
- **Fire-tracer bug fix** - `mpPrimaryPositionAt` was using the live `currentCmdIdx` for sparse-sample primaries instead of the cmd-index of the requested historical tick. All fire markers in CS:S piled up at the recorder's tick-0 spawn position. Fixed by routing through `findCmdIdx(tick)`.
- **Multi-player by default** - every `--html` run decodes the position, name, and life state of every player entity in the demo. No flag, no feature, no external dependency: the native `source_demo` decoder is the only one shipped.
- **Mid-demo renames + disconnect-reconnect** - `svc_UpdateStringTable` is now decoded for the `userinfo` table, so a player who renamed `name "Lunascape" → "Sierra"` mid-match shows up under their final name. Prior aliases are preserved per slot and displayed as `was X, Y` next to the current name. Primary-entity detection matches the recorder's header nick against any alias.
- **Spectator avatar tracks the camera** - the spec recorder's `m_vecOrigin` only updates at spawn, so demoscope falls back to `democmdinfo.viewOrigin` for sparse-sample entities. Their 3D avatar, route line, minimap dot, and Follow-camera target all use the camera trajectory.
- **BSP displacement support** - terrain surfaces (`LUMP_DISPINFO` + `LUMP_DISP_VERTS`) are now tessellated into the rendered mesh using qbyte's SourceImporter algorithm. Maps like `d2_coast_07` go from boxy brushwork to full hilly terrain.
- **Console panel** - replaces the old Chat panel. Combines chat + kills + spawns + round transitions + connect/disconnect / name-change lines into a single Source-style log. Click any line to seek to that tick; category chips filter what's shown; the `⛶` button pops it out into a fullscreen overlay that auto-scrolls during playback.
- **GIF export** - `Record GIF` button captures the live 3D scene as an animated GIF, defaulting to first-person camera at 10× playback for ~10s of compressed action.
- **Real player positions** - extracted from `democmdinfo.viewOrigin` (single-POV) and per-entity `m_vecOrigin` SendProps (multi-player), so rocket jumps, teleporters, and all physics are captured exactly.
- **Full 3D BSP map overlay** - entire map mesh rendered alongside player paths; supports LZMA-compressed lumps (common in TF2 workshop maps).
- **Teleport arc visualisation**, **sharp-line suppression**, **minimap**, **event timeline** - see Features below.

---

## Installation

Download a pre-built binary from the [Releases](https://github.com/programmer1o1/demoscope/releases) page (Windows, macOS, Linux), or build from source:

```bash
cargo build --release
# binary: target/release/demoscope
```

Requires Rust stable (≥ 1.85). No external system libraries. No external Rust crates beyond `base64` and `lzma-rs` for the CLI; the WASM build adds `wasm-bindgen` and `console_error_panic_hook` (gated to `target_arch = "wasm32"`).

### WASM (in-browser) build

demoscope also compiles to WebAssembly, exposing the parser to JavaScript so users can drag a `.dem` onto a webpage and skip the CLI entirely.

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli   # matching version in Cargo.lock
./scripts/build-wasm.sh
python3 -m http.server 8088 --directory web
# open http://localhost:8088/
```

Drop a `.dem` (and optionally the matching `.bsp`) onto the page. The viewer renders entirely in your browser - files never leave your machine. WASM parse time is ~5–8× slower than the native CLI (a 14 MB demo parses in ~1.6 s on Apple Silicon vs ~250 ms native), but for sharing demos with someone who doesn't have the CLI installed it's a meaningful upgrade.

Build artifacts land in `web/` (the `.wasm` + JS glue are gitignored and built fresh - only `web/index.html` is committed). The `.github/workflows/pages.yml` workflow runs `build-wasm.sh` and deploys `web/` to GitHub Pages on every push to `master`, so the hosted viewer always matches source (enable it once under repo Settings → Pages → Source: "GitHub Actions"). The library entry point is `pub fn parse_demo_to_html(demo: &[u8], bsp: Option<Vec<u8>>, name_hint: &str, jump_threshold: f32) -> Result<String, JsValue>` in `src/lib.rs`.

---

## Usage

```
demoscope <demo.dem> [OPTIONS]
```

| Flag | Description |
|------|-------------|
| *(none)* | Print every usercmd with view angles, movement, keys, mouse |
| `--all` | Also print non-usercmd packets (signon, packet, synctick, …) |
| `--csv` | Output usercmds as CSV (one row per frame) |
| `--json` | Output usercmds as a JSON array |
| `--summary` | Print header info and packet counts only |
| `--html [FILE]` | Generate self-contained interactive 3D HTML visualisation (always includes multi-player tracks) |
| `--jump-threshold N` | Distance (units) above which a position jump breaks the rendered path. Default 750. |

### Examples

```bash
# Quick header check
demoscope demo.dem --summary

# Human-readable per-frame breakdown
demoscope demo.dem

# Pipe into other tools
demoscope demo.dem --csv | head -5
demoscope demo.dem --json | jq '.[0]'

# Generate interactive 3D HTML viewer (multi-player included)
demoscope demo.dem --html
# → demo.html  (open in any browser, no server needed)

# Custom output path
demoscope demo.dem --html /tmp/out.html

# Tighter path-break threshold (default 750)
demoscope demo.dem --html --jump-threshold 400
```

---

## HTML visualisation

`demoscope --html` produces a single self-contained HTML file with all data embedded. Three.js is loaded from a CDN (works offline once cached).

Place the `.bsp` map file in the same directory as the demo for the full 3D map overlay. LZMA-compressed BSP lumps (common in TF2 workshop maps) are decompressed automatically.

### Controls

| Input | Action |
|-------|--------|
| Left-drag | Orbit |
| Scroll | Zoom |
| Right-drag | Pan |
| Space | Play / pause |
| Timeline scrubber | Seek |

### Features

| Section | Content |
|---------|---------|
| **3D scene** | Full BSP map mesh (incl. displacement terrain) + per-player path |
| **Playback** | Play/pause, speed 0.5×–10×, timeline scrubber |
| **Metadata header** | Map, game, client, server name, duration, tickrate, demo protocol, usercmd / life / death / teleport counts |
| **Speedometer** | Real engine-derived horizontal velocity (current + peak) computed from `viewOrigin` deltas |
| **Players panel** | Sidebar entry per detected player. Click to toggle their path; right-click to set as primary (controls camera follow + Lives panel) |
| **Lives panel** | Primary player's alive intervals from `m_lifeState`; click any row to seek |
| **Deaths** | Off → YOU (primary only) → ALL three-state toggle |
| **Rounds** | Auto-detected round windows. Prev/Next/All buttons, per-round seek, timeline shading |
| **Camera modes** | Orbit · Follow Player · First Person |
| **Player avatars** | Simple coloured box per entity. Primary tinted blue; YOU tag in the sidebar |
| **Fire markers** | Toggle small spheres at every `IN_ATTACK` rising edge, coloured by `weaponselect` |
| **Teleport arcs** | Cyan bezier arcs for teleporter / Eureka Effect events |
| **Minimap** | 2D top-down overlay with per-player paths, death markers, and current-position dots. `Heatmap` button overlays a log-scaled density grid (per-entity when MP is active) |
| **Event log** | Scrollable table of parsed game events - click a row to seek |
| **Console** | Chat + kills + spawns + rounds + connects in a Source-style log. Filter chips per category; expand-to-fullscreen button (⛶) auto-scrolls during playback |
| **GIF export** | `Record GIF` captures the canvas at configurable speed/fps/duration. Optional HUD overlay composites the minimap + player name onto every frame |
| **Video export** | `Record Video` records MP4/H.264 (preferred) or WebM/VP9/VP8 via the `MediaRecorder` API. Higher quality and smaller files than GIF |
| **Active weapon readout** | Below the WASD/M1 panel: shows which weapon the primary player is holding, derived from `m_hActiveWeapon` and the wielded entity's class name |
| **Hide specs toggle** | Players panel button that suppresses bots / observers / HLTV / recorder-while-spectating from the 3D scene + minimap + death markers. Combines a < 3-sample-count heuristic with the `svc_SetView` spectator-switch intervals so CS:S recorders don't render on top of whoever they're watching |

### Game-aware event filtering

`demoscope` reads the demo's `game_dir` and selects an appropriate event set. Custom filters for: `tf`, `cstrike` / `csgo`, `left4dead` / `left4dead2`, `portal` / `portal2`, `hl2mp`, plus a generic Source fallback.

---

## Compatibility

| Game | Status |
|------|--------|
| Team Fortress 2 | ✅ Full - events, BSP (incl. LZMA), life breaks, teleport arcs, multi-player tracks |
| Half-Life 2 (all versions) | ✅ Full |
| HL2: Episode One / Two | ✅ |
| HL2: Lost Coast | ✅ |
| HL2 Deathmatch | ✅ |
| Counter-Strike: Source | ✅ |
| Day of Defeat: Source | ✅ |
| Portal | ✅ |
| Garry's Mod 9 (legacy) | ⚠ Header + console log parse; **no positions/inputs/entities** (net_protocol 7 usercmd + entity format unsupported). Viewer loads empty. |
| HL2 Old Engine | ⚠ Same - too old (demo_protocol = 2, net_protocol = 7). Empty viewer. |
| Portal 2 / Aperture Tag / Portal Stories / Portal Reloaded | ✅ Entity positions, eye-yaw, full DataTables (306 tables / 235 classes). Real `m_vecOrigin` decode verified against an `engine.dll` bit-trace. No game events yet (Portal 2 event schema differs). |
| The Stanley Parable | ✅ Same Portal 2-engine path - 237 classes, real positions (2226 samples on the sample demo). `net_protocol = 1000` but shares the 19+8 flag format, splitscreen = 2, and message-ID remap. |
| L4D1 / L4D2 | ⚠ Parse end-to-end: header, inputs (509 / 432 usercmds), names, and recorder camera path all work, and DataTables now decode (222 / 278 classes - L4D1 = 16-bit flags + `m_nBits` 6; L4D2 = 19+8 + `m_nBits` 6). Per-player entity *tracks* still 0 - the net-message map remains. See [Investigating Stanley Parable & L4D2](#investigating-stanley-parable--l4d2). |
| CS:GO (Source 1, ≤ 2023) | ⚠ Parses end-to-end: inputs, names/SteamIDs (big-endian `player_info_s`), recorder camera path. Entity tracks need a protobuf decoder (DataTables + PacketEntities are protobuf-encoded). 0 entity tracks for now. |
| GMod 13+ (`GMODEMO`) / SFM (DMX) / Titanfall (`R1DEMO`) / CS2 + Dota 2 (`PBDEMS2`, Source 2) | ✗ Different file format entirely - rejected on magic |

Multi-player entity decode is native on TF2 / CS:S / Portal 2 / Stanley Parable; on other Source 1 games it's best-effort and depends on common SendProp table names being present.

### Proto-4 (Portal 2 / Stanley Parable / L4D) decode

Demos with `demo_protocol = 4` (Portal 2, Aperture Tag, Stanley Parable, L4D1/L4D2, old CS:GO ≤ 2020) share the `HL2DEMO` magic and bit-level wire conventions of proto-3 but diverge in several layers. Each had to be peeled back in order before a single entity position came out. **Portal 2 and The Stanley Parable now fully decode**; L4D is documented but not yet working (see the investigation guide below).

The layers, outermost to innermost:

1. **Signon-block skip.** Proto-4's first `DEM_SIGNON` packet has its embedded `length` field set to `0` even though the signon section is hundreds of KB. The walker fast-forwards using the header's `sign_on_length` when `demo_protocol > 3`.
2. **DEM command shift.** Proto-4 inserted `DEM_CUSTOMDATA = 8`, pushing `DEM_STRINGTABLES` from 8 → 9. Remapped in the walker.
3. **Splitscreen preamble.** Per the Alien Swarm SDK ([`NicolasDe/AlienSwarm`](https://github.com/NicolasDe/AlienSwarm), `src/public/demofile/demoformat.h`), the proto-3 single `Split_t` (76 bytes) became `Split_t[MAX_SPLITSCREEN_CLIENTS]`. **Portal 2 / Stanley = 2, L4D1/L4D2 = 4.** Pinned to 2 for known Portal 2-engine games; a length-probe (try N = 4, 2, 1) is the fallback for unidentified proto-4 games. The probe can false-positive - a puzzlemaker-export demo probed as 4 and desynced - which is why known games are pinned.
4. **Net-message ID remap.** Portal 2 renumbers the net messages (`NetSplitScreenUser`@3, `SvcSplitScreen`@22 new; `SvcPrint` 7→16; NetTick/StringCmd/SetConVar/SignonState each −1). `scan_game_payload` remaps to canonical IDs. This is what got `svc_PacketEntities` headers reading aligned. **L4D's map is different and unverified.**
5. **`svc_ServerInfo` / `svc_PaintMapData`.** Proto-4 `svc_ServerInfo` uses a 4-byte `mapCrc` (not TF2's 16-byte hash) plus a 32-bit `unk`; Portal 2 adds `svc_PaintMapData` at msg ID 33. (Details cross-checked against [`NeKzor/sdp`](https://github.com/NeKzor/sdp).)
6. **SendProp flags = 19 bits + 8-bit priority.** What NeKzor/sdp reads as "16-bit flags + 11-bit unk" is really `SPROP_NUMFLAGBITS_NETWORKED = 19` plus an 8-bit priority byte (same 27-bit total). `normalize_portal2_flags` maps the shifted bit positions back to TF2-canonical so flatten + decode stay engine-agnostic. Gated by `DataTableQuirks::portal2_extra_bits`.
7. **Entity-index + field-index encodings** (cracked with IDA Pro on `engine.dll`, tracing `CL_ParsePacketEntities → CL_CopyNewEntity → RecvTable_MergeDeltas → CDeltaBitsReader::ReadNextPropIndex`):
   - Entity-index delta = `ReadUBitInt` (6-bit base, low 4 kept, + 4/8/28-bit extension `<< 4`), **not** the 2-bit-selector UBitVar of TF2/CS:S.
   - Field index = a `new_way` bit read **once per entity** by the `CDeltaBitsReader` ctor, then per-prop: consecutive fast-path → 3-bit small gap → 7-bit base with `0x20/0x40/0x60` → `2/4/7`-bit extension. Missing the ctor's pre-read left every prop one bit out of phase.
8. **Flatten priority-sort - the final bug.** This is what blocked real positions even after 1–7 were correct. `SendTable_SortByPriority` (Source `dt.cpp`) arranges flat props by ascending priority, claiming each prop at the **first** pass where `prop.priority == pass` **OR** `(changes_often && pass == 64)`. We were forcing *every* `CHANGES_OFTEN` prop to priority 64; in reality a changes-often prop whose own priority is below 64 (e.g. `m_vecOrigin` @ priority 2) is claimed at its low priority, not at 64. The wrong assumption scrambled the flat table after the first ~8 props. See `src/source_demo/datatable.rs::sort_by_priority`.

**How the last bug was found (the reusable method).** Static disassembly had confirmed every *individual* decoder matched the engine, yet decode still drifted within the player's first ~8 props - the consecutive-index fast-path hid exactly where. The fix came from a **runtime bit-trace**: `scripts/p2_bittrace.js` (Frida) hooks `CDeltaBitsReader::ReadNextPropIndex` in `engine.dll` while playing the demo and logs, per prop, the field index and the bit position before/after. The engine's per-class **instance-baseline** stream walks *every* prop in flat order, so it yields an authoritative width-per-flat-index fingerprint for the class. Diffing that against demoscope's flat-table dump (`DUMP_FLAT=<class_id>`) showed our props were ordered differently - `m_flFallVelocity`(17 bits) sat at index 5 but the engine had it at 10. That pinned the priority-sort. Full writeup and the captured trace: `scripts/p2_engine_trace.md`. Tooling: `scripts/p2_bittrace.js` + `scripts/run_trace.py`.

**Net result.** Portal 2 and Stanley Parable decode real `m_vecOrigin` positions, eye-yaw (`m_angEyeAngles[1]`), and full DataTables. Two follow-ons completed the playback: the player class carries two `m_vecOrigin` copies (a live predicted one at low priority and a stale duplicate) - `scrape_player_state` takes the first in flat order; and demos with no usercmds synthesize their playback timeline from the decoded entity track (`src/template.html`). All proto-4 fixes are gated on `demo_protocol >= 4` / `portal2_extra_bits` with zero TF2/CS:S regression.

### Investigating Stanley Parable & L4D2

The diagnostic tooling that cracked Portal 2 is in the tree and env-gated, so the next proto-4 game is a tractable diff rather than a from-scratch RE session.

**Built-in trace switches** (all write to stderr; combine with `--html /tmp/x.html` to force the full multi-player decode path):

| Env var | What it prints |
|---|---|
| `DUMP_SCAN=1` | header (`proto / net / game_dir / portal2_engine / splitscreen`), each game packet, whether `DEM_DATATABLES` is reached and how many classes it parsed, and the offset/cmd where the walk aborts. **First thing to run on any failing demo.** |
| `DUMP_FLAT=0` | lists every server class (`id / name / data_table`) - find the player class id (look for `CPortal_Player` / `CBasePlayer` / `CTerrorPlayer`). |
| `DUMP_FLAT=<id>` | dumps that class's flattened prop list (`index / type / bits / priority / CO`) - the thing you diff against an engine bit-trace fingerprint. |
| `DUMP_ENT=1` | per `svc_PacketEntities` packet: `delta / max-entries / updates / length-bits / decode ok\|NONE / world-size / class histogram`. Shows whether the baseline ever establishes. |
| `DUMP_ENT2=1` | per entity within a packet (first 12): `index / eid / update-type / class_id / flat-len`. Pinpoints the exact entity where a snapshot desyncs. |
| `DUMP_USERINFO=1` | every `player_info_s` userdata blob as length + ASCII - shows where the name actually sits (proto-4 prepends an 8-byte `xuid` → name@8; CS:GO prepends 16 bytes → name@16). |
| `DUMP_MSG=1` | every raw 6-bit net-message id. Pipe through `sort \| uniq -c \| sort -rn` to histogram a new game's message enum - the once-per-packet ids are `net_Tick` and the `svc_PacketEntities` equivalent. **The tool for the remaining L4D net-message map.** |

```bash
DUMP_SCAN=1   demoscope demo.dem --html /tmp/x.html
DUMP_FLAT=0   demoscope demo.dem --html /tmp/x.html | grep CLASS
DUMP_FLAT=108 demoscope demo.dem --html /tmp/x.html | grep FLAT   # Stanley's CPortal_Player
```

**Current state of each game** (from `DUMP_SCAN`):

| Game | game_dir | net | splitscreen | DataTables | Status |
|---|---|---|---|---|---|
| The Stanley Parable | `thestanleyparable` | 1000 | 2 | 237 classes ✅ | **Works** - added to the Portal 2-engine allowlist; ships `CPortal_Player`, real positions. |
| Portal 2 | `portal2` | 2001 | 2 | 235 classes ✅ | **Works.** |
| L4D2 | `left4dead2` | 2100 | 4 | 278 classes ✅ | DataTables decode; entity *tracks* blocked on the net-message map (below). |
| L4D1 | `left4dead` | 1041 | 4 | 222 classes ✅ | Same - DataTables decode; tracks blocked on net-message map. |

**Why Stanley "just worked":** it's the Portal 2 engine (Source 2013 SP) under a different `game_dir` and `net_protocol`. Adding `"thestanleyparable"` to the Portal 2-engine list in `DataTableQuirks::for_game` (`src/source_demo/datatable.rs`) flipped it from `0 classes` to `237 classes, 2226 position samples`. If you find another Portal 2-engine mod, the fix is likely one line in that list - confirm with `DUMP_SCAN` showing `N classes` after the change.

**L4D DataTables - solved (v0.4.0).** The single `portal2_engine` boolean used to conflate three independent things; they're now decoupled into separate axes:

1. **Splitscreen count** - driven from a per-game table (L4D = **4**, Portal 2 / Stanley = 2); the length-probe stays as fallback for unidentified proto-4 games.
2. **Command enum** - L4D's newer enum inserts `dem_customdata` at 8 and moves `dem_stringtables` to **9**; `userinfo` lives in that cmd-9 block.
3. **Flag format + `m_nBits`** - found by sweeping flag-width × priority × `m_nBits` for a sane class count (`DUMP_SCAN`): **L4D1 = 16-bit TF2 flags, no priority, `m_nBits` 6** → 222 classes; **L4D2 = 19-bit Alien-Swarm flags + 8-bit priority, `m_nBits` 6** → 278 classes. The shared L4D quirk is the **6-bit `m_nBits`** field (`bit_count_bits` in `DataTableQuirks`) - everything else uses 7, and a one-bit miscount desyncs the table walk. `is_portal2_engine()` (container: splitscreen=2 + message remap) is now separate from `portal2_extra_bits` (flag format), so L4D2 can share the flag format without the Portal 2 container.

**What remains - the net-message map.** DataTables decode, but `svc_PacketEntities` never fires because L4D renumbered and widened the 6-bit svc enum (ids run up to 62; `net_Tick` is raw 4 not 3, PacketEntities looks like raw ~48 not 26). `scan_game_payload` needs an L4D remap like the existing Portal 2 one. Histogram the enum with `DUMP_MSG=1 … | sort | uniq -c | sort -rn`, but the authoritative id→name table (and the `svc_PacketEntities` header layout) is best confirmed with the same Frida bit-trace used for Portal 2 (`scripts/p2_bittrace.js`, repointed at the L4D `engine.dll`). NeKzor/sdp covers only the Portal 2 engine; the Alien Swarm SDK is the closest open reference.

#### Known gap: full-snapshot demos (speedruns, mid-game recordings)

Portal 2 / Stanley demos that *start at a level load* decode because the engine
sends each entity as an **Enter-PVS** update (class + serial + props), which the
decoder reads cleanly. But a demo recorded **mid-game** (e.g. a speedrun finale
segment) opens with delta packets against a baseline that lived in a previous
demo/level, followed by one large **full snapshot** (`is_delta = false`) that is
supposed to re-establish everything. That full snapshot currently desyncs:

```
DUMP_ENT2=1 demoscope fullgame_…_66.dem --html /tmp/x.html
  #0 eid=0  type=10 ENTER class_id=0 flat_len=46   ← decodes
  #1 eid=12 type=00 (Delta)                        ← garbage: prop decode of
                                                      class 0 over/under-read
```

Entity #0 enters and its prop decode consumes the wrong number of bits, so
entity #1's header reads as nonsense - and because one bad entity aborts the
whole packet, the baseline never forms and **0 positions** come out (the viewer
still loads with metadata). This is the *same class* of bug as the original
Portal 2 priority-sort issue, but on a class (`class 0`) that the simple chamber
demos never send as the leading entity - so it needs the same per-class
bit-trace diff (`DUMP_FLAT` vs `scripts/p2_bittrace.js`) to find which prop
width is wrong. Until then, full-snapshot / speedrun demos parse to a
metadata-only viewer.

### Smoke-test corpus

A **450-demo corpus** across 16 game folders (TF2, CS:S, CS:GO, HL2 + episodes, DoD:S, Portal, Portal 2, Stanley, L4D1/L4D2, DIPRIP, Dystopia, plus Source 2 cs2/dota2 and HL2-beta) is run through the full `--html` pipeline as a release smoke check, in a **debug build** (so integer-overflow checks are on - `--release` silently wraps them). Latest full-sweep results:

| Outcome | Count | What it means |
|---|---|---|
| ✅ Valid HTML viewer produced | **440 / 450** | parsed cleanly to `DEM_STOP`, no crash |
| ✗ Rejected on magic | 10 / 450 | 6 × CS2 + 3 × Dota 2 (`PBDEMS2`, Source 2) and 1 × HL2-beta (`HLDEMO`) - not `HL2DEMO`. Clean error, no panic. |
| 💥 Panics / crashes | **0** | the overflow-hardened walkers held across the whole corpus |

Of the 440 valid viewers, by position source:

| Position source | Games |
|---|---|
| Multi-player entity tracks (`m_vecOrigin`) | TF2, CS:S, DoD:S, HL2DM, Portal, **Portal 2**, **Stanley Parable**, multiplayer HL2 |
| Single-POV camera path (`viewOrigin`) | single-player HL2 / EP1 / EP2 / Portal - the recorder's own `m_vecOrigin` isn't networked, so they render the recorder camera path (correct, not a gap) |
| Camera path + names, no entity tracks | **L4D1, L4D2, CS:GO** - parse fully; per-player entity tracks pending (see [Investigating Stanley Parable & L4D2](#investigating-stanley-parable--l4d2)) |

Multi-player entity decode behaves well on TF2 + CS:S (per-player tracks, weapons, eye-yaw, life states) and on Portal 2 + Stanley Parable (recorder positions, synthesized playback timeline, dense `democmdinfo.viewAngles` POV camera). On HL2 single-player and Portal 1, only the recorder's `viewOrigin` is captured and the viewer renders the playthrough path. L4D1 / L4D2 / CS:GO parse end-to-end (inputs, names, recorder camera) but yield zero MP entity tracks - the viewer renders the recorder path the same way.

---

## Demo format reference

Magic: `HL2DEMO\0`

### Packet layout

All packets share a 5-byte prefix: `cmd (u8) + tick (i32)`.
For **demo_protocol = 4** (L4D, Portal 2, Stanley, CS:GO …) there is an extra `player_slot (u8)` byte after the prefix (6-byte prefix).

| cmd (proto-3) | Name | Payload |
|-----|------|---------|
| 1 | Signon | `democmdinfo(76) + in_seq(i32) + out_seq(i32) + length(i32) + data[length]` |
| 2 | Packet | same as Signon |
| 3 | SyncTick | *(nothing)* |
| 4 | ConsoleCmd | `length(i32) + data[length]` |
| 5 | **UserCmd** | `out_seq(i32) + length(i32) + data[length]` |
| 6 | DataTables | `length(i32) + data[length]` |
| 7 | Stop | *(prefix only)* |
| 8 | StringTables | `length(i32) + data[length]` |

**Proto-4 differences** (see [the proto-4 section](#proto-4-portal-2--stanley-parable--l4d-decode) for the full chain):
- The cmd IDs shift at 8: `8 = DEM_CUSTOMDATA` (new, length-prefixed), `9 = StringTables`.
- The `democmdinfo` preamble in cmd 1/2 is a `Split_t[MAX_SPLITSCREEN_CLIENTS]` array - **76 × N bytes** (N = 2 for Portal 2 / Stanley, 4 for L4D) - in place of the single 76-byte struct.
- The first `DEM_SIGNON`'s embedded `length` is `0`; use the header's `sign_on_length` to skip the signon block.

### democmdinfo layout (76 bytes, all cmd=1/2 packets)

```
flags          (i32)
viewOrigin     (3× f32) ← recorder eye position in world units
viewAngles     (3× f32)
localViewAngles(3× f32)
viewOrigin2    (3× f32)
viewAngles2    (3× f32)
localViewAngles2(3× f32)
```

### UserCmd bit format (CBitBuf, LSB-first)

Each field is preceded by a 1-bit **has-flag**; if 0 the field is unchanged from the previous frame (delta encoding).

| Field | Bits | Notes |
|-------|------|-------|
| command_number | 32 | u32 |
| tick_count | 32 | u32 |
| pitch | 32 | raw IEEE-754 float (degrees) |
| yaw | 32 | raw IEEE-754 float (degrees) |
| roll | 32 | float (typically 0) |
| forwardmove | 32 | float, ±450 max |
| sidemove | 32 | float - positive = right, negative = left |
| upmove | 32 | float |
| buttons | 32 | bitmask (see below) |
| impulse | 8 | u8 |
| weaponselect | 11 | entity index |
| weaponsubtype | 6 | only present when weaponselect present |
| mousedx | 16 | i16 raw mouse delta |
| mousedy | 16 | i16 raw mouse delta |

### Button masks

| Constant | Value | Key |
|----------|-------|-----|
| `IN_ATTACK` | `0x001` | Primary fire |
| `IN_JUMP` | `0x002` | Jump |
| `IN_DUCK` | `0x004` | Duck / crouch |
| `IN_FORWARD` | `0x008` | W |
| `IN_BACK` | `0x010` | S |
| `IN_USE` | `0x020` | E |
| `IN_MOVELEFT` | `0x200` | A |
| `IN_MOVERIGHT` | `0x400` | D |
| `IN_ATTACK2` | `0x800` | Secondary fire |
| `IN_RELOAD` | `0x2000` | R |
| `IN_SCORE` | `0x10000` | Tab |
| `IN_SPEED` | `0x20000` | Shift |

### Proto-4 SendTable / PacketEntities wire format (Portal 2 engine)

Reference for porting the entity decode to other proto-4 games. Verified against `engine.dll` for Portal 2; **the values below may differ on L4D - confirm with a bit-trace.** Implemented in `src/source_demo/{datatable,packetentities,sendprop}.rs`.

**SendProp definition** (`DEM_DATATABLES`, per prop):
```
type            (5 bits)
name            (null-terminated string)
flags           (19 bits)   ← SPROP_NUMFLAGBITS_NETWORKED, not TF2's 16
priority         (8 bits)   ← Portal 2-engine only (DataTableQuirks.portal2_extra_bits)
… then type-specific: exclude/DT name, or array count(10), or low/high float + nbits(7)
```
Flag bit positions differ from TF2 and are remapped by `normalize_portal2_flags` (e.g. `CHANGES_OFTEN` = bit 18; `CELL_COORD` family at bits 15/16/17).

**Flatten** - gather leaf props depth-first (collapsible DTs merge inline), then `sort_by_priority`: for each priority pass ascending, claim a prop where `prop.priority == pass || (changes_often && pass == 64)`. Do **not** force changes-often props to 64 unconditionally.

**`svc_PacketEntities` (msg 26)** per updated entity:
```
entity-index delta   ReadUBitInt   (6-bit base, low 4 kept, + 4/8/28-bit ext << 4)
update-type          2 bits        (00 delta · 01 leave · 10 enter · 11 delete)
if enter:  class_id (class_bits) + serial (10 bits)
then prop deltas (CDeltaBitsReader):
  new_way            1 bit         (read ONCE per entity, in the ctor)
  per prop: field index via ReadFieldIndex, then the SendProp value
```

**`ReadFieldIndex`** (returns −1 to stop):
```
if new_way && read_bit:          return last + 1                 (consecutive)
else if new_way && read_bit:     ret = read_bits(3)              (small gap)
else:                            ret = read_bits(7); by ret & 0x60:
    0x20 → ret = (ret & ~0x60) | (read_bits(2) << 5)
    0x40 → ret = (ret & ~0x60) | (read_bits(4) << 5)
    0x60 → ret = (ret & ~0x60) | (read_bits(7) << 5)
if ret == 0xFFF: return -1   else return last + ret + 1
```

**Value decoders** confirmed identical to TF2: COORD (2 flag bits + sign + 14 int + 5 frac), COORD_MP (flag bits 12/13/14), NOSCALE (bit 2 → raw 32-bit float), plus CELL_COORD for `m_vecOrigin`.

---

## CSV column reference

```
tick, cmd_num, pitch, yaw, roll, fwd, side, up, buttons, impulse, weapon, mousedx, mousedy
```

Empty fields = unchanged from previous frame. Fill-forward to get the current value at any tick.

---

## Notes

- **Recorder position** is read from `democmdinfo.viewOrigin` in every cmd=1/2 packet - the engine-recorded eye position. Captures rocket jumps, teleporters, and all game physics accurately.
- **Per-player positions** are extracted from `CTFPlayer.m_vecOrigin` / `m_vecOrigin[2]` SendProps in `svc_PacketEntities` messages. Names and SteamIDs come from the `userinfo` string table.
- **Spectators** don't get `m_vecOrigin` updates after spawn - demoscope falls back to `viewOrigin` for sparse-sample entities so their avatar, path, and minimap dot still track the actual viewpoint.
- **BSP lookup** searches the demo's directory and the binary's directory for `<map_name>.bsp`. LZMA-compressed lumps are decompressed automatically.
- Truncated demos (no `dem_stop` packet) parse cleanly up to the last complete packet.

---

## Roadmap

Ranked roughly easy → hard. PRs welcome.

### Done (in v0.4.0)

- ✅ **L4D1 / L4D2 parse end-to-end** - container quirks (splitscreen = 4, `dem_customdata`@8 / `dem_stringtables`@9) and the cracked SendTable format (L4D1 16-bit flags + `m_nBits` 6; L4D2 19+8 + `m_nBits` 6). DataTables decode (222 / 278 classes); per-player entity tracks pending the net-message map.
- ✅ **CS:GO parses end-to-end** - command-enum + splitscreen handling, plus a big-endian `player_info_s` (`name[128]`@16) for correct names/SteamIDs. Entity tracks need a protobuf decoder.
- ✅ **Dense POV camera** - first-person view angle sourced from per-frame `democmdinfo.viewAngles` (`__VIEW_ANGLES__` / `viewAngleAt()`) instead of the sparse networked eye-angle, for a smooth, accurate POV on usercmd-less demos.
- ✅ **Smoother camera & timeline** - OrbitControls inertia off + snapped follow target; tick-based timeline playhead that redraws every frame; continuous-tick timeline dragging with interpolated avatar/camera.
- ✅ **Overflow-hardened walkers** - every packet-length read guards negatives + uses saturating arithmetic, fixing debug-build panics on desync. Validated on a 450-demo corpus with zero crashes.

### Done

- ✅ **Demo metadata panel** - server, map, game, tickrate, demo protocol, lives/deaths/teleports surfaced in the header.
- ✅ **Round filter** - auto-detected round windows; Prev/Next/All buttons isolate the event log and shade the timeline.
- ✅ **Speedometer overlay** - corner readout of current and peak engine velocity, derived from `viewOrigin` deltas.
- ✅ **Weapon-fire markers** - toggleable spheres at every `IN_ATTACK` rising edge, coloured by weaponselect.
- ✅ **Configurable jump threshold** - `--jump-threshold N` CLI flag (default 750), applied consistently to 3D path, minimap, and live playback.
- ✅ **Native Source 1 decoder** - `src/source_demo/` decodes DataTables, StringTables (incl. userinfo), and PacketEntities (incl. all SendProp value types). Zero external runtime deps. Bit-for-bit parity with `tf-demo-parser` was reached and the comparison harness was retired.
- ✅ **Multi-player tracks default-on** - every `--html` run decodes per-player positions, names, and life states. Spectators included via `viewOrigin` fallback.
- ✅ **Mid-demo userinfo updates** - `svc_UpdateStringTable` is decoded for the userinfo table so renames and slot-reconnects are captured; aliases preserved per slot.
- ✅ **BSP displacement support** - `LUMP_DISPINFO` + `LUMP_DISP_VERTS` decoded; bilinear-interp + per-vert dist offset following qbyte's SourceImporter algorithm.
- ✅ **GIF export** - record the live 3D scene to an animated GIF; defaults to first-person camera at 20× speed, full canvas resolution, 10 fps × 10 s capture (~200 seconds of game time per clip). Encoder runs in web workers via gif.js with a same-origin Blob URL.
- ✅ **Console panel** - chat + kills + spawns + rounds + name-changes in one filtered Source-style log. Speaker names tinted by entity hue, kill rows show a weapon-class SVG icon, current line auto-highlights in blue during playback. `⛶` button opens a fullscreen overlay with the same content + autoscroll.
- ✅ **MP speedometer + input panel** - speedometer reads the primary player's `m_vecOrigin` velocity (with per-primary peak cache). Input panel shows real WASD/M1/JMP/DUCK for the recorder; for non-recorder primaries it derives WSAD arrows from velocity projected through their `m_angEyeAngles[1]` yaw, with a `derived (no usercmd)` tag.
- ✅ **Eye-yaw SendProp decoding** - `m_angEyeAngles[1]` / `m_angRotation[1]` extracted with a 2° dedupe; surfaced as `ENTITY_YAWS` in the HTML for local-frame projection.
- ✅ **Synthetic death markers** - `m_lifeState` 0→non-0 transitions without a matching `player_death` event get fallback `[no event]` lines in the Console and thin red ticks on the timeline (catches round resets, engine-forced kills, slot-reconnects).
- ✅ **Death-aware avatar visibility** - every entity with `m_lifeState` data hides on death, including sparse-sample primaries (recorder/spectator). Augmented by `player_death` / `player_spawn` events when the SendProp lagged.
- ✅ **Player aliases sidebar badge** - slots that changed name mid-demo show `was X, Y` next to the current name; primary detection matches the recorder's header nick against any alias.

- ✅ **Derived jump threshold** - `--jump-threshold` defaults to `0` (auto). The viewer computes the 99th-percentile horizontal position delta from `WORLD_POSITIONS` and uses 2.5× that, clamped to [250, 2000]. CLI override still wins.
- ✅ **Keyboard round shortcuts** - `[` / `]` step through rounds; ignored while typing in form fields.
- ✅ **Per-player colour overrides** - click the hue swatch next to any name in the Players panel to open the OS colour picker. Updates path-line, avatar, label, sidebar swatch, death markers, console name colour, and minimap dot. Primary's avatar stays blue regardless (it's the YOU anchor).
- ✅ **Kill icon in event log** - the bottom event-table now mirrors the Console: every `player_death` row gets the weapon-class SVG icon in front of the event name.
- ✅ **GIF settings dialog** - clicking Record GIF opens a small popover with selectors for Camera mode / Playback speed (1× → 50×) / Capture FPS (5 / 10 / 15 / 20) / Duration (1–60s) / Scale (50 / 75 / 100 %). Settings persist to `localStorage` between sessions. Click outside the popover or hit Cancel to dismiss.

### Easy (a few hours each)

- *(all of the previously-listed easy items have shipped - see Done above. Open to suggestions.)*

- ✅ **Heatmap mode** - minimap density overlay binned into a 48×48 grid with a log-scaled viridis-ish palette. New `Heatmap` button in the Camera panel toggles it. Rebuilt on first activation from the current sample set (per-entity for MP demos, recorder trajectory for single-POV).
- ✅ **HUD overlay capture in GIF** - `HUD overlay` checkbox in the GIF settings dialog stamps the minimap and a player-name / time / speed strip onto every captured frame. Toggled per-recording, persisted to `localStorage`.
- ✅ **Regression tests** - `.github/workflows/regression.yml` runs `cargo build --release` then drives the binary against every `.dem` in `DEMOS TESTING/`, asserts the HTML parses as valid JS, and verifies a reference demo's parse stats haven't drifted (entity/sample/life counts).

### Medium (a weekend)

- **Damage / kill arcs** - connect attacker → victim on `player_hurt` / `player_death` in 3D (positions for both are already in the event stream).
- **Jump / bhop detector** - flag segments where vertical velocity + `IN_JUMP` indicate rocket-jumps or bhops; expose as a filter.
- **Source split** - carve `main.rs` into `bsp.rs`, `events.rs`, `html.rs`, `usercmd.rs` (currently ~2300 lines).

### Done (in v0.3.0 plans)

- ✅ **MP4 / WebM export** - new `Record Video` button uses the `MediaRecorder` API on the canvas's `captureStream()` to record actual video at the canvas's native resolution. Falls back through MP4/H.264 → WebM/VP9 → WebM/VP8 based on what the browser supports. Default 8 Mbps bitrate, honours the same camera / speed / duration settings as the GIF dialog.
- ✅ **Weapon-aware decoding** - `m_hActiveWeapon` SendProp + the wielded weapon entity's class name decoded together. Output as two new JSON tables: `ENTITY_WEAPONS` (per-player switch stream) + `WEAPON_CLASSES` (eid → short class name like `rocketlauncher_directhit`, `scattergun`, `bat`). Currently surfaced as a live readout below the input keys; ready to plug into per-shot fire markers, kill-feed icon selection, accuracy stats, etc.

- ✅ **In-browser parsing (WASM)** - `src/lib.rs` re-uses the same parser via `#[path = "main.rs"]`, exposes a `parse_demo_to_html(demo, bsp?, name, jump_threshold)` function via `wasm-bindgen`. The CLI flow stayed intact; lib + bin build from the same code. `scripts/build-wasm.sh` produces `web/demoscope.js` + `web/demoscope_bg.wasm` (~600 KB after wasm-bindgen). `web/index.html` is the drag-and-drop UI - drop a `.dem` (and optionally `.bsp`) and the parser runs in-page. Verified end-to-end on a 14 MB demo: ~1.6 s parse, identical output to the CLI.
- ✅ **CS:S decode fixes** - velocity-based teleport detection (path-line, interpolation, speedometer), 0.6 s `m_lifeState` flicker coalesce, fire-tracer historical-tick fix in `mpPrimaryPositionAt`, and a `Hide specs` toggle that uses `svc_SetView` intervals to suppress spec/observer entities. Tested on the 77-demo SUBMISSION corpus: 68 / 77 produce valid HTML, 9 / 77 are correctly rejected as non-HL2DEMO formats (newer GMOD, SFM, CSGO Source 2, Titanfall, HL2BETA).

### Hard (a real project)

- **L4D1 / L4D2 entity tracks** - DataTables now decode (v0.4.0); the remaining blocker is the L4D net-message map (`svc_PacketEntities` lands at a renumbered id). Histogram with `DUMP_MSG=1`, confirm the id→name table + PacketEntities header layout with an L4D `engine.dll` bit-trace (`scripts/p2_bittrace.js` repointed), then add an L4D remap to `scan_game_payload` like the Portal 2 one. Full breakdown in [Investigating Stanley Parable & L4D2](#investigating-stanley-parable--l4d2).
- **CS:GO / Source 2 entity decode** - CS:GO's DataTables and PacketEntities are protobuf-encoded (and Source 2 / CS2 / Dota 2 are protobuf throughout), so they need a protobuf message reader rather than the bit-packed SendTable path. CS:GO already parses inputs/names/camera; this is the entity-track layer.
- **Proto-4 game events** - Portal 2 / Stanley decode positions but no game events yet (their event schema differs from the TF2/CS:S filters).
- **Side-by-side demo diff** - overlay two demos in the same scene; useful for jump-map comparisons and speedrun analysis.
- **Streaming parse** - mmap or chunked I/O for 1 GB+ STV demos that currently get fully loaded into memory.
- **VPK / VMT material loading** - decode the materials referenced by each face so the BSP overlay shows the actual map textures, not just untextured wireframe (qbyte's SourceImporter shows this is doable without external deps).
