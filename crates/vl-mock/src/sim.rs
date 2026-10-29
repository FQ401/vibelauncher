//! Simulated long operations on background threads.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use vl_core::account::{Account, AccountEvent, AccountKind, DeviceCodePrompt};
use vl_core::instance::InstanceState;
use vl_core::launch::{GameExit, LaunchEvent};
use vl_core::task::{
    Progress, ProgressUnit, TaskEvent, TaskInfo, TaskKind, TaskOutcome, TaskSnapshot,
};
use vl_core::{AccountId, Error, Event, InstanceId, Response, Result, TaskId};

use crate::{Inner, console};

/// A registered task: reports progress and finishes exactly once.
pub(crate) struct Task {
    pub(crate) inner: Arc<Inner>,
    pub(crate) id: TaskId,
    pub(crate) cancel: Arc<AtomicBool>,
}

impl Task {
    pub(crate) fn start(inner: &Arc<Inner>, kind: TaskKind, subject: Option<String>) -> Self {
        let id = TaskId(inner.next_task.fetch_add(1, Ordering::Relaxed));
        let info = TaskInfo {
            id,
            parent: None,
            kind,
            subject,
            cancellable: true,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        inner.lock().tasks.insert(
            id,
            (
                TaskSnapshot {
                    info: info.clone(),
                    progress: None,
                },
                Arc::clone(&cancel),
            ),
        );
        inner.publish(Event::Task(TaskEvent::Started(info)));
        Self {
            inner: Arc::clone(inner),
            id,
            cancel,
        }
    }

    pub(crate) fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub(crate) fn progress(&self, stage: &str, done: u64, total: u64) {
        let progress = Progress {
            done,
            total: Some(total),
            unit: ProgressUnit::Items,
            stage: Some(stage.into()),
        };
        if let Some((snapshot, _)) = self.inner.lock().tasks.get_mut(&self.id) {
            snapshot.progress = Some(progress.clone());
        }
        self.inner.publish(Event::Task(TaskEvent::Progress {
            id: self.id,
            progress,
        }));
    }

    pub(crate) fn finish(self, outcome: TaskOutcome) {
        self.inner.lock().tasks.remove(&self.id);
        self.inner.publish(Event::Task(TaskEvent::Finished {
            id: self.id,
            outcome,
        }));
    }

    /// Runs `stage` for `total` paced steps; `false` if cancelled.
    pub(crate) fn run_stage(&self, stage: &str, total: u64) -> bool {
        for done in 1..=total {
            if self.cancelled() {
                return false;
            }
            pause(&self.inner);
            self.progress(stage, done, total);
        }
        !self.cancelled()
    }
}

pub(crate) fn pause(inner: &Inner) {
    let delay = inner.options.step_delay;
    if !delay.is_zero() {
        std::thread::sleep(delay);
    }
}

/// Starts `task` on a thread running `work`; answers `Response::Task`.
pub(crate) fn spawn(task: Task, work: impl FnOnce(Task) + Send + 'static) -> Result<Response> {
    let id = task.id;
    let inner = Arc::clone(&task.inner);
    match std::thread::Builder::new()
        .name(format!("vl-mock-{id}"))
        .spawn(move || work(task))
    {
        Ok(_) => Ok(Response::Task(id)),
        Err(e) => {
            let error = Error::internal(format!("cannot start a thread: {e}"));
            inner.lock().tasks.remove(&id);
            inner.publish(Event::Task(TaskEvent::Finished {
                id,
                outcome: TaskOutcome::Failed(error.clone()),
            }));
            Err(error)
        }
    }
}

/// A task of `total` steps that calls `done` when it succeeds.
pub(crate) fn steps(
    inner: &Arc<Inner>,
    kind: TaskKind,
    subject: Option<String>,
    total: u64,
    done: impl FnOnce(&Inner) + Send + 'static,
) -> Result<Response> {
    let task = Task::start(inner, kind, subject);
    spawn(task, move |task| {
        if task.run_stage("working", total) {
            done(&task.inner);
            task.finish(TaskOutcome::Succeeded);
        } else {
            task.finish(TaskOutcome::Cancelled);
        }
    })
}

/// A task that runs `stages` (a name and a number of paced steps each) and then calls `done`;
/// the task fails with the error `done` returns. A cancelled task never calls `done`, so it
/// leaves nothing behind.
pub(crate) fn staged(
    inner: &Arc<Inner>,
    kind: TaskKind,
    subject: Option<String>,
    stages: Vec<(String, u64)>,
    done: impl FnOnce(&Inner) -> Result<()> + Send + 'static,
) -> Result<Response> {
    let task = Task::start(inner, kind, subject);
    spawn(task, move |task| {
        if !stages
            .iter()
            .all(|(stage, total)| task.run_stage(stage, *total))
        {
            task.finish(TaskOutcome::Cancelled);
            return;
        }
        match done(&task.inner) {
            Ok(()) => task.finish(TaskOutcome::Succeeded),
            Err(error) => task.finish(TaskOutcome::Failed(error)),
        }
    })
}

/// A Microsoft sign-in: shows a device code, then adds a synthetic account.
pub(crate) fn sign_in(inner: &Arc<Inner>) -> Result<Response> {
    let task = Task::start(inner, TaskKind::SignIn, None);
    inner.publish(Event::Account(AccountEvent::DeviceCode(DeviceCodePrompt {
        task: task.id,
        user_code: "MOCK-CODE".into(),
        verification_uri: "https://example.invalid/device".into(),
        expires_in_secs: 900,
    })));
    spawn(task, |task| {
        if !task.run_stage("waiting for the user", 5) {
            task.finish(TaskOutcome::Cancelled);
            return;
        }
        {
            let mut state = task.inner.lock();
            let n = state.accounts.len() + 1;
            let is_default = state.accounts.is_empty();
            if let Some(id) = AccountId::new(&format!("microsoft-mock-{n}")) {
                state.accounts.push(Account {
                    id,
                    kind: AccountKind::Microsoft,
                    name: format!("MockCrafter{n}"),
                    uuid: format!("00000000-0000-4000-8000-{n:012}"),
                    is_default,
                    needs_sign_in: false,
                });
            }
        }
        task.inner
            .publish(Event::Account(AccountEvent::ListChanged));
        task.finish(TaskOutcome::Succeeded);
    })
}

/// Prepares and runs a simulated game of `instance`. The launch task's cancel flag is also the
/// game's stop flag, so `LaunchCommand::Stop` works in both phases.
pub(crate) fn game(
    inner: &Arc<Inner>,
    instance: InstanceId,
    name: String,
    account: Account,
) -> Result<Response> {
    let task = Task::start(inner, TaskKind::Launch, Some(name));
    let stop = Arc::clone(&task.cancel);
    inner.lock().games.insert(instance.clone(), stop);
    inner.set_state(&instance, InstanceState::Preparing { task: task.id });
    let id = instance.clone();
    let result = spawn(task, move |task| run_game(task, &id, &account));
    if result.is_err() {
        inner.lock().games.remove(&instance);
        inner.set_state(&instance, InstanceState::Stopped);
    }
    result
}

fn run_game(task: Task, instance: &InstanceId, account: &Account) {
    let inner = Arc::clone(&task.inner);
    let stop = Arc::clone(&task.cancel);
    let prepared = [("verify", 3), ("libraries", 12), ("assets", 20)]
        .into_iter()
        .all(|(stage, total)| task.run_stage(stage, total));
    if !prepared {
        task.finish(TaskOutcome::Cancelled);
        end_game(&inner, instance, None, true);
        return;
    }
    task.finish(TaskOutcome::Succeeded);
    console::start(&inner, instance);
    inner.set_state(instance, InstanceState::Running { pid: Some(4242) });

    console::emit(&inner, instance, console::greeting(&account.name));
    let mut stopped = false;
    for n in 1..=inner.options.game_log_lines {
        if stop.load(Ordering::Relaxed) {
            stopped = true;
            break;
        }
        pause(&inner);
        console::emit(&inner, instance, console::paced(n));
    }
    if !stopped {
        pause(&inner);
        console::emit(&inner, instance, console::stack_trace());
        pause(&inner);
        console::emit(&inner, instance, console::spam());
    }
    if !inner.options.exit_after_log {
        let idle = inner.options.step_delay.max(Duration::from_millis(20));
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(idle);
        }
    }
    end_game(&inner, instance, Some(0), stop.load(Ordering::Relaxed));
}

fn end_game(inner: &Inner, instance: &InstanceId, code: Option<i32>, stopped_by_user: bool) {
    inner.lock().games.remove(instance);
    inner.set_state(instance, InstanceState::Stopped);
    inner.publish(Event::Launch(LaunchEvent::Exited {
        instance: instance.clone(),
        exit: GameExit {
            code,
            crashed: false,
            stopped_by_user,
            session_ms: 0,
            crash_report: None,
            hs_err: None,
            log_file: None,
            tail: Vec::new(),
        },
    }));
}
