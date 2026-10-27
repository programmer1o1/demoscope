# Output contract

demoscope has three kinds of output, with different promises:

| Output | Promise |
|--------|---------|
| CLI flags, exit codes, `--csv`, `--json`, `--check` status words | **Stable.** Covered by semver from 1.0. |
| The tables embedded in the HTML viewer | **Internal and unstable.** Documented here as they are; may change in any release. |
| Human-readable text (default output, `--summary`, `--all`, stderr messages, the `--check` table layout) | Not covered. Don't parse it. |

Both stable outputs are produced by Source 1 demos only (`--csv`, `--json`).
Everything else works on every engine through `--html` and `--check`.

## 1. Stable contract

### Command line

| Flag | Meaning |
|------|---------|
| `<demo-file>` | The demo to read. Must be the first argument, except for `--check`. |
| `--summary` | Print header information only. |
| `--all` | Print every packet (Source 1 only; other engines print the header and a note). |
| `--csv` | Print usercmds as CSV (Source 1 only). |
| `--json` | Print usercmds as a JSON array (Source 1 only). |
| `--html [FILE]` | Write the interactive viewer. Default path: the demo path with an `.html` extension. |
| `--diff D` | Overlay a second Source 1 demo as ghosts (with `--html`). |
| `--diff-split D` | Side-by-side dual viewer instead of an overlay (with `--html`). |
| `--jump-threshold N` | Path-break distance in world units. Default: derived from the data. |
| `--map FILE` | Add a BSP / VPK map layer. Repeatable. |
| `--check <file\|dir>` | Decode self-check. |
| `--help`, `-h` | Print usage and exit 0. |

Standard output carries the requested data. Diagnostics go to standard error.
With `--csv` and `--json`, nothing but the data is written to stdout. Passing
`--csv` and `--json` together is an error: nothing is written to stdout, a
message goes to stderr and the exit code is 1.

### Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success. Also `--help`, `--check` runs with no `FAIL`, and a reader that closes stdout early (`demoscope demo.dem --csv \| head -n 1`): the run stops quietly with no message. |
| `1` | Any error: no arguments, unreadable file, unsupported or corrupt demo, a failed `--html`, `--csv` and `--json` together, `--csv` / `--json` on a non-Source 1 demo, or a `--check` run with at least one `FAIL` or with no supported demo files to check. |

A stdout write that fails for any other reason (for example a full disk) prints
an error to stderr and exits `1`. No other exit codes are promised. A panic
exits with Rust's default of 101 and is a bug, not part of the contract.

### `--csv`

Source 1 only. On any other engine the command writes nothing to stdout, prints
`--csv is only supported for Source 1 demos` to stderr and exits 1.

The first line is the header. It currently reads:

```
tick,cmd_num,pitch,yaw,roll,fwd,side,up,buttons,impulse,weapon,mousedx,mousedy
```

Consumers must read columns by header name, not position: columns can be added
(and reordered) in a minor release. One row per usercmd, in demo order. An empty
field means the value is unchanged from the previous row (fill forward to get
the current value).

| Column | Type | Unit / meaning |
|--------|------|----------------|
| `tick` | integer | Demo tick of the packet. |
| `cmd_num` | integer | Usercmd command number. |
| `pitch`, `yaw`, `roll` | decimal, at least 4 places | Degrees. |
| `fwd`, `side`, `up` | decimal, at least 2 places | Movement request in Source units. |
| `buttons` | integer | Button bitmask (`IN_*` flags). |
| `impulse` | integer | Impulse command. |
| `weapon` | integer | Weapon-select value. |
| `mousedx`, `mousedy` | integer | Mouse delta. |

### `--json`

Source 1 only. On any other engine the command writes nothing to stdout, prints
`--json is only supported for Source 1 demos` to stderr and exits 1.

Stdout is one JSON array with one object per usercmd, in demo order. A demo
with no usercmds prints an empty array. A key is present only when the value
is present for that usercmd; a missing key means "unchanged from the previous
object", like an empty CSV field.

| Key | JSON type | Unit / meaning |
|-----|-----------|----------------|
| `tick` | integer | Demo tick. Always present. |
| `cmd` | integer | Usercmd command number. Always present. |
| `pitch`, `yaw`, `roll` | number | Degrees. |
| `fwd`, `side`, `up` | number | Movement request in Source units. |
| `buttons` | integer | Button bitmask. Always paired with `buttons_str`. |
| `buttons_str` | string | Active buttons joined by `\|` (`ATTACK\|JUMP`), or `none`. |
| `impulse` | integer | Impulse command. |
| `weapon` | integer | Weapon-select value. |
| `weapon_sub` | integer | Weapon subtype. JSON only: there is no CSV column. |
| `mousedx`, `mousedy` | integer | Mouse delta. |

Numbers are printed with at least 4 decimal places for `pitch`, `yaw` and `roll`
and at least 2 for `fwd`, `side` and `up`; the exact number of places is not
part of the contract, so don't compare the text. Key order and whitespace are
not part of the contract either. The golden test pins the exact bytes so that any change is
noticed and reviewed (see [Snapshot tests](#4-snapshot-tests)).

### `--check`

`demoscope --check <file|dir>` decodes each demo with its engine's decoder and
prints one row per demo. The first column is a status word:

| Word | Meaning |
|------|---------|
| `PASS` | Native tracks were decoded: player tracks that come from the demo's entity data, not only the recorder camera. |
| `WARN` | Usable, with a caveat: only the recorder-camera fallback produced tracks, or the result has partial packet loss. |
| `FAIL` | The decoder errored, panicked, or produced no tracks. |

The `decode` column says where the tracks came from: `native`, `mixed` (the
recorder's track is the camera copy but other players decoded natively),
`fallback` (the recorder camera only) or `-` (nothing decoded).

A Source 1 demo with no gameplay (about 0 ticks) and no tracks is `PASS` with
decode `-`. Before 1.0, `PASS` meant "tracks > 0" and camera-only demos passed;
they are now `WARN`.

Exit code: `0` when no demo is `FAIL` (`WARN` is not a failure), `1` otherwise.
A path that can't be read counts as `FAIL`. A directory with no supported demo
files prints a message to stderr, writes nothing to stdout and exits `1`. A
directory sweep covers `.dem`, `.dm2`, `.mvd`, `.qwd` and every `.dm_NN` file.
Quake 3 files outside `.dm_66`-`.dm_68` (Quake Live) are experimental: they are
`WARN`, never `PASS` and never `FAIL`, whether or not they decode, so they can't
make a sweep exit `1`. (A file that can't be read at all is still `FAIL`.) A single-file check and a sweep give the
same result for the same file.

Apart from the status word, the layout of a row, including the `decode` column,
is informational and not covered.

## 2. Embedded HTML tables (internal, unstable)

Every `--html` page contains one `const NAME = <literal>;` line per table,
filled from the decoded demo. They are the interface between the decoders and
the viewer. They are **not** a public API: names, shapes, units and per-engine
availability can change in any release, including patch releases. Use `--json`
or `--csv` for stable data.

This section documents the tables as they are in this release, including their
inconsistencies. Nothing here is a promise.

Conventions:

- Coordinates are engine world units, as recorded (Source units, Quake units).
  The viewer swaps axes itself.
- `tick` is an engine tick. Its length depends on the engine; see `META.tick_rate`.
- Engines: S1 = Source 1, S2 = Source 2, GS = GoldSrc, Q = Quake family.
- "empty" means `[]`, `{}` or `""`.
- Output is deterministic for the test fixtures (a test runs each twice) and for
  nearly all of a larger local corpus. It is not guaranteed in general: see known
  inconsistency 11.

| Table | Shape | S1 | S2 | GS | Q |
|-------|-------|:-:|:-:|:-:|:-:|
| `META` | `{map, client, server, game, demo_protocol, duration, ncmds, tick_rate, jump_threshold}` | yes | yes | yes | yes |
| `CMDS` | `[[tick, pitch, yaw, fwd, side, buttons, weapon], …]` | yes | empty | empty | empty |
| `LIFE_BREAKS` | `[index, …]` into `CMDS` | yes | empty | empty | empty |
| `TELEPORT_BREAKS` | `[index, …]` into `CMDS` | yes | empty | empty | empty |
| `EVENTS` | `[{event, tick, …}, …]` | yes | yes | empty | empty |
| `WORLD_POSITIONS` | `[[tick, x, y, z], …]` | yes | empty | yes | empty |
| `BSP_VERTS` | base64 string, little-endian `f32` x, y, z | map | `.vpk` | map | map |
| `BSP_IDX` | base64 string, little-endian `u32` indices | map | `.vpk` | map | map |
| `BSP_SPAWN` | `[x, y, z]` | yes | `[0,0,0]` | yes | yes |
| `VIEW_SWITCHES` | `[[start, end], …]` | yes | empty | empty | empty |
| `ENTITY_TRACKS` | `{eid: [[tick, x, y, z], …]}` | yes | yes | yes | yes |
| `ENTITY_NAMES` | `{eid: {name, steam_id, user_id, is_fake, is_hltv, aliases, …}}` | yes | yes | yes | yes |
| `GHOST_EIDS` | `new Set([eid, …])` | `--diff` | empty | empty | empty |
| `PRIMARY_ENTITY` | number or `null` | yes | yes | yes | yes |
| `ENTITY_LIFE_STATES` | `{eid: [[tick, state], …]}` | yes | yes | yes | yes |
| `ENTITY_OBSERVER` | `{eid: [[tick, mode], …]}` | yes | empty | empty | QuakeWorld |
| `ENTITY_YAWS` | `{eid: [[tick, yaw, pitch], …]}` | yes | yes (empty on the Dota 2 sample) | yes | yes |
| `VIEW_ANGLES` | `[[tick, pitch, yaw], …]` | yes | empty | yes | yes |
| `ENTITY_WEAPONS` | `{eid: [[tick, weapon_id], …]}` | TF2, CS:S, CS:GO | CS2 | yes | empty |
| `WEAPON_CLASSES` | `{weapon_id: "name"}` | TF2, CS:S, CS:GO | CS2 | yes | empty |
| `ENTITY_BUTTONS` | `{eid: [[tick, mask], …]}` | empty | CS2 client recordings | empty | empty |

Notes per table:

- **`META`.** `duration` is seconds. `ncmds` counts the engine's own unit
  (usercmds, packets or frames). `jump_threshold` is `0.0` unless
  `--jump-threshold` was given; the viewer then derives it.
- **`CMDS`.** Strided to at most 20,000 entries; the last usercmd is always kept.
- **`EVENTS`.** Values are strings, numbers, booleans or `null`. CS2 adds
  synthetic `round_end` events.
- **`WORLD_POSITIONS`.** The recorder's camera path. On Source 1 this is
  `democmdinfo.viewOrigin`, which is the recorder's origin, not its eye. On
  GoldSrc it is `RefParams.vieworg`, which is the eye.
- **`ENTITY_TRACKS`.** The keys are every entity that has samples, a name or a
  life state, so a key can map to an empty array.
- **`GHOST_EIDS`.** Only with `--diff`. Ghost entity ids are offset by 900000.
- **`ENTITY_LIFE_STATES`.** `0` is alive. Other values are engine specific.
- **`BSP_*`.** Empty when no map file is found beside the demo. On Source 2 the
  mesh comes from a matching `.vpk`.
- **`ENTITY_BUTTONS`.** Masks use the classic `IN_*` layout (attack 1,
  forward 8, back 16, left 512, right 1024; jump and duck from separate flags).
  Not present on GOTV recordings, Deadlock or Dota 2.

### Known inconsistencies

Recorded as they are. They are not being fixed in this release.

1. **`ENTITY_NAMES` has a different shape per engine.** Source 1 and Quake
   entries carry 13 keys (`name, steam_id, user_id, is_fake, is_hltv, aliases,
   kills, deaths, assists, money, score, mvps, team`). Source 2 carries the same
   without `mvps`. GoldSrc carries only the first six. On Quake the economy and
   scoreboard keys are always `0` and `steam_id` is empty.
2. **Placeholders and synthetic values.** `steam_id` is `""` on most engines
   and `"STEAM_ID_PENDING"` on the Portal sample. On Source 2 and GoldSrc,
   `aliases` holds only the current name, synthesized to equal `name`. On the
   Condition Zero sample, non-recorder players are named `player N`.
3. **`weapon_id` means different things.** On Source 1 and GoldSrc it is the
   weapon *entity* id, and `WEAPON_CLASSES` maps that id to a class name. On
   Source 2 it is the weapon *class* id.
4. **Weapon names are not normalized.** Names are lowercased class names with
   engine-specific prefixes left in place (`rocketlauncher`, `cweaponmedigun`,
   `cknife`, `deagle`).
5. **`META` fields mean different things.** `game` is the game directory on
   Source 1 and GoldSrc (`tf`, `czero`), the final component of the game
   directory on Source 2 (`csgo`, `citadel`, `dota`; the recorder's absolute
   path is dropped), and a family id on Quake (`quake1`, `quakeworld`, `quake2`, `quake3`).
   `demo_protocol` is the demo protocol on Source 1 and GoldSrc, the network
   protocol on Source 2 (for example 14165 on CS2, 48 on Deadlock) and the game
   protocol on Quake (15, 28, 34, 68). On Quake 1 and 2, `map` is the level's
   title, not the file name.
6. **Tick length differs.** `tick_rate` is 64-66.67 on Source, 82.64 (derived)
   on GoldSrc, 1000 on Quake 1, QuakeWorld and Quake 3 (ticks are
   milliseconds) and 10 on Quake 2.
7. **`META.duration` can be `0.0`.** The CS:GO GOTV sample has no playback time
   in its header.
8. **Float precision differs per table.** Coordinates and `CMDS` values use 3
   decimals with trailing zeros stripped (`11877.875`, `-8048`). `ENTITY_YAWS`
   uses a fixed 1 decimal (`117.8`, `0.0`). `VIEW_ANGLES` uses a fixed 2
   decimals. `META.duration` and `META.tick_rate` use 2 decimals,
   `jump_threshold` 1.
9. **Point budgets thin long tracks.** `ENTITY_TRACKS`: 4,000 points per entity;
   Quake uses 12 per second, clamped to 4,000-20,000. `ENTITY_YAWS`: 1,500 per
   entity. `VIEW_ANGLES`: 6,000. Large single-step jumps keep both ends.
   `CMDS`: 20,000. Shorter tracks are unchanged.
10. **Availability depends on how the demo was recorded.** For example,
    `ENTITY_BUTTONS` is filled for CS2 client recordings and empty for a CS2
    GOTV match, and `ENTITY_WEAPONS` is empty on the Portal 2 sample.
11. **Output is not always byte-stable.** Found by running a local corpus
    repeatedly (44 demos, 3 runs each). Two cases still differ from run to run in
    the data: a CS:S SourceTV demo gave 27 or 31 `VIEW_SWITCHES` entries, and a
    Dota 2 demo gave a first track sample at tick 781 or 784 for one entity. Every
    other demo in that corpus, with or without a map, was byte-identical.
    (Earlier releases also wrote `BSP_VERTS` / `BSP_IDX` in a run-dependent order;
    that is fixed.) `--json` and `--csv` are not affected, and the fixtures are
    byte-stable.

## 3. Versioning policy

demoscope follows semantic versioning from 1.0. The versioned surface is the
stable contract in section 1.

**Major (2.0, 3.0, …)** changes:

- removing or renaming a CLI flag;
- removing or renaming a `--csv` column or a `--json` key;
- changing the type or unit of a `--csv` column or `--json` key, the coordinate
  convention of a value, or what a field means (even if the fix is a bug fix);
- changing an exit code or the meaning of one;
- renaming `PASS`, `WARN` or `FAIL`, or changing which results make `--check` exit non-zero.

**Minor (1.1, 1.2, …)** changes:

- adding a CLI flag;
- adding a `--json` key (consumers must ignore keys they don't know);
- adding a `--csv` column (consumers read columns by header name);
- adding a supported engine, format or game.

**Patch (1.0.1, …)** changes: bug fixes that keep the contract, including
decoder fixes that change *values* in the output for a demo that was decoded
wrongly before. A decoder fix is a patch unless it changes a unit, a coordinate
convention or the meaning of a field; that is a major change.

**Not versioned:** the embedded HTML tables, the viewer, the HTML file
contents, human-readable text, stderr wording, and the layout of the `--check`
table. These can change in any release. Changes to them go in the changelog
when users would notice.

Every change to the stable contract needs a `CHANGELOG.md` entry, and a
changed or removed item needs a major-version bump.

## 4. Snapshot tests

`tests/golden.rs` runs every file in `tests/fixtures/` and compares:

- the full `--json` output with `tests/golden/<fixture>.json`, where `--json`
  applies (Source 1 fixtures);
- a digest of the embedded HTML tables with `tests/golden/<fixture>.digest.txt`.
  For each table the digest has the name, kind, entry count, sample count (for
  `{eid: [samples]}` tables), size, an FNV-1a hash of the literal, and the first
  and last element. It never contains a full track dump.

`tests/cli_contract.rs` covers exit codes and the `--csv` / `--json` rejection
on other engines.

A new fixture without golden files fails the tests. After an *intentional*
output change, regenerate the snapshots and review the diff:

```bash
UPDATE_GOLDEN=1 cargo test --test golden
git diff tests/golden/
```

A change to `--json` output is a contract change (section 3). A digest change
only means the internal tables changed; the commit message should say why.
