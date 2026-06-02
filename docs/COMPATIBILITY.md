# Compatibility

## Source 1 (HL2DEMO)

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
| Portal 2 / Aperture Tag / Portal Stories / Portal Reloaded | ✅ Entity positions, eye-yaw, full DataTables (306 tables / 235 classes). Real `m_vecOrigin` decode verified against an `engine.dll` bit-trace. Mid-game / SAR speedrun (`fullgame`) recordings now decode too (the 12-bit `svc_UserMessage` fix). The event parser is proto-4-aware, but single-player Portal demos carry no networked `svc_GameEvent` messages (events fire locally), so the event log is empty for them - correct, not a gap. |
| The Stanley Parable | ✅ Same Portal 2-engine path - 237 classes, real positions (2226 samples on the sample demo). `net_protocol = 1000` but shares the 19+8 flag format, splitscreen = 2, and message-ID remap. |
| L4D1 / L4D2 | ✅ Per-player entity positions, eye-yaw, weapon switches, names, life states - plus inputs and recorder camera path. DataTables (222 / 278 classes - L4D1 = 16-bit flags + `m_nBits` 6; L4D2 = 19+8 + `m_nBits` 6). The final piece was the net-message map: L4D shares the Portal 2 engine's renumbering (`NetTick = 4`, `SvcPrint = 16`), verified against L4D1 `engine.dll`. `demogaty.dem` (L4D2) = 447 position samples + 324 game events on `c8m2_subway`. L4D's `svc_UserMessage` length stays 11-bit (Portal 2 widened it to 12). |
| CS:GO (Source 1, ≤ 2023) | ⚠ Parses end-to-end: inputs, names/SteamIDs (big-endian `player_info_s`), recorder camera path. Entity tracks need a protobuf decoder (DataTables + PacketEntities are protobuf-encoded). 0 entity tracks for now. |
| Garry's Mod 13+ (`GMODEMO`) | ⚠ The container is byte-for-byte HL2DEMO with only a renamed 8-byte magic, so it's accepted as a Source demo: inputs, game events, names, and the recorder camera path all decode. Entity tracks are 0 - GMod reports `demo_protocol=3` but its engine networks entities in the newer proto-4 style, so the proto-3 entity decoder desyncs (DataTables parse: 248 classes; `svc_PacketEntities` reached but yields 0 entities). Would need the same "header lies about the protocol" routing L4D/Stanley got. |
| SFM (DMX) / Titanfall (`R1DEMO`) / CS2 + Dota 2 (`PBDEMS2`, Source 2) | ✗ Different file format entirely - rejected on magic |

Multi-player entity decode is native on TF2 / CS:S / Portal 2 / Stanley Parable; on other Source 1 games it's best-effort and depends on common SendProp table names being present.

## Quake family

Separate engine lineage, decoded in `src/quake/`. Quake demos are recordings of the server's network stream, so entity **positions are already in the bytes** - the decoder reads them, it doesn't re-simulate. Each parser emits the same `MultiPlayerData` (per-entity position/angle tracks + names + death/spectate state) the Source path uses, so the full 3D viewer / minimap / heatmap / POV camera / **death-hiding** work unchanged. Files are routed by extension; HL2DEMO demos are never misclassified (the magic is checked first).

**Map overlay.** All three Quake BSP formats render if the matching `.bsp` is placed beside the demo (Quake 3 maps come in a `.pk3` - extract `maps/<name>.bsp` from it): **Quake 1** (version 29, shared with the GoldSrc decoder), **Quake 2** (`IBSP` v38 - edge/surfedge walk, 76-byte texinfo, SURF-flag sky/nodraw skip), **Quake 3** (`IBSP` v46 - meshvert-indexed drawverts, polygon + mesh faces; bezier patches are counted and skipped, tool/sky shaders dropped by name). Routed by magic/version through `bsp::extract_any_bsp`. Verified: `dm6.bsp` (Q1, 6750 tris), `base64.bsp` (Q2, 64962 tris), `cpm4.bsp` (Q3, 10711 tris) - on the cpm4 demo the recorder track sits 100% inside the decoded map bounds.

**Death & spectator hiding.** The viewer hides an avatar while it's dead or spectating (same machinery as Source `m_lifeState`/`m_iObserverMode`). Quake feeds it from: Q3 entity `eFlags & EF_DEAD` (death→respawn) + the recorder's `pm_type == PM_DEAD` + players whose configstring team is `TEAM_SPECTATOR`; Q2 the recorder's `pm_type` (`PM_DEAD`/`PM_GIB`). The recorder is **not** hidden for `PM_SPECTATOR` - in-eye/POV demos sit there the whole time and that camera is what you follow.

| Game | Status |
|------|--------|
| **Quake 2** (`.dm2`, protocol 34) | ✅ Per-player tracks (playerstate origin + packetentities delta), viewangles, names, map, recorder death-hiding. Byte/short oriented, no compression. Verified on a real DM demo (5 players, 9,464 frames, 11.3k samples); effect ops incl. the variable-length `svc_temp_entity` are decoded so the stream stays in sync. |
| **Quake 3 Arena** (`.dm_66`-`.dm_68`, protocol 66-68) | ✅ Huffman-compressed bitstream + delta-compressed snapshots; player tracks, viewangles, names, map, per-player death-hiding + spectator-team hiding. Static Huffman tree plus the `entityState`/`playerState` field tables and delta logic are ported verbatim from GPL ioquake3 (`qcommon/msg.c`, `qcommon/huffman.c`); the 256-entry frequency table is byte-checked against source. Verified on 3 demos (q3tourney6, cpm4, pro-q3dm6). **`.dm_71`/`.dm_73`/`.dm_90`+ (Quake Live) are NOT supported** - QL extends the netfield tables, so it's detected and warned but the decode will desync. |
| **Quake 1** (NetQuake `.dem`, protocol 15) | ✅ Entity/player position tracks, recorder POV angles, names, map (`CL_ParseServerMessage` / `CL_ParseUpdate`). Verified on real DM demos (`boodwand2` "The Dark Zone", `1134_rd1` "The Abandoned Base" - 12 players each, ~24k frames, names de-quaked from the colour/gold-font charset so the `1134` clan tag etc. read correctly). |
| QuakeWorld (`.qwd` / `.mvd`) | ⚠ Detected but not decoded - QuakeWorld uses an extension-negotiated protocol (the sample carries FTE extensions) distinct from NetQuake. Clean error, no crash. |

## GoldSrc (HLDEMO)

The Quake-derived engine behind Half-Life 1 and its mods - a separate lineage from both Source (`HL2DEMO`) and id Quake. Decoded in `src/goldsrc.rs`. Routed by the `HLDEMO\0\0` magic, which is checked before the Quake `.dem` fallthrough so GoldSrc demos are never misclassified as NetQuake.

| Game | Status |
|------|--------|
| **Half-Life 1 / CS 1.6 / DoD / Condition Zero** (`HLDEMO`, demo proto 5, net proto 48) | ✅ Container + recorder POV camera. The 544-byte header (map, game dir, protocols), the directory at byte 540, the 92-byte directory entries, and the frame stream (9-byte `type/time/frame` header; NetMsg / DemoStart / ConsoleCommand / ClientData / NextSection / Event / WeaponAnim / Sound / DemoBuffer) are parsed and verified byte-for-byte against a real Condition Zero demo (`cs_assault`, `czero`, **95.78 s / 7916 frames**, directory math exact). The **recorder camera path renders**: eye origin + view angles come from each NetMsg `RefParams` (`vieworg` @ body+4, `viewangles` @ body+16, `msg_length` @ body+464). Frame layout transcribed from the open-source `hldemo` parser - no IDA needed (GoldSrc's structs are public). The walker resyncs past the occasional length-quirk frame (≈26 / 24k on the sample) so the trajectory is complete - `cs_assault` = 7916 samples across the full map. The **GoldSrc BSP map overlay** also renders (version 30 / Quake-1 version 29): same lump-based face walk as VBSP but a 15-lump header, 20-byte `dface_t`, 40-byte texinfo, and no LZMA/displacements; sky and tool brushes (`sky*`, `aaatrigger`, `clip`, …) are dropped by texture name. Verified on `cs_assault.bsp` (2805 verts / 5547 tris) - the map geometry envelops the recorder trajectory exactly. Drop the `.bsp` next to the demo, same as Source. **Other-player entity tracks** (the delta-compressed `svc_packetentities` stream) are not decoded yet - see [ROADMAP.md](../ROADMAP.md). |

## Smoke-test corpus

A **450-demo corpus** across 16 game folders (TF2, CS:S, CS:GO, HL2 + episodes, DoD:S, Portal, Portal 2, Stanley, L4D1/L4D2, DIPRIP, Dystopia, plus Source 2 cs2/dota2 and HL2-beta) is run through the full `--html` pipeline as a release smoke check, in a **debug build** (so integer-overflow checks are on - `--release` silently wraps them). Latest full-sweep results:

| Outcome | Count | What it means |
|---|---|---|
| ✅ Valid HTML viewer produced | **440 / 450** | parsed cleanly to `DEM_STOP`, no crash |
| ✗ Rejected on magic | 10 / 450 | 6 × CS2 + 3 × Dota 2 (`PBDEMS2`, Source 2) and 1 × HL2-beta (`HLDEMO`) - not `HL2DEMO`. Clean error, no panic. |
| 💥 Panics / crashes | **0** | the overflow-hardened walkers held across the whole corpus |

Of the 440 valid viewers, by position source:

| Position source | Games |
|---|---|
| Multi-player entity tracks (`m_vecOrigin`) | TF2, CS:S, DoD:S, HL2DM, Portal, **Portal 2**, **Stanley Parable**, **L4D1**, **L4D2**, multiplayer HL2 |
| Single-POV camera path (`viewOrigin`) | single-player HL2 / EP1 / EP2 / Portal - the recorder's own `m_vecOrigin` isn't networked, so they render the recorder camera path (correct, not a gap) |
| Camera path + names, no entity tracks | **CS:GO (Source 1)** - parses fully; per-player entity tracks need a protobuf decoder |

Multi-player entity decode behaves well on TF2 + CS:S (per-player tracks, weapons, eye-yaw, life states), on Portal 2 + Stanley Parable (recorder positions, synthesized playback timeline, dense `democmdinfo.viewAngles` POV camera), and now on L4D1 / L4D2 (per-player survivor/infected tracks, weapons, eye-yaw, life states). On HL2 single-player and Portal 1, only the recorder's `viewOrigin` is captured and the viewer renders the playthrough path. CS:GO (Source 1) parses end-to-end (inputs, names, recorder camera) but yields zero MP entity tracks (its DataTables + PacketEntities are protobuf-encoded).
