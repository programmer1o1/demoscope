<p align="center">
  <img src="assets/logo/demoscope-banner.svg" alt="demoscope: pure-Rust parser and 3D visualiser for game demo files" width="100%">
</p>

A fast, pure-Rust parser and interactive 3D viewer for game demo files. No C toolchain needed.

- **Source 1** - TF2, Half-Life 2, CS:S, CS:GO, DoD:S, Portal, Portal 2, The Stanley Parable, L4D1/2, Garry's Mod
- **Source 2** - CS2, Dota 2, Deadlock
- **GoldSrc** - Half-Life 1, CS 1.6, Day of Defeat, Condition Zero
- **Quake** - Quake 1, QuakeWorld, Quake 2, Quake 3 Arena

Every format opens in the same viewer: a 3D scene with player paths, a minimap, heatmap, POV camera, scoreboard and kill arcs. Put the map file next to the demo to draw the level around the paths (a `.bsp`, or the map's `.vpk` for CS2).

See the **[full compatibility matrix](docs/COMPATIBILITY.md)** for per-game status.

---

## Installation

Download a pre-built binary from the [Releases](https://github.com/programmer1o1/demoscope/releases) page (Windows, macOS, Linux), or build from source:

```bash
cargo build --release
# binary: target/release/demoscope
```

Requires Rust stable (≥ 1.85). No external system libraries and no C toolchain are needed because every dependency is pure Rust. The CLI pulls only `base64`, `lzma-rs`, and `ruzstd` (a pure-Rust zstd decoder, used for CS2 map resources); the WASM build adds `wasm-bindgen` and `console_error_panic_hook` (gated to `target_arch = "wasm32"`). Everything engine-specific, including the protobuf reader, Snappy, and the Source 2 KV3/VPK stack, is hand-rolled in-tree.

### WASM (in-browser) build

demoscope also compiles to WebAssembly, exposing the parser to JavaScript so users can drag a `.dem` onto a webpage and skip the CLI entirely.

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli   # matching version in Cargo.lock
./scripts/build-wasm.sh
python3 -m http.server 8088 --directory web
# open http://localhost:8088/
```

Drop a `.dem` (and optionally the matching `.bsp`) onto the page. The viewer renders entirely in your browser - files never leave your machine. WASM parse time is ~5-8× slower than the native CLI (a 14 MB demo parses in ~1.6 s on Apple Silicon vs ~250 ms native), but for sharing demos with someone who doesn't have the CLI installed it's a meaningful upgrade.

The viewer template is compiled into the wasm build, so rerun `./scripts/build-wasm.sh` after editing `src/render/html/template.html`.

Build artifacts land in `web/` (the `.wasm` + JS glue are gitignored and built fresh - only `web/index.html` is committed). The `.github/workflows/pages.yml` workflow runs `build-wasm.sh` and deploys `web/` to GitHub Pages on every push to `master`, so the hosted viewer always matches source (enable it once under repo Settings → Pages → Source: "GitHub Actions"). The wasm entry point is `parse_demo_to_html(demo, bsp, name_hint, jump_threshold, diff, diff_name) -> Result<String, JsValue>` in `src/lib.rs`: `bsp` is an optional map buffer (a `.vpk` for CS2), `jump_threshold` of `0` means auto-derive, and `diff` / `diff_name` optionally add a ghost demo for comparison. Native consumers can call `generate_html_string`, re-exported from the same file.

---

## Usage

```
demoscope <demo-file> [OPTIONS]
```

| Flag | Description |
|------|-------------|
| *(none)* | Source 1: print usercmds; other formats: print a summary and HTML hint |
| `--all` | Also print non-usercmd packets (signon, packet, synctick, …) |
| `--csv` | Output usercmds as CSV (one row per frame) |
| `--json` | Output usercmds as a JSON array |
| `--summary` | Print header info and packet counts only |
| `--html [FILE]` | Generate self-contained interactive 3D HTML visualisation (always includes multi-player tracks) |
| `--diff FILE` | Overlay a second Source demo as translucent ghost tracks |
| `--diff-split FILE` | Compare two Source demos in a synchronized split viewer |
| `--map FILE` | Add a BSP/VPK map layer; repeat for multi-map GoldSrc viewers |
| `--jump-threshold N` | Override the automatically derived path-break distance |
| `--check FILE_OR_DIR` | Run the decoder self-check and fail if any demo cannot be decoded |

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

# Load several Half-Life/GoldSrc maps into one viewer
demoscope meow.dem --html --map c1a0.bsp --map c1a1.bsp --map c1a2.bsp

# Override the automatically derived path-break threshold
demoscope demo.dem --html --jump-threshold 400
```

The CSV columns are documented in [docs/FORMAT.md](docs/FORMAT.md#csv-column-reference).

---

## HTML visualisation

`demoscope --html` produces a single HTML file with all demo data embedded. Viewer libraries are loaded from a CDN, so the first open requires a network connection; they work offline afterward when the browser cache is available.

To preview a demo quickly, generate a viewer and open it directly, or serve its folder:

```bash
demoscope demo.dem --html out/index.html
python3 -m http.server 8088 --directory out
# open http://localhost:8088/
```

Place the `.bsp` map file in the same directory as the demo for the full 3D map overlay. Source VBSP (incl. LZMA-compressed lumps common in TF2 workshop maps), GoldSrc / Quake 1 (v30 / v29), Quake 2 (`IBSP` v38), and Quake 3 (`IBSP` v46) maps are all decoded automatically by version. Quake 3 maps ship inside a `.pk3` - extract `maps/<name>.bsp` and place it beside the demo.

For campaigns split across several GoldSrc maps, repeat `--map FILE` or drop
multiple BSPs into the browser. All layers render together in their native map
coordinates. When consecutive maps share an `info_landmark` (the usual case for
Half-Life campaign changelevels), they are aligned into one campaign space
automatically; maps with no shared landmark keep their native coordinates. See
[docs/COMPATIBILITY.md](docs/COMPATIBILITY.md#goldsrc-hldemo).

### Controls

| Input | Action |
|-------|--------|
| Left-drag | Orbit |
| Scroll | Zoom |
| Right-drag | Pan |
| Space | Play / pause |
| ← / → | Step one frame (hold Shift for 10) |
| ↑ / ↓ | Playback speed up / down (0.5×, 1×, 2×, 5×, 10×) |
| `[` / `]` | Previous / next round |
| S | Open / close the scoreboard (when the demo has scoreboard data) |
| Esc | Close the scoreboard, then the console overlay |
| Timeline scrubber | Seek |

### Features

| Section | Content |
|---------|---------|
| **3D scene** | The map (incl. displacement terrain) with each player's path |
| **Playback** | Play/pause, speed 0.5×-10×, timeline scrubber |
| **Header** | Map, game, client, server, duration, tickrate, protocol, and life / death / teleport counts |
| **Speedometer** | Current and peak horizontal speed |
| **Players** | One entry per player. Click to show or hide their path; right-click to make them the primary player (followed by the camera) |
| **Lives** | The primary player's lives; click one to jump to it |
| **Deaths** | Death markers: off → primary player only → everyone |
| **Rounds** | Detected rounds with Prev / Next / All, shaded on the timeline |
| **Camera modes** | Orbit · Follow · First Person |
| **Fire markers** | A marker at each shot, coloured by weapon |
| **Kill arcs** | An arc from killer to victim at each kill (headshots gold), with a legend |
| **Teleport arcs** | Arcs for teleporter / Eureka Effect events |
| **Minimap** | Top-down view of paths, deaths and positions, with an optional heatmap |
| **Event log** | Game events; click a row to jump to it |
| **Console** | Chat, kills, spawns, rounds and connects, with filters and a fullscreen view |
| **GIF / video export** | Record a GIF, or an MP4 / WebM video for higher quality |
| **Inputs** | Live WASD / mouse / jump / duck keys and the held weapon |
| **Hide specs** | Hides spectators, idle bots and the HLTV camera |

### Game-aware event filtering

`demoscope` reads the demo's `game_dir` and selects an appropriate event set. Custom filters for: `tf`, `cstrike` / `csgo`, `left4dead` / `left4dead2`, `portal` / `portal2`, `hl2mp`, plus a generic Source fallback.

---

## Documentation

| Doc | Contents |
|-----|----------|
| [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md) | Per-game support matrix, verification, and decoder notes |
| [docs/FORMAT.md](docs/FORMAT.md) | Demo file format, packet layout, UserCmd bit format, button masks, proto-4 SendTable/PacketEntities wire format, CSV columns, decode notes |
| [docs/OUTPUT.md](docs/OUTPUT.md) | Output contract: stable CLI / `--csv` / `--json` / `--check` behaviour, the internal HTML tables, versioning policy, and how to update snapshot tests |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Source layout, how a demo is routed to a decoder, and how to add a new engine |
| [docs/PROTO4.md](docs/PROTO4.md) | How proto-4 (Portal 2 / Stanley / L4D) decode was reverse-engineered, plus the env-gated trace switches for investigating a new proto-4 game |
| [scripts/p2_engine_trace.md](scripts/p2_engine_trace.md) | Raw `engine.dll` bit-trace of the Portal 2 player class, used as ground truth for PROTO4 |
| [CHANGELOG.md](CHANGELOG.md) | Release notes and the full list of completed work |
| [ROADMAP.md](ROADMAP.md) | Planned work |
| [CREDITS.md](CREDITS.md) | Reference implementations, format docs, RE tooling, and demo contributors |
