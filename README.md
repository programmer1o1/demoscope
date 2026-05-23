# demoscope

A fast Rust parser and interactive 3D visualiser for Source Engine demo files (`.dem`).

Supports Half-Life 2, HL2 Episodes, HL2: Lost Coast, HL2DM, Team Fortress 2,
Counter-Strike: Source, Day of Defeat: Source, Portal, Garry's Mod 9, and any
other Source 1 game that uses the **HL2DEMO** format (`demo_protocol = 2 / 3 / 4`).

---

## Build

```bash
cargo build --release
# binary: target/release/demoscope
```

Rust stable, no external dependencies beyond `base64`.

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
| `--html [FILE]` | Generate self-contained interactive 3D HTML visualisation |

### Examples

```bash
# Quick header check
demoscope demo.dem --summary

# Human-readable per-frame breakdown
demoscope demo.dem

# Pipe into other tools
demoscope demo.dem --csv | head -5
demoscope demo.dem --json | jq '.[0]'

# Generate interactive 3D HTML
demoscope demo.dem --html
# → demo.html  (open in any browser, no server needed)
```

---

## HTML visualisation

`demoscope --html` produces a single self-contained HTML file with all data
embedded. Three.js is loaded from a CDN (or works offline if already cached).

### Features

| Section | Content |
|---------|---------|
| **3D scene** | Drag to orbit, scroll to zoom, right-drag to pan |
| **Playback** | Play/pause (or Space), speed control 0.5×–10×, draggable timeline scrubber |
| **Per-life selector** | Sidebar checkboxes — toggle individual lives on/off |
| **Camera modes** | Orbit · Follow Player · First Person (player viewpoint) |
| **Player avatar** | Box mesh with view-direction arrow and frustum cone |
| **Death markers** | Red ✕ at every player_death position |
| **Teleport arcs** | Cyan bezier arcs for `player_teleported` events (Eureka Effect, teleporter exits) |
| **Round events** | Yellow markers for round_start / round_win |
| **BSP map overlay** | If a matching `.bsp` is in the same folder, spawn position is read for path alignment |
| **Event log** | Scrollable table of parsed game events — click a row to seek |

### Game-aware event filtering

`demoscope` recognises the demo's `game_dir` and selects the appropriate event
set. Custom sets for: `tf`, `cstrike` / `csgo`, `left4dead` / `left4dead2`,
`portal` / `portal2`, `hl2mp`, plus a generic Source fallback.

---

## Compatibility matrix

Tested against demos from many Source 1 games:

| Game | Status |
|------|--------|
| Team Fortress 2 | ✅ Full — events, BSP, life breaks, teleport arcs |
| Half-Life 2 (all versions) | ✅ Full |
| HL2: Episode One / Two | ✅ |
| HL2: Lost Coast | ✅ |
| HL2 Deathmatch | ✅ |
| Counter-Strike: Source | ✅ |
| Day of Defeat: Source | ✅ |
| Portal | ✅ |
| Garry's Mod 9 (legacy) | ⚠ header/BSP OK, usercmds skipped (net_protocol = 7) |
| HL2 Old Engine | ⚠ same — too old (demo_protocol = 2, net_protocol = 7) |
| L4D1 / L4D2 / Portal 2 / Stanley Parable | ⚠ packets parse, no DEM_USERCMD (proto 4 records input differently) |
| GMod 13+ / SFM / Titanfall / new CS:GO | ✗ Different file format entirely |

Old engines (`net_protocol ≤ 7`) print a clear note and skip usercmd parsing
rather than producing NaN garbage.

---

## Demo format reference

Magic: `HL2DEMO\0`

### Packet layout

All packets share a 5-byte prefix: `cmd (u8) + tick (i32)`.
For **demo_protocol = 4** (L4D, Portal 2, CS:GO …) there is an extra
`player_slot (u8)` byte after the prefix.

| cmd | Name | Payload |
|-----|------|---------|
| 1 | Signon | `democmdinfo(76) + in_seq(i32) + out_seq(i32) + length(i32) + data[length]` |
| 2 | Packet | same as Signon |
| 3 | SyncTick | *(nothing — 5 bytes total)* |
| 4 | ConsoleCmd | `length(i32) + data[length]` |
| 5 | **UserCmd** | `out_seq(i32) + length(i32) + data[length]` |
| 6 | DataTables | `length(i32) + data[length]` |
| 7 | Stop | *(5 bytes total)* |
| 8 | StringTables | `length(i32) + data[length]` |

### UserCmd bit format (CBitBuf, LSB-first)

Each field is preceded by a 1-bit **has-flag**; if 0 the field is unchanged
from the previous frame (delta encoding).

| Field | Bits | Notes |
|-------|------|-------|
| command_number | 32 | u32 |
| tick_count | 32 | u32 |
| pitch | 32 | raw IEEE-754 float (degrees) |
| yaw | 32 | raw IEEE-754 float (degrees) |
| roll | 32 | float (typically 0) |
| forwardmove | 32 | float, ±450 max |
| sidemove | 32 | float — positive = right, negative = left |
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

---

## CSV column reference

```
tick, cmd_num, pitch, yaw, roll, fwd, side, up, buttons, impulse, weapon, mousedx, mousedy
```

Empty fields = unchanged from previous frame. Fill-forward to get the current
value at any tick.

---

## Notes

- **Dead-reckoning** in the visualiser integrates `fwd`/`side`/`yaw` at 66 Hz
  to approximate the path. It does not account for collision, knockback,
  teleporters, or any server-side physics — the path is a rough approximation
  of actual movement.
- **BSP lookup** searches the demo's directory and the binary's directory for
  `<map_name>.bsp`. If found, the spawn position is read from the
  `info_player_teamspawn` / `info_player_start` entity for alignment.
- Truncated demos (no `dem_stop` packet) parse cleanly up to the last complete
  packet.
