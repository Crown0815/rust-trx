use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn workspace() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path().join("workspace");
    fs::create_dir(&cwd).expect("create workspace");
    (dir, cwd)
}

fn run_cli<I, S>(cwd: &Path, args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut command = Command::new(env!("CARGO_BIN_EXE_rust-trx"));
    command.current_dir(cwd).env_remove("CI");
    for arg in args {
        command.arg(arg.into());
    }
    command.output().expect("run cli")
}

fn output_text(output: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, contents).expect("write file");
}

fn trx(results: &str, definitions: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<TestRun>
  <Results>
{results}
  </Results>
  <TestDefinitions>
{definitions}
  </TestDefinitions>
</TestRun>"#
    )
}

fn passed(id: &str, name: &str, duration: &str, stdout: &str) -> String {
    format!(
        r#"    <UnitTestResult testId="{id}" testName="{name}" outcome="Passed" duration="{duration}">
      <Output><StdOut>{stdout}</StdOut></Output>
    </UnitTestResult>"#
    )
}

fn failed(id: &str, name: &str, duration: &str, message: &str, stack_trace: &str) -> String {
    format!(
        r#"    <UnitTestResult testId="{id}" testName="{name}" outcome="Failed" duration="{duration}">
      <Output>
        <ErrorInfo>
          <Message>{message}</Message>
          <StackTrace>{stack_trace}</StackTrace>
        </ErrorInfo>
      </Output>
    </UnitTestResult>"#
    )
}

fn skipped(id: &str, name: &str, reason: &str) -> String {
    format!(
        r#"    <UnitTestResult testId="{id}" testName="{name}" outcome="NotExecuted" duration="00:00:01">
      <Output><ErrorInfo><Message>{reason}</Message></ErrorInfo></Output>
    </UnitTestResult>"#
    )
}

fn definition(id: &str, class_name: &str, method: &str) -> String {
    format!(
        r#"    <UnitTest id="{id}" name="{method}">
      <TestMethod className="{class_name}" name="{method}"/>
    </UnitTest>"#
    )
}

#[test]
fn question_mark_alias_prints_help() {
    let (_dir, cwd) = workspace();

    let output = run_cli(&cwd, ["-?"]);
    let (stdout, stderr) = output_text(&output);

    assert!(output.status.success());
    assert!(stderr.is_empty());
    assert!(stdout.contains("Pretty-print test results in TRX format"));
    assert!(stdout.contains("Usage: trx"));
}

#[test]
fn empty_directory_reports_zero_tests() {
    let (_dir, cwd) = workspace();

    let output = run_cli(&cwd, std::iter::empty::<&str>());
    let (stdout, stderr) = output_text(&output);

    assert!(output.status.success());
    assert!(stderr.is_empty());
    assert!(stdout.contains("👉 Run 0 tests in ~ 0s ✅"));
}

#[test]
fn failed_results_exit_with_255_unless_no_exit_code_is_set() {
    let (_dir, cwd) = workspace();
    let stack_file = cwd.join("src/test.cs");
    let stack = format!("at Tests.Failing in {}:line 10", stack_file.display());
    let xml = trx(
        &failed("1", "Failing", "00:00:01.500", "boom", &stack),
        &definition("1", "Tests", "Failing"),
    );
    write_file(&cwd.join("results.trx"), &xml);

    let failed = run_cli(&cwd, ["--path", "."]);
    let (stdout, stderr) = output_text(&failed);
    assert_eq!(failed.status.code(), Some(255));
    assert!(stderr.is_empty());
    assert!(stdout.contains("❌ Failing"));
    assert!(stdout.contains("boom"));
    assert!(stdout.contains("👉 Run 1 tests in ~ 1s ❌"));

    let ignored = run_cli(&cwd, ["--path", ".", "--no-exit-code"]);
    let (stdout, stderr) = output_text(&ignored);
    assert!(ignored.status.success());
    assert!(stderr.is_empty());
    assert!(stdout.contains("❌ Failing"));
    assert!(stdout.contains("👉 Run 1 tests in ~ 1s ❌"));
}

#[test]
fn verbosity_and_output_expose_passed_skipped_failed_and_sorted_results() {
    let (_dir, cwd) = workspace();
    let stack_file = cwd.join("src/test.cs");
    let stack = format!(
        "at Tests.Failing in {}:line 10\nat Other.Frame in /outside/file.cs:line 2",
        stack_file.display()
    );
    let results = [
        passed("1", "abc", "01:00:00", "alpha\nbeta"),
        failed("2", "ABC", "00:02:00", "boom", &stack),
        skipped("3", "Skipped", "not today"),
        r#"    <UnitTestResult testId="4" testName="Other" outcome="Timeout" duration="00:00:05"/>"#
            .to_string(),
    ]
    .join("\n");
    let definitions = definition("2", "Tests", "Failing");
    let xml = trx(&results, &definitions);
    let file = cwd.join("RESULTS.TRX");
    write_file(&file, &xml);

    let output = run_cli(
        &cwd,
        [
            "--path",
            file.to_str().expect("utf8 path"),
            "--verbosity",
            "verbose",
            "--output",
            "--no-exit-code",
        ],
    );
    let (stdout, stderr) = output_text(&output);

    assert!(output.status.success());
    assert!(stderr.is_empty());
    assert!(stdout.contains("❌ ABC"));
    assert!(stdout.contains("at Tests.Failing in"));
    assert!(stdout.contains("src/test.cs:line 10"));
    assert!(!stdout.contains("Other.Frame"));
    assert!(stdout.contains("✅ abc"));
    assert!(stdout.contains("     alpha\n     beta"));
    assert!(stdout.contains("❔ Skipped => not today"));
    assert!(stdout.contains("👉 Run 3 tests in ~ 1h 2m ❌"));
    assert!(stdout.contains("   ✅ 1 passed"));
    assert!(stdout.contains("   ❌ 1 failed"));
    assert!(stdout.contains("   ❔ 1 skipped"));

    let fail_index = stdout.find("❌ ABC").expect("failed result");
    let pass_index = stdout.find("✅ abc").expect("passed result");
    assert!(fail_index < pass_index);
}

#[test]
fn recursive_discovery_skips_hidden_directories_and_can_be_disabled() {
    let (_dir, cwd) = workspace();
    let visible = trx(&passed("1", "Visible", "00:00:01", ""), "");
    let nested = trx(&passed("2", "Nested", "00:00:01", ""), "");
    let hidden = trx(&passed("3", "Hidden", "00:00:01", ""), "");
    write_file(&cwd.join("visible.trx"), &visible);
    write_file(&cwd.join("nested/nested.trx"), &nested);
    write_file(&cwd.join(".hidden/hidden.trx"), &hidden);

    let recursive = run_cli(&cwd, ["--verbosity", "verbose"]);
    let (stdout, stderr) = output_text(&recursive);
    assert!(recursive.status.success());
    assert!(stderr.is_empty());
    assert!(stdout.contains("✅ Nested"));
    assert!(stdout.contains("✅ Visible"));
    assert!(!stdout.contains("Hidden"));
    assert!(stdout.contains("👉 Run 2 tests in ~ 2s ✅"));

    let non_recursive = run_cli(&cwd, ["--recursive=false", "--verbosity", "verbose"]);
    let (stdout, stderr) = output_text(&non_recursive);
    assert!(non_recursive.status.success());
    assert!(stderr.is_empty());
    assert!(!stdout.contains("Nested"));
    assert!(stdout.contains("✅ Visible"));
    assert!(stdout.contains("👉 Run 1 tests in ~ 1s ✅"));
}

#[test]
fn parse_errors_are_reported_and_do_not_fail_the_run() {
    let (_dir, cwd) = workspace();
    write_file(&cwd.join("bad.trx"), "<TestRun>");

    let output = run_cli(&cwd, std::iter::empty::<&str>());
    let (stdout, stderr) = output_text(&output);

    assert!(output.status.success());
    assert!(stderr.contains("Failed to parse"));
    assert!(stdout.contains("👉 Run 0 tests in ~ 0s ✅"));
}

#[test]
fn github_summary_contains_badges_details_author_and_run_marker() {
    let (_dir, cwd) = workspace();
    let summary_path = cwd.join("summary.md");
    let comment_path = cwd.join("comment.md");
    let fake_bin = cwd.join("bin");
    fs::create_dir(&fake_bin).expect("create bin");
    let fake_gh = fake_bin.join("gh");
    write_file(
        &fake_gh,
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  echo "gh version 2.0.0"
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "comment" ]; then
  while [ "$#" -gt 0 ]; do
    if [ "$1" = "--body-file" ]; then
      cp "$2" "$CAPTURE_COMMENT"
      exit 0
    fi
    shift
  done
fi
exit 0
"#,
    );
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&fake_gh).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_gh, permissions).expect("chmod");
    }

    let stack_file = cwd.join("src/test.cs");
    let stack = format!("at Tests.Failing in {}:line 10", stack_file.display());
    let results = [
        passed("1", "Passing", "00:01:05", "stdout"),
        failed("2", "Failing", "00:00:10", "boom", &stack),
        skipped("3", "Skipped", "later"),
    ]
    .join("\n");
    let definitions = definition("2", "Tests", "Failing");
    write_file(&cwd.join("results.trx"), &trx(&results, &definitions));

    let mut command = Command::new(env!("CARGO_BIN_EXE_rust-trx"));
    command
        .current_dir(&cwd)
        .args(["--no-exit-code", "--output", "--verbosity", "verbose"])
        .env("CI", "true")
        .env("GITHUB_EVENT_NAME", "pull_request")
        .env("GITHUB_ACTIONS", "true")
        .env("GITHUB_REF_NAME", "123/merge")
        .env("GITHUB_REPOSITORY", "owner/repo")
        .env("GITHUB_RUN_ID", "456")
        .env("GITHUB_JOB", "test")
        .env("GITHUB_SERVER_URL", "https://github.example")
        .env("JOB_CHECK_RUN_ID", "789")
        .env("GITHUB_STEP_SUMMARY", &summary_path)
        .env("CAPTURE_COMMENT", &comment_path)
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );

    let output = command.output().expect("run cli");
    let (stdout, stderr) = output_text(&output);
    assert!(output.status.success());
    assert!(stderr.is_empty());
    assert!(stdout.contains("👉 Run 3 tests in ~ 1m 15s ❌"));

    let summary = fs::read_to_string(&summary_path).expect("summary");
    assert!(summary.contains("failed-1"));
    assert!(summary.contains("passed-1"));
    assert!(summary.contains("skipped-1"));
    assert!(summary.contains("1m%2015s"));
    assert!(summary.contains("https://github.example/owner/repo/actions/runs/456/jobs/789?pr=123"));
    assert!(summary.contains("<summary>:test_tube: Details on"));
    assert!(summary.contains(":white_check_mark: Passing"));
    assert!(summary.contains("<summary>:x: Failing</summary>"));
    assert!(summary.contains("src/test.cs:line 10"));
    assert!(summary.contains(":grey_question: Skipped => later"));
    assert!(summary.contains("from [rust-trx]"));

    let comment = fs::read_to_string(&comment_path).expect("comment");
    assert!(comment.contains("<!-- trx:456 -->"));
}

#[test]
fn invalid_directory_exits_with_discovery_error() {
    let (_dir, cwd) = workspace();
    let missing = PathBuf::from("missing");

    let output = run_cli(&cwd, ["--path".into(), missing.into_os_string()]);
    let (stdout, stderr) = output_text(&output);

    assert_eq!(output.status.code(), Some(2));
    assert!(stdout.is_empty());
    assert!(stderr.contains("Failed to discover TRX files under"));
}
