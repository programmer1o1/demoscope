// Golden-output tests. For every file in tests/fixtures/:
//
//   (a) `--json` stdout is compared byte-for-byte with
//       tests/golden/<fixture>.json. Skipped where `--json` is not supported
//       (anything but Source 1); the CLI contract tests cover that case.
//   (b) a compact digest of the tables embedded in the `--html` output is
//       compared with tests/golden/<fixture>.digest.txt. The digest has one
//       entry per table: name, kind, entry count, first / last element and an
//       FNV-1a hash of the raw literal. It never contains a full track dump.
//
// To update the snapshots after an intentional output change:
//
//     UPDATE_GOLDEN=1 cargo test --test golden
//
// then review `git diff tests/golden/` and see docs/OUTPUT.md before
// committing. No external crates: the JSON handling below only has to split
// the top level of the literals the generator writes.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const PREVIEW_CHARS: usize = 100;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn updating() -> bool {
    std::env::var("UPDATE_GOLDEN").is_ok_and(|v| !v.is_empty() && v != "0")
}

// ── Digest helpers ───────────────────────────────────────────────────────────

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Split the inside of a JSON array/object at its top-level commas.
fn split_top_level(inner: &str) -> Vec<&str> {
    let bytes = inner.as_bytes();
    let (mut depth, mut in_str, mut esc) = (0i32, false, false);
    let mut parts = Vec::new();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'[' | b'{' => depth += 1,
            b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                parts.push(inner[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = inner[start..].trim();
    if !last.is_empty() {
        parts.push(last);
    }
    parts
}

/// Value part of an `"key":value` object entry.
fn object_entry_value(entry: &str) -> &str {
    let bytes = entry.as_bytes();
    if bytes.first() != Some(&b'"') {
        return entry;
    }
    let mut i = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 1,
            b'"' => break,
            _ => {}
        }
        i += 1;
    }
    entry[(i + 1).min(entry.len())..]
        .trim_start()
        .strip_prefix(':')
        .unwrap_or("")
        .trim_start()
}

fn preview(s: &str) -> String {
    let mut out: String = s.chars().take(PREVIEW_CHARS).collect();
    if s.chars().count() > PREVIEW_CHARS {
        out.push('…');
    }
    out
}

fn tail_preview(s: &str) -> String {
    let n = s.chars().count();
    if n <= PREVIEW_CHARS {
        return s.to_string();
    }
    let tail: String = s.chars().skip(n - PREVIEW_CHARS).collect();
    format!("…{tail}")
}

fn digest_table(name: &str, literal: &str) -> String {
    let hash = format!("fnv1a64:{:016x}", fnv1a64(literal.as_bytes()));
    let (kind, entries, samples, first, last) =
        if let Some(inner) = literal.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            let parts = split_top_level(inner);
            (
                "array",
                parts.len(),
                None,
                parts.first().map_or(String::new(), |p| preview(p)),
                parts.last().map_or(String::new(), |p| preview(p)),
            )
        } else if let Some(inner) = literal.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            let parts = split_top_level(inner);
            let all_arrays =
                !parts.is_empty() && parts.iter().all(|p| object_entry_value(p).starts_with('['));
            if all_arrays {
                // { key: [sample, ...] }: report the first and last sample, not
                // the start and end of the text.
                let samples_of = |p: &str| -> Vec<String> {
                    let v = object_entry_value(p);
                    split_top_level(&v[1..v.len().saturating_sub(1)])
                        .into_iter()
                        .map(String::from)
                        .collect()
                };
                let key_of = |p: &str| -> String {
                    let v = object_entry_value(p);
                    p[..p.len() - v.len()]
                        .trim_end_matches(|c: char| c == ':' || c.is_whitespace())
                        .to_string()
                };
                let total: usize = parts.iter().map(|p| samples_of(p).len()).sum();
                let first = parts
                    .iter()
                    .find_map(|p| samples_of(p).first().map(|v| format!("{}: {v}", key_of(p))));
                let last = parts
                    .iter()
                    .rev()
                    .find_map(|p| samples_of(p).last().map(|v| format!("{}: {v}", key_of(p))));
                (
                    "object",
                    parts.len(),
                    Some(total),
                    first.map_or(String::new(), |f| preview(&f)),
                    last.map_or(String::new(), |l| preview(&l)),
                )
            } else {
                (
                    "object",
                    parts.len(),
                    None,
                    parts.first().map_or(String::new(), |p| preview(p)),
                    parts.last().map_or(String::new(), |p| preview(p)),
                )
            }
        } else if literal.starts_with('"') {
            ("string", 1, None, preview(literal), tail_preview(literal))
        } else {
            ("scalar", 1, None, preview(literal), tail_preview(literal))
        };
    let samples = samples.map_or(String::new(), |n| format!(" samples={n}"));
    format!(
        "table {name} kind={kind} entries={entries}{samples} bytes={} hash={hash}\n  first: {first}\n  last:  {last}\n",
        literal.len()
    )
}

/// Names of the embedded tables, in template order: every
/// `const NAME = __PLACEHOLDER__;` line (optionally `new Set(__X__)`).
fn template_tables() -> Vec<String> {
    let template = fs::read_to_string(root().join("src/render/html/template.html"))
        .expect("read template.html");
    let mut names = Vec::new();
    for line in template.lines() {
        let Some(rest) = line.strip_prefix("const ") else {
            continue;
        };
        let Some((name, value)) = rest.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let value = value.strip_prefix("new Set(").unwrap_or(value);
        if value.starts_with("__") {
            names.push(name.trim().to_string());
        }
    }
    names
}

fn table_literal<'a>(html: &'a str, name: &str) -> Option<&'a str> {
    for line in html.lines() {
        let Some(rest) = line.strip_prefix("const ") else {
            continue;
        };
        let Some(rest) = rest.strip_prefix(name) else {
            continue;
        };
        let Some(value) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let value = value.trim().trim_end_matches(';');
        let value = value
            .strip_prefix("new Set(")
            .and_then(|v| v.strip_suffix(')'))
            .unwrap_or(value);
        return Some(value.trim());
    }
    None
}

fn html_digest(fixture: &str, html: &str) -> String {
    let mut out =
        format!("# demoscope embedded-table digest (internal, unstable)\nfixture: {fixture}\n");
    for name in template_tables() {
        let literal = table_literal(html, &name)
            .unwrap_or_else(|| panic!("table {name} not found in generated HTML"));
        out.push_str(&digest_table(&name, literal));
    }
    out
}

// ── Snapshot plumbing ────────────────────────────────────────────────────────

fn assert_golden(path: &Path, actual: &str) {
    if updating() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, actual).unwrap();
        return;
    }
    let expected = fs::read_to_string(path).unwrap_or_else(|_| {
        panic!(
            "missing golden file {}\nCreate it with: UPDATE_GOLDEN=1 cargo test --test golden",
            path.display()
        )
    });
    let expected = expected.replace("\r\n", "\n");
    if expected == actual {
        return;
    }
    let line = expected
        .lines()
        .zip(actual.lines())
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| expected.lines().count().min(actual.lines().count()));
    panic!(
        "golden mismatch in {} at line {}\n  expected: {}\n  actual:   {}\nIf the change is intentional, update with: UPDATE_GOLDEN=1 cargo test --test golden (see docs/OUTPUT.md)",
        path.display(),
        line + 1,
        preview(expected.lines().nth(line).unwrap_or("<end of file>")),
        preview(actual.lines().nth(line).unwrap_or("<end of file>")),
    );
}

fn fixtures() -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(root().join("tests/fixtures"))
        .expect("read tests/fixtures")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.to_lowercase().starts_with("readme"))
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no fixtures found");
    names
}

fn demoscope() -> Command {
    Command::new(env!("CARGO_BIN_EXE_demoscope"))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[test]
fn json_output_matches_golden() {
    let mut compared = 0;
    for name in fixtures() {
        let golden = root().join("tests/golden").join(format!("{name}.json"));
        let out = demoscope()
            .arg(root().join("tests/fixtures").join(&name))
            .arg("--json")
            .output()
            .expect("run demoscope");
        if out.status.success() {
            let actual = String::from_utf8(out.stdout).expect("--json output is UTF-8");
            assert_golden(&golden, &actual);
            compared += 1;
        } else {
            assert!(
                out.stdout.is_empty(),
                "{name}: --json failed but wrote to stdout"
            );
            if updating() {
                let _ = fs::remove_file(&golden);
            } else {
                assert!(
                    !golden.exists(),
                    "{name}: --json is unsupported but {} exists",
                    golden.display()
                );
            }
        }
    }
    assert!(compared > 0, "no fixture produced --json output");
}

#[test]
fn html_table_digest_matches_golden() {
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("golden-html");
    fs::create_dir_all(&tmp).unwrap();
    for name in fixtures() {
        let html_path = tmp.join(format!("{name}.html"));
        let out = demoscope()
            .arg(root().join("tests/fixtures").join(&name))
            .arg("--html")
            .arg(&html_path)
            .output()
            .expect("run demoscope");
        assert!(
            out.status.success(),
            "{name}: --html failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let html = fs::read_to_string(&html_path).expect("read generated HTML");
        let golden = root()
            .join("tests/golden")
            .join(format!("{name}.digest.txt"));
        assert_golden(&golden, &html_digest(&name, &html));
    }
}

#[test]
fn html_output_is_deterministic() {
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("golden-determinism");
    fs::create_dir_all(&tmp).unwrap();
    for name in fixtures() {
        let mut outputs = Vec::new();
        for run in 0..2 {
            let path = tmp.join(format!("{run}-{name}.html"));
            let status = demoscope()
                .arg(root().join("tests/fixtures").join(&name))
                .arg("--html")
                .arg(&path)
                .output()
                .expect("run demoscope")
                .status;
            assert!(status.success());
            outputs.push(fs::read(&path).unwrap());
        }
        assert!(
            outputs[0] == outputs[1],
            "{name}: HTML differs between runs"
        );
    }
}

#[test]
fn every_template_table_is_found() {
    let tables = template_tables();
    assert!(tables.len() >= 20, "template parsing found {tables:?}");
    assert!(tables.iter().any(|t| t == "META"));
    assert!(tables.iter().any(|t| t == "GHOST_EIDS"));
}

#[test]
fn split_top_level_ignores_nested_and_quoted_commas() {
    let parts = split_top_level(r#"[1,2],"a,b",{"k":[3,4]}, 5"#);
    assert_eq!(parts, vec!["[1,2]", r#""a,b""#, r#"{"k":[3,4]}"#, "5"]);
    assert_eq!(split_top_level(""), Vec::<&str>::new());
    assert_eq!(split_top_level(r#""q\",x",1"#), vec![r#""q\",x""#, "1"]);
}

#[test]
fn digest_counts_entries_and_samples() {
    let d = digest_table("T", r#"{"1":[[0,1],[2,3]],"2":[[4,5]]}"#);
    assert!(d.contains("kind=object entries=2 samples=3"), "{d}");
    let d = digest_table("A", "[]");
    assert!(d.contains("kind=array entries=0"), "{d}");
    let d = digest_table("S", "[0,0,0]");
    assert!(d.contains("kind=array entries=3"), "{d}");
    let d = digest_table("P", "260");
    assert!(d.contains("kind=scalar entries=1"), "{d}");
}
