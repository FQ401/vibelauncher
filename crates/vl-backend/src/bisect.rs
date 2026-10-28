//! Bisection commands, served by `vl-resources`.
//!
//! The session lives in `<instance>/.vibelauncher/bisect.json`. Every command touches files, so
//! each runs on its own thread.

use std::thread;

use vl_core::bisect::BisectCommand;
use vl_core::{Replier, Response};
use vl_resources::Bisect;

use crate::Backend;

const META_DIR: &str = ".vibelauncher";

pub(crate) fn handle(backend: &Backend, command: BisectCommand, replier: Replier) {
    let instance = match &command {
        BisectCommand::Start { instance, .. }
        | BisectCommand::Status { instance }
        | BisectCommand::Report { instance, .. }
        | BisectCommand::Stop { instance } => instance,
        other => return crate::not_yet(replier, "bisect", other),
    };
    let opened = crate::resources::open(backend, instance).and_then(|resources| {
        let meta = backend.instances.dir(instance)?.join(META_DIR);
        Ok(Bisect::new(resources, &meta))
    });
    let bisect = match opened {
        Ok(b) => b,
        Err(e) => return replier.send(Err(e)),
    };
    // A thread that cannot start drops the replier; the caller sees a dropped command.
    let _ = thread::Builder::new()
        .name("bisect".into())
        .spawn(move || replier.send(run(&bisect, command)));
}

fn run(bisect: &Bisect, command: BisectCommand) -> vl_core::Result<Response> {
    match command {
        BisectCommand::Start { suspects, .. } => bisect.start(&suspects).map(Response::Bisect),
        BisectCommand::Status { .. } => bisect.status().map(Response::Bisect),
        BisectCommand::Report { outcome, .. } => bisect.report(outcome).map(Response::Bisect),
        BisectCommand::Stop { .. } => bisect.stop().map(|()| Response::Done),
        other => Err(vl_core::Error::unsupported(format!("{other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use vl_core::bisect::BisectOutcome;
    use vl_core::instance::{InstanceCommand, NewInstance};
    use vl_core::{Command, ErrorKind, Facade};

    use super::*;
    use crate::BackendConfig;

    #[test]
    fn runs_a_session_through_the_facade() {
        let dir = std::env::temp_dir().join(format!("vl-backend-bisect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::new(BackendConfig {
            data_dir: Some(dir.clone()),
        })
        .unwrap();
        let call = |c: BisectCommand| backend.call(Command::Bisect(c)).wait();
        let created = backend
            .call(Command::Instances(InstanceCommand::Create(NewInstance {
                name: "Modded".into(),
                game_version: "1.20.1".into(),
                group: None,
                loader: None,
            })))
            .wait()
            .unwrap();
        let Response::Instance(summary) = created else {
            panic!("unexpected {created:?}");
        };
        let id = summary.id;
        let mods = backend.instances.game_dir(&id).unwrap().join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        for name in ["a.jar", "b.jar", "c.jar", "d.jar"] {
            std::fs::write(mods.join(name), b"not really a jar").unwrap();
        }
        let status = || BisectCommand::Status {
            instance: id.clone(),
        };
        assert_eq!(call(status()).unwrap_err().kind, ErrorKind::NotFound);
        let started = call(BisectCommand::Start {
            instance: id.clone(),
            suspects: Vec::new(),
        })
        .unwrap();
        assert!(matches!(started, Response::Bisect(s) if s.candidates == 4));
        let reported = call(BisectCommand::Report {
            instance: id.clone(),
            outcome: BisectOutcome::Good,
        })
        .unwrap();
        assert!(matches!(reported, Response::Bisect(s) if s.verdict.is_some()));
        let stop = BisectCommand::Stop {
            instance: id.clone(),
        };
        assert_eq!(call(stop).unwrap(), Response::Done);
        assert_eq!(call(status()).unwrap_err().kind, ErrorKind::NotFound);
        let missing = BisectCommand::Status {
            instance: vl_core::InstanceId::new("nope").unwrap(),
        };
        assert_eq!(call(missing).unwrap_err().kind, ErrorKind::NotFound);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }
}
