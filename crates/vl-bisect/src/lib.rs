//! Mod bisection: the search logic, with no files and no processes.
//!
//! A [`Session`] holds the state of one search for the mod (or the few mods together) that make
//! the game fail. It hands out the next set of mod files to enable ([`Session::probe`]) and takes
//! the user's answer to it ([`Session::answer`]: bad, good or skipped). The caller applies the
//! sets and runs the game; the session serializes, so a search survives a restart.
//!
//! The search is delta debugging (`ddmin`) over the mods, with these rules:
//!
//! - **Dependencies.** Every probe is closed under required dependencies: a file is never
//!   disabled while an enabled file requires a mod it provides.
//! - **Libraries.** A file that another file depends on is a library. Libraries stay enabled
//!   while the other mods are searched; only when the problem persists without any other mod
//!   (or no other mod is left) are the libraries searched themselves.
//! - **Hints.** Mod ids from a crash diagnosis are tried first; when they reproduce the problem
//!   the search starts from them.
//! - **Several mods.** A problem that needs two mods together ends with both as culprits.
//! - **Cost.** One culprit is found in about `log2(n)` steps, at most about twice that when
//!   the first half of each split is the wrong one; see [`Session::steps_estimate`].

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use vl_core::bisect::{BisectOutcome, BisectVerdict};
use vl_core::{Error, ErrorKind};

pub mod auto;
#[cfg(test)]
mod tests;

/// One file under test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    /// The file name, as the resource folder lists it.
    pub file: String,
    /// The mod ids the file provides.
    pub provides: Vec<String>,
    /// The mod ids the file requires. Ids no candidate provides (`minecraft`, the loader) are
    /// ignored.
    pub requires: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Stage {
    /// Trying the hinted mods.
    Hint,
    /// Trying every mod.
    Baseline,
    /// Trying the libraries alone.
    Zero,
    /// Searching the non-library mods with the libraries enabled.
    Leaves,
    /// Searching the libraries with all other mods disabled.
    Libs,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Hint,
    Baseline,
    Zero,
    /// A part of the suspects.
    Part,
    /// The suspects without a part.
    Complement,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Pending {
    kind: Kind,
    /// The units tested (before dependencies are added).
    set: Vec<usize>,
    /// The files enabled, sorted.
    enabled: Vec<usize>,
}

/// The state of one search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    candidates: Vec<Candidate>,
    hints: Vec<usize>,
    stage: Stage,
    /// The files the current search can narrow down.
    units: Vec<usize>,
    /// Files enabled in every probe of the current search besides the tested units.
    fixed: Vec<usize>,
    /// The suspects: a set that reproduces the problem, sorted.
    current: Vec<usize>,
    granularity: usize,
    cursor: usize,
    steps: u32,
    skipped: bool,
    memo: Vec<(Vec<usize>, BisectOutcome)>,
    pending: Option<Pending>,
    verdict: Option<BisectVerdict>,
}

/// Dependencies between candidates.
struct Graph {
    deps: Vec<Vec<usize>>,
    is_lib: Vec<bool>,
}

impl Graph {
    fn lib(&self, i: usize) -> bool {
        self.is_lib.get(i).copied().unwrap_or(false)
    }
}

fn damaged(what: &str) -> Error {
    Error::new(
        ErrorKind::Integrity,
        format!("damaged bisect session: {what}"),
    )
}

impl Session {
    /// Starts a search over `candidates` (the enabled mod files). `hint_ids` are mod ids to try
    /// first; unknown ids are ignored. At least two candidates are needed.
    pub fn new(candidates: Vec<Candidate>, hint_ids: &[String]) -> vl_core::Result<Self> {
        if candidates.len() < 2 {
            return Err(Error::invalid_input(
                "bisecting needs at least two enabled mods",
            ));
        }
        let mut seen = BTreeSet::new();
        if !candidates.iter().all(|c| seen.insert(c.file.as_str())) {
            return Err(Error::invalid_input("duplicate mod file"));
        }
        let hints = candidates
            .iter()
            .enumerate()
            .filter(|(_, c)| c.provides.iter().any(|p| hint_ids.contains(p)))
            .map(|(i, _)| i)
            .collect();
        let mut session = Self {
            candidates,
            hints,
            stage: Stage::Baseline,
            units: Vec::new(),
            fixed: Vec::new(),
            current: Vec::new(),
            granularity: 2,
            cursor: 0,
            steps: 0,
            skipped: false,
            memo: Vec::new(),
            pending: None,
            verdict: None,
        };
        session.begin();
        Ok(session)
    }

    /// Checks a session loaded from a file: every index must point at a candidate.
    pub fn validate(&self) -> vl_core::Result<()> {
        let n = self.candidates.len();
        let ok = |v: &[usize]| v.iter().all(|&i| i < n);
        let memo_ok = self.memo.iter().all(|(set, _)| ok(set));
        let pending_ok = self
            .pending
            .as_ref()
            .is_none_or(|p| ok(&p.set) && ok(&p.enabled));
        if n < 2
            || !ok(&self.hints)
            || !ok(&self.units)
            || !ok(&self.fixed)
            || !ok(&self.current)
            || !memo_ok
            || !pending_ok
        {
            return Err(damaged("index out of range"));
        }
        if (self.stage == Stage::Done) != self.pending.is_none() {
            return Err(damaged("state"));
        }
        Ok(())
    }

    fn graph(&self) -> Graph {
        let n = self.candidates.len();
        let mut deps = vec![Vec::new(); n];
        let mut is_lib = vec![false; n];
        for (i, c) in self.candidates.iter().enumerate() {
            for (j, other) in self.candidates.iter().enumerate() {
                if i != j && c.requires.iter().any(|r| other.provides.contains(r)) {
                    if let Some(d) = deps.get_mut(i) {
                        d.push(j);
                    }
                    if let Some(l) = is_lib.get_mut(j) {
                        *l = true;
                    }
                }
            }
        }
        Graph { deps, is_lib }
    }

    /// `set` plus everything it requires, directly or not; sorted.
    fn closure(graph: &Graph, set: &[usize]) -> Vec<usize> {
        let mut seen: BTreeSet<usize> = BTreeSet::new();
        let mut stack: Vec<usize> = set.to_vec();
        while let Some(i) = stack.pop() {
            if seen.insert(i)
                && let Some(d) = graph.deps.get(i)
            {
                stack.extend(d.iter().copied());
            }
        }
        seen.into_iter().collect()
    }

    fn union(a: &[usize], b: &[usize]) -> Vec<usize> {
        let set: BTreeSet<usize> = a.iter().chain(b).copied().collect();
        set.into_iter().collect()
    }

    fn files(&self, set: &[usize]) -> Vec<String> {
        set.iter()
            .filter_map(|&i| self.candidates.get(i))
            .map(|c| c.file.clone())
            .collect()
    }

    fn all(&self) -> Vec<usize> {
        (0..self.candidates.len()).collect()
    }

    /// The non-library and the library files.
    fn split(&self, graph: &Graph) -> (Vec<usize>, Vec<usize>) {
        self.all().into_iter().partition(|&i| !graph.lib(i))
    }

    /// Chooses the first probe.
    fn begin(&mut self) {
        let graph = self.graph();
        let (leaves, libs) = self.split(&graph);
        if !self.hints.is_empty() {
            let mut enabled = Self::closure(&graph, &self.hints);
            if self.hints.iter().any(|i| leaves.contains(i)) {
                enabled = Self::union(&enabled, &libs);
            }
            if enabled != self.all() {
                self.stage = Stage::Hint;
                self.pending = Some(Pending {
                    kind: Kind::Hint,
                    set: self.hints.clone(),
                    enabled,
                });
                return;
            }
        }
        self.start_baseline();
    }

    fn start_baseline(&mut self) {
        self.stage = Stage::Baseline;
        self.pending = Some(Pending {
            kind: Kind::Baseline,
            set: self.all(),
            enabled: self.all(),
        });
    }

    /// The problem happens with every mod; continues with the non-library mods `suspects`.
    fn begin_search(&mut self, suspects: Vec<usize>) {
        let graph = self.graph();
        let (leaves, libs) = self.split(&graph);
        if leaves.is_empty() {
            return self.begin_libs(libs);
        }
        self.units = leaves;
        self.fixed = libs.clone();
        self.current = suspects;
        self.granularity = 2;
        self.cursor = 0;
        if libs.is_empty() {
            self.stage = Stage::Leaves;
            self.next_probe();
        } else {
            self.stage = Stage::Zero;
            self.pending = Some(Pending {
                kind: Kind::Zero,
                set: Vec::new(),
                enabled: libs,
            });
        }
    }

    /// The problem happens with `suspects` (libraries) alone.
    fn begin_libs(&mut self, suspects: Vec<usize>) {
        let graph = self.graph();
        self.units = self.split(&graph).1;
        self.fixed = Vec::new();
        self.current = Self::closure(&graph, &suspects);
        self.granularity = 2;
        self.cursor = 0;
        self.stage = Stage::Libs;
        self.next_probe();
    }

    /// The parts of the suspects and then their complements, in the order they are tried.
    fn tests(&self) -> Vec<(Vec<usize>, bool)> {
        let len = self.current.len();
        let n = self.granularity.clamp(2, len.max(2));
        let mut parts: Vec<Vec<usize>> = Vec::new();
        let mut start = 0;
        for k in 0..n {
            let end = start + (len - start).div_ceil(n - k);
            parts.push(self.current.get(start..end).unwrap_or_default().to_vec());
            start = end;
        }
        let mut tests: Vec<(Vec<usize>, bool)> = parts.iter().map(|p| (p.clone(), false)).collect();
        if n > 2 {
            for p in &parts {
                let rest = self
                    .current
                    .iter()
                    .copied()
                    .filter(|i| !p.contains(i))
                    .collect();
                tests.push((rest, true));
            }
        }
        tests
    }

    fn lookup(&self, enabled: &[usize]) -> Option<BisectOutcome> {
        self.memo
            .iter()
            .find(|(set, _)| set == enabled)
            .map(|(_, o)| *o)
    }

    /// Picks the next probe of the running search, or finishes it.
    fn next_probe(&mut self) {
        let graph = self.graph();
        'search: loop {
            if self.current.len() <= 1 {
                return self.finish_found(&graph);
            }
            let here = Self::union(&Self::closure(&graph, &self.current), &self.fixed);
            let tests = self.tests();
            while let Some((set, complement)) = tests.get(self.cursor).cloned() {
                let enabled = Self::union(&Self::closure(&graph, &set), &self.fixed);
                if set.is_empty() || enabled == here {
                    self.cursor += 1;
                    continue;
                }
                match self.lookup(&enabled) {
                    Some(BisectOutcome::Bad) => {
                        self.narrow(&graph, &set, complement);
                        continue 'search;
                    }
                    Some(_) => {
                        self.cursor += 1;
                        continue;
                    }
                    None => {}
                }
                self.pending = Some(Pending {
                    kind: if complement {
                        Kind::Complement
                    } else {
                        Kind::Part
                    },
                    set,
                    enabled,
                });
                return;
            }
            if self.granularity < self.current.len() {
                self.granularity = (self.granularity * 2).min(self.current.len());
                self.cursor = 0;
            } else {
                return self.finish_found(&graph);
            }
        }
    }

    /// The problem happens with only `set` (and its dependencies and the fixed files).
    fn narrow(&mut self, graph: &Graph, set: &[usize], complement: bool) {
        self.current = Self::closure(graph, set)
            .into_iter()
            .filter(|i| self.units.contains(i))
            .collect();
        self.granularity = if complement {
            self.granularity.saturating_sub(1).max(2)
        } else {
            2
        };
        self.cursor = 0;
    }

    fn finish_found(&mut self, graph: &Graph) {
        self.stage = Stage::Done;
        self.pending = None;
        self.verdict = Some(if self.skipped && self.current.len() > 1 {
            BisectVerdict::Inconclusive {
                suspects: self.files(&self.current),
            }
        } else {
            let needs: Vec<usize> = Self::closure(graph, &self.current)
                .into_iter()
                .filter(|i| !self.current.contains(i))
                .collect();
            BisectVerdict::Found {
                culprits: self.files(&self.current),
                needs: self.files(&needs),
            }
        });
    }

    fn finish(&mut self, verdict: BisectVerdict) {
        self.stage = Stage::Done;
        self.pending = None;
        self.verdict = Some(verdict);
    }

    /// The files to enable in the current step; every other candidate is disabled. `None` when
    /// the search is over.
    pub fn probe(&self) -> Option<Vec<String>> {
        self.pending.as_ref().map(|p| self.files(&p.enabled))
    }

    /// Records what the current step did and chooses the next one.
    pub fn answer(&mut self, outcome: BisectOutcome) -> vl_core::Result<()> {
        let Some(pending) = self.pending.take() else {
            return Err(Error::conflict("the search is already finished"));
        };
        self.steps += 1;
        if outcome == BisectOutcome::Skipped {
            self.skipped = true;
        }
        if self.lookup(&pending.enabled).is_none() {
            self.memo.push((pending.enabled.clone(), outcome));
        }
        let graph = self.graph();
        match (pending.kind, outcome) {
            (Kind::Hint, BisectOutcome::Bad) => {
                let leaves: Vec<usize> = pending
                    .set
                    .iter()
                    .copied()
                    .filter(|&i| !graph.lib(i))
                    .collect();
                if leaves.is_empty() {
                    self.begin_libs(pending.enabled);
                } else {
                    self.begin_search(leaves);
                }
            }
            (Kind::Hint, _) => self.start_baseline(),
            (Kind::Baseline, BisectOutcome::Bad) => {
                let leaves = self.split(&graph).0;
                self.begin_search(leaves);
            }
            (Kind::Baseline, BisectOutcome::Good) => self.finish(BisectVerdict::NotReproduced),
            (Kind::Baseline, _) => {
                let suspects = self.files(&self.all());
                self.finish(BisectVerdict::Inconclusive { suspects });
            }
            (Kind::Zero, BisectOutcome::Bad) => {
                let libs = self.fixed.clone();
                self.begin_libs(libs);
            }
            (Kind::Zero, _) => {
                self.stage = Stage::Leaves;
                self.next_probe();
            }
            (Kind::Part | Kind::Complement, BisectOutcome::Bad) => {
                self.narrow(&graph, &pending.set, pending.kind == Kind::Complement);
                self.next_probe();
            }
            (Kind::Part | Kind::Complement, _) => {
                self.cursor += 1;
                self.next_probe();
            }
        }
        Ok(())
    }

    /// The result, once the search is over.
    pub fn verdict(&self) -> Option<&BisectVerdict> {
        self.verdict.as_ref()
    }

    /// Whether the search is over.
    pub fn is_finished(&self) -> bool {
        self.stage == Stage::Done
    }

    /// Reports received so far.
    pub fn steps_done(&self) -> u32 {
        self.steps
    }

    /// About how many more steps are needed: `ceil(log2(suspects))`, plus one for a step that
    /// only checks the starting point. Zero when finished. A wrong first half costs an extra
    /// step on some splits, so the real count can reach about twice this.
    pub fn steps_estimate(&self) -> u32 {
        let size = match self.stage {
            Stage::Done => return 0,
            Stage::Leaves | Stage::Libs => self.current.len(),
            _ => self.candidates.len(),
        };
        let extra = u32::from(matches!(
            self.stage,
            Stage::Hint | Stage::Baseline | Stage::Zero
        ));
        size.max(1).next_power_of_two().trailing_zeros() + extra
    }

    /// The files the cause is narrowed down to so far (every candidate before the first
    /// narrowing).
    pub fn suspects(&self) -> Vec<String> {
        match self.stage {
            Stage::Leaves | Stage::Libs | Stage::Zero | Stage::Done if !self.current.is_empty() => {
                self.files(&self.current)
            }
            _ => self.candidate_files(),
        }
    }

    /// Every file under test.
    pub fn candidate_files(&self) -> Vec<String> {
        self.candidates.iter().map(|c| c.file.clone()).collect()
    }

    /// Every file under test except `files` and the files that require them, directly or not:
    /// the set that checks a verdict.
    pub fn without(&self, files: &[String]) -> Vec<String> {
        let graph = self.graph();
        self.all()
            .into_iter()
            .filter(|&i| {
                Self::closure(&graph, &[i])
                    .iter()
                    .filter_map(|&j| self.candidates.get(j))
                    .all(|c| !files.contains(&c.file))
            })
            .filter_map(|i| self.candidates.get(i))
            .map(|c| c.file.clone())
            .collect()
    }
}
