//! Console commands: the bounded output buffer of an instance's current or last game run.
//!
//! The launch service registers each started game here. A run's buffer stays readable after the
//! game exits until the next launch of the instance; at most [`KEEP_ENDED`] ended runs are kept,
//! the oldest leaving first, so ended games hold a bounded amount of memory.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use vl_core::console::{ConsoleCommand, ConsoleStatus};
use vl_core::{Error, InstanceId, Replier, Response};
use vl_launch::process::RunningGame;

use crate::Backend;

/// How many buffers of ended runs are kept.
const KEEP_ENDED: usize = 4;

/// The consoles of started games, oldest first.
#[derive(Clone, Default)]
pub(crate) struct Consoles {
    runs: Arc<Mutex<Vec<(InstanceId, RunningGame)>>>,
}

impl Consoles {
    fn runs(&self) -> MutexGuard<'_, Vec<(InstanceId, RunningGame)>> {
        self.runs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Registers the console of a started game; it replaces the instance's earlier run.
    pub(crate) fn started(&self, instance: &InstanceId, game: RunningGame) {
        let mut runs = self.runs();
        runs.retain(|(id, _)| id != instance);
        runs.push((instance.clone(), game));
        let ended = runs.iter().filter(|(_, g)| g.has_exited()).count();
        let mut excess = ended.saturating_sub(KEEP_ENDED);
        runs.retain(|(_, g)| {
            if excess > 0 && g.has_exited() {
                excess -= 1;
                return false;
            }
            true
        });
    }

    fn get(&self, instance: &InstanceId) -> vl_core::Result<RunningGame> {
        self.runs()
            .iter()
            .find(|(id, _)| id == instance)
            .map(|(_, game)| game.clone())
            .ok_or_else(|| Error::not_found(format!("instance {instance} has no game output")))
    }
}

pub(crate) fn handle(backend: &Backend, command: ConsoleCommand, replier: Replier) {
    replier.send(answer(&backend.launch.consoles, command));
}

fn answer(consoles: &Consoles, command: ConsoleCommand) -> vl_core::Result<Response> {
    match command {
        ConsoleCommand::Status { instance } => {
            let game = consoles.get(&instance)?;
            Ok(Response::ConsoleStatus(ConsoleStatus {
                running: !game.has_exited(),
                stats: game.console_stats(),
                log_file: game.log_file().map(std::path::Path::to_path_buf),
            }))
        }
        ConsoleCommand::Fetch {
            instance,
            from,
            limit,
        } => Ok(Response::ConsolePage(
            consoles.get(&instance)?.console_page(from, limit),
        )),
        ConsoleCommand::Search { instance, query } => Ok(Response::ConsoleMatches(
            consoles.get(&instance)?.console_search(&query),
        )),
        ConsoleCommand::OpenLogFile { instance } => {
            let game = consoles.get(&instance)?;
            let path = game.log_file().ok_or_else(|| {
                Error::not_found(format!("the game of instance {instance} has no log file"))
            })?;
            vl_launch::logfile::open_with_system(path)?;
            Ok(Response::Done)
        }
        other => Err(Error::unsupported(format!(
            "vl-backend does not handle console {other:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::Duration;

    use vl_core::console::ConsoleQuery;
    use vl_core::log::LogLevel;
    use vl_launch::CommandLine;
    use vl_launch::process::{SpawnOptions, spawn};

    use super::*;

    /// A finished process that printed two lines.
    fn finished_game() -> RunningGame {
        #[cfg(windows)]
        let (program, script) = (
            "cmd",
            ["/C", "echo [12:00:00] [main/WARN]: low memory& echo plain"],
        );
        #[cfg(not(windows))]
        let (program, script) = (
            "sh",
            [
                "-c",
                "echo '[12:00:00] [main/WARN]: low memory'; echo plain",
            ],
        );
        let command = CommandLine::new(
            PathBuf::from(program),
            script.into_iter().map(str::to_owned).collect(),
            std::env::temp_dir(),
            Vec::new(),
        );
        let (tx, rx) = mpsc::channel();
        let game = spawn(
            &command,
            &SpawnOptions::new(100),
            |_| {},
            move |e| {
                let _ = tx.send(e);
            },
        )
        .unwrap();
        rx.recv_timeout(Duration::from_secs(20)).unwrap();
        game
    }

    fn id(name: &str) -> InstanceId {
        InstanceId::new(name).unwrap()
    }

    #[test]
    fn answers_status_pages_and_searches_after_the_exit() {
        let consoles = Consoles::default();
        consoles.started(&id("a"), finished_game());

        let status = answer(&consoles, ConsoleCommand::Status { instance: id("a") });
        let Ok(Response::ConsoleStatus(status)) = status else {
            panic!("expected a status: {status:?}");
        };
        assert!(!status.running);
        assert_eq!(status.stats.lines, 2);
        assert!(status.log_file.is_none());

        let page = answer(
            &consoles,
            ConsoleCommand::Fetch {
                instance: id("a"),
                from: None,
                limit: 10,
            },
        );
        let Ok(Response::ConsolePage(page)) = page else {
            panic!("expected a page: {page:?}");
        };
        assert_eq!(page.lines.len(), 2);
        assert_eq!(page.lines[0].thread.as_deref(), Some("main"));

        let found = answer(
            &consoles,
            ConsoleCommand::Search {
                instance: id("a"),
                query: ConsoleQuery {
                    levels: vec![LogLevel::Warn],
                    limit: 10,
                    ..ConsoleQuery::default()
                },
            },
        );
        let Ok(Response::ConsoleMatches(found)) = found else {
            panic!("expected matches: {found:?}");
        };
        assert_eq!(found.total, 1);

        let missing = answer(&consoles, ConsoleCommand::Status { instance: id("b") });
        assert_eq!(missing.unwrap_err().kind, vl_core::ErrorKind::NotFound);
        let no_file = answer(&consoles, ConsoleCommand::OpenLogFile { instance: id("a") });
        assert_eq!(no_file.unwrap_err().kind, vl_core::ErrorKind::NotFound);
    }

    #[test]
    fn keeps_a_bounded_number_of_ended_runs() {
        let consoles = Consoles::default();
        let game = finished_game();
        for n in 0..(KEEP_ENDED + 3) {
            consoles.started(&id(&format!("i{n}")), game.clone());
        }
        // The same instance again replaces its earlier run.
        consoles.started(&id(&format!("i{}", KEEP_ENDED + 2)), game);
        let kept: Vec<_> = consoles.runs().iter().map(|(i, _)| i.to_string()).collect();
        assert_eq!(kept.len(), KEEP_ENDED);
        assert_eq!(kept.last().map(String::as_str), Some("i6"));
        assert!(!kept.contains(&"i0".to_owned()));
    }
}
