//! The synthetic data the mock starts with.

use std::path::PathBuf;

use vl_core::account::{Account, AccountKind};
use vl_core::instance::{InstanceState, InstanceSummary, Loader};
use vl_core::java::JavaRuntime;
use vl_core::platform::Arch;
use vl_core::version::{GameVersion, VersionKind};
use vl_core::{AccountId, InstanceId};

pub(crate) fn versions() -> Vec<GameVersion> {
    [
        (
            "26.4-snapshot-6",
            VersionKind::Snapshot,
            "2026-10-20T10:00:00+00:00",
        ),
        ("26.3", VersionKind::Release, "2026-09-15T10:00:00+00:00"),
        ("26.2", VersionKind::Release, "2026-06-16T10:00:00+00:00"),
        ("1.21.11", VersionKind::Release, "2025-12-09T10:00:00+00:00"),
        ("1.20.1", VersionKind::Release, "2023-06-12T10:00:00+00:00"),
        ("1.12.2", VersionKind::Release, "2017-09-18T10:00:00+00:00"),
        ("b1.7.3", VersionKind::OldBeta, "2011-07-08T10:00:00+00:00"),
        ("a1.2.6", VersionKind::OldAlpha, "2010-12-03T10:00:00+00:00"),
    ]
    .into_iter()
    .map(|(id, kind, time)| GameVersion {
        id: id.into(),
        kind,
        release_time: time.into(),
    })
    .collect()
}

pub(crate) fn instances() -> Vec<InstanceSummary> {
    let rows = [
        (
            "survival",
            "Survival",
            Some("Survival"),
            "26.3",
            Loader::Vanilla,
            43_200,
            Some(1_793_000_000),
            None,
        ),
        (
            "creative",
            "Creative builds",
            Some("Building"),
            "1.21.11",
            Loader::Fabric("0.20.1".into()),
            5_400,
            Some(1_792_500_000),
            None,
        ),
        (
            "old-beta",
            "Old beta",
            None,
            "b1.7.3",
            Loader::Vanilla,
            0,
            None,
            None,
        ),
        (
            "broken",
            "Broken pack",
            None,
            "1.20.1",
            Loader::Vanilla,
            0,
            None,
            Some("instance.toml: expected a table at line 3"),
        ),
    ];
    rows.into_iter()
        .filter_map(|(id, name, group, version, loader, play, last, problem)| {
            Some(InstanceSummary {
                id: InstanceId::new(id)?,
                name: name.into(),
                group: group.map(Into::into),
                game_version: version.into(),
                loader,
                play_time_secs: play,
                last_played_unix: last,
                state: InstanceState::Stopped,
                problem: problem.map(Into::into),
            })
        })
        .collect()
}

pub(crate) fn accounts() -> Vec<Account> {
    AccountId::new("offline-mockplayer")
        .map(|id| Account {
            id,
            kind: AccountKind::Offline,
            name: "MockPlayer".into(),
            uuid: "00000000-0000-4000-8000-000000000001".into(),
            is_default: true,
            needs_sign_in: false,
        })
        .into_iter()
        .collect()
}

pub(crate) fn javas() -> Vec<JavaRuntime> {
    [(8, "1.8.0_472"), (17, "17.0.17"), (21, "21.0.9")]
        .into_iter()
        .map(|(major, version)| JavaRuntime {
            path: PathBuf::from(format!("/mock/java/{major}/bin/java")),
            major,
            version: version.into(),
            arch: Arch::X86_64,
            vendor: "Mock Vendor".into(),
            managed: major == 21,
        })
        .collect()
}

/// A synthetic game log line number `n`.
pub(crate) fn game_log_line(n: usize) -> (vl_core::log::LogLevel, String) {
    use vl_core::log::LogLevel;
    let level = if n % 7 == 6 {
        LogLevel::Warn
    } else {
        LogLevel::Info
    };
    let tag = if level == LogLevel::Warn {
        "WARN"
    } else {
        "INFO"
    };
    (
        level,
        format!(
            "[12:00:{:02}] [Render thread/{tag}]: Mock game line {n}",
            n % 60
        ),
    )
}

/// A free instance id derived from `name`.
pub(crate) fn slug(name: &str, taken: impl Fn(&str) -> bool) -> Option<InstanceId> {
    let base: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let base = base.trim_matches('-');
    let base = if base.is_empty() { "instance" } else { base };
    let mut candidate = base.to_owned();
    let mut n = 2;
    while taken(&candidate) {
        candidate = format!("{base}-{n}");
        n += 1;
    }
    InstanceId::new(&candidate)
}
