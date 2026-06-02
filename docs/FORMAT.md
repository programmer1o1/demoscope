# Demo format reference

Magic: `HL2DEMO\0`

## Packet layout

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

**Proto-4 differences** (see [PROTO4.md](PROTO4.md) for the full chain):
- The cmd IDs shift at 8: `8 = DEM_CUSTOMDATA` (new, length-prefixed), `9 = StringTables`.
- The `democmdinfo` preamble in cmd 1/2 is a `Split_t[MAX_SPLITSCREEN_CLIENTS]` array - **76 × N bytes** (N = 2 for Portal 2 / Stanley, 4 for L4D) - in place of the single 76-byte struct.
- The first `DEM_SIGNON`'s embedded `length` is `0`; use the header's `sign_on_length` to skip the signon block.

## democmdinfo layout (76 bytes, all cmd=1/2 packets)

```
flags          (i32)
viewOrigin     (3× f32) ← recorder eye position in world units
viewAngles     (3× f32)
localViewAngles(3× f32)
viewOrigin2    (3× f32)
viewAngles2    (3× f32)
localViewAngles2(3× f32)
```

## UserCmd bit format (CBitBuf, LSB-first)

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

## Button masks

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

## Proto-4 SendTable / PacketEntities wire format (Portal 2 engine)

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

## CSV column reference

```
tick, cmd_num, pitch, yaw, roll, fwd, side, up, buttons, impulse, weapon, mousedx, mousedy
```

Empty fields = unchanged from previous frame. Fill-forward to get the current value at any tick.

## Notes

- **Recorder position** is read from `democmdinfo.viewOrigin` in every cmd=1/2 packet - the engine-recorded eye position. Captures rocket jumps, teleporters, and all game physics accurately.
- **Per-player positions** are extracted from `CTFPlayer.m_vecOrigin` / `m_vecOrigin[2]` SendProps in `svc_PacketEntities` messages. Names and SteamIDs come from the `userinfo` string table.
- **Spectators** don't get `m_vecOrigin` updates after spawn - demoscope falls back to `viewOrigin` for sparse-sample entities so their avatar, path, and minimap dot still track the actual viewpoint.
- **BSP lookup** searches the demo's directory and the binary's directory for `<map_name>.bsp`. LZMA-compressed lumps are decompressed automatically.
- Truncated demos (no `dem_stop` packet) parse cleanly up to the last complete packet.
