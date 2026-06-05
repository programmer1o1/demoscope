// Decode self-check harness. Runs each demo through the real format-appropriate
// decoder and reports a one-line health verdict, so a regression that silently
// breaks one game's decode (a desync, a panic, zero tracks) shows up as a FAIL
// across the test-demo set instead of going unnoticed.
//
// Wired to the CLI as `--check <file|dir>`; a directory is swept for `*.dem`.
// Exit code is non-zero if any demo FAILs, so it doubles as a CI gate.
//
// Health tiers:
//   PASS — decoded and produced player tracks.
//   WARN — decoded with tracks, but with a soft concern (e.g. Source 2 packet
//          decode failures: the file still yields tracks, but some packets
//          desynced — worth surfacing, not a hard break).
//   FAIL — panicked, errored, or produced zero tracks.

use std::panic::{catch_unwind, AssertUnwindSafe};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pass,
    Warn,
    Fail,
}

impl Status {
    fn tag(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Warn => "WARN",
            Status::Fail => "FAIL",
        }
    }
}

pub struct DemoHealth {
    pub name: String,
    pub format: &'static str,
    pub tracks: usize,
    pub samples: usize,
    pub named: usize,
    pub note: String,
    pub status: Status,
}

impl DemoHealth {
    fn fail(name: &str, format: &'static str, note: impl Into<String>) -> Self {
        DemoHealth {
            name: name.to_string(), format, tracks: 0, samples: 0, named: 0,
            note: note.into(), status: Status::Fail,
        }
    }
}

/// Run the real decoder for `bytes` (format auto-detected, same order as the
/// viewer's `parse_demo_to_html`) and return its decode health. Never panics:
/// a panic inside a decoder is caught and reported as a FAIL.
pub fn check_demo(bytes: &[u8], name_hint: &str) -> DemoHealth {
    // Source 2 (PBDEMS2: CS2 / Dota 2 / Deadlock) — unambiguous magic, checked
    // first so the Quake `.dem` route can't swallow it.
    if super::source2::is_source2(bytes) {
        return guard(name_hint, "source2", || {
            let Some(t) = super::source2::parser::parse(bytes) else {
                return DemoHealth::fail(name_hint, "source2", "parse returned None");
            };
            let tracks = t.tracks.len();
            let samples = t.tracks.values().map(|v| v.len()).sum();
            let named = t.names.values().filter(|n| !n.is_empty()).count();
            let note = format!("{} ok / {} failed packets, {} events", t.pe_ok, t.pe_fail, t.events.len());
            let status = if tracks == 0 {
                Status::Fail
            } else if t.pe_fail > 0 {
                Status::Warn
            } else {
                Status::Pass
            };
            DemoHealth { name: name_hint.to_string(), format: "source2", tracks, samples, named, note, status }
        });
    }

    // Quake family (Q1/Q2/Q3) — matched by extension+content; returns None for
    // HL2DEMO so Source 1 demos fall through.
    if let Some(kind) = super::quake::detect(name_hint, bytes) {
        return guard(name_hint, "quake", || match super::quake::parse(kind, bytes, name_hint) {
            Ok(d) => {
                let tracks = d.mpd.tracks.len();
                let samples = d.mpd.tracks.values().map(|v| v.len()).sum();
                let named = d.mpd.names.values().filter(|n| !n.name.is_empty()).count();
                let status = if tracks == 0 { Status::Fail } else { Status::Pass };
                DemoHealth { name: name_hint.to_string(), format: "quake", tracks, samples, named,
                    note: format!("{:?} {}", kind, d.meta.map), status }
            }
            Err(e) => DemoHealth::fail(name_hint, "quake", e.to_string()),
        });
    }

    // GoldSrc (HL1) HLDEMO container.
    if super::goldsrc::is_goldsrc(bytes) {
        return guard(name_hint, "goldsrc", || {
            let Some(meta) = super::goldsrc::parse(bytes) else {
                return DemoHealth::fail(name_hint, "goldsrc", "parse returned None");
            };
            let ents = super::goldsrc::entities::extract_entities(bytes, &meta);
            let tracks = ents.tracks.len();
            let samples = ents.tracks.values().map(|v| v.len()).sum();
            let status = if tracks == 0 { Status::Fail } else { Status::Pass };
            DemoHealth { name: name_hint.to_string(), format: "goldsrc", tracks, samples, named: 0,
                note: format!("map {}", meta.map_name), status }
        });
    }

    // Source 1 (HL2DEMO: TF2 / CS:S / CS:GO / Portal 2 / L4D / GMod …).
    guard(name_hint, "source1", || match super::source::multi_player::extract_from_bytes(bytes) {
        Ok(d) => {
            let tracks = d.tracks.len();
            let samples = d.tracks.values().map(|v| v.len()).sum();
            let named = d.names.values().filter(|n| !n.name.is_empty()).count();
            // 0 tracks is ambiguous: it can mean a decode gap, OR a recording that
            // was started and stopped instantly (decodes cleanly but has no
            // gameplay). The header tick count tells them apart — an empty
            // recording carries ~0 ticks, so 0 tracks there is correct, not a fail.
            let ticks = super::header::parse_header(bytes).map(|h| h.ticks).unwrap_or(0);
            let (status, note) = if tracks > 0 {
                (Status::Pass, String::new())
            } else if ticks <= 1 {
                (Status::Pass, format!("no gameplay, {ticks} ticks"))
            } else {
                (Status::Fail, format!("0 tracks despite {ticks} ticks"))
            };
            DemoHealth { name: name_hint.to_string(), format: "source1", tracks, samples, named, note, status }
        }
        Err(e) => DemoHealth::fail(name_hint, "source1", e.to_string()),
    })
}

/// Run `f`, converting a panic into a FAIL verdict instead of unwinding out of
/// the harness (so one bad demo doesn't abort a whole directory sweep).
fn guard(name: &str, format: &'static str, f: impl FnOnce() -> DemoHealth) -> DemoHealth {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(h) => h,
        Err(_) => DemoHealth::fail(name, format, "panicked during decode"),
    }
}

/// Sweep a file or directory of `.dem` files, printing one health row each.
/// Returns the number of FAILs (0 = everything healthy), which the CLI maps to
/// the process exit code.
pub fn run(path: &std::path::Path) -> std::io::Result<usize> {
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let p = entry?.path();
            if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dem")) {
                files.push(p);
            }
        }
        files.sort();
    } else {
        files.push(path.to_path_buf());
    }

    if files.is_empty() {
        eprintln!("no .dem files found under {}", path.display());
        return Ok(0);
    }

    println!("{:<6} {:<8} {:>6} {:>9} {:>6}  demo", "status", "format", "tracks", "samples", "named");
    let (mut pass, mut warn, mut fail) = (0usize, 0usize, 0usize);
    for f in &files {
        let name = f.file_name().and_then(|s| s.to_str()).unwrap_or("?").to_string();
        let h = match std::fs::read(f) {
            Ok(bytes) => check_demo(&bytes, &name),
            Err(e) => DemoHealth::fail(&name, "io", format!("read error: {e}")),
        };
        match h.status {
            Status::Pass => pass += 1,
            Status::Warn => warn += 1,
            Status::Fail => fail += 1,
        }
        println!("{:<6} {:<8} {:>6} {:>9} {:>6}  {}{}",
            h.status.tag(), h.format, h.tracks, h.samples, h.named, h.name,
            if h.note.is_empty() { String::new() } else { format!("  ({})", h.note) });
    }
    println!("\n{} demos: {} pass, {} warn, {} fail", files.len(), pass, warn, fail);
    Ok(fail)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Garbage / truncated input must degrade to a FAIL verdict, never panic out
    // of the harness — that safety is the whole point of `guard`.
    #[test]
    fn garbage_input_fails_cleanly() {
        for bytes in [&b""[..], &b"not a demo"[..], &[0xff; 64][..], &b"HL2DEMO\0"[..]] {
            let h = check_demo(bytes, "junk.dem");
            assert_eq!(h.status, Status::Fail, "expected FAIL for {} bytes", bytes.len());
        }
    }

    // A PBDEMS2 magic with no real body should be detected as source2 and fail,
    // not misroute to another format or panic.
    #[test]
    fn truncated_source2_fails_as_source2() {
        let h = check_demo(b"PBDEMS2\0garbage-tail", "x.dem");
        assert_eq!(h.format, "source2");
        assert_eq!(h.status, Status::Fail);
    }
}
