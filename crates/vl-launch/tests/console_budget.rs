//! The console's budget under a log flood: the buffer stays at its limit, batches stay at most
//! ten a second, memory stays flat and the launcher side uses little CPU. The game is this test
//! binary started again as a producer that writes at a fixed rate (lines of every level and a
//! stack trace every thousand lines).
//!
//! `budget_report` is the measurement (ignored by default; run it optimized):
//! `cargo test -p vl-launch --release --test console_budget -- --ignored --nocapture`. The CPU
//! figure is the whole test process (reader, collector and waiter threads, one memory sample a
//! second) over the run; the OS counts CPU time in ticks of about 16 ms on Windows.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::io::Write;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::TempDir;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
use vl_core::console::ConsoleStats;
use vl_core::launch::GameExit;
use vl_launch::CommandLine;
use vl_launch::process::{SpawnOptions, spawn};

const RATE: &str = "VL_SYNTHETIC_RATE";
const SECONDS: &str = "VL_SYNTHETIC_SECONDS";
/// The pacing step of the producer.
const TICK: Duration = Duration::from_millis(10);

/// The producer: `RATE` lines a second for `SECONDS` seconds.
#[test]
fn synthetic_producer() {
    let (Ok(rate), Ok(seconds)) = (std::env::var(RATE), std::env::var(SECONDS)) else {
        return;
    };
    let rate: u64 = rate.parse().unwrap();
    let seconds: f64 = seconds.parse().unwrap();
    let out = std::io::stdout();
    let mut out = out.lock();
    let _ = writeln!(out);
    let start = Instant::now();
    let total = (rate as f64 * seconds) as u64;
    let mut n = 0u64;
    let mut text = String::new();
    while n < total {
        // Lines due by now; then wait for the next tick.
        let due = ((start.elapsed().as_secs_f64() * rate as f64) as u64).min(total);
        text.clear();
        while n < due {
            line(n, &mut text);
            n += 1;
        }
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
        std::thread::sleep(TICK);
    }
}

/// Line `n` of the synthetic flood.
fn line(n: u64, text: &mut String) {
    use std::fmt::Write as _;
    if n % 1000 == 999 {
        let _ = write!(
            text,
            "[12:00:00] [Render thread/ERROR] [net.example.Mod]: Failed tick {n}\n\
             java.lang.IllegalStateException: synthetic\n\
             \tat net.example.Mod.tick(Mod.java:42)\n\
             \tat net.example.Loop.run(Loop.java:7)\n"
        );
        return;
    }
    let level = match n % 10 {
        0 => "WARN",
        1 => "DEBUG",
        _ => "INFO",
    };
    let _ = writeln!(
        text,
        "[12:00:00] [Server thread/{level}] [net.example.World]: chunk {n} loaded at {} {}",
        n * 7,
        n % 97
    );
}

struct Outcome {
    exit: GameExit,
    stats: ConsoleStats,
    batch_times: Vec<Instant>,
    delivered: u64,
    skipped: u64,
    cpu_ms: u64,
    wall: Duration,
    rss_growth: u64,
    /// Holds the log file until the checks are done.
    _dir: TempDir,
}

fn rss_and_cpu(system: &mut System) -> (u64, u64) {
    let pid = sysinfo::get_current_pid().unwrap();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_memory().with_cpu(),
    );
    system
        .process(pid)
        .map_or((0, 0), |p| (p.memory(), p.accumulated_cpu_time()))
}

fn rss(system: &mut System) -> u64 {
    let pid = sysinfo::get_current_pid().unwrap();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        false,
        ProcessRefreshKind::nothing().with_memory(),
    );
    system.process(pid).map_or(0, sysinfo::Process::memory)
}

fn flood(rate: u64, seconds: f64, history_lines: usize) -> Outcome {
    let dir = TempDir::new();
    let exe = std::env::current_exe().unwrap();
    let args = [
        "--exact",
        "synthetic_producer",
        "--nocapture",
        "--test-threads=1",
    ];
    let mut command = CommandLine::new(
        exe,
        args.iter().map(|a| (*a).to_owned()).collect(),
        dir.path().to_path_buf(),
        Vec::new(),
    );
    command.env.insert(RATE.to_owned(), rate.to_string());
    command.env.insert(SECONDS.to_owned(), seconds.to_string());
    let mut options = SpawnOptions::new(history_lines);
    options.log_dir = Some(dir.path().join("logs"));

    let mut system = System::new();
    let (before, cpu_before) = rss_and_cpu(&mut system);
    let started = Instant::now();
    let batches = Arc::new(Mutex::new((Vec::new(), 0u64, 0u64)));
    let sink = Arc::clone(&batches);
    let (tx, rx) = mpsc::channel();
    let game = spawn(
        &command,
        &options,
        move |b| {
            let mut sink = sink.lock().unwrap();
            sink.0.push(Instant::now());
            sink.1 += b.lines.len() as u64;
            sink.2 += b.skipped;
        },
        move |e| {
            let _ = tx.send(e);
        },
    )
    .unwrap();
    let mut peak = before;
    let exit = loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(exit) => break exit,
            Err(mpsc::RecvTimeoutError::Timeout) => peak = peak.max(rss(&mut system)),
            Err(e) => panic!("{e}"),
        }
    };
    let wall = started.elapsed();
    let (rss, cpu_after) = rss_and_cpu(&mut system);
    peak = peak.max(rss);
    let (batch_times, delivered, skipped) = batches.lock().unwrap().clone();
    Outcome {
        exit,
        stats: game.console_stats(),
        batch_times,
        delivered,
        skipped,
        cpu_ms: cpu_after.saturating_sub(cpu_before),
        wall,
        rss_growth: peak.saturating_sub(before),
        _dir: dir,
    }
}

/// The most batches delivered within any one second.
fn max_batches_per_second(times: &[Instant]) -> usize {
    let mut most = 0;
    let mut start = 0;
    for end in 0..times.len() {
        while times[end].duration_since(times[start]) >= Duration::from_secs(1) {
            start += 1;
        }
        most = most.max(end - start + 1);
    }
    most
}

fn check_bounds(out: &Outcome, history_lines: usize) {
    assert_eq!(out.exit.code, Some(0));
    let stats = out.stats;
    // The buffer is at its limit, and every line is accounted for.
    assert_eq!(stats.lines, history_lines as u64, "{stats:?}");
    assert_eq!(stats.lines + stats.dropped, stats.next_seq);
    assert_eq!(out.delivered + out.skipped, stats.next_seq);
    assert_eq!(stats.skipped, out.skipped);
    assert!(stats.bytes <= stats.max_bytes);
    // A batch every 100 ms at most, and the last one after the end: at most 11 in a second.
    let rate = max_batches_per_second(&out.batch_times);
    assert!(rate <= 11, "{rate} batches in one second");
    let file = std::fs::read_to_string(out.exit.log_file.as_ref().unwrap()).unwrap();
    assert_eq!(
        file.lines().count() as u64,
        stats.next_seq,
        "the file is whole"
    );
}

#[test]
fn a_flood_keeps_the_buffer_at_its_limit_and_the_batch_rate_capped() {
    let out = flood(50_000, 1.0, 1000);
    check_bounds(&out, 1000);
    assert!(out.stats.next_seq >= 50_000);
    assert!(out.skipped > 0, "a flood is cut");
}

#[test]
#[ignore = "a measurement: run optimized with --ignored --nocapture"]
fn budget_report() {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    for (rate, seconds) in [(100u64, 5.0), (10_000, 5.0), (100_000, 5.0)] {
        let out = flood(rate, seconds, 10_000);
        check_bounds(&out, 10_000.min(out.stats.next_seq as usize));
        let cpu = out.cpu_ms as f64 / out.wall.as_millis() as f64 * 100.0;
        println!(
            "{rate} lines/s for {seconds} s: {} lines, launcher CPU {cpu:.1}% of one core \
             ({} ms over {} ms, {cores} cores), batches max {}/s, delivered {}, skipped {}, \
             buffer {} lines / {} KiB, dropped {}, RSS growth {} KiB",
            out.stats.next_seq,
            out.cpu_ms,
            out.wall.as_millis(),
            max_batches_per_second(&out.batch_times),
            out.delivered,
            out.skipped,
            out.stats.lines,
            out.stats.bytes / 1024,
            out.stats.dropped,
            out.rss_growth / 1024,
        );
        assert!(out.rss_growth < 32 * 1024 * 1024, "memory grew too much");
        // The target is 2-3 % at this rate; the margin absorbs a busy machine.
        if rate == 10_000 && !cfg!(debug_assertions) {
            assert!(cpu < 10.0, "launcher CPU {cpu:.1}% at {rate} lines/s");
        }
    }
}
