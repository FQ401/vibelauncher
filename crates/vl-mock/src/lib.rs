//! A facade with synthetic data: the UI and `vl-cli` run on it without the native layer.
//!
//! [`MockFacade`] answers every facade command from in-memory synthetic data (instances, versions,
//! accounts, Java runtimes, settings) and simulates long operations on background threads: a
//! launch reports task progress, moves the instance to `Running`, streams game log lines and
//! exits. Nothing touches files, the network or real processes.

mod data;
mod sim;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use vl_core::account::{Account, AccountCommand, AccountEvent, AccountKind};
use vl_core::instance::{InstanceCommand, InstanceEvent, InstanceState, InstanceSummary, Loader};
use vl_core::java::{JavaCommand, JavaEvent, JavaRuntime};
use vl_core::launch::{LaunchCommand, LaunchEvent, LaunchRequest};
use vl_core::settings::{Overrides, Settings, SettingsCommand, SettingsEvent};
use vl_core::task::{TaskCommand, TaskKind, TaskSnapshot};
use vl_core::version::{GameVersion, VersionCommand, VersionEvent};
use vl_core::{
    AccountId, BackendInfo, Command, Error, ErrorKind, Event, EventBus, EventStream, Facade,
    InstanceId, Reply, Response, Result, TaskId,
};

/// How the mock paces its simulations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockOptions {
    /// The pause between simulated steps (progress ticks, log lines).
    pub step_delay: Duration,
    /// How many log lines a simulated game prints after it starts.
    pub game_log_lines: usize,
    /// Whether a simulated game exits by itself after its log lines; otherwise it runs until
    /// `LaunchCommand::Stop`.
    pub exit_after_log: bool,
}

impl Default for MockOptions {
    /// Paced for watching in the UI: the game keeps running until stopped.
    fn default() -> Self {
        Self {
            step_delay: Duration::from_millis(120),
            game_log_lines: 40,
            exit_after_log: false,
        }
    }
}

impl MockOptions {
    /// No pauses and a short game that exits by itself: for tests.
    pub fn instant() -> Self {
        Self {
            step_delay: Duration::ZERO,
            game_log_lines: 5,
            exit_after_log: true,
        }
    }
}

/// The mock facade. Cheap to clone; clones share state.
#[derive(Clone)]
pub struct MockFacade {
    inner: Arc<Inner>,
}

struct Inner {
    options: MockOptions,
    events: EventBus,
    next_task: AtomicU64,
    state: Mutex<State>,
}

struct State {
    instances: Vec<InstanceSummary>,
    versions: Vec<GameVersion>,
    accounts: Vec<Account>,
    javas: Vec<JavaRuntime>,
    settings: Settings,
    overrides: HashMap<InstanceId, Overrides>,
    tasks: HashMap<TaskId, (TaskSnapshot, Arc<AtomicBool>)>,
    /// Stop flags of running (or preparing) games.
    games: HashMap<InstanceId, Arc<AtomicBool>>,
}

impl MockFacade {
    /// A mock with the synthetic data set.
    pub fn new(options: MockOptions) -> Self {
        let state = State {
            instances: data::instances(),
            versions: data::versions(),
            accounts: data::accounts(),
            javas: data::javas(),
            settings: Settings::default(),
            overrides: HashMap::new(),
            tasks: HashMap::new(),
            games: HashMap::new(),
        };
        Self {
            inner: Arc::new(Inner {
                options,
                events: EventBus::default(),
                next_task: AtomicU64::new(1),
                state: Mutex::new(state),
            }),
        }
    }

    fn handle(&self, command: Command) -> Result<Response> {
        match command {
            Command::Hello => Ok(Response::Hello(BackendInfo {
                name: "vl-mock".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                mock: true,
            })),
            Command::Instances(c) => self.instances(c),
            Command::Versions(c) => self.versions(c),
            Command::Accounts(c) => self.accounts(c),
            Command::Java(c) => self.java(c),
            Command::Settings(c) => self.settings(c),
            Command::Launch(c) => self.launch(c),
            Command::Tasks(c) => self.tasks(c),
            other => Err(unsupported(&other)),
        }
    }

    fn instances(&self, command: InstanceCommand) -> Result<Response> {
        let inner = &self.inner;
        match command {
            InstanceCommand::List => Ok(Response::Instances(inner.lock().instances.clone())),
            InstanceCommand::Create(new) => {
                let name = non_empty(&new.name, "instance name")?;
                let mut state = inner.lock();
                if !state.versions.iter().any(|v| v.id == new.game_version) {
                    return Err(Error::not_found(format!(
                        "game version {}",
                        new.game_version
                    )));
                }
                let id = data::slug(name, |c| state.instances.iter().any(|i| i.id.as_str() == c))
                    .ok_or_else(|| Error::internal("no instance id"))?;
                let summary = InstanceSummary {
                    id,
                    name: name.into(),
                    group: new.group,
                    game_version: new.game_version,
                    loader: mock_loader(new.loader.as_ref()),
                    play_time_secs: 0,
                    last_played_unix: None,
                    state: InstanceState::Stopped,
                    problem: None,
                };
                state.instances.push(summary.clone());
                drop(state);
                inner.publish(Event::Instance(InstanceEvent::Added(summary.clone())));
                Ok(Response::Instance(summary))
            }
            InstanceCommand::Rename { id, name } => {
                let name = non_empty(&name, "instance name")?;
                let summary = {
                    let mut state = inner.lock();
                    let instance = find_instance(&mut state, &id)?;
                    instance.name = name.into();
                    instance.clone()
                };
                inner.publish(Event::Instance(InstanceEvent::Changed(summary.clone())));
                Ok(Response::Instance(summary))
            }
            InstanceCommand::Delete { id } => {
                {
                    let mut state = inner.lock();
                    if find_instance(&mut state, &id)?.state != InstanceState::Stopped {
                        return Err(Error::conflict(format!("instance {id} is running")));
                    }
                    state.instances.retain(|i| i.id != id);
                    state.overrides.remove(&id);
                }
                inner.publish(Event::Instance(InstanceEvent::Removed(id)));
                Ok(Response::Done)
            }
            other => Err(unsupported(&other)),
        }
    }

    fn versions(&self, command: VersionCommand) -> Result<Response> {
        match command {
            VersionCommand::List(filter) => Ok(Response::Versions(
                self.inner
                    .lock()
                    .versions
                    .iter()
                    .filter(|v| filter.accepts(v.kind))
                    .cloned()
                    .collect(),
            )),
            VersionCommand::Refresh => {
                sim::steps(&self.inner, TaskKind::RefreshVersions, None, 3, |inner| {
                    inner.publish(Event::Version(VersionEvent::ListChanged));
                })
            }
            other => Err(unsupported(&other)),
        }
    }

    fn accounts(&self, command: AccountCommand) -> Result<Response> {
        let inner = &self.inner;
        match command {
            AccountCommand::List => Ok(Response::Accounts(inner.lock().accounts.clone())),
            AccountCommand::AddOffline { name } => {
                let name = non_empty(&name, "player name")?;
                let account = {
                    let mut state = inner.lock();
                    let id = AccountId::new(&format!("offline-{}", name.to_lowercase()))
                        .ok_or_else(|| Error::internal("no account id"))?;
                    if state.accounts.iter().any(|a| a.id == id) {
                        return Err(Error::conflict(format!("account {name} exists")));
                    }
                    let account = Account {
                        id,
                        kind: AccountKind::Offline,
                        name: name.into(),
                        uuid: format!("00000000-0000-4000-8000-{:012}", state.accounts.len() + 1),
                        is_default: state.accounts.is_empty(),
                        needs_sign_in: false,
                    };
                    state.accounts.push(account.clone());
                    account
                };
                inner.publish(Event::Account(AccountEvent::ListChanged));
                Ok(Response::Account(account))
            }
            AccountCommand::Remove { id } => {
                {
                    let mut state = inner.lock();
                    let before = state.accounts.len();
                    state.accounts.retain(|a| a.id != id);
                    if state.accounts.len() == before {
                        return Err(Error::not_found(format!("account {id}")));
                    }
                }
                inner.publish(Event::Account(AccountEvent::ListChanged));
                Ok(Response::Done)
            }
            AccountCommand::SetDefault { id } => {
                {
                    let mut state = inner.lock();
                    if !state.accounts.iter().any(|a| a.id == id) {
                        return Err(Error::not_found(format!("account {id}")));
                    }
                    for account in &mut state.accounts {
                        account.is_default = account.id == id;
                    }
                }
                inner.publish(Event::Account(AccountEvent::ListChanged));
                Ok(Response::Done)
            }
            AccountCommand::SignInMicrosoft => sim::sign_in(inner),
            other => Err(unsupported(&other)),
        }
    }

    fn java(&self, command: JavaCommand) -> Result<Response> {
        match command {
            JavaCommand::List => Ok(Response::Javas(self.inner.lock().javas.clone())),
            JavaCommand::Rescan => sim::steps(&self.inner, TaskKind::JavaScan, None, 4, |inner| {
                inner.publish(Event::Java(JavaEvent::ListChanged));
            }),
            other => Err(unsupported(&other)),
        }
    }

    fn settings(&self, command: SettingsCommand) -> Result<Response> {
        let inner = &self.inner;
        match command {
            SettingsCommand::GetGlobal => Ok(Response::Settings(inner.lock().settings.clone())),
            SettingsCommand::SetGlobal(settings) => {
                check_settings(&settings)?;
                inner.lock().settings = settings;
                inner.publish(Event::Settings(SettingsEvent::GlobalChanged));
                Ok(Response::Done)
            }
            SettingsCommand::GetInstance { id } => {
                let mut state = inner.lock();
                find_instance(&mut state, &id)?;
                let overrides = state.overrides.get(&id).cloned().unwrap_or_default();
                let effective = state.settings.with_overrides(&overrides);
                Ok(Response::InstanceSettings {
                    overrides,
                    effective,
                })
            }
            SettingsCommand::SetInstance { id, overrides } => {
                {
                    let mut state = inner.lock();
                    find_instance(&mut state, &id)?;
                    check_settings(&state.settings.with_overrides(&overrides))?;
                    state.overrides.insert(id.clone(), overrides);
                }
                inner.publish(Event::Settings(SettingsEvent::InstanceChanged(id)));
                Ok(Response::Done)
            }
            other => Err(unsupported(&other)),
        }
    }

    fn launch(&self, command: LaunchCommand) -> Result<Response> {
        match command {
            LaunchCommand::Launch(request) => self.start_game(request),
            LaunchCommand::Stop { instance } => {
                let state = self.inner.lock();
                let flag = state.games.get(&instance).ok_or_else(|| {
                    Error::conflict(format!("instance {instance} is not running"))
                })?;
                flag.store(true, Ordering::Relaxed);
                Ok(Response::Done)
            }
            other => Err(unsupported(&other)),
        }
    }

    fn start_game(&self, request: LaunchRequest) -> Result<Response> {
        let (name, account) = {
            let mut state = self.inner.lock();
            let instance = find_instance(&mut state, &request.instance)?;
            if let Some(problem) = &instance.problem {
                return Err(Error::new(ErrorKind::InvalidInput, problem.clone()));
            }
            if instance.state != InstanceState::Stopped {
                return Err(Error::conflict(format!(
                    "instance {} is already running",
                    request.instance
                )));
            }
            let name = instance.name.clone();
            let account = match &request.account {
                Some(id) => state.accounts.iter().find(|a| &a.id == id),
                None => state.accounts.iter().find(|a| a.is_default),
            }
            .cloned()
            .ok_or_else(|| Error::new(ErrorKind::Auth, "no account to launch with"))?;
            (name, account)
        };
        sim::game(&self.inner, request.instance, name, account)
    }

    fn tasks(&self, command: TaskCommand) -> Result<Response> {
        match command {
            TaskCommand::List => Ok(Response::Tasks(
                self.inner
                    .lock()
                    .tasks
                    .values()
                    .map(|(snapshot, _)| snapshot.clone())
                    .collect(),
            )),
            TaskCommand::Cancel { id } => {
                let state = self.inner.lock();
                let (snapshot, flag) = state
                    .tasks
                    .get(&id)
                    .ok_or_else(|| Error::not_found(format!("{id}")))?;
                if !snapshot.info.cancellable {
                    return Err(Error::conflict(format!("{id} cannot be cancelled")));
                }
                flag.store(true, Ordering::Relaxed);
                Ok(Response::Done)
            }
            other => Err(unsupported(&other)),
        }
    }
}

impl Default for MockFacade {
    fn default() -> Self {
        Self::new(MockOptions::default())
    }
}

impl Facade for MockFacade {
    fn call(&self, command: Command) -> Reply {
        Reply::ready(self.handle(command))
    }

    fn subscribe(&self) -> EventStream {
        self.inner.events.subscribe()
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn publish(&self, event: Event) {
        self.events.publish(&event);
    }

    /// Sets an instance's state and announces it.
    fn set_state(&self, id: &InstanceId, state: InstanceState) {
        let summary = {
            let mut guard = self.lock();
            let Some(instance) = guard.instances.iter_mut().find(|i| &i.id == id) else {
                return;
            };
            instance.state = state.clone();
            instance.clone()
        };
        self.publish(Event::Launch(LaunchEvent::StateChanged {
            instance: id.clone(),
            state,
        }));
        self.publish(Event::Instance(InstanceEvent::Changed(summary)));
    }
}

fn find_instance<'a>(state: &'a mut State, id: &InstanceId) -> Result<&'a mut InstanceSummary> {
    state
        .instances
        .iter_mut()
        .find(|i| &i.id == id)
        .ok_or_else(|| Error::not_found(format!("instance {id}")))
}

fn non_empty<'a>(value: &'a str, what: &str) -> Result<&'a str> {
    let value = value.trim();
    if value.is_empty() {
        Err(Error::invalid_input(format!("empty {what}")))
    } else {
        Ok(value)
    }
}

fn check_settings(settings: &Settings) -> Result<()> {
    settings.validate()
}

fn unsupported(command: &impl std::fmt::Debug) -> Error {
    Error::unsupported(format!("vl-mock does not simulate {command:?}"))
}

/// The loader of a mock instance: the requested kind at a fixed synthetic version.
fn mock_loader(request: Option<&vl_core::instance::LoaderRequest>) -> Loader {
    use vl_core::component::ComponentKind;
    let Some(request) = request else {
        return Loader::Vanilla;
    };
    let version = request.version.clone().unwrap_or_else(|| "1.0.0".into());
    match request.kind {
        ComponentKind::Fabric => Loader::Fabric(version),
        ComponentKind::Quilt => Loader::Quilt(version),
        ComponentKind::NeoForge => Loader::NeoForge(version),
        ComponentKind::Forge => Loader::Forge(version),
        _ => Loader::Vanilla,
    }
}
