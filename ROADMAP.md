# Roadmap

Forward-looking work, ranked roughly easy → hard. PRs welcome. For everything
already shipped, see [CHANGELOG.md](CHANGELOG.md).

## Direction & non-goals

demoscope's niche is **breadth × visualisation × zero system dependencies**: one
zero-install tool that turns almost any GoldSrc / Source / Quake demo into a
shareable, self-contained 3D scene. It is not a stats/analytics engine
(demoinfocs-golang, clarity, CS Demo Manager), a speedrun verifier (p2sr/mdp),
or a parsing library you code against (NeKzor/sdp, tf-demo-parser): those each
go deeper in one engine; demoscope goes wider and renders. New work is weighed
against that niche: widen engine coverage and improve the viewer; don't chase
analytics depth or pull heavy dependencies.

- ~~**Non-goal: Source 2 / CS2 / Dota 2 (`PBDEMS2`).**~~ **Shipped** (see
  Recently shipped). It was the "worst ROI" call: a from-scratch field-path
  serializer (FlattenedSerializer + Huffman) + snappy + a new container, none of
  which reuses the SendTable pipeline, but it's now implemented end-to-end in
  `src/source2/` and decodes CS2 with zero failed packets.

## Recently shipped

- **Ancient Source/GoldSrc usability**: ✅ done. GMod 9/10 Classic and HL2 old
  engine (`net_protocol 7`) now decode recorder inputs, camera angles, and a
  recorder position track; GoldSrc/Source 2/Quake files also route correctly in
  normal CLI mode instead of being rejected as bad `HL2DEMO` magic.
- **HL2 old-engine entity decoder**: ✅ core decoder done. The matching
  November 2004 `engine.dll` established the two missing rules: entity and field
  gaps share an unsigned Exp-Golomb code, and flattened SendProps undergo a
  `SPROP_CHANGES_OFTEN` front-shuffle after hierarchy construction. Packet
  application is transactional. `gmancheapledemo` now produces a native
  1,976-sample CHL2_Player path instead of relying solely on viewOrigin.
- **GoldSrc multi-map layers**: ✅ done. Repeat `--map FILE` or drop several
  BSPs in the browser to render split HL1 campaign maps together in one viewer.
  Consecutive campaign maps now also align automatically: matching
  `info_landmark` targetnames produce the same translation GoldSrc applies at a
  changelevel, chained across the stack; camera and player tracks switch into
  the translated layer when the raw coordinate stream crosses map bounds.
- **CS gameplay depth pass (CS:S / CS:GO / CS2)**: ✅ done (see
  [CHANGELOG.md](CHANGELOG.md)). A wide round of CS-family features: **real input
  monitoring** (CS2 held-button mask off the movement services; Portal 2 usercmds
  unlocked by fixing the proto-4 `DEM_CustomData` walk), **held weapon** (CS2),
  **kill attribution** (CS2 `*_pawn` handle remap → who killed whom), **round/bomb
  events** (CS:S/CS:GO real, CS2 synthesized from `m_totalRoundsPlayed`),
  **grenade landing markers + thrower + trajectory** (CS2/CS:GO, with molotov-fire
  attribution via the `CInferno` owner on CS2), **GOTV player names** (CS:GO, via
  `player_connect` backfill), **avatar hide-on-PVS-gap** (no more frozen phantoms),
  and a **route-cutoff fix** (teleport threshold raised so fast movement isn't
  chopped).
- **CS2 / Source 2 (`PBDEMS2`) per-player tracks + gameplay**: ✅ done (see
  [CHANGELOG.md](CHANGELOG.md) / [COMPATIBILITY.md](docs/COMPATIBILITY.md)). A
  complete from-scratch Source 2 pipeline in `src/source2/` (~2.6k lines): a
  dependency-free Snappy decompressor, the PBDEMS2 container walk, a
  `CSVCMsg_FlattenedSerializer` model with **(name, version)**-accurate
  serializer linking, the canonical field-path Huffman tree, every Source 2
  field decoder (quantized / coord / normal / qangle variants / sim-time /
  **pointer-boolean** / …, matched against LaihoE-demoparser + clarity), string
  tables, instance baselines via `CDemoStringTables`, and the
  `CSVCMsg_PacketEntities` delta walk. Surfaces **player world positions**,
  eye-angles, **names** (incl. the loopback host via the `userinfo` table),
  **death/respawn** (`m_lifeState`), **game events** (kills / bomb / round /
  mvp / grenades), and an **economy + scoreboard** (money, K/D/A, score, team).
  Verified on a full `de_nuke` match: 10 players, ~190K samples, **0 failed
  packets**, 703 events. The one decisive bug was a pointer field terminating in
  a 1-bit boolean (not a varint); fixing it took decode from desyncing to 100%.
- **GMod 13 (`GMODEMO`) per-player entity tracks**: ✅ done (see
  [CHANGELOG.md](CHANGELOG.md)). Cracked with GMod's own `engine.dll` in IDA. Three
  GMod-specific encodings, all gated on `game_dir=="garrysmod"` so no other game is
  touched: (1) `MAX_EDICT_BITS = 13` (8192 edicts) widens the PacketEntities
  count fields + removed list + entity-index bound; (2) `svc_PacketEntities`
  `length` is **24 bits**, not 20 (`SVC_PacketEntities::ReadFromBuffer`): the
  4-bit miss was desyncing every entity body; (3) `svc_CreateStringTable` uses a
  hybrid layout (16-bit max_entries + **varint** length + a compressed bit) so the
  signon string-table walk stays aligned. Entity index / prop index / SendProp
  value decoders are all the same as TF2 (confirmed against `CL_ParseDeltaHeader` /
  `RecvTable_Decode` / `sub_1018ABE0`). Verified: `type6interloperedgmod` decodes
  100% of entity packets → 2 players with map-sane coords; `garrythirteen` 93% →
  player track across gm_construct. No regression on TF2 / Portal 2 / CS:GO.
- **CS:GO per-player entity tracks**: ✅ done (see [CHANGELOG.md](CHANGELOG.md)).
  A dependency-free protobuf reader (`src/protobuf/`) + a `csgo` layer
  (`src/source/csgo/`) decode `CSVCMsg_SendTable` / `CSVCMsg_PacketEntities`
  through the existing flatten / SendProp / entity machinery and surface tracks in
  the viewer. This was the high-value breadth play before the later Source 2 work.
  **Map overlays verified** end-to-end: `ar_monastery.bsp` (VBSP v21) loads through
  the version-agnostic BSP loader and all 7 decoded players (5604 samples) sit
  inside the map geometry on every axis.

## CS2 follow-ups (now that the decoder is in)

- ~~**Scoreboard panel in the viewer.**~~ ✅ **done**: a **floating scoreboard
  window** (opened from the sidebar or **S**, Esc to close) splits players by
  team (T / CT) and shows **K / D / A / K/D / Score / MVP / $** plus
  distance/top-speed/time-alive, sortable on any column, click-a-row-to-follow.
  One shared `template.html` overlay lights up for CS:S, CS:GO **and** CS2 (all
  three embed the same per-player metadata fields); non-CS multi-player demos get
  the movement columns.
- ~~**`player_death` events → on-map kill markers.**~~ ✅ **done**: the Deaths
  overlay (✕ markers at death sites, you → all → off) now pairs each death with
  its `player_death` game event: **headshot kills get a gold ring**, and the
  killer / weapon / distance ride along on each marker (`mpDeathPoints`, for the
  minimap + future hover). Deaths the `m_lifeState` prop missed (it doesn't
  always re-network before the next big delta) are **gap-filled** straight from
  the events, so the on-map death count matches the scoreboard. Verified on
  `s.dem` (CS:S): 9 deaths all marked, killer/weapon resolved, 4 headshot rings,
  no double-draws.
- ~~**CS:GO game events (near-free reuse of the CS2 decoder).**~~ ✅ **done** :
  `src/source/csgo/events.rs` decodes `CSVCMsg_GameEvent` / `CSVCMsg_GameEventList`
  (a near-verbatim port of the CS2 decoder; identical proto, ids 25/30) into the
  event timeline. Verified on `ar_monastery`: real `player_death`
  (attacker/weapon/headshot/distance), `round_mvp`, `round_end`, bomb events.
  (CS:S, being older bit-packed Source 1, already decoded events via the
  existing `svc_GameEvent` scanner.)
- ~~**Economy + scoreboard for Source 1 (CS:S / CS:GO).**~~ ✅ **done**: CS:S
  and CS:GO now reach parity with CS2. **Money + team** (`m_iAccount` /
  `m_iTeamNum`) come off the player entity; **kills / deaths / assists / score /
  MVPs** come off the `CCSPlayerResource` scoreboard arrays. Those arrays are
  engine-generated `SendPropArray`s whose elements flatten to bare per-slot
  indices (`000`..`063`) with the array name dropped, so flattening now records
  an `array_parent` on each leaf (`m_iScore.001`, `m_iDeaths.001`, …) to recover
  them, indexed by player entity id. The result is embedded in each player's
  metadata JSON exactly like CS2 and surfaced by the viewer's scoreboard panel.
  **Verified:** `s.dem` (CS:S full match) → top fragger 61K/3D, 14 MVPs, sane
  money; `monasterydemo` (CS:GO GOTV) → 7 players with K/D/A + MVPs + cash.
  CS:S ships only `m_iScore` (the frag count), so kills falls back to score there.
- ~~**CS2 map overlay (`.vmap_c`).**~~ ✅ **done**: CS2 demos now render their
  world geometry behind the player tracks, the last missing render piece for the
  CS2 family. CS2 maps are VPK-packed Source 2 resources, not VBSP; rather than
  the meshopt-compressed render meshes scattered across 806 model instances, the
  overlay decodes the single `maps/<map>/world_physics.vmdl_c`, whose one `PHYS`
  block holds the whole world collision mesh as plain vertex/triangle arrays: a
  wireframe hull exactly like the existing BSP overlay.
  - **Pipeline (all pure-Rust):** a VPK v2 reader (`src/source2/vpk.rs`), the
    Source 2 resource block container (`resource.rs`), a full **KV3 v5 decoder**
    with header and zstd buffer/blob decompression (pure-Rust `ruzstd`) plus the
    dual-buffer typed value-tree walk (`kv3.rs`), a physics shape walker
    (`vphys.rs`: `m_meshes` plain arrays + `m_hulls` half-edge convex hulls,
    fan-triangulated), and a `.vpk` → base64 verts/idx adapter (`map.rs`) feeding
    the viewer's existing `__BSP_VERTS__`/`__BSP_IDX__` path (zero template
    change). The KV3 v5 layout + node-type enum were pinned against
    ValveResourceFormat and the real file.
  - **Wired both ways:** the CLI resolves `<map>.vpk` beside the demo (the map
    name comes from `svc_ServerInfo`, blank in the header on loopback recordings);
    the WASM build reuses the optional second buffer, so the drag-and-drop UI
    accepts a `.bsp` *or* a `.vpk`.
  - **Verified end-to-end:** `de_nuke.vpk` → **5988 hulls + 11 meshes → 115,738
    verts / 191,628 triangles**; the `w.dem` CS2 match embeds that geometry next
    to 10 player tracks, whose coordinates sit inside the map bounds (same space →
    aligned). Render-mesh textured geometry (the meshopt codec + instance
    aggregation) remains out of scope; the collision hull is the overlay.

## Next up

Ranked by value. These are bounded follow-ups, not release blockers.

- **Ballistic-physics grenade arc.** The current arc is an aim-launched bezier
  *model*; ~30% of POV throws fall back to a plain bow because the thrower's eye
  angle wasn't networked at the throw moment. A real lob (launch from eye along
  aim at CS's throw speed, integrate under gravity to the detonation point) would
  fill those and be physically faithful without needing the (local-frame,
  unusable) projectile entity.
- **Deadlock live input.** Deadlock's `CCitadelPlayer_MovementServices` networks
  only `m_nToggleButtonDownMask` (toggle state) + subtick move timing: no live
  WASD mask like CS2. Real input would need decoding the analog
  `m_arrForceSubtickMoveWhen` / ability system. Dota likewise (MOBA, no FPS WASD).
- **CS:GO molotov-fire attribution.** CS:GO doesn't network the `CInferno`
  owner (CS2 does), so its 1–2 fires/demo stay unattributed. Would need
  position/timing matching to the molotov throw: low value.

## Hard (a real project)

- **GMod 9/10 Classic remote-player completeness**: the shared net-7 core is
  now authoritative and HL2 old-engine entity tracking works. GMod Classic also
  decodes its initial CGMOD_Player/world snapshot plus partial remote entities
  and yaw, but later mod-specific SendProp variants remain too sparse for a
  complete local entity path on `zombieslayer`. Demoscope keeps those recovered
  channels and selects the dense recorder camera for entity 1 when native
  coverage is below 25%. Completing remote-player fidelity needs a matching
  GMod 9/10 `engine.dll`, not more guesses against the HL2 retail binary.
