//! The launcher's budget while it watches a game without its window: memory and CPU of the
//! desktop app process on the real backend, with a test program as the game.
//!
//! The game is this test binary started again (`synthetic_game`): silent for a while, then a
//! log flood at a fixed rate, then a normal exit. The launcher is the `vibelauncher` binary built
//! with the `dev-game` feature (`VL_DEV_GAME` replaces the game, no account, Java or game files)
//! on a temporary data folder, started with `--launch`, so its window closes once the game runs.
//!
//! Run it optimized, on a desktop session (it opens a window for a moment):
//! `cargo test -p vl-app --release --features dev-game --test watcher_budget -- --ignored --nocapture`.
//!
//! Memory is reported as the resident size (RSS, for information: it counts shared libraries
//! and clean file pages that the OS evicts on its own) and the figure that is the budget:
//! on Windows the private bytes (memory committed by the process itself, without shared images),
//! on Linux `RssAnon` (private dirty memory; PSS is printed beside it), on macOS `phys_footprint`
//! (what Activity Monitor shows as Memory; read from `footprint -p`). The target is `RssAnon`
//! at most 30 MiB on Linux and `phys_footprint` at most 30 MB on macOS while the game is silent
//! (the log flood may add up to 10 MiB for the console buffer); the test fails 5 MiB above it. CPU is the process's
//! CPU time over the sampled span, as a share of one core.

#![cfg(feature = "dev-game")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use vl_backend::{Backend, BackendConfig};
use vl_core::instance::{InstanceCommand, NewInstance};
use vl_core::{Command as FacadeCommand, Facade, Response};

const QUIET: &str = "VL_SYNTHETIC_QUIET";
const RATE: &str = "VL_SYNTHETIC_RATE";
const SECONDS: &str = "VL_SYNTHETIC_SECONDS";

/// The test game: one line, `QUIET` seconds of silence, `RATE` lines a second for `SECONDS`
/// seconds, exit 0.
#[test]
fn synthetic_game() {
    let (Ok(quiet), Ok(rate), Ok(seconds)) = (
        std::env::var(QUIET),
        std::env::var(RATE),
        std::env::var(SECONDS),
    ) else {
        return;
    };
    let quiet: u64 = quiet.parse().unwrap();
    let rate: u64 = rate.parse().unwrap();
    let seconds: f64 = seconds.parse().unwrap();
    let out = std::io::stdout();
    let mut out = out.lock();
    let _ = writeln!(out, "[12:00:00] [main/INFO]: synthetic game started");
    let _ = out.flush();
    std::thread::sleep(Duration::from_secs(quiet));
    let start = Instant::now();
    let total = (rate as f64 * seconds) as u64;
    let mut n = 0u64;
    let mut text = String::new();
    while n < total {
        let due = ((start.elapsed().as_secs_f64() * rate as f64) as u64).min(total);
        text.clear();
        while n < due {
            use std::fmt::Write as _;
            let level = if n.is_multiple_of(10) { "WARN" } else { "INFO" };
            let _ = writeln!(
                text,
                "[12:00:00] [Render thread/{level}] [net.example.Mod]: synthetic line {n} \
                 with some ordinary payload text"
            );
            n += 1;
        }
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("vl-watcher-budget-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Creates an instance in the data folder and returns its id.
fn create_instance(dir: &TempDir) -> String {
    let backend = Backend::new(BackendConfig {
        data_dir: Some(dir.0.clone()),
    })
    .unwrap();
    let reply = backend
        .call(FacadeCommand::Instances(InstanceCommand::Create(
            NewInstance {
                name: "Budget".into(),
                game_version: "1.21.1".into(),
                group: None,
                loader: None,
            },
        )))
        .wait()
        .unwrap();
    let Response::Instance(instance) = reply else {
        panic!("unexpected reply {reply:?}");
    };
    instance.id.to_string()
}

/// The target for the budget figure on Linux and macOS, in MiB; Windows reports without a limit.
const BUDGET_MIB: f64 = 30.0;

/// What the test accepts over the target, in MiB: the software renderers of the CI runners sit
/// within a few MiB of it from run to run.
const SLACK_MIB: f64 = 5.0;

/// What the console buffer may add to the budget figure under the log flood, in MiB.
const FLOOD_MIB: f64 = 10.0;

/// The name of the budget figure on this OS.
const BUDGET_NAME: &str = if cfg!(windows) {
    "private"
} else if cfg!(target_os = "macos") {
    "phys_footprint"
} else {
    "RssAnon"
};

/// A sample of the launcher process.
#[derive(Debug, Clone, Copy)]
struct Sample {
    at: Instant,
    /// Resident size (the working set on Windows).
    rss: u64,
    /// The budget figure: private bytes, `RssAnon` or `phys_footprint`.
    budget: u64,
    /// Proportional set size (Linux only, else 0).
    pss: u64,
    cpu_ms: u64,
}

/// A value in kB from a `/proc` line such as `RssAnon:     1234 kB`, in bytes.
#[cfg(target_os = "linux")]
fn proc_kb(text: &str, key: &str) -> Option<u64> {
    let rest = text.lines().find_map(|l| l.strip_prefix(key))?;
    let kb: u64 = rest.trim().strip_suffix("kB")?.trim().parse().ok()?;
    Some(kb * 1024)
}

/// `RssAnon` and PSS of the process, in bytes.
#[cfg(target_os = "linux")]
fn linux_memory(pid: Pid) -> (u64, u64) {
    let read =
        |file: &str| std::fs::read_to_string(format!("/proc/{pid}/{file}")).unwrap_or_default();
    let anon = proc_kb(&read("status"), "RssAnon:").unwrap_or(0);
    let pss = proc_kb(&read("smaps_rollup"), "Pss:").unwrap_or(0);
    (anon, pss)
}

/// `phys_footprint` of the process in bytes, from the `Footprint:` figure of `footprint -p`
/// (for example `vibelauncher [123]: 64-bit    Footprint: 28 MB (16384 bytes per page)`).
#[cfg(target_os = "macos")]
fn macos_footprint(pid: Pid) -> u64 {
    let Ok(out) = Command::new("footprint")
        .arg("-p")
        .arg(pid.to_string())
        .output()
    else {
        return 0;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let Some(rest) = text
        .lines()
        .find_map(|l| l.split_once("Footprint:").map(|(_, r)| r))
    else {
        return 0;
    };
    let mut words = rest.split_whitespace();
    let (Some(number), Some(unit)) = (words.next(), words.next()) else {
        return 0;
    };
    let Ok(number) = number.replace(',', ".").parse::<f64>() else {
        return 0;
    };
    let factor = match unit {
        "KB" => 1024.0,
        "MB" => 1024.0 * 1024.0,
        "GB" => 1024.0 * 1024.0 * 1024.0,
        _ => 1.0,
    };
    (number * factor) as u64
}

fn sample(system: &mut System, pid: Pid) -> Sample {
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_memory().with_cpu(),
    );
    let process = system.process(pid).expect("the launcher runs");
    #[cfg(target_os = "linux")]
    let (budget, pss) = linux_memory(pid);
    #[cfg(target_os = "macos")]
    let (budget, pss) = (macos_footprint(pid), 0);
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let (budget, pss) = (process.virtual_memory(), 0);
    Sample {
        at: Instant::now(),
        rss: process.memory(),
        budget,
        pss,
        cpu_ms: process.accumulated_cpu_time(),
    }
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / 1024.0 / 1024.0
}

/// The peaks of a sampled span, in MiB, and the CPU share of one core.
struct Peaks {
    rss: f64,
    budget: f64,
    cpu: f64,
}

/// Samples once a second from `from` to `to` (seconds since `start`); prints and returns the
/// peak resident size, the peak budget figure and the CPU share of one core.
fn span(label: &str, system: &mut System, pid: Pid, start: Instant, from: u64, to: u64) -> Peaks {
    let at = |s: u64| start + Duration::from_secs(s);
    std::thread::sleep(at(from).saturating_duration_since(Instant::now()));
    let first = sample(system, pid);
    let mut peak = first;
    let mut last = first;
    while Instant::now() < at(to) {
        std::thread::sleep(Duration::from_secs(1));
        last = sample(system, pid);
        peak.rss = peak.rss.max(last.rss);
        peak.budget = peak.budget.max(last.budget);
        peak.pss = peak.pss.max(last.pss);
    }
    let wall = last.at.duration_since(first.at).as_millis().max(1) as f64;
    let cpu = last.cpu_ms.saturating_sub(first.cpu_ms) as f64 / wall * 100.0;
    let result = Peaks {
        rss: mib(peak.rss),
        budget: mib(peak.budget),
        cpu,
    };
    let pss = if cfg!(target_os = "linux") {
        format!(", PSS {:.1} MiB (last {:.1})", mib(peak.pss), mib(last.pss))
    } else {
        String::new()
    };
    println!(
        "{label}: RSS {:.1} MiB (last {:.1}), {BUDGET_NAME} {:.1} MiB (last {:.1}){pss}, CPU \
         {:.2}% of one core over {:.0} s",
        result.rss,
        mib(last.rss),
        result.budget,
        mib(last.budget),
        result.cpu,
        wall / 1000.0
    );
    result
}

/// Starts the launcher on `dir`; with `game`, it launches the instance with the test game.
fn start_launcher(dir: &TempDir, game: Option<(&str, u64, u64, u64)>) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_vibelauncher"));
    command.arg("--dir").arg(&dir.0);
    if let Some((instance, quiet, rate, seconds)) = game {
        let me = std::env::current_exe().unwrap();
        let game = serde_json_array(&[
            &me.display().to_string(),
            "synthetic_game",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ]);
        command
            .arg("--launch")
            .arg(instance)
            .env("VL_DEV_GAME", game)
            .env(QUIET, quiet.to_string())
            .env(RATE, rate.to_string())
            .env(SECONDS, seconds.to_string());
    }
    command.spawn().unwrap()
}

/// A JSON array of strings.
fn serde_json_array(items: &[&str]) -> String {
    let quoted: Vec<String> = items
        .iter()
        .map(|s| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect();
    format!("[{}]", quoted.join(","))
}

#[test]
#[ignore = "measurement: opens a window; run optimized with --ignored --nocapture"]
fn budget_report() {
    let mut system = System::new();

    // The window open on an empty launcher, for comparison.
    let dir = TempDir::new("window");
    let mut launcher = start_launcher(&dir, None);
    let pid = Pid::from_u32(launcher.id());
    let start = Instant::now();
    span("window open, idle", &mut system, pid, start, 8, 18);
    let _ = launcher.kill();
    let _ = launcher.wait();
    drop(dir);

    // The window closes once the game runs; the game is quiet, then floods its log.
    let dir = TempDir::new("watch");
    let instance = create_instance(&dir);
    let rate = 10_000;
    let mut launcher = start_launcher(&dir, Some((&instance, 30, rate, 24)));
    let pid = Pid::from_u32(launcher.id());
    let start = Instant::now();
    let idle = span("watching, game silent", &mut system, pid, start, 18, 30);
    let flood = span(
        &format!("watching, game writes {rate} lines/s"),
        &mut system,
        pid,
        start,
        37,
        53,
    );
    let _ = launcher.kill();
    let _ = launcher.wait();
    // The game ends by itself shortly after.
    std::thread::sleep(Duration::from_secs(8));
    drop(dir);

    assert!(idle.cpu < 1.0, "idle CPU {:.2}%", idle.cpu);
    assert!(flood.cpu < 15.0, "CPU under flood {:.2}%", flood.cpu);
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        // The silent game is the budget; the log flood adds the console buffer on top.
        assert!(idle.budget > 0.0, "{BUDGET_NAME} was not read");
        assert!(
            idle.budget <= BUDGET_MIB + SLACK_MIB,
            "{BUDGET_NAME} {:.1} MiB while the game is silent, over {BUDGET_MIB} MiB (+{SLACK_MIB})",
            idle.budget
        );
        assert!(
            flood.budget <= BUDGET_MIB + SLACK_MIB + FLOOD_MIB,
            "{BUDGET_NAME} {:.1} MiB under the log flood, over {} MiB",
            flood.budget,
            BUDGET_MIB + SLACK_MIB + FLOOD_MIB
        );
    }
}
