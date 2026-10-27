// CLI contract tests: exit codes and stdout/stderr behaviour documented in
// docs/OUTPUT.md. These run the built binary against the tracked fixtures.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

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

#[test]
fn csv_and_json_together_exit_one_with_empty_stdout() {
    for demo in ["backgrounddem.dem", "hi.dem"] {
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
            .arg(fixture("backgrounddem.dem"))
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
