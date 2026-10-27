# Architecture

How demoscope is laid out, and how a demo file becomes a viewer.

## Crate shape

One package, two targets that share the same source:

| Target | Entry | Purpose |
|--------|-------|---------|
| Binary `demoscope` | `src/main.rs` | The CLI |
| Library `demoscope` (`cdylib` + `rlib`) | `src/lib.rs` | Native callers and the WebAssembly build |

`src/lib.rs` pulls `main.rs` in as a module (`#[path = "main.rs"] mod cli`) so the
parsers are not duplicated. On `wasm32` it adds a `#[wasm_bindgen]` shim,
`parse_demo_to_html`, which is what the hosted viewer in `web/index.html` calls.

## Source layout

```
src/
├── main.rs, lib.rs      CLI and library entry points
├── header.rs            HL2DEMO container header + usercmd parsing
├── check.rs             `--check` decode self-check (PASS / WARN / FAIL)
├── source/              Source 1 (HL2DEMO / GMODEMO)
│   ├── packets.rs, datatable.rs, packetentities.rs, sendprop.rs, stringtable.rs
│   ├── events.rs        game-event decoding and per-game event filters
│   ├── player_tracks/   walks the demo and builds per-entity position/life tracks
│   ├── multi_player.rs  the MultiPlayerData model shared with the Quake path
│   └── csgo/            CS:GO's protobuf-based message, SendTable and entity layers
├── source2/             Source 2 (PBDEMS2: CS2, Dota 2, Deadlock)
│   ├── container, parser, serializer, fieldpath, entities, stringtable, snappy, …
│   └── vpk, resource, kv3, vphys, map   CS2 map overlay (VPK → collision mesh)
├── goldsrc/             GoldSrc (HLDEMO): Half-Life 1, CS 1.6, DoD, CZ
├── quake/               Quake family: q1.rs (NetQuake), qw.rs (QuakeWorld), q2.rs, q3.rs
├── bsp/                 Map geometry: source.rs (VBSP), goldsrc.rs, quake.rs
├── protobuf/            Minimal hand-written protobuf reader
├── render/
│   ├── json.rs          Serialises the parsed model into JSON for the page
│   └── html/            mod.rs (Source), engines.rs (Quake / Source 2 / GoldSrc),
│                        diff.rs (ghost overlay), template.html, dual_template.html
└── util/                bitreader, byte helpers, protocol constants
```

Non-Rust pieces: `web/index.html` (the drag-and-drop page that loads the wasm
build), `scripts/` (wasm build, viewer check and the Portal 2 trace tooling),
`tests/fixtures/` (small demos for the viewer check), `assets/` (logo
SVGs) and `.github/workflows/` (`regression`, `release`, `pages`).

## How a demo is routed

The wasm shim (`lib.rs`) and HTML generation (`render/html/mod.rs`) pick an
engine by sniffing the file, in this order:

1. **Source 2**: `source2::is_source2` (`PBDEMS2` magic).
2. **Quake**: `quake::detect`, which goes by file extension (`.dem`, `.dm2`,
   `.mvd`, `.qwd`, `.dm_66`…). It declines any file that starts with the
   `HL2DEMO`, `GMODEMO` or `HLDEMO` magic.
3. **GoldSrc**: `goldsrc::is_goldsrc` (`HLDEMO` magic).
4. **Source 1**: everything else goes through the `HL2DEMO` header parser.

The CLI summary path (`main.rs`) checks GoldSrc, then Source 2, then Quake. The
result is the same, because the Source 2 and GoldSrc magics don't overlap with
each other or with Source 1, and `quake::detect` declines `HLDEMO`.

Quake detection is extension-based, so `quake::detect` repeats the Source and
GoldSrc magic checks to keep a `.dem` file from another engine from being
misread as NetQuake. The extension is still the only signal for Quake files: a
renamed file can be misdetected (a `.dm2` renamed to `.dem` is read as
NetQuake, and any `.dm_NN` is read as Quake 3, including Quake Live).

## Data flow

```
demo bytes ─► engine decoder ─► MultiPlayerData / events / metadata ─► render::json ─► template.html ─► one .html file
                                         ▲
map bytes ─► bsp::extract_any_bsp ───────┘   (optional map overlay)
```

- Each engine decoder produces the same `MultiPlayerData` model (per-entity
  position and angle tracks, names, life/spectator state), so the viewer,
  minimap, heatmap and camera modes work unchanged across engines.
- `render::json` serialises that model by hand (no `serde`) to keep the
  dependency list and the wasm binary small.
- `template.html` holds the whole viewer (HTML, CSS and JavaScript). The
  generator splices the JSON into it and writes a single self-contained file.
  `dual_template.html` is a thin wrapper that embeds two generated viewers in
  side-by-side iframes for `--diff-split`.
- Both templates are compiled into the binary and the wasm build with
  `include_str!`. After editing them, rebuild the CLI to see the change in
  `--html` output, and run `./scripts/build-wasm.sh` to update the
  drag-and-drop page in `web/` (the pages workflow does this on deploy).

## Checking the viewer

`./scripts/check-viewer.sh` generates a viewer for each demo in
`tests/fixtures/` (or the demos you pass), loads it in headless Chromium,
starts playback, and fails on any JavaScript error. The regression workflow
runs it on every push. Playwright and its Chromium are installed on first run
into `~/.cache/demoscope-playwright`; they are not project dependencies.

## Adding support for a new engine or game

1. **A new game on an existing engine** is usually a small change. For example,
   a Portal 2-engine mod is typically one line in `DataTableQuirks::for_game` in
   `src/source/datatable.rs`; see [PROTO4.md](PROTO4.md#investigating-a-new-proto-4-game)
   for the `DUMP_*` switches that show where a decode goes wrong.
2. **A new engine family** needs:
   - a module with an `is_<engine>` / `detect` function and a `parse` function
     that returns `MultiPlayerData`;
   - a route in `main.rs` and in the wasm shim in `lib.rs`, placed before any
     extension-based fallback;
   - a `generate_<engine>_html` function in `render/html/engines.rs`;
   - a branch in `check.rs` so `--check` covers it.
3. Add the game to [COMPATIBILITY.md](COMPATIBILITY.md) and the
   [CHANGELOG](../CHANGELOG.md).
