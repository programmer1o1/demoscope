# Credits

demoscope stands on a lot of prior reverse-engineering work. No Valve source was
used for any of the proprietary net protocols. Every wire format was either read
from a public reference implementation, taken from open-source engine code, or
recovered from a running engine with IDA / Frida. This file lists what was
consulted and who supplied the demos used to verify it.

## Reference implementations & parsers

These open-source parsers were diffed against, ported from, or cross-checked for
wire-format details. demoscope reuses none of their code wholesale except where
noted as a direct port.

| Project | Engine | Used for |
|---------|--------|----------|
| [tf-demo-parser](https://github.com/demostf/parser) (icewind1991) | Source 1 / TF2 | Bit-for-bit parity target for the SendTable / SendProp / PacketEntities pipeline (`src/source/packetentities.rs`, `src/source/datatable.rs`) |
| [demoinfocs-golang](https://github.com/markus-wa/demoinfocs-golang) | Source 1 / CS:GO | CS:GO entity field-index encoding (separated index/value lists) |
| [NeKzor/sdp](https://github.com/NeKzor/sdp) | Source 1 / Portal 2 | proto-4 `svc_ServerInfo` / `svc_PaintMapData`, SendProp flag bit positions, net-message renumbering (`docs/PROTO4.md`) |
| [dotabuff/manta](https://github.com/dotabuff/manta) | Source 2 | Direct Rust ports of the quantized-float decoder, field-path parser, bit reader, string table and parser flow (`src/source2/*`) |
| [LaihoE/demoparser](https://github.com/LaihoE/demoparser) | Source 2 / CS2 | Source 2 field decoders (quantized / coord / normal / qangle) and the field-path Huffman tree |
| [skadistats/clarity](https://github.com/skadistats/clarity) | Source 2 | Source 2 field-path operation definitions, decoder cross-validation |
| [khanghugo/dem](https://github.com/khanghugo/dem) | GoldSrc | Delta-compression protocol field tables and entity-state structure (`src/goldsrc/entities.rs`) |
| [YaLTeR/hldemo-rs](https://github.com/YaLTeR/hldemo-rs) | GoldSrc | GoldSrc demo container format (`src/goldsrc/mod.rs`) |

## Open-source engine & format references

| Source | Used for |
|--------|----------|
| [ioquake3](https://github.com/ioquake/ioq3) (GPL) - `qcommon/msg.c`, `qcommon/huffman.c` | Quake 3 Huffman bitstream, delta-snapshot field tables, static frequency table (`src/quake/q3.rs`) |
| Quake 2 source (GPL) - `qcommon/qcommon.h`, `client/cl_ents.c` | Quake 2 entity-state delta bit flags (`src/quake/q2.rs`) |
| [ValveResourceFormat](https://github.com/ValveResourceFormat/ValveResourceFormat) | KV3 v5 binary layout + node-type enum for CS2 map overlays (`src/source2/kv3.rs`) |
| [NicolasDe/AlienSwarm](https://github.com/NicolasDe/AlienSwarm) SDK - `demoformat.h`, `dt_common.h` | Splitscreen proto-4 format, `SPROP_*` SendProp flag defines |
| Valve Developer Community - [VPK format](https://developer.valvesoftware.com/wiki/VPK_(file_format)) | VPK v2 reader (`src/source2/vpk.rs`) |
| [google/snappy](https://github.com/google/snappy/blob/main/format_description.txt) | Snappy frame format for Source 2 decompression (`src/source2/snappy.rs`) |
| [ruzstd](https://github.com/KillingSpark/zstd-rs) | Pure-Rust zstd for KV3 buffer/blob decompression |
| qbyte - SourceImporter | BSP displacement surface tessellation algorithm (`src/bsp/source.rs`) |

## Reverse-engineering tooling

Protocols with no public reference (Portal 2 priority-sort, GMod 13 encodings,
L4D net-message IDs) were recovered from the shipping engine binaries:

- **IDA Pro** - disassembly of `engine.dll` to extract proto-4 entity/field-index
  encoding (`CL_ParsePacketEntities`, `CDeltaBitsReader`), the SendTable
  priority-sort, L4D net-message IDs, Portal 2's 12-bit `svc_UserMessage`, and the
  three GMod 13 encodings (`MAX_EDICT_BITS`, 24-bit packet length, hybrid string table).
- **Frida** - runtime bit-tracing of `CDeltaBitsReader::ReadNextPropIndex`
  (`scripts/p2_bittrace.js`) to pin down the Portal 2 priority-sort bug.

## Demo contributors

Verification rests on real demos from real games. Thanks to everyone who recorded
and shared one:

- **Everyone** who worked on demo submissions for INTERLOPER ARG on Anomalous Materials discord server.

- **Anomidae** who made Tuesday Manifest demos for INTERLOPER ARG.

- **a6hawk** who provided Deadlock demos.

- **Quake Players** who made Quake Tournaments demos. 

- **Celestyn**  who provided Dota 2 Reborn, CSS and CS2 demos.

- **leough** for Portal 2 WR demo

If you supplied a demo that helped validate a format and you're not listed here,
open a PR or an issue - credit is owed and gladly given.

### Demos used for verification

The per-game decode was checked against the following recordings (see
[docs/COMPATIBILITY.md](docs/COMPATIBILITY.md) and [CHANGELOG.md](CHANGELOG.md)
for the detailed results):

| Game / engine | Demos |
|---------------|-------|
| GoldSrc | `cs_assault` (Condition Zero) |
| Counter-Strike: Source | `s.dem`, `111.dem` |
| Counter-Strike: Global Offensive | `monasterydemo` (GOTV, ar_monastery) |
| Counter-Strike 2 | `w.dem` (de_nuke) |
| Portal 2 / proto-4 | `youareamoron.dem`, `testingportal.dem`, `fullgame_…finale4.dem` (SAR) |
| Left 4 Dead 1 / 2 | `data23.dem`, `ellisfloor.dem`, `demogaty.dem` (c8m2_subway) |
| Garry's Mod 13 | `type6interloperedgmod`, `garrythirteen` (gm_construct), `gm_flatgrass` |
| Quake 1 (NetQuake) | `boodwand2`, `1134_rd1` |
| Quake 3 Arena | `cpm4`, `q3tourney6`, `pro-q3dm6` |

Plus a broader smoke-test corpus (~450 demos across TF2, CS:S, CS:GO, HL2 +
episodes, DoD:S, Portal, Portal 2, Stanley, L4D1/2, DIPRIP, Dystopia, CS2 and
Dota 2) used for crash/regression testing.
