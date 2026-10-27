// CLI contract tests: exit codes and stdout/stderr behaviour documented in
// docs/OUTPUT.md. These run the built binary against the tracked fixtures.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

// A Source 1 fixture with usercmds, and a Source 2 fixture (no `--csv` / `--json`).
const S1: &str = "portal_testchmb_a_10.dem";
const S2: &str = "deadlock_bots_clip.dem";
// A Source 1 fixture whose `--json` / `--csv` output is larger than a pipe buffer.
const BIG_S1: &str = "portal2_sp_a3_01.dem";

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn run(args: &[&str], demo: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_demoscope"))
        .arg(fixture(demo))
        .args(args)
        .output()
        .expect("failed to run demoscope")
}

#[test]
fn json_on_source2_exits_nonzero_with_empty_stdout() {
    let out = run(&["--json"], S2);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        out.stdout.is_empty(),
        "stdout must be empty, got {} bytes",
        out.stdout.len()
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("--json is only supported for Source 1 demos"),
        "unexpected stderr: {err}"
    );
}

#[test]
fn csv_on_source2_exits_nonzero_with_empty_stdout() {
    let out = run(&["--csv"], S2);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("--csv is only supported for Source 1 demos"),
        "unexpected stderr: {err}"
    );
}

#[test]
fn summary_on_source2_still_succeeds() {
    let out = run(&["--summary"], S2);
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("Source 2 PBDEMS2"));
}

#[test]
fn json_on_source1_is_a_json_array() {
    let out = run(&["--json"], S1);
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("[\n"));
    assert!(stdout.trim_end().ends_with(']'));
}

#[test]
fn csv_on_source1_starts_with_the_documented_header() {
    let out = run(&["--csv"], S1);
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.lines().next(),
        Some("tick,cmd_num,pitch,yaw,roll,fwd,side,up,buttons,impulse,weapon,mousedx,mousedy")
    );
}

#[test]
fn check_passes_fixtures_with_exit_zero() {
    let dir = fixture("");
    let out = Command::new(env!("CARGO_BIN_EXE_demoscope"))
        .arg("--check")
        .arg(&dir)
        .output()
        .expect("failed to run demoscope");
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("PASS"));
    assert!(!stdout.contains("FAIL"));
    // Both fixtures decode natively, and the table has a `decode` column.
    let header = stdout.lines().next().unwrap_or_default();
    assert!(header.contains("decode"), "header: {header}");
    for line in stdout.lines().filter(|l| l.starts_with("PASS")) {
        assert!(line.contains(" native "), "row: {line}");
    }
}

#[test]
fn check_on_a_missing_path_fails_with_exit_one() {
    let out = Command::new(env!("CARGO_BIN_EXE_demoscope"))
        .arg("--check")
        .arg(fixture("does-not-exist.dem"))
        .output()
        .expect("failed to run demoscope");
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("FAIL"));
}

#[test]
fn csv_and_json_together_exit_one_with_empty_stdout() {
    for demo in [S1, S2] {
        let out = run(&["--csv", "--json"], demo);
        assert_eq!(out.status.code(), Some(1), "{demo}");
        assert!(out.stdout.is_empty(), "{demo}: stdout must be empty");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("--csv and --json can't be used together"),
            "{demo}: unexpected stderr: {err}"
        );
    }
}

#[test]
fn closed_stdout_exits_zero_without_panicking() {
    // The JSON output is larger than a pipe buffer, so the process is still
    // writing when the reader goes away.
    for flag in ["--json", "--csv"] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_demoscope"))
            .arg(fixture(BIG_S1))
            .arg(flag)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to run demoscope");
        let stdout = child.stdout.take().unwrap();
        let mut first = String::new();
        BufReader::new(stdout).read_line(&mut first).unwrap();
        assert!(!first.is_empty(), "{flag}: no output");
        // The BufReader (and the pipe) is dropped here, like `head -n 1`.
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{flag}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!err.contains("panicked"), "{flag}: {err}");
        assert!(err.is_empty(), "{flag}: unexpected stderr: {err}");
    }
}

#[test]
fn check_on_a_directory_without_demos_exits_one() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("check-no-demos");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("notes.txt"), "not a demo").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_demoscope"))
        .arg("--check")
        .arg(&dir)
        .output()
        .expect("failed to run demoscope");
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty(), "stdout must be empty");
    assert!(String::from_utf8_lossy(&out.stderr).contains("no supported demo files found"));
}

#[test]
fn check_sweep_includes_quake_live_and_matches_single_file_result() {
    // Not a real Quake Live demo: a truncated file is enough to show the sweep
    // picks the extension up, and that it gives the same verdict as a
    // single-file check.
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("check-quake-live");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("match.dm_91");
    std::fs::write(&file, [0u8; 64]).unwrap();
    let check = |p: &PathBuf| {
        let out = Command::new(env!("CARGO_BIN_EXE_demoscope"))
            .arg("--check")
            .arg(p)
            .output()
            .expect("failed to run demoscope");
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).to_string(),
        )
    };
    let (sweep_code, sweep) = check(&dir);
    let (single_code, single) = check(&file);
    assert!(sweep.contains("match.dm_91"), "sweep skipped it: {sweep}");
    assert_eq!(sweep_code, single_code);
    // An experimental format that can't be decoded is a WARN, not a FAIL.
    assert_eq!(sweep_code, Some(0), "{sweep}");
    assert!(sweep.contains("WARN") && !sweep.contains("FAIL"), "{sweep}");
    let row = |s: &str| {
        s.lines()
            .find(|l| l.contains("match.dm_91"))
            .map(String::from)
    };
    assert_eq!(row(&sweep), row(&single));
}
