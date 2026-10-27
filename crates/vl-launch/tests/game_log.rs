//! The game process against a synthetic game: this test binary started again with a mode in the
//! environment prints logs, writes crash files, spams or hangs. No Minecraft is involved.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::TempDir;
use vl_core::launch::GameExit;
use vl_core::log::{LogEntry, LogLevel};
use vl_launch::CommandLine;
use vl_launch::process::{GameBatch, RunningGame, SpawnOptions, spawn};

const MODE: &str = "VL_SYNTHETIC_GAME";

/// The synthetic game. Run on its own it does nothing; the tests below start it with a mode.
#[test]
fn synthetic_game() {
    let Ok(mode) = std::env::var(MODE) else {
        return;
    };
    let out = std::io::stdout();
    let mut out = out.lock();
    match mode.as_str() {
        "xml" => {
            // The test harness leaves "test synthetic_game ... " on the current line.
            let _ = writeln!(out);
            let _ = writeln!(
                out,
                r#"<log4j:Event logger="net.minecraft.client.Minecraft" timestamp="1" level="INFO" thread="Render thread"><log4j:Message><![CDATA[Setting user: Steve]]></log4j:Message></log4j:Event>"#
            );
            let _ = writeln!(
                out,
                r#"<log4j:Event logger="Mod" timestamp="2" level="ERROR" thread="main">"#
            );
            let _ = writeln!(out, "<log4j:Message><![CDATA[Broken]]></log4j:Message>");
            let _ = writeln!(
                out,
                "<log4j:Throwable><![CDATA[java.lang.IllegalStateException: x\n\tat a.B.c(B.java:1)]]></log4j:Throwable>"
            );
            let _ = writeln!(out, "</log4j:Event>");
            let _ = writeln!(out, "[12:00:00] [main/WARN]: plain after xml");
            eprintln!("Exception in thread \"main\" java.lang.Error");
        }
        "crash" => {
            let _ = fs::create_dir_all("crash-reports");
            let _ = fs::write(
                "crash-reports/crash-2026-01-01_00.00.00-client.txt",
                "report",
            );
            let _ = writeln!(out, "[12:00:00] [main/ERROR]: crashing");
            drop(out);
            std::process::exit(1);
        }
        "crash_exit0" => {
            let _ = fs::create_dir_all("crash-reports");
            let _ = fs::write("crash-reports/crash-new.txt", "report");
        }
        "hs_err" => {
            let _ = fs::write(format!("hs_err_pid{}.log", std::process::id()), "fatal");
            drop(out);
            std::process::exit(134);
        }
        "quiet" => {}
        "hang" => {
            let _ = writeln!(out, "[12:00:00] [main/INFO]: running");
            let _ = out.flush();
            drop(out);
            std::thread::sleep(Duration::from_secs(120));
        }
        "spam" => {
            let line = format!("[12:00:00] [main/INFO]: {}\n", "x".repeat(100));
            let block = line.repeat(1000);
            for _ in 0..100 {
                let _ = out.write_all(block.as_bytes());
            }
        }
        other => panic!("unknown mode {other}"),
    }
}

fn game(mode: &str, dir: &Path) -> CommandLine {
    let exe = std::env::current_exe().unwrap();
    let args = [
        "--exact",
        "synthetic_game",
        "--nocapture",
        "--test-threads=1",
    ];
    let mut command = CommandLine::new(
        exe,
        args.iter().map(|a| (*a).to_owned()).collect(),
        dir.to_path_buf(),
        Vec::new(),
    );
    command.env.insert(MODE.to_owned(), mode.to_owned());
    command
}

struct Run {
    lines: Vec<LogEntry>,
    skipped: u64,
    batches: usize,
    exit: GameExit,
    game: RunningGame,
}

fn start(
    command: &CommandLine,
    options: &SpawnOptions,
) -> (
    RunningGame,
    mpsc::Receiver<GameExit>,
    Arc<Mutex<Vec<GameBatch>>>,
) {
    let batches = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&batches);
    let (tx, rx) = mpsc::channel();
    let game = spawn(
        command,
        options,
        move |b| sink.lock().unwrap().push(b),
        move |e| {
            let _ = tx.send(e);
        },
    )
    .unwrap();
    (game, rx, batches)
}

fn run(mode: &str, dir: &Path, options: &SpawnOptions) -> Run {
    let (game, rx, batches) = start(&game(mode, dir), options);
    let exit = rx.recv_timeout(Duration::from_secs(60)).expect("exit");
    let batches = batches.lock().unwrap();
    Run {
        lines: batches.iter().flat_map(|b| b.lines.clone()).collect(),
        skipped: batches.iter().map(|b| b.skipped).sum(),
        batches: batches.len(),
        exit,
        game,
    }
}

fn options(dir: &Path) -> SpawnOptions {
    let mut options = SpawnOptions::new(1000);
    options.log_dir = Some(dir.join("logs"));
    options
}

fn level_of(lines: &[LogEntry], needle: &str) -> LogLevel {
    lines
        .iter()
        .find(|l| l.text.contains(needle))
        .unwrap_or_else(|| panic!("no line with {needle:?}"))
        .level
}

#[test]
fn xml_and_plain_output_get_levels_and_reach_the_file() {
    let dir = TempDir::new();
    let run = run("xml", dir.path(), &options(dir.path()));
    assert_eq!(run.exit.code, Some(0));
    assert!(!run.exit.crashed);
    assert_eq!(level_of(&run.lines, "Setting user: Steve"), LogLevel::Info);
    assert_eq!(level_of(&run.lines, "Broken"), LogLevel::Error);
    assert_eq!(
        level_of(&run.lines, "IllegalStateException"),
        LogLevel::Error
    );
    assert_eq!(level_of(&run.lines, "plain after xml"), LogLevel::Warn);
    assert_eq!(level_of(&run.lines, "Exception in thread"), LogLevel::Error);
    assert!(!run.lines.iter().any(|l| l.text.contains("<log4j:")));

    let file = run.exit.log_file.clone().expect("log file");
    assert_eq!(Some(file.as_path()), run.game.log_file());
    assert!(file.starts_with(dir.path().join("logs")));
    let content = fs::read_to_string(&file).unwrap();
    assert!(
        content
            .contains("[Render thread/INFO] [net.minecraft.client.Minecraft]: Setting user: Steve")
    );
    assert!(run.exit.tail.iter().any(|l| l.contains("plain after xml")));
}

#[test]
fn a_new_crash_report_marks_a_crash_even_with_exit_code_zero() {
    let dir = TempDir::new();
    let run = run("crash_exit0", dir.path(), &options(dir.path()));
    assert_eq!(run.exit.code, Some(0));
    assert!(run.exit.crashed);
    assert_eq!(
        run.exit.crash_report,
        Some(dir.path().join("crash-reports").join("crash-new.txt"))
    );
    assert!(run.exit.hs_err.is_none());
}

#[test]
fn a_crash_report_and_the_exit_code_are_reported_with_the_tail() {
    let dir = TempDir::new();
    // An old report must not be mistaken for the new one.
    fs::create_dir_all(dir.path().join("crash-reports")).unwrap();
    let old = dir.path().join("crash-reports").join("crash-old.txt");
    fs::write(&old, "old").unwrap();
    let past = std::time::SystemTime::now() - Duration::from_secs(3600);
    fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(past)
        .unwrap();

    let run = run("crash", dir.path(), &options(dir.path()));
    assert_eq!(run.exit.code, Some(1));
    assert!(run.exit.crashed);
    let report: PathBuf = run.exit.crash_report.clone().expect("crash report");
    assert!(
        report.ends_with("crash-2026-01-01_00.00.00-client.txt"),
        "{report:?}"
    );
    assert!(run.exit.tail.iter().any(|l| l.contains("crashing")));
}

#[test]
fn an_old_crash_report_alone_is_not_a_crash() {
    let dir = TempDir::new();
    fs::create_dir_all(dir.path().join("crash-reports")).unwrap();
    let old = dir.path().join("crash-reports").join("crash-old.txt");
    fs::write(&old, "old").unwrap();
    let past = std::time::SystemTime::now() - Duration::from_secs(3600);
    fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(past)
        .unwrap();
    let run = run("quiet", dir.path(), &options(dir.path()));
    assert_eq!(run.exit.code, Some(0));
    assert!(!run.exit.crashed);
    assert!(run.exit.crash_report.is_none());
}

#[test]
fn a_jvm_error_log_is_found_by_the_process_id() {
    let dir = TempDir::new();
    let run = run("hs_err", dir.path(), &options(dir.path()));
    assert_eq!(run.exit.code, Some(134));
    assert!(run.exit.crashed);
    let hs = run.exit.hs_err.clone().expect("hs_err");
    assert_eq!(
        hs.file_name().unwrap().to_string_lossy(),
        format!("hs_err_pid{}.log", run.game.pid())
    );
}

#[test]
fn stop_ends_the_game_and_is_not_a_crash() {
    let dir = TempDir::new();
    let (game, rx, batches) = start(&game("hang", dir.path()), &options(dir.path()));
    // Wait for the first line so the game is certainly running.
    for _ in 0..200 {
        if batches.lock().unwrap().iter().any(|b| !b.lines.is_empty()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    game.stop();
    let exit = rx.recv_timeout(Duration::from_secs(30)).expect("exit");
    assert!(exit.stopped_by_user);
    assert!(!exit.crashed);
    assert!(exit.session_ms < 60_000);
}

#[test]
fn old_logs_are_rotated_between_launches() {
    let dir = TempDir::new();
    let mut options = options(dir.path());
    options.keep_logs = 2;
    let mut last = None;
    for _ in 0..4 {
        last = run("quiet", dir.path(), &options).exit.log_file;
        // File names carry the second; keep the launches apart.
        std::thread::sleep(Duration::from_millis(1100));
    }
    let count = fs::read_dir(dir.path().join("logs")).unwrap().count();
    assert_eq!(count, 2);
    assert!(last.is_some_and(|p| p.exists()));
}

#[test]
fn spam_is_batched_bounded_and_written_whole() {
    let dir = TempDir::new();
    let mut options = options(dir.path());
    options.history_lines = 500;
    let run = run("spam", dir.path(), &options);
    assert_eq!(run.exit.code, Some(0));

    let file = fs::read_to_string(run.exit.log_file.as_ref().unwrap()).unwrap();
    let spam = file.lines().filter(|l| l.contains("xxxxxxxxxx")).count();
    assert_eq!(spam, 100_000, "the file holds every line");

    // Everything is accounted for: delivered or counted as skipped.
    let total = file.lines().count() as u64;
    assert_eq!(run.lines.len() as u64 + run.skipped, total);
    assert!(run.skipped > 0, "a flood is cut");
    // Far fewer events than lines.
    assert!(run.batches < 1000, "{} batches", run.batches);
    assert!(run.lines.len() <= run.batches * options.max_batch_lines);

    assert_eq!(run.game.history().len(), 500);
    assert_eq!(run.exit.tail.len(), vl_launch::process::TAIL_LINES);
}
