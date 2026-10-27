// CLI contract tests: exit codes and stdout/stderr behaviour documented in
// docs/OUTPUT.md. These run the built binary against the tracked fixtures.

use std::path::PathBuf;
use std::process::{Command, Output};

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
    let out = run(&["--json"], "hi.dem");
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
    let out = run(&["--csv"], "hi.dem");
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
    let out = run(&["--summary"], "hi.dem");
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("Source 2 PBDEMS2"));
}

#[test]
fn json_on_source1_is_a_json_array() {
    let out = run(&["--json"], "backgrounddem.dem");
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("[\n"));
    assert!(stdout.trim_end().ends_with(']'));
}

#[test]
fn csv_on_source1_starts_with_the_documented_header() {
    let out = run(&["--csv"], "backgrounddem.dem");
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
