//! Account commands, served by `vl-auth`.
//!
//! Microsoft sign-in and refresh run as tasks on their own threads and report through the event
//! bus; the account list is a small JSON file in the data directory (public data only) and the
//! tokens live in a [`TokenStore`].

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use vl_auth::{
    AccountBook, AuthError, Authenticator, MemoryTokenStore, MsaConfig, Session, TokenStore,
    UreqHttp,
};
use vl_core::account::{Account, AccountCommand, AccountEvent, AccountKind, DeviceCodePrompt};
use vl_core::task::{TaskEvent, TaskInfo, TaskKind, TaskOutcome};
use vl_core::{AccountId, Error, Event, EventBus, Replier, Response, TaskId};
use vl_net::{CancelToken, NetConfig};
use vl_store::Store;

use crate::Backend;

pub(crate) fn handle(backend: &Backend, command: AccountCommand, replier: Replier) {
    backend.accounts.handle(command, replier);
}

/// The account domain: list, sign-in, refresh, removal.
pub(crate) struct Service {
    shared: Arc<Shared>,
}

struct Shared {
    auth: Authenticator,
    tokens: Arc<dyn TokenStore>,
    book: Mutex<AccountBook>,
    events: Arc<EventBus>,
    next_task: AtomicU64,
}

impl Service {
    /// The real service: the network stack, the account file in the data directory and tokens in
    /// memory (the OS keyring replaces them).
    pub(crate) fn new(store: &Store, events: Arc<EventBus>) -> vl_core::Result<Self> {
        let http = UreqHttp::new(&NetConfig::new(env!("CARGO_PKG_VERSION")))?;
        let auth = Authenticator::new(Arc::new(http), MsaConfig::from_env());
        let book = AccountBook::load(&store.data_dir().join("accounts.json"))?;
        Self::with_parts(auth, Arc::new(MemoryTokenStore::new()), book, events)
    }

    /// A service on given parts. A Microsoft account without stored tokens must sign in again.
    pub(crate) fn with_parts(
        auth: Authenticator,
        tokens: Arc<dyn TokenStore>,
        mut book: AccountBook,
        events: Arc<EventBus>,
    ) -> vl_core::Result<Self> {
        let missing: Vec<AccountId> = book
            .accounts()
            .iter()
            .filter(|a| a.kind == AccountKind::Microsoft && !a.needs_sign_in)
            .filter(|a| !matches!(tokens.get(a.id.as_str()), Ok(Some(_))))
            .map(|a| a.id.clone())
            .collect();
        for id in &missing {
            book.set_needs_sign_in(id, true);
        }
        if !missing.is_empty() {
            book.save()?;
        }
        Ok(Self {
            shared: Arc::new(Shared {
                auth,
                tokens,
                book: Mutex::new(book),
                events,
                next_task: AtomicU64::new(1),
            }),
        })
    }

    pub(crate) fn handle(&self, command: AccountCommand, replier: Replier) {
        let shared = &self.shared;
        let result = match command {
            AccountCommand::List => Ok(Response::Accounts(shared.book().accounts().to_vec())),
            AccountCommand::SignInMicrosoft => shared.sign_in(),
            AccountCommand::Refresh { id } => shared.refresh(&id),
            AccountCommand::Remove { id } => shared.remove(&id),
            AccountCommand::SetDefault { id } => shared.set_default(&id),
            other => Err(Error::unsupported(format!(
                "vl-backend does not handle accounts {other:?} yet"
            ))),
        };
        replier.send(result);
    }
}

impl Shared {
    fn book(&self) -> MutexGuard<'_, AccountBook> {
        self.book.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn publish(&self, event: Event) {
        self.events.publish(&event);
    }

    fn list_changed(&self) {
        self.publish(Event::Account(AccountEvent::ListChanged));
    }

    fn sign_in(self: &Arc<Self>) -> vl_core::Result<Response> {
        self.spawn_task(None, |shared, task| {
            let session = shared.auth.device_code_sign_in(
                &mut |code| {
                    shared.publish(Event::Account(AccountEvent::DeviceCode(DeviceCodePrompt {
                        task,
                        user_code: code.user_code.clone(),
                        verification_uri: code.verification_uri.clone(),
                        expires_in_secs: code.expires_in,
                    })));
                },
                &CancelToken::new(),
            )?;
            shared.store_session(&session)
        })
    }

    fn refresh(self: &Arc<Self>, id: &AccountId) -> vl_core::Result<Response> {
        let account = self
            .book()
            .get(id)
            .cloned()
            .ok_or_else(|| Error::not_found(format!("account {id}")))?;
        if account.kind != AccountKind::Microsoft {
            return Err(Error::invalid_input(format!(
                "account {id} has no sign-in to refresh"
            )));
        }
        self.spawn_task(Some(account.name), {
            let id = id.clone();
            move |shared, _| {
                let result = match shared.tokens.get(id.as_str())? {
                    Some(t) => shared.auth.refresh(&t.refresh_token),
                    None => Err(AuthError::SignInRequired),
                };
                match result {
                    Ok(session) => shared.store_session(&session),
                    Err(AuthError::SignInRequired) => {
                        shared.mark_sign_in_required(&id)?;
                        Err(AuthError::SignInRequired)
                    }
                    Err(e) => Err(e),
                }
            }
        })
    }

    fn mark_sign_in_required(&self, id: &AccountId) -> Result<(), AuthError> {
        let found = {
            let mut book = self.book();
            let found = book.set_needs_sign_in(id, true);
            if found {
                book.save()?;
            }
            found
        };
        if found {
            self.list_changed();
        }
        Ok(())
    }

    fn remove(&self, id: &AccountId) -> vl_core::Result<Response> {
        {
            let mut book = self.book();
            if book.remove(id).is_none() {
                return Err(Error::not_found(format!("account {id}")));
            }
            book.save()?;
        }
        self.tokens.remove(id.as_str())?;
        self.list_changed();
        Ok(Response::Done)
    }

    fn set_default(&self, id: &AccountId) -> vl_core::Result<Response> {
        {
            let mut book = self.book();
            if !book.set_default(id) {
                return Err(Error::not_found(format!("account {id}")));
            }
            book.save()?;
        }
        self.list_changed();
        Ok(Response::Done)
    }

    /// Keeps a completed sign-in: tokens first, then the public account entry.
    fn store_session(&self, session: &Session) -> Result<(), AuthError> {
        let id = AccountId::new(&session.profile.uuid)
            .ok_or_else(|| AuthError::Protocol("profile has no id".into()))?;
        self.tokens.put(id.as_str(), session.stored_tokens())?;
        {
            let mut book = self.book();
            book.upsert(Account {
                id,
                kind: AccountKind::Microsoft,
                name: session.profile.name.clone(),
                uuid: session.profile.uuid.clone(),
                is_default: false,
                needs_sign_in: false,
            });
            book.save()?;
        }
        self.list_changed();
        Ok(())
    }

    /// Runs `work` on its own thread as a sign-in task and answers with the task id.
    fn spawn_task(
        self: &Arc<Self>,
        subject: Option<String>,
        work: impl FnOnce(&Arc<Self>, TaskId) -> Result<(), AuthError> + Send + 'static,
    ) -> vl_core::Result<Response> {
        let id = TaskId(self.next_task.fetch_add(1, Ordering::Relaxed));
        self.publish(Event::Task(TaskEvent::Started(TaskInfo {
            id,
            parent: None,
            kind: TaskKind::SignIn,
            subject,
            cancellable: false,
        })));
        let shared = Arc::clone(self);
        std::thread::Builder::new()
            .name("vl-sign-in".into())
            .spawn(move || {
                let outcome = match work(&shared, id) {
                    Ok(()) => TaskOutcome::Succeeded,
                    Err(AuthError::Cancelled) => TaskOutcome::Cancelled,
                    Err(e) => TaskOutcome::Failed(e.into()),
                };
                shared.publish(Event::Task(TaskEvent::Finished { id, outcome }));
            })
            .map_err(|e| {
                Error::internal(format!("cannot start the sign-in thread: {}", e.kind()))
            })?;
        Ok(Response::Task(id))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use vl_auth::{AuthHttp, AuthRequest, AuthResponse, Clock, Secret, StoredTokens};
    use vl_core::ErrorKind;
    use vl_core::account::AccountCommand;
    use vl_core::reply_channel;
    use vl_net::NetError;

    use super::*;

    /// Answers the whole chain with synthetic data.
    struct Chain {
        login_status: u16,
        token_error: bool,
    }

    impl AuthHttp for Chain {
        fn send(&self, r: &AuthRequest) -> Result<AuthResponse, NetError> {
            let u = r.url.as_str();
            let (status, body) = if u.ends_with("/devicecode") {
                (
                    200,
                    r#"{"device_code":"d","user_code":"WXYZ1234","verification_uri":"https://example.invalid/link","expires_in":900,"interval":5}"#,
                )
            } else if u.ends_with("/token") && self.token_error {
                (400, r#"{"error":"invalid_grant"}"#)
            } else if u.ends_with("/token") {
                (200, r#"{"access_token":"a","refresh_token":"r"}"#)
            } else if u.contains("user.auth.xboxlive.com") || u.contains("xsts.auth") {
                (
                    200,
                    r#"{"Token":"t","DisplayClaims":{"xui":[{"uhs":"h"}]}}"#,
                )
            } else if u.ends_with("login_with_xbox") {
                (
                    self.login_status,
                    r#"{"access_token":"mc","expires_in":86400}"#,
                )
            } else if u.ends_with("mcstore") {
                (200, r#"{"items":[{"name":"game_minecraft"}]}"#)
            } else if u.ends_with("/profile") {
                (
                    200,
                    r#"{"id":"0123456789abcdef0123456789abcdef","name":"TestPlayer","skins":[]}"#,
                )
            } else {
                (404, "")
            };
            Ok(AuthResponse {
                status,
                body: body.as_bytes().to_vec(),
            })
        }
    }

    struct NoWait;

    impl Clock for NoWait {
        fn now_unix(&self) -> u64 {
            0
        }
        fn sleep(&self, _: Duration, _: &CancelToken) -> Result<(), AuthError> {
            Ok(())
        }
    }

    struct Rig {
        service: Service,
        events: Arc<EventBus>,
        tokens: Arc<MemoryTokenStore>,
    }

    fn rig(login_status: u16, token_error: bool, name: &str) -> Rig {
        let dir =
            std::env::temp_dir().join(format!("vl-backend-acc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let events = Arc::new(EventBus::default());
        let tokens = Arc::new(MemoryTokenStore::new());
        let auth = Authenticator::with_clock(
            Arc::new(Chain {
                login_status,
                token_error,
            }),
            Arc::new(NoWait),
            MsaConfig::with_client_id("test-client"),
        );
        let book = AccountBook::load(&dir.join("accounts.json")).unwrap();
        let service = Service::with_parts(auth, tokens.clone(), book, events.clone()).unwrap();
        Rig {
            service,
            events,
            tokens,
        }
    }

    impl Rig {
        fn call(&self, command: AccountCommand) -> vl_core::Result<Response> {
            let (replier, reply) = reply_channel();
            self.service.handle(command, replier);
            reply.wait()
        }

        fn task(&self, command: AccountCommand) -> TaskId {
            match self.call(command).unwrap() {
                Response::Task(id) => id,
                other => panic!("{other:?}"),
            }
        }

        fn accounts(&self) -> Vec<Account> {
            match self.call(AccountCommand::List).unwrap() {
                Response::Accounts(a) => a,
                other => panic!("{other:?}"),
            }
        }

        /// Runs a task command and waits for its outcome.
        fn run(&self, command: AccountCommand) -> TaskOutcome {
            let stream = self.events.subscribe();
            let task = self.task(command);
            finished(&stream, task)
        }
    }

    fn finished(stream: &vl_core::EventStream, task: TaskId) -> TaskOutcome {
        loop {
            match stream.next_timeout(Duration::from_secs(10)) {
                Some(Event::Task(TaskEvent::Finished { id, outcome })) if id == task => {
                    return outcome;
                }
                Some(_) => {}
                None => panic!("the task did not finish"),
            }
        }
    }

    #[test]
    fn signs_in_lists_and_removes() {
        let rig = rig(200, false, "ok");
        assert_eq!(
            rig.run(AccountCommand::SignInMicrosoft),
            TaskOutcome::Succeeded
        );
        let list = rig.accounts();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "TestPlayer");
        assert_eq!(list[0].uuid, "01234567-89ab-cdef-0123-456789abcdef");
        assert!(list[0].is_default && !list[0].needs_sign_in);
        assert!(rig.tokens.get(list[0].id.as_str()).unwrap().is_some());

        let id = list[0].id.clone();
        rig.call(AccountCommand::Remove { id: id.clone() }).unwrap();
        assert!(rig.accounts().is_empty());
        assert!(rig.tokens.get(id.as_str()).unwrap().is_none());
        let err = rig.call(AccountCommand::Remove { id }).unwrap_err();
        assert_eq!(err.kind, ErrorKind::NotFound);
    }

    #[test]
    fn announces_the_device_code() {
        let rig = rig(200, false, "prompt");
        let stream = rig.events.subscribe();
        let task = rig.task(AccountCommand::SignInMicrosoft);
        let mut prompt = None;
        loop {
            match stream.next_timeout(Duration::from_secs(10)) {
                Some(Event::Account(AccountEvent::DeviceCode(p))) => prompt = Some(p),
                Some(Event::Task(TaskEvent::Finished { id, .. })) if id == task => break,
                Some(_) => {}
                None => panic!("the task did not finish"),
            }
        }
        let prompt = prompt.unwrap();
        assert_eq!(prompt.task, task);
        assert_eq!(prompt.user_code, "WXYZ1234");
        assert_eq!(prompt.verification_uri, "https://example.invalid/link");
    }

    #[test]
    fn unapproved_application_is_an_auth_error_not_a_panic() {
        let rig = rig(403, false, "403");
        match rig.run(AccountCommand::SignInMicrosoft) {
            TaskOutcome::Failed(e) => {
                assert_eq!(e.kind, ErrorKind::Auth);
                assert!(e.message.contains("app not permitted yet"), "{}", e.message);
            }
            other => panic!("{other:?}"),
        }
        assert!(rig.accounts().is_empty());
    }

    #[test]
    fn refresh_renews_or_marks_the_account() {
        let rig = rig(200, false, "refresh");
        rig.run(AccountCommand::SignInMicrosoft);
        let id = rig.accounts()[0].id.clone();
        assert_eq!(
            rig.run(AccountCommand::Refresh { id: id.clone() }),
            TaskOutcome::Succeeded
        );

        // Tokens lost (a new process with the in-memory store): the user must sign in again.
        rig.tokens.remove(id.as_str()).unwrap();
        let outcome = rig.run(AccountCommand::Refresh { id });
        assert!(matches!(outcome, TaskOutcome::Failed(e) if e.kind == ErrorKind::Auth));
        assert!(rig.accounts()[0].needs_sign_in);

        let missing = AccountId::new("missing").unwrap();
        let err = rig
            .call(AccountCommand::Refresh { id: missing })
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::NotFound);
    }

    #[test]
    fn a_rejected_refresh_token_marks_the_account() {
        let rig = rig(200, true, "rejected");
        let id = AccountId::new("seeded").unwrap();
        rig.service.shared.book().upsert(Account {
            id: id.clone(),
            kind: AccountKind::Microsoft,
            name: "Seeded".into(),
            uuid: "00000000-0000-4000-8000-000000000001".into(),
            is_default: false,
            needs_sign_in: false,
        });
        rig.tokens
            .put(
                "seeded",
                StoredTokens {
                    refresh_token: Secret::new("r"),
                    minecraft_token: Secret::new("m"),
                    minecraft_expires_at: 0,
                },
            )
            .unwrap();
        let outcome = rig.run(AccountCommand::Refresh { id });
        assert!(matches!(outcome, TaskOutcome::Failed(e) if e.kind == ErrorKind::Auth));
        assert!(rig.accounts()[0].needs_sign_in);
    }

    #[test]
    fn default_can_change_and_offline_is_not_handled_yet() {
        let rig = rig(200, false, "default");
        rig.run(AccountCommand::SignInMicrosoft);
        let id = rig.accounts()[0].id.clone();
        rig.call(AccountCommand::SetDefault { id }).unwrap();
        let err = rig
            .call(AccountCommand::AddOffline { name: "x".into() })
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Unsupported);
    }
}
