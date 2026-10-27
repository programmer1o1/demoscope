// Decode self-check harness. Runs each demo through the real format-appropriate
// decoder and reports a one-line health verdict, so a regression that silently
// breaks one game's decode (a desync, a panic, zero tracks) shows up as a FAIL
// across the test-demo set instead of going unnoticed.
//
// Wired to the CLI as `--check <file|dir>`; a directory is swept for the demo
// extensions in `is_supported_demo_path`. Exit code is non-zero if any demo
// FAILs, so it doubles as a CI gate.
//
// Health tiers (experimental formats, i.e. Quake Live, are capped at WARN):
//   PASS: decoded and produced native player tracks (not just the recorder
//          camera copied into a track).
//   WARN: usable, with a soft concern: only the recorder-camera fallback
//          produced tracks, some packets failed to decode (Source 2), or the
//          format is experimental (Quake Live).
//   FAIL: panicked, errored, or produced no tracks.
//
// The `decode` column says where the tracks came from: `native` (entity
// decode), `mixed` (the recorder track is the camera copy, other players are
// native), `fallback` (camera only) or `-` (nothing decoded / no gameplay).

use std::panic::{AssertUnwindSafe, catch_unwind};

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
    /// Where the tracks came from: native / mixed / fallback / -.
    pub decode: &'static str,
    pub note: String,
    pub status: Status,
}

impl DemoHealth {
    fn warn(name: &str, format: &'static str, note: impl Into<String>) -> Self {
        DemoHealth {
            name: name.to_string(),
            format,
            tracks: 0,
            samples: 0,
            named: 0,
            decode: "-",
            note: note.into(),
            status: Status::Warn,
        }
    }

    fn fail(name: &str, format: &'static str, note: impl Into<String>) -> Self {
        DemoHealth {
            name: name.to_string(),
            format,
            tracks: 0,
            samples: 0,
            named: 0,
            decode: "-",
            note: note.into(),
            status: Status::Fail,
        }
    }
}

/// Run the real decoder for `bytes` (format auto-detected, same order as the
/// viewer's `parse_demo_to_html`) and return its decode health. Never panics:
/// a panic inside a decoder is caught and reported as a FAIL.
pub fn check_demo(bytes: &[u8], name_hint: &str) -> DemoHealth {
    // Source 2 (PBDEMS2: CS2 / Dota 2 / Deadlock): unambiguous magic, checked
    // first so the Quake `.dem` route can't swallow it.
    if super::source2::is_source2(bytes) {
        return guard(name_hint, "source2", || {
            let Some(t) = super::source2::parser::parse(bytes) else {
                return DemoHealth::fail(name_hint, "source2", "parse returned None");
            };
            let tracks = t.tracks.len();
            let samples = t.tracks.values().map(|v| v.len()).sum();
            let named = t.names.values().filter(|n| !n.is_empty()).count();
            let note = format!(
                "{} ok / {} failed packets, {} events",
                t.pe_ok,
                t.pe_fail,
                t.events.len()
            );
            let status = if tracks == 0 {
                Status::Fail
            } else if t.pe_fail > 0 {
                Status::Warn
            } else {
                Status::Pass
            };
            DemoHealth {
                name: name_hint.to_string(),
                format: "source2",
                tracks,
                samples,
                named,
                decode: if tracks == 0 { "-" } else { "native" },
                note,
                status,
            }
        });
    }

    // Quake family (Q1/Q2/Q3): matched by extension+content; returns None for
    // HL2DEMO so Source 1 demos fall through.
    if let Some(kind) = super::quake::detect(name_hint, bytes) {
        let health = guard(name_hint, "quake", || {
            match super::quake::parse(kind, bytes, name_hint) {
                Ok(d) => {
                    let tracks = d.mpd.tracks.len();
                    let samples = d.mpd.tracks.values().map(|v| v.len()).sum();
                    let named = d.mpd.names.values().filter(|n| !n.name.is_empty()).count();
                    // Quake Live and other `.dm_NN` outside 66-68 are attempted
                    // but unverified (see docs/COMPATIBILITY.md): a decode is
                    // reported as WARN, never PASS.
                    let experimental = is_experimental_quake(name_hint);
                    let status = if tracks == 0 {
                        Status::Fail
                    } else if experimental {
                        Status::Warn
                    } else {
                        Status::Pass
                    };
                    let mut note = format!("{:?} {}", kind, d.meta.map);
                    if experimental && tracks > 0 {
                        note.push_str(", experimental");
                    }
                    DemoHealth {
                        name: name_hint.to_string(),
                        format: "quake",
                        tracks,
                        samples,
                        named,
                        decode: if tracks == 0 { "-" } else { "native" },
                        note,
                        status,
                    }
                }
                Err(e) => DemoHealth::fail(name_hint, "quake", e.to_string()),
            }
        });
        // Experimental formats (Quake Live) never fail a check: an undecodable
        // file is reported as WARN so a sweep over a mixed directory still
        // exits 0.
        return cap_experimental(health);
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
            let named = ents.names.values().filter(|n| !n.trim().is_empty()).count();
            // No decoded players: the viewer falls back to the recorder camera,
            // when that camera actually moves.
            let (status, decode, note) = if tracks > 0 {
                (Status::Pass, "native", format!("map {}", meta.map_name))
            } else {
                let cam = super::goldsrc::extract_camera(bytes, &meta);
                let moves = cam.first().is_some_and(|c| {
                    cam.iter().any(|s| {
                        (s.1 - c.1).abs() > 1.0
                            || (s.2 - c.2).abs() > 1.0
                            || (s.3 - c.3).abs() > 1.0
                    })
                });
                if moves {
                    (
                        Status::Warn,
                        "fallback",
                        format!("map {}, recorder camera only", meta.map_name),
                    )
                } else {
                    (Status::Fail, "-", format!("map {}", meta.map_name))
                }
            };
            DemoHealth {
                name: name_hint.to_string(),
                format: "goldsrc",
                tracks,
                samples,
                named,
                decode,
                note,
                status,
            }
        });
    }

    // Source 1 (HL2DEMO: TF2 / CS:S / CS:GO / Portal 2 / L4D / GMod …).
    guard(
        name_hint,
        "source1",
        || match super::source::player_tracks::extract_from_bytes(bytes) {
            Ok(d) => {
                let tracks = d.tracks.len();
                let samples = d.tracks.values().map(|v| v.len()).sum();
                let named = d.names.values().filter(|n| !n.name.is_empty()).count();
                // Tracks that came from entity decode. When the camera fallback
                // ran, entity 1 is a copy of the recorder camera, not a decode.
                let native = d
                    .tracks
                    .iter()
                    .filter(|(eid, v)| !v.is_empty() && !(d.camera_fallback && **eid == 1))
                    .count();
                let decode = if tracks == 0 {
                    "-"
                } else if native == 0 {
                    "fallback"
                } else if d.camera_fallback {
                    "mixed"
                } else {
                    "native"
                };
                // 0 tracks is ambiguous: it can mean a decode gap, OR a recording that
                // was started and stopped instantly (decodes cleanly but has no
                // gameplay). The header tick count tells them apart: an empty
                // recording carries ~0 ticks, so 0 tracks there is correct, not a fail.
                let ticks = super::header::parse_header(bytes)
                    .map(|h| h.ticks)
                    .unwrap_or(0);
                let (status, note) = if tracks == 0 {
                    if ticks <= 1 {
                        (Status::Pass, format!("no gameplay, {ticks} ticks"))
                    } else {
                        (Status::Fail, format!("0 tracks despite {ticks} ticks"))
                    }
                } else if native == 0 {
                    (Status::Warn, "recorder camera fallback only".to_string())
                } else {
                    (Status::Pass, String::new())
                };
                DemoHealth {
                    name: name_hint.to_string(),
                    format: "source1",
                    tracks,
                    samples,
                    named,
                    decode,
                    note,
                    status,
                }
            }
            Err(e) => DemoHealth::fail(name_hint, "source1", e.to_string()),
        },
    )
}

/// Run `f`, converting a panic into a FAIL verdict instead of unwinding out of
/// the harness (so one bad demo doesn't abort a whole directory sweep).
fn guard(name: &str, format: &'static str, f: impl FnOnce() -> DemoHealth) -> DemoHealth {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(h) => h,
        Err(_) => DemoHealth::fail(name, format, "panicked during decode"),
    }
}

/// `.dm_NN` Quake 3 demo extensions outside the verified 66-68 range (Quake
/// Live and variants). They are attempted but unverified.
fn is_experimental_quake(name: &str) -> bool {
    super::quake::dm_protocol(name).is_some_and(|p| !(66..=68).contains(&p))
}

/// Experimental formats are capped at WARN, including files that fail to decode.
fn cap_experimental(mut h: DemoHealth) -> DemoHealth {
    if is_experimental_quake(&h.name) && h.status == Status::Fail {
        h.status = Status::Warn;
        h.note = if h.note.is_empty() {
            "experimental".to_string()
        } else {
            format!("experimental, {}", h.note)
        };
    }
    h
}

fn is_supported_demo_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            let ext = ext.to_ascii_lowercase();
            matches!(ext.as_str(), "dem" | "dm2" | "mvd" | "qwd")
                || ext
                    .strip_prefix("dm_")
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
}

/// Sweep a supported demo file or directory, printing one health row each.
/// Returns the number of FAILs (0 = everything healthy), which the CLI maps to
/// the process exit code.
pub fn run(path: &std::path::Path) -> std::io::Result<usize> {
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let p = entry?.path();
            if is_supported_demo_path(&p) {
                files.push(p);
            }
        }
        files.sort();
    } else {
        files.push(path.to_path_buf());
    }

    if files.is_empty() {
        // Checking nothing is not a pass: count it as one failure so the exit
        // code is non-zero.
        eprintln!("no supported demo files found under {}", path.display());
        return Ok(1);
    }

    println!(
        "{:<6} {:<8} {:>6} {:>9} {:>6} {:<8}  demo",
        "status", "format", "tracks", "samples", "named", "decode"
    );
    let (mut pass, mut warn, mut fail) = (0usize, 0usize, 0usize);
    for f in &files {
        let name = f
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        let h = match std::fs::read(f) {
            Ok(bytes) => check_demo(&bytes, &name),
            Err(e) => DemoHealth::fail(&name, "io", format!("read error: {e}")),
        };
        match h.status {
            Status::Pass => pass += 1,
            Status::Warn => warn += 1,
            Status::Fail => fail += 1,
        }
        println!(
            "{:<6} {:<8} {:>6} {:>9} {:>6} {:<8}  {}{}",
            h.status.tag(),
            h.format,
            h.tracks,
            h.samples,
            h.named,
            h.decode,
            h.name,
            if h.note.is_empty() {
                String::new()
            } else {
                format!("  ({})", h.note)
            }
        );
    }
    println!(
        "\n{} demos: {} pass, {} warn, {} fail",
        files.len(),
        pass,
        warn,
        fail
    );
    Ok(fail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_discovery_covers_supported_extensions() {
        for name in [
            "match.dem",
            "match.dm2",
            "match.dm_66",
            "match.dm_68",
            "match.mvd",
            "match.qwd",
        ] {
            assert!(is_supported_demo_path(std::path::Path::new(name)), "{name}");
        }
        for name in ["match.dm_71", "match.dm_73", "match.dm_91"] {
            assert!(is_supported_demo_path(std::path::Path::new(name)), "{name}");
        }
        for name in [
            "map.bsp",
            "notes.txt",
            "match.dm_",
            "match.dm_x",
            "match.dmo",
        ] {
            assert!(
                !is_supported_demo_path(std::path::Path::new(name)),
                "{name}"
            );
        }
    }

    #[test]
    fn quake_live_extensions_are_experimental() {
        for name in ["a.dm_71", "a.dm_73", "a.dm_90", "a.dm_91", "A.DM_91"] {
            assert!(is_experimental_quake(name), "{name}");
        }
        for name in ["a.dm_66", "a.dm_67", "a.dm_68", "a.dm2", "a.dem", "a.mvd"] {
            assert!(!is_experimental_quake(name), "{name}");
        }
    }

    #[test]
    fn experimental_quake_never_fails_but_supported_quake_does() {
        for name in ["x.dm_71", "x.dm_91"] {
            let h = check_demo(&[0u8; 64], name);
            assert_eq!(h.format, "quake", "{name}");
            assert_eq!(h.status, Status::Warn, "{name}: {}", h.note);
            assert!(h.note.contains("experimental"), "{name}: {}", h.note);
        }
        for name in ["x.dm_68", "x.dm2", "x.dem"] {
            let h = check_demo(&[0u8; 64], name);
            assert_eq!(h.status, Status::Fail, "{name}");
        }
    }

    #[test]
    fn garbage_quakeworld_fails_cleanly() {
        for name in ["match.qwd", "match.mvd"] {
            let h = check_demo(b"placeholder", name);
            assert_eq!(h.format, "quake");
            assert_eq!(h.status, Status::Fail, "{name}");
        }
    }

    // Garbage / truncated input must degrade to a FAIL verdict, never panic out
    // of the harness: that safety is the whole point of `guard`.
    #[test]
    fn garbage_input_fails_cleanly() {
        for bytes in [
            &b""[..],
            &b"not a demo"[..],
            &[0xff; 64][..],
            &b"HL2DEMO\0"[..],
        ] {
            let h = check_demo(bytes, "junk.dem");
            assert_eq!(
                h.status,
                Status::Fail,
                "expected FAIL for {} bytes",
                bytes.len()
            );
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
