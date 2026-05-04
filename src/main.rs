use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::time::Duration;

use clap::{ArgAction, Parser, ValueEnum};
use regex::Regex;
use roxmltree::{Document, Node};
use serde_json::json;
use tempfile::NamedTempFile;
use walkdir::WalkDir;

const HEADER: &str = "<!-- header -->";
const FOOTER: &str = "<!-- footer -->";

#[derive(Clone, Debug, ValueEnum, Default, PartialEq, Eq)]
enum Verbosity {
    #[default]
    Quiet,
    Normal,
    Verbose,
}

#[derive(Parser, Debug)]
#[command(
    name = "trx",
    bin_name = "trx",
    about = "Pretty-print test results in TRX format"
)]
struct Cli {
    #[arg(short, long)]
    path: Option<PathBuf>,

    #[arg(short, long, default_value_t = false)]
    output: bool,

    #[arg(short = 'r', long, default_value_t = true, action = ArgAction::Set)]
    recursive: bool,

    #[arg(short, long, value_enum, default_value_t = Verbosity::Quiet)]
    verbosity: Verbosity,

    #[arg(long, default_value_t = false)]
    no_exit_code: bool,

    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    gh_comment: bool,

    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    gh_summary: bool,
}

#[derive(Clone, Debug)]
struct TestResult {
    test_id: String,
    test_name: String,
    outcome: Outcome,
    duration: Duration,
    stdout: Option<String>,
    skipped_reason: Option<String>,
    message: Option<String>,
    stack_trace: Option<String>,
    method_full_name: Option<String>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Outcome {
    Passed,
    Failed,
    NotExecuted,
    Other,
}

#[derive(Clone, Debug)]
struct Summary {
    passed: usize,
    failed: usize,
    skipped: usize,
    duration: Duration,
}

impl Summary {
    fn total(&self) -> usize {
        self.passed + self.failed + self.skipped
    }
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
struct Failed {
    title: String,
    message: String,
    file: String,
    line: usize,
}

fn main() {
    let args = normalize_args(env::args().collect());
    let cli = Cli::parse_from(args);

    let base_path = normalize_path(cli.path.clone());

    let discover = discover_trx_files(&base_path, cli.recursive);
    let files = match discover {
        Ok(v) => v,
        Err(err) => {
            eprintln!(
                "Failed to discover TRX files under {}: {err}",
                base_path.display()
            );
            process::exit(2);
        }
    };

    let mut test_ids = HashSet::new();
    let mut results = Vec::new();

    for file in files {
        let parsed = match parse_trx_file(&file) {
            Ok(v) => v,
            Err(err) => {
                eprintln!("Failed to parse {}: {err}", file.display());
                continue;
            }
        };

        for result in parsed {
            if test_ids.insert(result.test_id.clone()) {
                results.push(result);
            }
        }
    }

    results.sort_by(|a, b| compare_names(&a.test_name, &b.test_name));

    let mut summary = Summary {
        passed: 0,
        failed: 0,
        skipped: 0,
        duration: Duration::ZERO,
    };
    let mut failures = Vec::new();
    let mut details = String::new();
    details.push_str("<details>\n\n");
    details.push_str(&format!(
        "<summary>:test_tube: Details on {}</summary>\n\n",
        os_description()
    ));

    for result in &results {
        match result.outcome {
            Outcome::Passed => {
                summary.passed += 1;
                summary.duration += result.duration;
                if cli.verbosity == Verbosity::Verbose {
                    println!("✅ {}", result.test_name);
                    if let Some(output) = cli.output.then(|| result.stdout.clone()).flatten() {
                        print_indented_block(&output);
                        details.push_str("<details>\n\n");
                        details.push_str(&format!(
                            "<summary>:white_check_mark: {}</summary>\n\n",
                            result.test_name
                        ));
                        append_markdown_indented(&mut details, &output, "> &gt; ");
                        details.push_str("\n</details>\n\n");
                    } else {
                        details.push_str(&format!(":white_check_mark: {}\n", result.test_name));
                    }
                }
            }
            Outcome::Failed => {
                summary.failed += 1;
                summary.duration += result.duration;
                println!("❌ {}", result.test_name);
                details.push_str("<details>\n\n");
                details.push_str(&format!("<summary>:x: {}</summary>\n\n", result.test_name));
                write_error(&base_path, &mut failures, result, &mut details);
                if let Some(output) = cli.output.then(|| result.stdout.clone()).flatten() {
                    append_markdown_indented(&mut details, &output, "> &gt; ");
                    print_indented_block(&output);
                }
                details.push_str("\n</details>\n\n");
            }
            Outcome::NotExecuted => {
                summary.skipped += 1;
                if cli.verbosity != Verbosity::Quiet {
                    match &result.skipped_reason {
                        Some(reason) => {
                            println!("❔ {} => {}", result.test_name, reason);
                            details.push_str(&format!(
                                ":grey_question: {} => {}\n",
                                result.test_name, reason
                            ));
                        }
                        None => {
                            println!("❔ {}", result.test_name);
                            details.push_str(&format!(":grey_question: {}\n", result.test_name));
                        }
                    }
                }
            }
            Outcome::Other => {}
        }
    }

    details.push_str("\n</details>\n");

    println!();
    print_summary(&summary);
    println!();

    if env::var("CI").ok().as_deref() == Some("true") && (cli.gh_comment || cli.gh_summary) {
        if let Err(err) = github_report(&cli, &summary, &details) {
            eprintln!("GitHub reporting failed: {err}");
        }

        if !failures.is_empty() {
            let _ = failures;
        }
    }

    let exit_code = exit_code_for_summary(cli.no_exit_code, &summary);
    if exit_code != 0 {
        process::exit(exit_code);
    }
}

fn exit_code_for_summary(no_exit_code: bool, summary: &Summary) -> i32 {
    if !no_exit_code && summary.failed > 0 {
        255
    } else {
        0
    }
}

fn normalize_args(args: Vec<String>) -> Vec<String> {
    args.into_iter()
        .map(|arg| if arg == "-?" { "-h".to_string() } else { arg })
        .collect()
}

fn normalize_path(path: Option<PathBuf>) -> PathBuf {
    let mut path = path.unwrap_or_else(|| env::current_dir().expect("cwd"));

    if !path.is_absolute() {
        path = env::current_dir().expect("cwd").join(path);
    }

    if path.is_file() {
        return path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| env::current_dir().expect("cwd"));
    }

    path
}

fn discover_trx_files(base: &Path, recursive: bool) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    if recursive {
        for entry in WalkDir::new(base)
            .into_iter()
            .filter_entry(|e| !is_hidden_dir(e.file_name()))
        {
            let entry = match entry {
                Ok(v) => v,
                Err(err) => {
                    let io_err = io::Error::other(err.to_string());
                    return Err(io_err);
                }
            };

            if entry.file_type().is_file() && is_trx(entry.path()) {
                files.push(entry.path().to_path_buf());
            }
        }
    } else {
        for entry in fs::read_dir(base)? {
            let entry = entry?;
            if entry.file_type()?.is_file() && is_trx(&entry.path()) {
                files.push(entry.path());
            }
        }
    }

    files.sort_by(|a, b| {
        let am = a.metadata().and_then(|m| m.modified()).ok();
        let bm = b.metadata().and_then(|m| m.modified()).ok();
        bm.cmp(&am)
    });

    Ok(files)
}

fn is_hidden_dir(name: &OsStr) -> bool {
    matches!(name.to_str(), Some(s) if s.starts_with('.') && s != "." && s != "..")
}

fn is_trx(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .map(|ext| ext.eq_ignore_ascii_case("trx"))
        .unwrap_or(false)
}

fn parse_trx_file(path: &Path) -> Result<Vec<TestResult>, String> {
    let source = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let doc = Document::parse(&source).map_err(|e| e.to_string())?;

    let mut methods = HashMap::new();
    for node in doc
        .descendants()
        .filter(|n| tag_name(*n) == Some("UnitTest"))
    {
        let Some(id) = node.attribute("id") else {
            continue;
        };

        let Some(test_method) = node
            .descendants()
            .find(|n| tag_name(*n) == Some("TestMethod"))
        else {
            continue;
        };

        let class_name = test_method.attribute("className").unwrap_or("");
        let method_name = test_method.attribute("name").unwrap_or("");
        if !class_name.is_empty() || !method_name.is_empty() {
            methods.insert(id.to_string(), format!("{class_name}.{method_name}"));
        }
    }

    let mut results = Vec::new();

    for result in doc
        .descendants()
        .filter(|n| tag_name(*n) == Some("UnitTestResult"))
    {
        let Some(test_id) = result.attribute("testId") else {
            continue;
        };

        let test_name = result
            .attribute("testName")
            .unwrap_or("(unknown)")
            .to_string();
        let outcome = match result.attribute("outcome") {
            Some("Passed") => Outcome::Passed,
            Some("Failed") => Outcome::Failed,
            Some("NotExecuted") => Outcome::NotExecuted,
            _ => Outcome::Other,
        };

        let duration = parse_duration(result.attribute("duration").unwrap_or("0"));
        let stdout = find_child_text(result, &["Output", "StdOut"]);
        let skipped_reason = find_child_text(result, &["Output", "ErrorInfo", "Message"]);
        let message = find_child_text(result, &["Output", "ErrorInfo", "Message"])
            .or_else(|| find_descendant_text(result, "Message"));
        let stack_trace = find_child_text(result, &["Output", "ErrorInfo", "StackTrace"])
            .or_else(|| find_descendant_text(result, "StackTrace"));

        results.push(TestResult {
            test_id: test_id.to_string(),
            test_name,
            outcome,
            duration,
            stdout,
            skipped_reason,
            message,
            stack_trace,
            method_full_name: methods.get(test_id).cloned(),
        });
    }

    Ok(results)
}

fn tag_name<'a, 'input>(node: Node<'a, 'input>) -> Option<&'input str> {
    node.is_element().then(|| node.tag_name().name())
}

fn find_child_text(node: Node<'_, '_>, path: &[&str]) -> Option<String> {
    let mut current = node;
    for segment in path {
        current = current
            .children()
            .find(|n| tag_name(*n) == Some(*segment))?;
    }

    current
        .text()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn find_descendant_text(node: Node<'_, '_>, target: &str) -> Option<String> {
    node.descendants()
        .find(|n| tag_name(*n) == Some(target))
        .and_then(|n| n.text())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn parse_duration(raw: &str) -> Duration {
    if raw == "0" || raw.is_empty() {
        return Duration::ZERO;
    }

    let mut split = raw.splitn(3, ':');
    let Some(h) = split.next().and_then(|s| s.parse::<u64>().ok()) else {
        return Duration::ZERO;
    };
    let Some(m) = split.next().and_then(|s| s.parse::<u64>().ok()) else {
        return Duration::ZERO;
    };
    let Some(sec_raw) = split.next() else {
        return Duration::ZERO;
    };

    let (s, nsec) = parse_seconds_fraction(sec_raw);
    Duration::from_secs(h * 3600 + m * 60 + s) + Duration::from_nanos(nsec)
}

fn parse_seconds_fraction(raw: &str) -> (u64, u64) {
    match raw.split_once('.') {
        Some((sec, frac)) => {
            let sec = sec.parse::<u64>().unwrap_or(0);
            let frac_clean: String = frac.chars().take(9).collect();
            let padded = format!("{frac_clean:0<9}");
            let nsec = padded.parse::<u64>().unwrap_or(0);
            (sec, nsec)
        }
        None => (raw.parse::<u64>().unwrap_or(0), 0),
    }
}

fn compare_names(a: &str, b: &str) -> Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

fn print_indented_block(output: &str) {
    for line in output.lines() {
        println!("     {line}");
    }
}

fn append_markdown_indented(target: &mut String, value: &str, indent: &str) {
    for line in value.lines() {
        target.push_str(indent);
        target.push_str(line);
        target.push('\n');
    }
}

fn write_error(
    base_dir: &Path,
    failures: &mut Vec<Failed>,
    result: &TestResult,
    details: &mut String,
) {
    let (Some(message), Some(stack_trace)) = (&result.message, &result.stack_trace) else {
        return;
    };

    let mut lines: Vec<String> = stack_trace
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(ToString::to_string)
        .collect();

    if let Some(full_name) = &result.method_full_name
        && let Some(last) = lines.iter().rposition(|x| x.contains(full_name))
    {
        lines.truncate(last + 1);
    }

    let parse_file = Regex::new(r" in (?P<file>.+):line (?P<line>\d+)").expect("regex");

    println!("     {message}");

    details.push_str("> ```csharp\n");
    append_markdown_indented(details, message, "> ");

    let mut last_failed: Option<Failed> = None;
    for line in lines {
        println!("     {line}");

        if let Some(caps) = parse_file.captures(&line) {
            let file = caps
                .name("file")
                .map(|m| m.as_str())
                .unwrap_or_default()
                .to_string();
            let line_no = caps
                .name("line")
                .and_then(|m| m.as_str().parse::<usize>().ok())
                .unwrap_or(0);

            let relative = make_relative(base_dir, &file);
            let rewritten = line.replacen(&file, &relative, 1);
            append_markdown_indented(details, &rewritten, "> ");

            last_failed = Some(Failed {
                title: result.test_name.clone(),
                message: message.clone(),
                file: relative,
                line: line_no,
            });
        } else {
            append_markdown_indented(details, &line, "> ");
        }
    }

    details.push_str("> ```\n");

    if let Some(failed) = last_failed {
        failures.push(failed);
    }
}

fn make_relative(base_dir: &Path, file: &str) -> String {
    let path = Path::new(file);
    if path.is_absolute()
        && let Ok(stripped) = path.strip_prefix(base_dir)
    {
        return stripped.to_string_lossy().to_string();
    }

    file.to_string()
}

fn print_summary(summary: &Summary) {
    println!(
        "👉 Run {} tests in ~ {}{}",
        summary.total(),
        humanize_duration(summary.duration),
        if summary.failed > 0 { " ❌" } else { " ✅" }
    );

    if summary.passed > 0 {
        println!("   ✅ {} passed", summary.passed);
    }
    if summary.failed > 0 {
        println!("   ❌ {} failed", summary.failed);
    }
    if summary.skipped > 0 {
        println!("   ❔ {} skipped", summary.skipped);
    }
}

fn humanize_duration(duration: Duration) -> String {
    let mut secs = duration.as_secs();
    let hours = secs / 3600;
    secs %= 3600;
    let minutes = secs / 60;
    let seconds = secs % 60;

    let mut parts = Vec::new();
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0 || parts.is_empty() {
        parts.push(format!("{seconds}s"));
    }
    parts.join(" ")
}

fn runtime() -> String {
    format!("{}-{}", env::consts::OS, env::consts::ARCH)
}

fn os_description() -> String {
    runtime().replace('-', "&dash;")
}

fn author() -> String {
    format!(
        "from [rust-trx](https://github.com/crown0815/rust-trx) v{} with [:purple_heart:](https://github.com/sponsors/crown0815) by @crown0815",
        env!("CARGO_PKG_VERSION")
    )
}

fn github_report(cli: &Cli, summary: &Summary, details: &str) -> Result<(), String> {
    if summary.total() == 0 {
        return Ok(());
    }

    if env::var("GITHUB_EVENT_NAME").ok().as_deref() != Some("pull_request")
        || env::var("GITHUB_ACTIONS").ok().as_deref() != Some("true")
    {
        return Ok(());
    }

    let Some(gh_version) = try_execute("gh", ["--version"], None) else {
        return Ok(());
    };
    if !gh_version.starts_with("gh version") {
        return Ok(());
    }

    let branch = env::var("GITHUB_REF_NAME").map_err(|_| "missing GITHUB_REF_NAME")?;
    if !branch.ends_with("/merge") {
        return Ok(());
    }
    let pr: u64 = branch[..branch.len() - 6]
        .parse()
        .map_err(|_| "invalid PR branch format")?;

    let repo = env::var("GITHUB_REPOSITORY").map_err(|_| "missing GITHUB_REPOSITORY")?;
    let run_id = env::var("GITHUB_RUN_ID").map_err(|_| "missing GITHUB_RUN_ID")?;
    let mut job_name = env::var("GITHUB_JOB").map_err(|_| "missing GITHUB_JOB")?;
    let server_url = env::var("GITHUB_SERVER_URL").map_err(|_| "missing GITHUB_SERVER_URL")?;

    if let Ok(override_name) = env::var("GH_JOB_NAME")
        && !override_name.is_empty()
    {
        job_name = override_name;
    }

    let mut job_url = env::var("JOB_CHECK_RUN_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|job_id| format!("{server_url}/{repo}/actions/runs/{run_id}/jobs/{job_id}?pr={pr}"));

    let elapsed = humanize_duration(summary.duration);

    if job_url.is_none() {
        let jq = format!(
            "[.jobs[] | select(.name == \"{}\") | .id]",
            job_name.replace('"', "\\\"")
        );
        if let Some(jobs_json) = try_execute(
            "gh",
            [
                "api",
                &format!("repos/{repo}/actions/runs/{run_id}/jobs"),
                "--jq",
                &jq,
            ],
            None,
        ) && let Ok(ids) = serde_json::from_str::<Vec<u64>>(&jobs_json)
            && ids.len() == 1
        {
            job_url = Some(format!(
                "{server_url}/{repo}/actions/runs/{run_id}/job/{}?pr={pr}",
                ids[0]
            ));
        }
    }

    let mut body = String::new();
    let mut comment_id = 0_u64;

    if let Some(comment) = try_execute(
        "gh",
        [
            "api",
            &format!("repos/{repo}/issues/{pr}/comments"),
            "--jq",
            "[.[] | { id:.id, body:.body } | select(.body | contains(\"<!-- trx\")) | .id][0]",
        ],
        None,
    ) && let Ok(id) = comment.trim().parse::<u64>()
        && let Some(existing_body) = try_execute(
            "gh",
            [
                "api",
                &format!("repos/{repo}/issues/comments/{id}"),
                "--jq",
                ".body",
            ],
            None,
        )
    {
        comment_id = id;
        if let (Some(start), Some(end)) = (existing_body.find(HEADER), existing_body.find(FOOTER))
            && end > start
            && run_id_matches(&existing_body, &run_id)
        {
            body.push_str(existing_body[..start].trim_end());
            append_badges(summary, &mut body, &elapsed, job_url.as_deref());
            body.push_str(existing_body[start..end].trim());
            body.push('\n');
            body.push_str(details);
            body.push('\n');
            body.push_str(existing_body[end..].trim_start());
        }
    }

    if body.trim().is_empty() {
        append_badges(summary, &mut body, &elapsed, job_url.as_deref());
        body.push_str(HEADER);
        body.push_str("\n\n");
        body.push_str(details);
        body.push('\n');
        body.push_str(FOOTER);
        body.push_str("\n\n");
        body.push_str(&author());
        body.push('\n');
    }

    if let Some(idx) = body.find("<!-- trx") {
        body = body[..idx].trim().to_string();
    }
    body.push('\n');
    body.push_str(&format!("<!-- trx:{run_id} -->"));

    if cli.gh_comment {
        if comment_id > 0 {
            let payload = json!({ "body": body });
            let mut file = NamedTempFile::new().map_err(|e| e.to_string())?;
            serde_json::to_writer(file.as_file_mut(), &payload).map_err(|e| e.to_string())?;
            let _ = try_execute(
                "gh",
                [
                    "api",
                    &format!("repos/{repo}/issues/comments/{comment_id}"),
                    "-X",
                    "PATCH",
                    "--input",
                    file.path().to_string_lossy().as_ref(),
                ],
                None,
            );
        } else {
            let file = NamedTempFile::new().map_err(|e| e.to_string())?;
            fs::write(file.path(), &body).map_err(|e| e.to_string())?;
            let _ = try_execute(
                "gh",
                [
                    "pr",
                    "comment",
                    &pr.to_string(),
                    "--body-file",
                    file.path().to_string_lossy().as_ref(),
                ],
                None,
            );
        }
    }

    if cli.gh_summary
        && let Ok(summary_path) = env::var("GITHUB_STEP_SUMMARY")
        && !summary_path.is_empty()
    {
        let mut text = String::new();
        append_badges(summary, &mut text, &elapsed, job_url.as_deref());
        text.push('\n');
        text.push_str(details);
        text.push('\n');
        text.push_str(&author());
        text.push('\n');
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(summary_path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()))
            .map_err(|e| e.to_string())?;
    }

    Ok(())
}

fn run_id_matches(body: &str, run_id: &str) -> bool {
    let re = Regex::new(r"<!--\strx:(?P<id>\d+)\s-->").expect("regex");
    re.captures(body)
        .and_then(|caps| caps.name("id"))
        .map(|m| m.as_str() == run_id)
        .unwrap_or(false)
}

fn append_badges(summary: &Summary, target: &mut String, elapsed: &str, job_url: Option<&str>) {
    let elapsed = elapsed.replace(' ', "%20");
    let runtime_badge = if summary.failed > 0 {
        format!(
            "![{} failed](https://img.shields.io/badge/❌-{}%20in%20{}-blue)",
            summary.failed,
            runtime(),
            elapsed
        )
    } else if summary.passed > 0 {
        format!(
            "![{} passed](https://img.shields.io/badge/✅-{}%20in%20{}-blue)",
            summary.passed,
            runtime(),
            elapsed
        )
    } else {
        format!(
            "![{} skipped](https://img.shields.io/badge/⚪-{}%20in%20{}-blue)",
            summary.skipped,
            runtime(),
            elapsed
        )
    };

    push_link(target, &runtime_badge, job_url);
    if summary.passed > 0 {
        push_link(
            target,
            &format!(
                "![{} passed](https://img.shields.io/badge/passed-{}-brightgreen)",
                summary.passed, summary.passed
            ),
            job_url,
        );
    }
    if summary.failed > 0 {
        push_link(
            target,
            &format!(
                "![{} failed](https://img.shields.io/badge/failed-{}-red)",
                summary.failed, summary.failed
            ),
            job_url,
        );
    }
    if summary.skipped > 0 {
        push_link(
            target,
            &format!(
                "![{} skipped](https://img.shields.io/badge/skipped-{}-silver)",
                summary.skipped, summary.skipped
            ),
            job_url,
        );
    }
    target.push('\n');
}

fn push_link(target: &mut String, image: &str, url: Option<&str>) {
    match url {
        Some(url) => {
            target.push_str(&format!("[{image}]({url}) "));
        }
        None => {
            target.push_str(image);
            target.push(' ');
        }
    }
}

fn try_execute<I, S>(program: &str, args: I, input: Option<&str>) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut command = Command::new(program);
    for arg in args {
        command.arg(arg.as_ref());
    }

    if input.is_some() {
        command.stdin(process::Stdio::piped());
    }

    command.stdout(process::Stdio::piped());
    command.stderr(process::Stdio::piped());

    let mut child = command.spawn().ok()?;

    if let Some(input) = input
        && let Some(mut stdin) = child.stdin.take()
    {
        let _ = std::io::Write::write_all(&mut stdin, input.as_bytes());
    }

    let output = child.wait_with_output().ok()?;
    if !output.status.success() || !output.stderr.is_empty() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}
