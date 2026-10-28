//! The pure rules of an automatic search: when a run reached its target, whether a crash is the
//! problem under search, and how repeated runs add up to one answer.
//!
//! The caller starts the game and watches it; these functions only judge what it saw.

use std::fmt;

use vl_core::bisect::{BisectOutcome, RunResult};
use vl_core::crash::CrashDiagnosis;

/// The game output texts that mark the main menu (`menu_markers.txt`).
const MENU_MARKERS: &str = include_str!("menu_markers.txt");

/// The texts of [`MENU_MARKERS`], without comments and blank lines.
pub fn menu_markers() -> impl Iterator<Item = &'static str> {
    MENU_MARKERS
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
}

/// Whether a line of game output shows that the game reached the main menu.
pub fn reached_menu(line: &str) -> bool {
    menu_markers().any(|m| line.contains(m))
}

/// Whether a game version has Quick Play (`--quickPlaySingleplayer`, `--quickPlayPath`): 1.20 and
/// later, and the snapshots from 23w14a on. Unknown version names are treated as older.
pub fn supports_quick_play(version: &str) -> bool {
    if let Some(release) = version.strip_prefix("1.") {
        let minor: String = release.chars().take_while(char::is_ascii_digit).collect();
        return minor.parse::<u32>().is_ok_and(|m| m >= 20);
    }
    // A snapshot: `23w14a`.
    let mut parts = version.splitn(2, 'w');
    let year = parts.next().and_then(|y| y.parse::<u32>().ok());
    let week = parts.next().map(|w| {
        w.chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
    });
    match (year, week.and_then(|w| w.parse::<u32>().ok())) {
        (Some(year), Some(week)) => (year, week) >= (23, 14),
        _ => false,
    }
}

/// The game arguments that enter `world` (a folder in `saves/`) at start and log the entry to
/// `log_path`.
pub fn quick_play_args(world: &str, log_path: &str) -> Vec<String> {
    vec![
        "--quickPlayPath".to_owned(),
        log_path.to_owned(),
        "--quickPlaySingleplayer".to_owned(),
        world.to_owned(),
    ]
}

/// The class of the root cause of the first Java stack trace in `text`: the last `Caused by:` of
/// that trace, or its first exception when it has none.
pub fn exception_class(text: &str) -> Option<String> {
    let mut found: Option<String> = None;
    let mut in_trace = false;
    for line in text.lines() {
        let trimmed = line.trim();
        let cause = trimmed.strip_prefix("Caused by: ");
        if let Some(class) = cause.and_then(throwable_class) {
            found = Some(class);
            in_trace = true;
            continue;
        }
        if in_trace {
            if trimmed.starts_with("at ") || trimmed.starts_with("...") {
                continue;
            }
            break;
        }
        if let Some(class) = throwable_class(trimmed) {
            found = Some(class);
            in_trace = true;
        }
    }
    found
}

/// `java.lang.IllegalStateException: text` → `java.lang.IllegalStateException`.
fn throwable_class(line: &str) -> Option<String> {
    let name = line.split([':', ' ']).next().unwrap_or_default();
    let simple = name.rsplit('.').next().unwrap_or_default();
    let qualified = name.contains('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '$'));
    let throwable = ["Exception", "Error", "Throwable"]
        .iter()
        .any(|s| simple.ends_with(s));
    (qualified && throwable).then(|| name.to_owned())
}

/// What identifies one problem across runs: the signature that matched and the class of the
/// root exception, when known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashKey {
    /// The signature id, or the message key of a diagnosis without one.
    pub kind: String,
    /// The root exception class.
    pub exception: Option<String>,
}

impl CrashKey {
    /// The key of a diagnosed crash; `crash_text` is the crash report, else the game output.
    pub fn new(diagnosis: &CrashDiagnosis, crash_text: &str) -> Self {
        Self {
            kind: diagnosis
                .signature
                .clone()
                .unwrap_or_else(|| diagnosis.message_key.clone()),
            exception: exception_class(crash_text),
        }
    }

    /// Whether `other` is the same problem: the same kind, and the same exception when both
    /// runs have one.
    pub fn same(&self, other: &CrashKey) -> bool {
        self.kind == other.kind
            && match (&self.exception, &other.exception) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            }
    }
}

impl fmt::Display for CrashKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.exception {
            Some(e) => write!(f, "{} ({e})", self.kind),
            None => f.write_str(&self.kind),
        }
    }
}

/// Repeated runs of one set of mods: bad from the first crash with the problem, good after
/// `repeats` clean runs in a row, skipped as soon as a run says nothing.
#[derive(Debug, Clone)]
pub struct Trial {
    repeats: u32,
    clean: u32,
}

impl Trial {
    /// A trial that needs `repeats` clean runs (at least one).
    pub fn new(repeats: u32) -> Self {
        Self {
            repeats: repeats.max(1),
            clean: 0,
        }
    }

    /// Counts one run; the answer once the trial is decided, `None` when another run is needed.
    pub fn record(&mut self, result: RunResult) -> Option<BisectOutcome> {
        match result {
            RunResult::Crashed => Some(BisectOutcome::Bad),
            RunResult::Reached => {
                self.clean += 1;
                (self.clean >= self.repeats).then_some(BisectOutcome::Good)
            }
            _ => Some(BisectOutcome::Skipped),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use vl_core::crash::{CrashCause, DiagnosisLevel};

    use super::*;

    #[test]
    fn menu_markers_are_data_and_match_game_lines() {
        assert!(menu_markers().count() >= 2);
        assert!(reached_menu(
            "[03:41:31] [Render thread/INFO]: Sound engine started"
        ));
        assert!(reached_menu(
            "[12:00:00] [Sound Library Loader/INFO]: Sound engine started"
        ));
        assert!(!reached_menu("[main/INFO]: Loading 4 mods:"));
        assert!(!reached_menu("# Game output lines"));
    }

    #[test]
    fn quick_play_starts_with_1_20_and_23w14a() {
        for v in [
            "1.20",
            "1.20.1",
            "1.21.4",
            "1.20-pre1",
            "1.20-rc1",
            "23w14a",
            "24w10a",
        ] {
            assert!(supports_quick_play(v), "{v}");
        }
        for v in [
            "1.19.4",
            "1.12.2",
            "1.7.10",
            "23w13a",
            "22w45a",
            "b1.7.3",
            "rd-132211",
            "",
        ] {
            assert!(!supports_quick_play(v), "{v}");
        }
        assert_eq!(
            quick_play_args("New World", "/q/log.json"),
            [
                "--quickPlayPath",
                "/q/log.json",
                "--quickPlaySingleplayer",
                "New World"
            ]
        );
    }

    #[test]
    fn the_root_cause_of_the_first_trace_is_the_exception() {
        let report = "---- Minecraft Crash Report ----\n\
            Description: Initializing game\n\
            \n\
            java.lang.RuntimeException: Could not execute entrypoint stage 'main'\n\
            \tat net.fabricmc.loader.impl.FabricLoaderImpl.invokeEntrypoints(FabricLoaderImpl.java:1)\n\
            Caused by: java.lang.IllegalStateException: boom\n\
            \tat example.Broken.onInitialize(Broken.java:5)\n\
            \t... 4 more\n\
            \n\
            A detailed walkthrough of the error\n\
            java.lang.NullPointerException: later\n";
        assert_eq!(
            exception_class(report).as_deref(),
            Some("java.lang.IllegalStateException")
        );
        let single = "[main/ERROR]: oops\njava.lang.OutOfMemoryError: Java heap space\n\tat a.B.c(B.java:1)\n";
        assert_eq!(
            exception_class(single).as_deref(),
            Some("java.lang.OutOfMemoryError")
        );
        assert_eq!(exception_class("no trace here\nDescription: x"), None);
        assert_eq!(exception_class("Exception in thread"), None);
    }

    fn diagnosis(signature: Option<&str>, key: &str) -> CrashDiagnosis {
        CrashDiagnosis {
            level: DiagnosisLevel::Signature,
            cause: CrashCause::ModLikely,
            loader_reported: false,
            signature: signature.map(str::to_owned),
            mods: Vec::new(),
            message_key: key.to_owned(),
            action_key: None,
            args: BTreeMap::new(),
            evidence: Vec::new(),
            files: Vec::new(),
        }
    }

    #[test]
    fn crash_keys_compare_the_kind_and_the_exception() {
        let npe = "java.lang.NullPointerException: x\n\tat a.B.c(B.java:1)\n";
        let ise = "java.lang.IllegalStateException: x\n\tat a.B.c(B.java:1)\n";
        let entry = CrashKey::new(&diagnosis(Some("entrypoint_failure"), "k"), npe);
        assert_eq!(
            entry.to_string(),
            "entrypoint_failure (java.lang.NullPointerException)"
        );
        assert!(entry.same(&CrashKey::new(
            &diagnosis(Some("entrypoint_failure"), "k"),
            npe
        )));
        assert!(entry.same(&CrashKey::new(
            &diagnosis(Some("entrypoint_failure"), "k"),
            ""
        )));
        assert!(!entry.same(&CrashKey::new(
            &diagnosis(Some("entrypoint_failure"), "k"),
            ise
        )));
        assert!(!entry.same(&CrashKey::new(&diagnosis(Some("out_of_memory"), "k"), npe)));
        let unknown = CrashKey::new(&diagnosis(None, "crash-undetermined"), ise);
        assert_eq!(unknown.kind, "crash-undetermined");
        assert!(!unknown.same(&entry));
    }

    #[test]
    fn a_trial_is_bad_at_once_and_good_after_k_clean_runs() {
        let mut trial = Trial::new(3);
        assert_eq!(trial.record(RunResult::Reached), None);
        assert_eq!(trial.record(RunResult::Reached), None);
        assert_eq!(trial.record(RunResult::Crashed), Some(BisectOutcome::Bad));
        let mut trial = Trial::new(2);
        assert_eq!(trial.record(RunResult::Reached), None);
        assert_eq!(trial.record(RunResult::Reached), Some(BisectOutcome::Good));
        let mut trial = Trial::new(0);
        assert_eq!(trial.record(RunResult::Reached), Some(BisectOutcome::Good));
        for result in [RunResult::Hung, RunResult::OtherCrash, RunResult::Closed] {
            assert_eq!(Trial::new(2).record(result), Some(BisectOutcome::Skipped));
        }
    }
}
