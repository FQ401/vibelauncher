//! View state mirrored from the facade: commands go out, replies and events fold into plain data.

use vl_core::account::Account;
use vl_core::account::AccountCommand;
use vl_core::instance::{InstanceCommand, InstanceEvent, InstanceState, InstanceSummary};
use vl_core::launch::{LaunchCommand, LaunchEvent, LaunchRequest};
use vl_core::task::{TaskCommand, TaskEvent, TaskOutcome, TaskSnapshot};
use vl_core::{
    AccountId, BackendInfo, Command, Event, EventStream, Facade, InstanceId, Reply, Response,
    TaskId,
};

/// What an outstanding reply was asked for.
#[derive(Debug, Clone, Copy)]
enum Pending {
    Hello,
    Instances,
    Accounts,
    Tasks,
    /// A command whose only interesting outcome is a failure.
    Fire,
}

/// Everything the screens draw from.
pub struct Model {
    facade: Box<dyn Facade>,
    events: EventStream,
    pending: Vec<(Pending, Reply)>,
    pub backend: Option<BackendInfo>,
    pub instances: Vec<InstanceSummary>,
    pub accounts: Vec<Account>,
    pub tasks: Vec<TaskSnapshot>,
    /// The instance shown on the home showcase; `None` means the most recently played one.
    pub selected: Option<InstanceId>,
    /// The latest error to show, in English technical form.
    pub last_error: Option<String>,
}

impl Model {
    /// Subscribes to events first, then asks for the initial state.
    pub fn new(facade: Box<dyn Facade>) -> Self {
        let events = facade.subscribe();
        let mut model = Self {
            facade,
            events,
            pending: Vec::new(),
            backend: None,
            instances: Vec::new(),
            accounts: Vec::new(),
            tasks: Vec::new(),
            selected: None,
            last_error: None,
        };
        model.refresh();
        model
    }

    /// Re-reads all state with commands.
    fn refresh(&mut self) {
        self.send(Pending::Hello, Command::Hello);
        self.send(
            Pending::Instances,
            Command::Instances(InstanceCommand::List),
        );
        self.send(Pending::Accounts, Command::Accounts(AccountCommand::List));
        self.send(Pending::Tasks, Command::Tasks(TaskCommand::List));
    }

    fn send(&mut self, purpose: Pending, command: Command) {
        let reply = self.facade.call(command);
        self.pending.push((purpose, reply));
    }

    /// Folds ready replies and queued events into the state. Call once per frame.
    pub fn poll(&mut self) {
        let mut waiting = Vec::new();
        for (purpose, reply) in std::mem::take(&mut self.pending) {
            match reply.try_take() {
                None => waiting.push((purpose, reply)),
                Some(Ok(response)) => self.apply_response(purpose, response),
                Some(Err(error)) => self.last_error = Some(error.to_string()),
            }
        }
        self.pending = waiting;
        while let Some(event) = self.events.try_next() {
            self.apply_event(event);
        }
    }

    fn apply_response(&mut self, purpose: Pending, response: Response) {
        match (purpose, response) {
            (Pending::Hello, Response::Hello(info)) => self.backend = Some(info),
            (Pending::Instances, Response::Instances(list)) => self.instances = list,
            (Pending::Accounts, Response::Accounts(list)) => self.accounts = list,
            (Pending::Tasks, Response::Tasks(list)) => self.tasks = list,
            (Pending::Fire, _) => {}
            _ => {}
        }
    }

    fn apply_event(&mut self, event: Event) {
        match event {
            Event::Instance(InstanceEvent::Added(summary) | InstanceEvent::Changed(summary)) => {
                match self.instances.iter_mut().find(|i| i.id == summary.id) {
                    Some(slot) => *slot = summary,
                    None => self.instances.push(summary),
                }
            }
            Event::Instance(InstanceEvent::Removed(id)) => {
                self.instances.retain(|i| i.id != id);
                if self.selected.as_ref() == Some(&id) {
                    self.selected = None;
                }
            }
            Event::Launch(LaunchEvent::StateChanged { instance, state }) => {
                if let Some(slot) = self.instances.iter_mut().find(|i| i.id == instance) {
                    slot.state = state;
                }
            }
            Event::Task(TaskEvent::Started(info)) => {
                self.tasks.retain(|t| t.info.id != info.id);
                self.tasks.push(TaskSnapshot {
                    info,
                    progress: None,
                });
            }
            Event::Task(TaskEvent::Progress { id, progress }) => {
                if let Some(task) = self.tasks.iter_mut().find(|t| t.info.id == id) {
                    task.progress = Some(progress);
                }
            }
            Event::Task(TaskEvent::Finished { id, outcome }) => {
                self.tasks.retain(|t| t.info.id != id);
                if let TaskOutcome::Failed(error) = outcome {
                    self.last_error = Some(error.to_string());
                }
            }
            Event::Account(_) => {
                self.send(Pending::Accounts, Command::Accounts(AccountCommand::List))
            }
            Event::Lagged { .. } => self.refresh(),
            _ => {}
        }
    }

    /// The instance the showcase shows: the selected one, else the most recently played.
    pub fn featured(&self) -> Option<&InstanceSummary> {
        let selected = self
            .selected
            .as_ref()
            .and_then(|id| self.instances.iter().find(|i| &i.id == id));
        selected.or_else(|| {
            self.instances
                .iter()
                .filter(|i| i.problem.is_none())
                .max_by_key(|i| i.last_played_unix.unwrap_or(i64::MIN))
                .or_else(|| self.instances.first())
        })
    }

    /// The progress (0..=1) of an instance's launch task, if it is preparing.
    pub fn launch_fraction(&self, instance: &InstanceSummary) -> Option<f32> {
        let InstanceState::Preparing { task } = &instance.state else {
            return None;
        };
        self.tasks
            .iter()
            .find(|t| t.info.id == *task)
            .and_then(|t| t.progress.as_ref())
            .and_then(|p| p.fraction())
    }

    pub fn launch(&mut self, instance: &InstanceId) {
        let account: Option<AccountId> = self
            .accounts
            .iter()
            .find(|a| a.is_default)
            .map(|a| a.id.clone());
        let request = LaunchRequest {
            instance: instance.clone(),
            account,
            offline: false,
        };
        self.send(
            Pending::Fire,
            Command::Launch(LaunchCommand::Launch(request)),
        );
    }

    pub fn stop(&mut self, instance: &InstanceId) {
        let command = Command::Launch(LaunchCommand::Stop {
            instance: instance.clone(),
        });
        self.send(Pending::Fire, command);
    }

    pub fn cancel_task(&mut self, id: TaskId) {
        self.send(Pending::Fire, Command::Tasks(TaskCommand::Cancel { id }));
    }

    /// Whether anything is changing on its own, so the UI should keep repainting quickly.
    pub fn busy(&self) -> bool {
        !self.tasks.is_empty()
            || !self.pending.is_empty()
            || self
                .instances
                .iter()
                .any(|i| i.state != InstanceState::Stopped)
    }
}
