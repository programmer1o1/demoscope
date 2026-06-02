# Proto-4 (Portal 2 / Stanley Parable / L4D) decode

Demos with `demo_protocol = 4` (Portal 2, Aperture Tag, Stanley Parable, L4D1/L4D2, old CS:GO ≤ 2020) share the `HL2DEMO` magic and bit-level wire conventions of proto-3 but diverge in several layers. Each had to be peeled back in order before a single entity position came out. **Portal 2, The Stanley Parable, and L4D1/L4D2 now fully decode**; only Source-1 CS:GO remains (protobuf).

The layers, outermost to innermost:

1. **Signon-block skip.** Proto-4's first `DEM_SIGNON` packet has its embedded `length` field set to `0` even though the signon section is hundreds of KB. The walker fast-forwards using the header's `sign_on_length` when `demo_protocol > 3`.
2. **DEM command shift.** Proto-4 inserted `DEM_CUSTOMDATA = 8`, pushing `DEM_STRINGTABLES` from 8 → 9. Remapped in the walker.
3. **Splitscreen preamble.** Per the Alien Swarm SDK ([`NicolasDe/AlienSwarm`](https://github.com/NicolasDe/AlienSwarm), `src/public/demofile/demoformat.h`), the proto-3 single `Split_t` (76 bytes) became `Split_t[MAX_SPLITSCREEN_CLIENTS]`. **Portal 2 / Stanley = 2, L4D1/L4D2 = 4.** Pinned to 2 for known Portal 2-engine games; a length-probe (try N = 4, 2, 1) is the fallback for unidentified proto-4 games. The probe can false-positive - a puzzlemaker-export demo probed as 4 and desynced - which is why known games are pinned.
4. **Net-message ID remap.** Portal 2 renumbers the net messages (`NetSplitScreenUser`@3, `SvcSplitScreen`@22 new; `SvcPrint` 7→16; NetTick/StringCmd/SetConVar/SignonState each −1). `scan_game_payload` remaps to canonical IDs. This is what got `svc_PacketEntities` headers reading aligned. **L4D1/L4D2 share this exact map** (verified in L4D1 `engine.dll`: `NET_Tick::GetType()`=4, `SVC_Print`=16, `SVC_UserMessage`=23), so the remap is applied to L4D too via `remap_msgs` - but **decoupled** from the 12-bit `svc_UserMessage` width, which is Portal 2-only (L4D reads `v & 0x7FF` = 11 bits).
5. **`svc_ServerInfo` / `svc_PaintMapData`.** Proto-4 `svc_ServerInfo` uses a 4-byte `mapCrc` (not TF2's 16-byte hash) plus a 32-bit `unk`; Portal 2 adds `svc_PaintMapData` at msg ID 33. (Details cross-checked against [`NeKzor/sdp`](https://github.com/NeKzor/sdp).)
6. **SendProp flags = 19 bits + 8-bit priority.** What NeKzor/sdp reads as "16-bit flags + 11-bit unk" is really `SPROP_NUMFLAGBITS_NETWORKED = 19` plus an 8-bit priority byte (same 27-bit total). `normalize_portal2_flags` maps the shifted bit positions back to TF2-canonical so flatten + decode stay engine-agnostic. Gated by `DataTableQuirks::portal2_extra_bits`.
7. **Entity-index + field-index encodings** (cracked with IDA Pro on `engine.dll`, tracing `CL_ParsePacketEntities → CL_CopyNewEntity → RecvTable_MergeDeltas → CDeltaBitsReader::ReadNextPropIndex`):
   - Entity-index delta = `ReadUBitInt` (6-bit base, low 4 kept, + 4/8/28-bit extension `<< 4`), **not** the 2-bit-selector UBitVar of TF2/CS:S.
   - Field index = a `new_way` bit read **once per entity** by the `CDeltaBitsReader` ctor, then per-prop: consecutive fast-path → 3-bit small gap → 7-bit base with `0x20/0x40/0x60` → `2/4/7`-bit extension. Missing the ctor's pre-read left every prop one bit out of phase.
8. **Flatten priority-sort - the final bug.** This is what blocked real positions even after 1-7 were correct. `SendTable_SortByPriority` (Source `dt.cpp`) arranges flat props by ascending priority, claiming each prop at the **first** pass where `prop.priority == pass` **OR** `(changes_often && pass == 64)`. We were forcing *every* `CHANGES_OFTEN` prop to priority 64; in reality a changes-often prop whose own priority is below 64 (e.g. `m_vecOrigin` @ priority 2) is claimed at its low priority, not at 64. The wrong assumption scrambled the flat table after the first ~8 props. See `src/source_demo/datatable.rs::sort_by_priority`.

**How the last bug was found (the reusable method).** Static disassembly had confirmed every *individual* decoder matched the engine, yet decode still drifted within the player's first ~8 props - the consecutive-index fast-path hid exactly where. The fix came from a **runtime bit-trace**: `scripts/p2_bittrace.js` (Frida) hooks `CDeltaBitsReader::ReadNextPropIndex` in `engine.dll` while playing the demo and logs, per prop, the field index and the bit position before/after. The engine's per-class **instance-baseline** stream walks *every* prop in flat order, so it yields an authoritative width-per-flat-index fingerprint for the class. Diffing that against demoscope's flat-table dump (`DUMP_FLAT=<class_id>`) showed our props were ordered differently - `m_flFallVelocity`(17 bits) sat at index 5 but the engine had it at 10. That pinned the priority-sort. Full writeup and the captured trace: `scripts/p2_engine_trace.md`. Tooling: `scripts/p2_bittrace.js` + `scripts/run_trace.py`.

**Net result.** Portal 2 and Stanley Parable decode real `m_vecOrigin` positions, eye-yaw (`m_angEyeAngles[1]`), and full DataTables. Two follow-ons completed the playback: the player class carries two `m_vecOrigin` copies (a live predicted one at low priority and a stale duplicate) - `scrape_player_state` takes the first in flat order; and demos with no usercmds synthesize their playback timeline from the decoded entity track (`src/template.html`). All proto-4 fixes are gated on `demo_protocol >= 4` / `portal2_extra_bits` with zero TF2/CS:S regression.

## Investigating a new proto-4 game

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
| `DUMP_MSG=1` | every raw 6-bit net-message id. Pipe through `sort \| uniq -c \| sort -rn` to histogram a new game's message enum - the once-per-packet ids are `net_Tick` and the `svc_PacketEntities` equivalent. |

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
| L4D2 | `left4dead2` | 2100 | 4 | 278 classes ✅ | **Works** - net-message remap added; `demogaty.dem` = 447 position samples on `c8m2_subway`. |
| L4D1 | `left4dead` | 1041 | 4 | 222 classes ✅ | **Works** - same engine net-message map (verified in L4D1 `engine.dll`). |

**Why Stanley "just worked":** it's the Portal 2 engine (Source 2013 SP) under a different `game_dir` and `net_protocol`. Adding `"thestanleyparable"` to the Portal 2-engine list in `DataTableQuirks::for_game` (`src/source_demo/datatable.rs`) flipped it from `0 classes` to `237 classes, 2226 position samples`. If you find another Portal 2-engine mod, the fix is likely one line in that list - confirm with `DUMP_SCAN` showing `N classes` after the change.

**L4D DataTables - solved (v0.4.0).** The single `portal2_engine` boolean used to conflate three independent things; they're now decoupled into separate axes:

1. **Splitscreen count** - driven from a per-game table (L4D = **4**, Portal 2 / Stanley = 2); the length-probe stays as fallback for unidentified proto-4 games.
2. **Command enum** - L4D's newer enum inserts `dem_customdata` at 8 and moves `dem_stringtables` to **9**; `userinfo` lives in that cmd-9 block.
3. **Flag format + `m_nBits`** - found by sweeping flag-width × priority × `m_nBits` for a sane class count (`DUMP_SCAN`): **L4D1 = 16-bit TF2 flags, no priority, `m_nBits` 6** → 222 classes; **L4D2 = 19-bit Alien-Swarm flags + 8-bit priority, `m_nBits` 6** → 278 classes. The shared L4D quirk is the **6-bit `m_nBits`** field (`bit_count_bits` in `DataTableQuirks`) - everything else uses 7, and a one-bit miscount desyncs the table walk. `is_portal2_engine()` (container: splitscreen=2 + message remap) is now separate from `portal2_extra_bits` (flag format), so L4D2 can share the flag format without the Portal 2 container.

**The net-message map - solved (v0.5.0).** DataTables decoded, but `svc_PacketEntities` never fired because the per-packet `net_Tick` (raw id 4, not the canonical 3) was misread as `net_StringCmd`, desyncing the cursor. L4D turns out to use the **exact same renumbering as the Portal 2 engine** - confirmed by reading L4D1's `engine.dll` directly over the IDA Pro MCP bridge: each net-message vtable's `GetType()` returns its id (`NET_Tick`=4, `SVC_Print`=16, `SVC_UserMessage`=23). So the existing Portal 2 remap in `scan_game_payload` is now applied to L4D via a `remap_msgs` flag - but kept **independent** of the 12-bit `svc_UserMessage` width, since L4D's `SVC_UserMessage::ReadFromBuffer` reads `v & 0x7FF` (11 bits) where Portal 2 reads `v & 0xFFF` (12). Reading the engine's `GetType()` constants directly (vtable slot 7; slot 9 = `GetName`; slot 4 = `ReadFromBuffer`) is faster and more authoritative than the histogram-then-Frida approach this guide originally prescribed.

**Game events - solved (v0.5.0).** Four walkers had to be made proto-4-aware (all now aligned with `scan_game_payload`): (1) the `DEM_CUSTOMDATA` command (cmd 8, `id(4)+length(4)` header) was misread as a bare length by `iterate_demo_packets`, `parse_userinfo_from_demo`, and the CLI display loop, desyncing the demo-command stream right after the signon - on Portal 2 this dropped every game packet (collection 0 → 2231); (2) `detect_splitscreen` could mis-probe Portal 2 as 4 splitscreen slots instead of 2 (now pinned); (3) the event parser (`extract_events_from_payload`) didn't apply the net-message remap; and (4) it `break`'d on `svc_Sounds` (17), `svc_UpdateStringTable` (13), and the Portal 2 `svc_CmdKeyValues`/`svc_PaintMapData` (32/33), which lead most proto-4 packets, so it never reached `svc_GameEvent` (25). With all four fixed, L4D2 extracts game events correctly (`demogaty.dem` displayed events 322 → 324; the walker now reaches all 52 networked `svc_GameEvent` messages in sync, most being types the default display filter drops). Single-player Portal 2 / Stanley demos legitimately carry **no** networked `svc_GameEvent` messages (events fire locally and aren't serialized), so they report 0 - correct, not a desync (verified: zero id-25 in the stream while the walker reaches `svc_PacketEntities` 2136×).

## Known gap: CS:GO full-snapshot detail

Portal 2 / Stanley demos that *start at a level load* decode because the engine sends each entity as an **Enter-PVS** update (class + serial + props), which the decoder reads cleanly. Mid-game Portal 2 recordings (speedrun finale segments) historically desynced on the leading full snapshot - that turned out to be the 12-bit `svc_UserMessage` length bug, fixed in v0.5.0 (see [CHANGELOG.md](../CHANGELOG.md)). The remaining true gap is **CS:GO (Source 1)**, whose DataTables and PacketEntities are protobuf-encoded and need a separate protobuf message reader rather than the bit-packed SendTable path.
