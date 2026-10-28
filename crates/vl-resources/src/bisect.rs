//! Mod bisection on disk: the session file and the switching of mods.
//!
//! [`Bisect`] drives a [`vl_bisect::Session`] over one instance's `mods/` folder:
//!
//! - the session is saved to `<instance>/.vibelauncher/bisect.json` before any mod is touched
//!   and after every answer;
//! - a step is applied by renaming files to or from `<name>.disabled`;
//! - every way a session ends (the search finishes, [`Bisect::stop`], a failure while applying a
//!   step) re-enables every file the session started with. The list of those files lives in the
//!   session file, so a stop restores the mods even after the launcher was closed mid-session.
//!
//! The game is not started here: the user runs it and reports the outcome.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vl_bisect::{Candidate, Session};
use vl_core::bisect::{BisectOutcome, BisectState, BisectStatus};
use vl_core::resources::{DependencyKind, ResourceKind};
use vl_core::{Error, ErrorKind};

use crate::Resources;

/// Name of the session file inside the instance's metadata folder.
pub const SESSION_FILE: &str = "bisect.json";

const FORMAT: u32 = 1;

#[derive(Serialize, Deserialize)]
struct SessionFile {
    format: u32,
    session: Session,
}

/// The bisection of one instance.
#[derive(Debug, Clone)]
pub struct Bisect {
    resources: Resources,
    file: PathBuf,
}

fn io_err(path: &Path, e: std::io::Error) -> Error {
    Error::new(ErrorKind::Io, format!("{}: {e}", path.display()))
}

impl Bisect {
    /// The bisection of `resources`, with its session file in `meta_dir`
    /// (`<instance>/.vibelauncher`).
    pub fn new(resources: Resources, meta_dir: &Path) -> Self {
        Self {
            resources,
            file: meta_dir.join(SESSION_FILE),
        }
    }

    /// Starts a session over the enabled mod jars and applies its first step. `suspects` are
    /// mod ids to try first. An unfinished session is a conflict; a finished one is replaced.
    pub fn start(&self, suspects: &[String]) -> vl_core::Result<BisectStatus> {
        if let Some(existing) = self.load_optional()?
            && !existing.is_finished()
        {
            return Err(Error::conflict(
                "a bisect session is already running; stop it first",
            ));
        }
        let candidates = self.candidates()?;
        let session = Session::new(candidates, suspects)?;
        self.save(&session)?;
        if let Err(e) = self.apply(&session) {
            return Err(self.abort(&session, e));
        }
        Ok(status(&session))
    }

    /// The current session; `NotFound` when there is none.
    pub fn status(&self) -> vl_core::Result<BisectStatus> {
        Ok(status(&self.load()?))
    }

    /// Records the outcome of the current step and applies the next one. When the search ends,
    /// the mods are restored and the verdict stays in the session until it is stopped or
    /// replaced.
    pub fn report(&self, outcome: BisectOutcome) -> vl_core::Result<BisectStatus> {
        let mut session = self.load()?;
        session.answer(outcome)?;
        self.save(&session)?;
        if let Err(e) = self.apply(&session) {
            return Err(self.abort(&session, e));
        }
        Ok(status(&session))
    }

    /// Ends the session, finished or not: restores the mods and deletes the session file. The
    /// file stays when a mod could not be restored, so a later stop can try again. Without a
    /// session this does nothing.
    pub fn stop(&self) -> vl_core::Result<()> {
        let Some(session) = self.load_optional()? else {
            return Ok(());
        };
        self.restore(&session)?;
        remove_file(&self.file)
    }

    /// The enabled mod jars with what they provide and require.
    fn candidates(&self) -> vl_core::Result<Vec<Candidate>> {
        let entries = self.resources.list(ResourceKind::Mods)?;
        Ok(entries
            .into_iter()
            .filter(|e| e.enabled && !e.is_dir && e.file.to_ascii_lowercase().ends_with(".jar"))
            .map(|e| Candidate {
                provides: e.mods.iter().map(|m| m.id.clone()).collect(),
                requires: e
                    .mods
                    .iter()
                    .flat_map(|m| &m.dependencies)
                    .filter(|d| d.kind == DependencyKind::Required)
                    .map(|d| d.id.clone())
                    .collect(),
                file: e.file,
            })
            .collect())
    }

    /// Puts the mods in the state of the session's current step, or of the start when the
    /// session is finished.
    fn apply(&self, session: &Session) -> vl_core::Result<()> {
        let Some(enabled) = session.probe() else {
            return self.restore(session);
        };
        let enabled: BTreeSet<String> = enabled.into_iter().collect();
        for file in session.candidate_files() {
            let on = enabled.contains(&file);
            self.resources.set_enabled(ResourceKind::Mods, &file, on)?;
        }
        Ok(())
    }

    /// Enables every file the session started with. Tries all of them and reports the first
    /// failure; a file that no longer exists is skipped.
    fn restore(&self, session: &Session) -> vl_core::Result<()> {
        let mut first = None;
        for file in session.candidate_files() {
            match self.resources.set_enabled(ResourceKind::Mods, &file, true) {
                Ok(()) => {}
                Err(e) if e.kind == ErrorKind::NotFound => {}
                Err(e) => {
                    first.get_or_insert(e);
                }
            }
        }
        first.map_or(Ok(()), Err)
    }

    /// A step could not be applied: restores the mods and ends the session. Returns the error to
    /// report, which also says when the restore failed too.
    fn abort(&self, session: &Session, cause: Error) -> Error {
        match self.restore(session) {
            Ok(()) => {
                let _ = remove_file(&self.file);
                Error::new(
                    cause.kind,
                    format!(
                        "{}; the bisect session was ended and the mods restored",
                        cause.message
                    ),
                )
            }
            Err(e) => Error::new(
                cause.kind,
                format!(
                    "{}; restoring the mods failed too ({}); run bisect stop to retry",
                    cause.message, e.message
                ),
            ),
        }
    }

    fn load_optional(&self) -> vl_core::Result<Option<Session>> {
        let bytes = match fs::read(&self.file) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_err(&self.file, e)),
        };
        let file: SessionFile = serde_json::from_slice(&bytes).map_err(|e| {
            Error::new(
                ErrorKind::Integrity,
                format!("{}: {e}", self.file.display()),
            )
        })?;
        if file.format != FORMAT {
            return Err(Error::new(
                ErrorKind::Integrity,
                format!("{}: unknown format {}", self.file.display(), file.format),
            ));
        }
        file.session.validate()?;
        Ok(Some(file.session))
    }

    fn load(&self) -> vl_core::Result<Session> {
        self.load_optional()?
            .ok_or_else(|| Error::not_found("bisect session"))
    }

    fn save(&self, session: &Session) -> vl_core::Result<()> {
        let bytes = serde_json::to_vec_pretty(&SessionFile {
            format: FORMAT,
            session: session.clone(),
        })
        .map_err(|e| Error::internal(e.to_string()))?;
        if let Some(dir) = self.file.parent() {
            fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
        }
        let temp = self.file.with_extension("json.tmp");
        fs::write(&temp, bytes).map_err(|e| io_err(&temp, e))?;
        fs::rename(&temp, &self.file).map_err(|e| {
            let _ = fs::remove_file(&temp);
            io_err(&self.file, e)
        })
    }
}

fn remove_file(path: &Path) -> vl_core::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_err(path, e)),
    }
}

fn status(session: &Session) -> BisectStatus {
    let enabled = session.probe().unwrap_or_default();
    let candidates = session.candidate_files();
    BisectStatus {
        state: if session.is_finished() {
            BisectState::Finished
        } else {
            BisectState::Running
        },
        steps_done: session.steps_done(),
        steps_estimate: session.steps_estimate(),
        candidates: u32::try_from(candidates.len()).unwrap_or(u32::MAX),
        suspects: session.suspects(),
        disabled: if session.is_finished() {
            0
        } else {
            u32::try_from(candidates.len().saturating_sub(enabled.len())).unwrap_or(u32::MAX)
        },
        enabled,
        verdict: session.verdict().cloned(),
    }
}
