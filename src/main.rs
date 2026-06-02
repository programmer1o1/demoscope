// This file is both the CLI binary entry point AND (via `#[path]`-include in
// lib.rs) the implementation sub-module for the wasm library. Many constants
// and helpers are reached only through `fn main()`, which the library build
// doesn't treat as a root entry point - so the dead-code lint fires across
// the board even though everything is used in the CLI path. Module-level
// allows mute that noise without affecting the CLI's analysis.
//
// The parser itself lives in the sibling modules declared below; this file is
// just the CLI shell (argument handling + the text/CSV/JSON packet dump) plus
// the module wiring. The byte-slice HTML entry points live in `html` and are
// re-exported here so `lib.rs` can reach them as `cli::generate_html_string`.
#![allow(dead_code, unused_imports)]

use std::env;
use std::fs::File;
use std::io::{self, Read, Write as IoWrite};
use std::path::{Path, PathBuf};

// Parser sub-modules (shared between the CLI binary and the wasm lib).
mod bitreader;
mod bsp;
mod bytes;
mod constants;
mod events;
pub mod goldsrc;
mod header;
mod html;
mod json;
mod packets;

mod source_demo;
mod multi_player;
pub mod quake;

// Re-export the byte-slice HTML entry points at the crate-module level so
// `lib.rs` (which pulls this file in as `mod cli`) can reach them unchanged.
pub use html::{generate_goldsrc_html, generate_html_string, generate_quake_html};

use self::bytes::{le_i32, read_cstring};
use self::constants::{
    DEM_CONSOLECMD, DEM_DATATABLES, DEM_PACKET, DEM_SIGNON, DEM_STOP, DEM_STRINGTABLES,
    DEM_STRINGTABLES_V2, DEM_SYNCTICK, DEM_USERCMD, HEADER_SIZE, SPLIT_SIZE,
};
use self::header::{fmt_buttons, parse_header, parse_usercmd};
use self::html::generate_html;
use self::packets::detect_splitscreen;

fn print_usage(prog: &str) {
    eprintln!("Usage: {prog} <demo.dem> [--all] [--csv] [--json] [--summary] [--html [FILE]]");
    eprintln!();
    eprintln!("  --all      Print every packet (not just usercmds)");
    eprintln!("  --csv      Output usercmds as CSV");
    eprintln!("  --json     Output usercmds as JSON array");
    eprintln!("  --summary  Print header info and packet counts only");
    eprintln!("  --html     Generate interactive 3D HTML visualization (always includes multi-player tracks)");
    eprintln!("  --jump-threshold N  Path-break distance in units (default: auto-derived from data)");
    eprintln!();
    eprintln!("Supports: TF2, CS:S, CS:GO, HL2, Portal, DOD, HL2DM, GMod, L4D, L4D2 (demo_protocol 2/3/4)");
}

// ─── Main ─────────────────────────────────────────────────────────────────────

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 || args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage(&args[0]);
        std::process::exit(if args.len() < 2 { 1 } else { 0 });
    }

    let filename = &args[1];

    // Check for --jump-threshold N. Default 0 = "auto" - the HTML template
    // will derive the cutoff from the 99th-percentile position delta when
    // META.jump_threshold is 0.
    let jump_threshold: f32 = {
        let mut v = 0.0_f32;
        if let Some(i) = args.iter().position(|a| a == "--jump-threshold") {
            if let Some(s) = args.get(i + 1) {
                if let Ok(n) = s.parse::<f32>() {
                    if n > 0.0 { v = n; }
                }
            }
        }
        v
    };

    // Check for --html flag
    let html_idx = args.iter().position(|a| a == "--html");
    if let Some(idx) = html_idx {
        // Determine output path - must not consume the value following --jump-threshold
        let next = args.get(idx + 1);
        let output_path = match next {
            Some(s) if !s.starts_with("--") && s != "--jump-threshold" => {
                // Make sure this token isn't the numeric arg to --jump-threshold appearing before --html
                PathBuf::from(s)
            }
            _ => Path::new(filename).with_extension("html"),
        };
        let dem_path = Path::new(filename);
        return generate_html(dem_path, &output_path, jump_threshold);
    }

    let show_all = args.iter().any(|a| a == "--all");
    let csv_mode = args.iter().any(|a| a == "--csv");
    let json_mode = args.iter().any(|a| a == "--json");
    let summary_only = args.iter().any(|a| a == "--summary");

    let mut file = File::open(filename)?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;

    let header = parse_header(&data).unwrap_or_else(|| {
        eprintln!("Error: not a valid HL2DEMO file (bad magic)");
        std::process::exit(1);
    });

    // ── Print header ─────────────────────────────────────────────────────────
    if !csv_mode && !json_mode {
        let stem = Path::new(filename)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        println!("╔══ {stem}");
        println!("║  Map      : {}", header.map_name);
        println!("║  Client   : {}", header.client_name);
        println!("║  Server   : {}", header.server_name);
        println!("║  Game     : {}", header.game_dir);
        println!(
            "║  Protocols: demo={} net={}",
            header.demo_protocol, header.net_protocol
        );
        println!(
            "║  Length   : {:.2}s  ticks={}  frames={}  signon_len={}",
            header.playback_time, header.ticks, header.frames, header.sign_on_length
        );
        println!("╚══ ({} bytes on disk)", data.len());
        println!();
    }

    if summary_only {
        return Ok(());
    }

    if csv_mode {
        println!("tick,cmd_num,pitch,yaw,roll,fwd,side,up,buttons,impulse,weapon,mousedx,mousedy");
    } else if json_mode {
        println!("[");
    }

    // demo_protocol > 3 adds a player_slot byte after cmd+tick
    let pkt_extra: usize = if header.demo_protocol > 3 { 1 } else { 0 };
    let ph = 5 + pkt_extra; // packet header size (cmd+tick+[slot])
    let democmdinfo = SPLIT_SIZE * detect_splitscreen(&data, header.demo_protocol, &header.game_dir); // L4D = 4 slots

    // ── Packet loop ───────────────────────────────────────────────────────────
    let mut offset = HEADER_SIZE;
    let mut pkt_num = 0u32;
    let mut counts = [0u32; 10]; // indexed by cmd byte (1-9)
    let mut usercmd_count = 0u32;
    let mut json_first = true;
    let mut last_tick = 0i32;

    while offset < data.len() {
        if offset + 5 > data.len() {
            break;
        }
        let cmd = data[offset];
        let tick = le_i32(&data, offset + 1);
        if tick > last_tick {
            last_tick = tick;
        }
        if (1..=9).contains(&cmd) {
            counts[cmd as usize] += 1;
        }

        match cmd {
            // ── Stop ──────────────────────────────────────────────────────────
            DEM_STOP => {
                if !csv_mode && !json_mode {
                    println!("[{pkt_num:>6}] STOP   tick={tick}");
                }
                break;
            }

            // ── Signon / Packet ───────────────────────────────────────────────
            DEM_SIGNON | DEM_PACKET => {
                let base = offset + ph;
                if base + democmdinfo + 12 > data.len() { break; }
                let in_seq = le_i32(&data, base + democmdinfo);
                let out_seq = le_i32(&data, base + democmdinfo + 4);
                let length = le_i32(&data, base + democmdinfo + 8);
                if length < 0 { break; }
                let length = length as usize;
                let next = base.saturating_add(democmdinfo + 12).saturating_add(length);
                if next > data.len() { break; }
                if show_all && !csv_mode && !json_mode {
                    let label = if cmd == DEM_SIGNON { "SIGNON " } else { "PACKET " };
                    println!("[{pkt_num:>6}] {label} tick={tick:>7}  in={in_seq}  out={out_seq}  len={length}");
                }
                offset = next;
            }

            DEM_SYNCTICK => {
                if show_all && !csv_mode && !json_mode {
                    println!("[{pkt_num:>6}] SYNCTICK tick={tick}");
                }
                offset += ph;
            }

            DEM_CONSOLECMD => {
                let p = offset + ph;
                if p + 4 > data.len() { break; }
                let length = le_i32(&data, p);
                if length < 0 { break; }
                let length = length as usize;
                let next = p.saturating_add(4).saturating_add(length);
                if next > data.len() { break; }
                if !csv_mode && !json_mode {
                    let s = read_cstring(&data, p + 4, length);
                    println!("[{pkt_num:>6}] CONSOLE  tick={tick:>7}  \"{s}\"");
                }
                offset = next;
            }

            DEM_USERCMD => {
                let p = offset + ph;
                if p + 8 > data.len() { break; }
                let out_seq = le_i32(&data, p);
                let length = le_i32(&data, p + 4);
                if length < 0 { break; }
                let length = length as usize;
                let next = p.saturating_add(8).saturating_add(length);
                if next > data.len() { break; }
                let ucmd_bytes = &data[p + 8..next];
                offset = next;

                // Skip usercmd parsing for old net protocols (garbage output)
                if header.net_protocol <= 7 { pkt_num += 1; continue; }

                usercmd_count += 1;
                match parse_usercmd(ucmd_bytes) {
                    Some(ucmd) => {
                        let cmd_num = ucmd.command_number.unwrap_or(out_seq as u32);

                        if csv_mode {
                            println!(
                                "{tick},{cmd_num},{},{},{},{},{},{},{},{},{},{},{}",
                                ucmd.pitch.map_or(String::new(), |v| format!("{v:.4}")),
                                ucmd.yaw.map_or(String::new(), |v| format!("{v:.4}")),
                                ucmd.roll.map_or(String::new(), |v| format!("{v:.4}")),
                                ucmd.forwardmove.map_or(String::new(), |v| format!("{v:.2}")),
                                ucmd.sidemove.map_or(String::new(), |v| format!("{v:.2}")),
                                ucmd.upmove.map_or(String::new(), |v| format!("{v:.2}")),
                                ucmd.buttons.map_or(String::new(), |v| v.to_string()),
                                ucmd.impulse.map_or(String::new(), |v| v.to_string()),
                                ucmd.weaponselect.map_or(String::new(), |v| v.to_string()),
                                ucmd.mousedx.map_or(String::new(), |v| v.to_string()),
                                ucmd.mousedy.map_or(String::new(), |v| v.to_string()),
                            );
                        } else if json_mode {
                            if !json_first {
                                println!(",");
                            }
                            json_first = false;
                            print!("  {{\"tick\":{tick},\"cmd\":{cmd_num}");
                            if let Some(v) = ucmd.pitch {
                                print!(",\"pitch\":{v:.4}");
                            }
                            if let Some(v) = ucmd.yaw {
                                print!(",\"yaw\":{v:.4}");
                            }
                            if let Some(v) = ucmd.roll {
                                print!(",\"roll\":{v:.4}");
                            }
                            if let Some(v) = ucmd.forwardmove {
                                print!(",\"fwd\":{v:.2}");
                            }
                            if let Some(v) = ucmd.sidemove {
                                print!(",\"side\":{v:.2}");
                            }
                            if let Some(v) = ucmd.upmove {
                                print!(",\"up\":{v:.2}");
                            }
                            if let Some(v) = ucmd.buttons {
                                print!(
                                    ",\"buttons\":{v},\"buttons_str\":\"{}\"",
                                    fmt_buttons(v)
                                );
                            }
                            if let Some(v) = ucmd.impulse {
                                print!(",\"impulse\":{v}");
                            }
                            if let Some(v) = ucmd.weaponselect {
                                print!(",\"weapon\":{v}");
                            }
                            if let Some(v) = ucmd.weaponsubtype {
                                print!(",\"weapon_sub\":{v}");
                            }
                            if let Some(v) = ucmd.mousedx {
                                print!(",\"mousedx\":{v}");
                            }
                            if let Some(v) = ucmd.mousedy {
                                print!(",\"mousedy\":{v}");
                            }
                            print!("}}");
                        } else {
                            println!(
                                "[{pkt_num:>6}] USERCMD  tick={tick:>7}  cmd={cmd_num}  seq={out_seq}"
                            );
                            if let (Some(pitch), Some(yaw)) = (ucmd.pitch, ucmd.yaw) {
                                println!(
                                    "               view  pitch={pitch:>9.3}°  yaw={yaw:>9.3}°  roll={:.3}°",
                                    ucmd.roll.unwrap_or(0.0)
                                );
                            }
                            let fwd = ucmd.forwardmove.unwrap_or(0.0);
                            let side = ucmd.sidemove.unwrap_or(0.0);
                            let up = ucmd.upmove.unwrap_or(0.0);
                            if fwd != 0.0 || side != 0.0 || up != 0.0 {
                                println!(
                                    "               move  fwd={fwd:>8.1}  side={side:>8.1}  up={up:>8.1}"
                                );
                            }
                            if let Some(btn) = ucmd.buttons {
                                if btn != 0 {
                                    println!(
                                        "               keys  {}  (0x{btn:08x})",
                                        fmt_buttons(btn)
                                    );
                                }
                            }
                            if let Some(dx) = ucmd.mousedx {
                                println!(
                                    "               mouse dx={dx}  dy={}",
                                    ucmd.mousedy.unwrap_or(0)
                                );
                            }
                            if let Some(w) = ucmd.weaponselect {
                                println!(
                                    "               weapon slot={w}  sub={}",
                                    ucmd.weaponsubtype.unwrap_or(0)
                                );
                            }
                        }
                    }
                    None => {
                        if !csv_mode && !json_mode {
                            eprintln!(
                                "[{pkt_num:>6}] USERCMD  tick={tick:>7}  (parse failed, {length} bytes)"
                            );
                        }
                    }
                }
                // offset already set before the net_protocol guard
            }

            DEM_DATATABLES => {
                let p = offset + ph;
                if p + 4 > data.len() { break; }
                let length = le_i32(&data, p);
                if length < 0 { break; }
                let length = length as usize;
                let next = p.saturating_add(4).saturating_add(length);
                if next > data.len() { break; }
                if show_all && !csv_mode && !json_mode {
                    println!("[{pkt_num:>6}] DATATABLES tick={tick:>7}  len={length}");
                }
                offset = next;
            }

            // Proto-4 DEM_CUSTOMDATA shares slot 8 with proto-3 StringTables but
            // has an id(4)+length(4) header, not a bare length. Handle it first
            // so the walk doesn't read the id as the length and desync (which cut
            // off all packets after the signon on Portal 2 / L4D demos).
            DEM_STRINGTABLES if header.demo_protocol > 3 => {
                let p = offset + ph;
                if p + 8 > data.len() { break; }
                let length = le_i32(&data, p + 4);
                if length < 0 { break; }
                let length = length as usize;
                let next = p.saturating_add(8).saturating_add(length);
                if next > data.len() { break; }
                if show_all && !csv_mode && !json_mode {
                    println!("[{pkt_num:>6}] CUSTOMDATA tick={tick:>7}  len={length}");
                }
                offset = next;
            }

            DEM_STRINGTABLES | DEM_STRINGTABLES_V2 => {
                let p = offset + ph;
                if p + 4 > data.len() { break; }
                let length = le_i32(&data, p);
                if length < 0 { break; }
                let length = length as usize;
                if show_all && !csv_mode && !json_mode {
                    println!("[{pkt_num:>6}] STRTABLES  tick={tick:>7}  len={length}");
                }
                offset = p.saturating_add(4).saturating_add(length).min(data.len());
            }

            other => {
                if !csv_mode && !json_mode {
                    eprintln!(
                        "Unknown cmd={other} at offset=0x{offset:x}, stopping."
                    );
                }
                break;
            }
        }

        pkt_num += 1;
    }

    // ── Close JSON ────────────────────────────────────────────────────────────
    if json_mode {
        if !json_first {
            println!();
        }
        println!("]");
        return Ok(());
    }

    // ── Summary ───────────────────────────────────────────────────────────────
    if !csv_mode {
        println!();
        println!("╔══ Summary");
        println!("║  Packets parsed   : {pkt_num}");
        println!("║  Last tick seen   : {last_tick}");
        println!(
            "║  Signon           : {}",
            counts[DEM_SIGNON as usize]
        );
        println!(
            "║  Packet (game)    : {}",
            counts[DEM_PACKET as usize]
        );
        println!(
            "║  ConsoleCmd       : {}",
            counts[DEM_CONSOLECMD as usize]
        );
        println!("║  UserCmd (inputs) : {usercmd_count}");
        println!(
            "║  DataTables       : {}",
            counts[DEM_DATATABLES as usize]
        );
        println!(
            "║  StringTables     : {}",
            counts[DEM_STRINGTABLES as usize] + counts[DEM_STRINGTABLES_V2 as usize]
        );
        println!("╚══");
    }

    Ok(())
}
