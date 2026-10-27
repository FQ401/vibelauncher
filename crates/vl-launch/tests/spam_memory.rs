//! A game that writes a huge amount of output does not make the launcher's memory grow with it.
//! The game is this test binary started again (see `game_log.rs`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::io::Write;
use std::sync::mpsc;
use std::time::Duration;

use common::TempDir;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
use vl_launch::CommandLine;
use vl_launch::process::{SpawnOptions, spawn};

const MODE: &str = "VL_SYNTHETIC_SPAM";
const LINES: usize = 400_000;

#[test]
fn synthetic_spammer() {
    if std::env::var(MODE).is_err() {
        return;
    }
    let out = std::io::stdout();
    let mut out = out.lock();
    let line = format!("[12:00:00] [main/INFO]: {}\n", "y".repeat(180));
    let block = line.repeat(1000);
    for _ in 0..LINES / 1000 {
        let _ = out.write_all(block.as_bytes());
    }
}

fn rss(system: &mut System) -> u64 {
    let pid = sysinfo::get_current_pid().unwrap();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_memory(),
    );
    system.process(pid).map_or(0, sysinfo::Process::memory)
}

#[test]
fn memory_stays_flat_under_a_spam_of_lines() {
    let dir = TempDir::new();
    let exe = std::env::current_exe().unwrap();
    let args = [
        "--exact",
        "synthetic_spammer",
        "--nocapture",
        "--test-threads=1",
    ];
    let mut command = CommandLine::new(
        exe,
        args.iter().map(|a| (*a).to_owned()).collect(),
        dir.path().to_path_buf(),
        Vec::new(),
    );
    command.env.insert(MODE.to_owned(), "1".to_owned());

    let mut options = SpawnOptions::new(1000);
    options.log_dir = Some(dir.path().join("logs"));

    let mut system = System::new();
    // Warm up the allocator and the threads before measuring.
    let before = rss(&mut system);
    let (tx, rx) = mpsc::channel();
    let game = spawn(
        &command,
        &options,
        |_| {},
        move |e| {
            let _ = tx.send(e);
        },
    )
    .unwrap();
    let mut peak = before;
    let exit = loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(exit) => break exit,
            Err(mpsc::RecvTimeoutError::Timeout) => peak = peak.max(rss(&mut system)),
            Err(e) => panic!("{e}"),
        }
    };
    peak = peak.max(rss(&mut system));
    assert_eq!(exit.code, Some(0));

    let file = std::fs::read_to_string(exit.log_file.as_ref().unwrap()).unwrap();
    assert!(file.lines().count() >= LINES);
    assert_eq!(game.history().len(), 1000);

    // The output is ~80 MB; keeping it would show up as at least that much growth.
    let growth = peak.saturating_sub(before);
    assert!(
        growth < 24 * 1024 * 1024,
        "memory grew by {} MiB",
        growth / (1024 * 1024)
    );
}
