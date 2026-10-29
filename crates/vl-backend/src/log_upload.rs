//! `ConsoleCommand::PreviewUpload` and `ConsoleCommand::Upload`: sharing a game log on mclo.gs.
//!
//! Both read the same file and prepare it the same way (`vl_logs::mclogs::prepare`: masked, cut
//! to the service's limits); only `Upload` sends it. The file is the newest run log of the
//! instance unless the command names one. Both run on a thread of their own.

use std::path::{Component, Path, PathBuf};

use vl_core::console::{LogUploadPreview, UploadedLog};
use vl_core::redact::Redactor;
use vl_core::{Error, InstanceId, Replier, Response};
use vl_logs::mclogs::{self, Limits, Prepared};
use vl_net::{Client, Control, NetConfig};

use crate::Backend;
use crate::crash::{RUN_LOG_PREFIX, newest_in};
use crate::updates::spawn_reply;

/// The paste service the backend uploads to.
pub(crate) struct Uploads {
    client: Client,
    api: String,
    limits: Limits,
}

impl Uploads {
    pub(crate) fn new() -> vl_core::Result<Self> {
        let mut config = NetConfig::new(env!("CARGO_PKG_VERSION"));
        // A retried upload could publish the log twice.
        config.retry.attempts = 1;
        Ok(Self::with_api(Client::new(config)?, mclogs::API))
    }

    pub(crate) fn with_api(client: Client, api: &str) -> Self {
        Self {
            client,
            api: api.to_owned(),
            limits: mclogs::LIMITS,
        }
    }
}

pub(crate) fn preview(
    backend: &Backend,
    instance: &InstanceId,
    file: Option<PathBuf>,
    replier: Replier,
) {
    let job = match job(backend, instance, file) {
        Ok(job) => job,
        Err(e) => return replier.send(Err(e)),
    };
    spawn_reply("log-upload-preview", replier, move || {
        let prepared = mclogs::prepare(&job.file, &job.redactor, job.limits)?;
        Ok(Response::LogUploadPreview(describe(&job, &prepared)))
    });
}

pub(crate) fn upload(
    backend: &Backend,
    instance: &InstanceId,
    file: Option<PathBuf>,
    replier: Replier,
) {
    let job = match job(backend, instance, file) {
        Ok(job) => job,
        Err(e) => return replier.send(Err(e)),
    };
    let client = backend.uploads.client.clone();
    let api = backend.uploads.api.clone();
    spawn_reply("log-upload", replier, move || {
        let prepared = mclogs::prepare(&job.file, &job.redactor, job.limits)?;
        let uploaded = mclogs::upload(&client, &api, &prepared.content, &Control::default())?;
        Ok(Response::LogUploaded(UploadedLog {
            url: uploaded.url,
            raw_url: uploaded.raw_url,
            preview: describe(&job, &prepared),
        }))
    });
}

struct Job {
    file: PathBuf,
    redactor: Redactor,
    limits: Limits,
}

fn job(backend: &Backend, instance: &InstanceId, file: Option<PathBuf>) -> vl_core::Result<Job> {
    let dir = backend.instances.dir(instance)?;
    if !dir.is_dir() {
        return Err(Error::not_found(format!("instance {instance}")));
    }
    let game = backend.instances.game_dir(instance)?;
    let file = match file {
        Some(path) => named(&game, &path)?,
        None => newest_run_log(&dir, &game)
            .ok_or_else(|| Error::not_found(format!("instance {instance} has no game log yet")))?,
    };
    Ok(Job {
        file,
        redactor: crate::logging::redactor(backend.settings()),
        limits: backend.uploads.limits,
    })
}

/// An absolute path as it is; a relative one inside the game folder.
fn named(game: &Path, path: &Path) -> vl_core::Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    if path.as_os_str().is_empty() || !path.components().all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(Error::invalid_input(format!(
            "{} is not a file inside the instance's game folder",
            path.display()
        )));
    }
    Ok(game.join(path))
}

/// The output log of the newest run, else the game's own `latest.log`.
fn newest_run_log(instance_dir: &Path, game: &Path) -> Option<PathBuf> {
    newest_in(&instance_dir.join("logs"), |n| {
        n.starts_with(RUN_LOG_PREFIX) && n.ends_with(".log")
    })
    .or_else(|| {
        let latest = game.join("logs").join("latest.log");
        latest.is_file().then_some(latest)
    })
}

fn describe(job: &Job, prepared: &Prepared) -> LogUploadPreview {
    LogUploadPreview {
        file: job.file.clone(),
        service: mclogs::SERVICE.to_owned(),
        file_bytes: prepared.file_bytes,
        file_lines: prepared.file_lines,
        bytes: prepared.content.len() as u64,
        lines: prepared.lines,
        omitted_lines: prepared.omitted_lines,
        home_masked: job.redactor.masks_home(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    use vl_core::console::ConsoleCommand;
    use vl_core::{Command, ErrorKind, Facade};

    use super::*;
    use crate::BackendConfig;

    fn backend(tag: &str) -> (Backend, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("vl-backend-upload-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::new(BackendConfig {
            data_dir: Some(dir.clone()),
        })
        .unwrap();
        (backend, dir)
    }

    /// An instance folder with a run log and the game's own log.
    fn instance(backend: &Backend) -> InstanceId {
        let id = InstanceId::new("shared").unwrap();
        let dir = backend.instances.dir(&id).unwrap();
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        std::fs::write(
            dir.join("logs").join("vl-game-20260101-000000.log"),
            "[main/INFO]: starting\n[main/INFO]: --accessToken synthetic-access-value\n",
        )
        .unwrap();
        let game = backend.instances.game_dir(&id).unwrap();
        std::fs::create_dir_all(game.join("logs")).unwrap();
        std::fs::write(game.join("logs").join("latest.log"), "latest\n").unwrap();
        id
    }

    fn call(backend: &Backend, command: ConsoleCommand) -> vl_core::Result<Response> {
        backend.call(Command::Console(command)).wait()
    }

    #[test]
    fn the_preview_names_the_newest_run_log_and_sends_nothing() {
        let (backend, dir) = backend("preview");
        let id = instance(&backend);
        let Response::LogUploadPreview(p) = call(
            &backend,
            ConsoleCommand::PreviewUpload {
                instance: id.clone(),
                file: None,
            },
        )
        .unwrap() else {
            panic!("not a preview")
        };
        assert!(p.file.ends_with("vl-game-20260101-000000.log"), "{p:?}");
        assert_eq!((p.file_lines, p.lines, p.omitted_lines), (2, 2, 0));
        assert_eq!(p.service, "mclo.gs");
        assert!(!p.truncated());
        assert!(p.bytes < p.file_bytes, "the token is masked: {p:?}");

        let Response::LogUploadPreview(p) = call(
            &backend,
            ConsoleCommand::PreviewUpload {
                instance: id.clone(),
                file: Some(PathBuf::from("logs/latest.log")),
            },
        )
        .unwrap() else {
            panic!("not a preview")
        };
        assert!(p.file.ends_with("latest.log"));

        let err = call(
            &backend,
            ConsoleCommand::PreviewUpload {
                instance: id,
                file: Some(PathBuf::from("../instance.json")),
            },
        )
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidInput);
        let err = call(
            &backend,
            ConsoleCommand::PreviewUpload {
                instance: InstanceId::new("missing").unwrap(),
                file: None,
            },
        )
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::NotFound);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_upload_sends_the_masked_log_to_the_service() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let api = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().unwrap();
                }
                if line == "\r\n" {
                    break;
                }
            }
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).unwrap();
            tx.send(String::from_utf8(body).unwrap()).unwrap();
            let answer = r#"{"success":true,"id":"Mock1","url":"https://mclo.gs/Mock1"}"#;
            let mut stream = stream;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{answer}",
                answer.len()
            )
            .unwrap();
        });

        let (mut backend, dir) = backend("upload");
        let mut config = NetConfig::new("0.0.0");
        config.proxy = vl_net::ProxyConfig::None;
        config.retry.attempts = 1;
        backend.uploads = Uploads::with_api(Client::new(config).unwrap(), &api);
        let id = instance(&backend);
        let Response::LogUploaded(uploaded) = call(
            &backend,
            ConsoleCommand::Upload {
                instance: id,
                file: None,
            },
        )
        .unwrap() else {
            panic!("not uploaded")
        };
        assert_eq!(uploaded.url, "https://mclo.gs/Mock1");
        assert_eq!(uploaded.raw_url, None);
        let body = rx.recv().unwrap();
        assert!(body.contains("--accessToken <redacted>"), "{body}");
        assert!(!body.contains("synthetic-access-value"), "{body}");
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }
}
