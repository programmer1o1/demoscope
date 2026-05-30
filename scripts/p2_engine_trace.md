# Portal 2 engine ground-truth trace (class 107 = local player)

Captured via Frida (`p2_bittrace.js`) hooking `CDeltaBitsReader::ReadNextPropIndex`
on `engine.dll`, playing `youareamoron`. Three streams seen per entity:

- `0x4f65db4` = the **packet** stream (advances monotonically across entities;
  27 bits of entity-header between entity #0 end and entity #1 start). This is
  what demoscope's Rust decoder reads.
- `0x4f65dc4`, `0x4f65de8` = per-class **instance baselines** (restart at idx 0
  each entity; separate buffers). The baseline walks EVERY prop in flat order,
  so it reveals the engine's bit-width for each flat index.

## Engine flat table for class 107 (flat index : value bits)

Derived from baseline stream 0x4f65dc4, entity #1:

```
 0:8   1:32  2:64  3:32  4:32  5:32  6:32  7:64  8:32  9:10
10:17 11:6  12:10 13:10 14:10 15:5  16:2  17:10 18:8  19:32
20:11 21:11 22:21 ...
```

## Engine packet (delta) props actually sent for the player, stream 0x4f65db4:

flat indices present (value bits):
```
0:8  1:32  2:64  3:32  4:32  5:32  6:32  7:64  8:32
10:17  17:10  20:11  21:11
62:8  63:8  64:8  66:8  67:8 .. 76:8
78:8  79:8 .. 94:8
207  224  249  433  434  435  436  485  506
```

## Diagnosis

Our flat table places m_flFallVelocity(17 bits) at index 5 and
m_vecPunchAngle(6 bits) at index 6. The engine places 17 at index **10** and
6 at index **11**. Our flatten/priority-sort emits these props ~5 slots too
early => decode desyncs after the first handful of props.

ROOT CAUSE: flatten / priority-sort order in datatable.rs does not match
Valve's SendTable_SortByPriority + BuildHierarchy gather order.

## Field-index encoding: VERIFIED CORRECT

ReadNextPropIndex reproduces e.g. delta 112 (94->207) via the 0x20 case
(base read 48, ext2=3 -> 11 bits), matching engine fiBits=11. read_field_index
in packetentities.rs is correct.

## RESOLUTION (fixed)

Root cause was `sort_by_priority` in datatable.rs forcing every CHANGES_OFTEN
prop to priority 64. Valve's `SendTable_SortByPriority` claims a prop at the
FIRST ascending priority pass where `prop.priority == pass` OR
`(changes_often && pass == 64)`. A CHANGES_OFTEN prop whose own priority is
below 64 (e.g. m_vecOrigin @ prio 2) is claimed at its own low pass, not at 64.
Fixing the predicate made demoscope's flat table for class 107 match the engine
fingerprint exactly (m_flSimulationTime@0:8, m_nTickBase@1:32, m_vecOrigin@2:64,
... m_flFallVelocity@10:17, m_vecPunchAngle@11:6, ...).

Two follow-on fixes for full Portal 2 positions:
1. The player class carries two m_vecOrigin copies (a live predicted one at a
   low priority, a stale duplicate at a higher one). scrape_player_state now
   takes the FIRST in flat order (the live one) and never overwrites it.
2. Splitscreen-count auto-detect false-positived as 4 on a puzzlemaker-export
   demo. Portal 2-engine games are pinned to MAX_SPLITSCREEN_CLIENTS=2.

Verified: youareamoron 242 samples, puzzlemaker 1288 samples, real sp_a2_core
coordinates. No regression: Portal 1 (2063), TF2 (2892), CS:S (1074).
