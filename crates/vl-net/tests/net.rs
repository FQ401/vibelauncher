//! Behaviour tests against a local HTTP server (no real network).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use vl_net::{
    CacheStatus, CancelToken, Checksum, Client, Control, Download, GetOptions, NetConfig, NetError,
    ProxyConfig, RetryPolicy, sha256_hex,
};

// ---------------------------------------------------------------- test server

struct Req {
    /// Zero-based index of the request on this server.
    n: usize,
    target: String,
    headers: Vec<(String, String)>,
}

impl Req {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

enum Reply {
    Ok {
        status: u16,
        headers: Vec<(&'static str, String)>,
        body: Vec<u8>,
    },
    /// Declares `declared` bytes, sends `body`, then drops the connection.
    Truncated { body: Vec<u8>, declared: usize },
    /// Sends `body` without a length and closes.
    NoLength { body: Vec<u8> },
    /// Sends the head and `first`, then waits until released (or `hold`), then the rest.
    Slow {
        first: Vec<u8>,
        total: usize,
        hold: Duration,
    },
    /// Reads the request and never answers (until `hold` has passed).
    Silent { hold: Duration },
}

fn ok(body: &[u8]) -> Reply {
    Reply::Ok {
        status: 200,
        headers: Vec::new(),
        body: body.to_vec(),
    }
}

fn status(code: u16) -> Reply {
    Reply::Ok {
        status: code,
        headers: Vec::new(),
        body: Vec::new(),
    }
}

struct Server {
    addr: String,
    count: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
}

impl Server {
    fn start(handler: impl Fn(&Req) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let count = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let handler = Arc::new(handler);
        let (c, s) = (count.clone(), stop.clone());
        thread::spawn(move || {
            for stream in listener.incoming() {
                if s.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(stream) = stream else { continue };
                let n = c.fetch_add(1, Ordering::SeqCst);
                let handler = handler.clone();
                thread::spawn(move || serve(stream, n, &*handler));
            }
        });
        Self { addr, count, stop }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    fn requests(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(&self.addr);
    }
}

fn serve(mut stream: TcpStream, n: usize, handler: &(dyn Fn(&Req) -> Reply + Send + Sync)) {
    let mut raw = Vec::new();
    let mut byte = [0u8; 1];
    while !raw.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => raw.push(byte[0]),
            _ => return,
        }
    }
    let text = String::from_utf8_lossy(&raw).into_owned();
    let mut lines = text.split("\r\n");
    let target = lines
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .unwrap_or("")
        .to_string();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let req = Req { n, target, headers };
    match handler(&req) {
        Reply::Ok {
            status,
            headers,
            body,
        } => {
            let mut head = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            for (k, v) in headers {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
            head.push_str("\r\n");
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
        Reply::Truncated { body, declared } => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
        Reply::NoLength { body } => {
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n");
            let _ = stream.write_all(&body);
        }
        Reply::Slow { first, total, hold } => {
            let head =
                format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n");
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&first);
            let _ = stream.flush();
            thread::sleep(hold);
        }
        Reply::Silent { hold } => thread::sleep(hold),
    }
    let _ = stream.shutdown(Shutdown::Both);
}

// ---------------------------------------------------------------- helpers

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vl-net-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn config(cache: Option<&Path>) -> NetConfig {
    let mut c = NetConfig::new("9.9.9");
    c.retry = RetryPolicy {
        attempts: 3,
        base_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(20),
    };
    c.proxy = ProxyConfig::None;
    c.cache_dir = cache.map(Path::to_path_buf);
    c
}

fn client(cfg: NetConfig) -> Client {
    Client::new(cfg).unwrap()
}

fn sha256(data: &[u8]) -> Checksum {
    Checksum::sha256(&sha256_hex(data)).unwrap()
}

fn leftovers(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

// ---------------------------------------------------------------- metadata and cache

#[test]
fn revalidates_with_etag_and_serves_304_from_the_cache() {
    let server = Server::start(|r| {
        if r.header("if-none-match") == Some("\"v1\"") {
            status(304)
        } else {
            Reply::Ok {
                status: 200,
                headers: vec![("ETag", "\"v1\"".into())],
                body: br#"{"a":1}"#.to_vec(),
            }
        }
    });
    let dir = temp_dir("etag");
    let c = client(config(Some(&dir)));
    let url = server.url("/manifest.json");
    let ctl = Control::default();

    let first = c.get_bytes(&url, &GetOptions::default(), &ctl).unwrap();
    assert_eq!(first.status, CacheStatus::Network);
    let second = c.get_bytes(&url, &GetOptions::default(), &ctl).unwrap();
    assert_eq!(second.status, CacheStatus::Revalidated);
    assert_eq!(second.bytes, first.bytes);
    assert_eq!(server.requests(), 2);
}

#[test]
fn last_modified_is_sent_back_and_a_young_copy_skips_the_network() {
    let server = Server::start(|r| {
        if r.header("if-modified-since") == Some("Mon, 01 Jan 2024 00:00:00 GMT") {
            status(304)
        } else {
            Reply::Ok {
                status: 200,
                headers: vec![("Last-Modified", "Mon, 01 Jan 2024 00:00:00 GMT".into())],
                body: b"{}".to_vec(),
            }
        }
    });
    let dir = temp_dir("lm");
    let c = client(config(Some(&dir)));
    let url = server.url("/x");
    let ctl = Control::default();
    c.get_bytes(&url, &GetOptions::default(), &ctl).unwrap();
    let again = c.get_bytes(&url, &GetOptions::default(), &ctl).unwrap();
    assert_eq!(again.status, CacheStatus::Revalidated);
    let young = GetOptions {
        max_age: Some(Duration::from_secs(60)),
        ..GetOptions::default()
    };
    let fresh = c.get_bytes(&url, &young, &ctl).unwrap();
    assert_eq!(fresh.status, CacheStatus::Fresh);
    assert_eq!(server.requests(), 2);
}

#[test]
fn offline_reads_from_the_cache_and_fails_without_it() {
    let server = Server::start(|_| ok(b"[1,2]"));
    let dir = temp_dir("offline");
    let online = client(config(Some(&dir)));
    let ctl = Control::default();
    online
        .get_bytes(&server.url("/a"), &GetOptions::default(), &ctl)
        .unwrap();
    let before = server.requests();

    let mut cfg = config(Some(&dir));
    cfg.offline = true;
    let offline = client(cfg);
    let hit = offline
        .get_bytes(&server.url("/a"), &GetOptions::default(), &ctl)
        .unwrap();
    assert_eq!(
        (hit.status, hit.bytes.as_slice()),
        (CacheStatus::Stale, &b"[1,2]"[..])
    );
    let miss = offline.get_bytes(&server.url("/b"), &GetOptions::default(), &ctl);
    assert!(matches!(miss, Err(NetError::Offline(_))));
    assert_eq!(server.requests(), before);
}

#[test]
fn falls_back_to_the_old_copy_when_the_server_fails() {
    let server = Server::start(|r| if r.n == 0 { ok(b"{}") } else { status(503) });
    let dir = temp_dir("stale");
    let c = client(config(Some(&dir)));
    let ctl = Control::default();
    let url = server.url("/m");
    c.get_bytes(&url, &GetOptions::default(), &ctl).unwrap();
    let again = c.get_bytes(&url, &GetOptions::default(), &ctl).unwrap();
    assert_eq!(again.status, CacheStatus::Stale);
    assert_eq!(
        server.requests(),
        1 + 3,
        "three tries after the first fetch"
    );
}

#[test]
fn json_is_decoded_and_bad_json_is_a_decode_error() {
    let server = Server::start(|r| {
        if r.target == "/good" {
            ok(br#"{"id":"1.21"}"#)
        } else {
            ok(b"<html>")
        }
    });
    #[derive(serde::Deserialize)]
    struct V {
        id: String,
    }
    let c = client(config(None));
    let ctl = Control::default();
    let (v, _): (V, _) = c
        .get_json(&server.url("/good"), &GetOptions::default(), &ctl)
        .unwrap();
    assert_eq!(v.id, "1.21");
    let bad: Result<(V, _), _> = c.get_json(&server.url("/bad"), &GetOptions::default(), &ctl);
    assert!(matches!(bad, Err(NetError::Decode(_))));
}

#[test]
fn metadata_over_the_cap_is_rejected() {
    let server = Server::start(|r| {
        if r.target == "/len" {
            ok(&[b'x'; 5000])
        } else {
            Reply::NoLength {
                body: vec![b'x'; 5000],
            }
        }
    });
    let c = client(config(None));
    let ctl = Control::default();
    let small = GetOptions {
        max_bytes: Some(1000),
        ..GetOptions::default()
    };
    for path in ["/len", "/nolen"] {
        let r = c.get_bytes(&server.url(path), &small, &ctl);
        assert!(
            matches!(r, Err(NetError::TooLarge { limit: 1000, .. })),
            "{path}: {r:?}"
        );
    }
    assert_eq!(server.requests(), 2, "an oversized response is not retried");
}

#[test]
fn sends_the_launcher_user_agent() {
    let seen = Arc::new(Mutex::new(String::new()));
    let s = seen.clone();
    let server = Server::start(move |r| {
        *s.lock().unwrap() = r.header("user-agent").unwrap_or("").to_string();
        ok(b"{}")
    });
    client(config(None))
        .get_bytes(
            &server.url("/"),
            &GetOptions::default(),
            &Control::default(),
        )
        .unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        "VibeLauncher/9.9.9 (+https://github.com/FQ401/vibelauncher)"
    );
}

#[test]
fn rejects_non_http_urls() {
    let c = client(config(None));
    for url in ["file:///etc/passwd", "ftp://h/x", "/relative"] {
        let r = c.get_bytes(url, &GetOptions::default(), &Control::default());
        assert!(matches!(r, Err(NetError::InvalidUrl(_))), "{url}");
    }
}

#[test]
fn client_errors_are_not_retried_and_hide_the_query() {
    let server = Server::start(|_| status(404));
    let c = client(config(None));
    let err = c
        .get_bytes(
            &server.url("/gone?token=secret"),
            &GetOptions::default(),
            &Control::default(),
        )
        .unwrap_err();
    assert!(matches!(err, NetError::Status { status: 404, .. }));
    assert!(!err.to_string().contains("secret"));
    assert_eq!(server.requests(), 1);
}

#[test]
fn a_silent_server_times_out_and_is_retried() {
    let server = Server::start(|_| Reply::Silent {
        hold: Duration::from_secs(3),
    });
    let mut cfg = config(None);
    cfg.read_timeout = Duration::from_millis(150);
    cfg.retry.attempts = 2;
    let err = client(cfg)
        .get_bytes(
            &server.url("/"),
            &GetOptions::default(),
            &Control::default(),
        )
        .unwrap_err();
    assert!(matches!(err, NetError::Timeout(_)), "{err:?}");
    assert_eq!(server.requests(), 2);
}

#[test]
fn requests_go_through_an_http_proxy() {
    // The fake proxy only records what it is asked for; the tunnel it never builds fails the call.
    let seen = Arc::new(Mutex::new(String::new()));
    let s = seen.clone();
    let proxy = Server::start(move |r| {
        *s.lock().unwrap() = r.target.clone();
        status(502)
    });
    let mut cfg = config(None);
    cfg.proxy = ProxyConfig::Url(format!("http://{}", proxy.addr));
    cfg.retry.attempts = 1;
    let r = client(cfg).get_bytes(
        "http://example.invalid/meta.json",
        &GetOptions::default(),
        &Control::default(),
    );
    assert!(r.is_err());
    assert_eq!(*seen.lock().unwrap(), "example.invalid:80");
}

#[test]
fn proxy_urls_are_validated() {
    for url in [
        "socks5://127.0.0.1:1080",
        "socks5h://u:p@h:1",
        "http://h:3128",
    ] {
        let mut cfg = config(None);
        cfg.proxy = ProxyConfig::Url(url.into());
        assert!(Client::new(cfg).is_ok(), "{url}");
    }
    let mut cfg = config(None);
    cfg.proxy = ProxyConfig::Url("not a proxy".into());
    assert!(Client::new(cfg).is_err());
}

// ---------------------------------------------------------------- downloads

#[test]
fn downloads_and_verifies_sha1_sha256_and_size() {
    let body = b"hello world".to_vec();
    let server = Server::start({
        let body = body.clone();
        move |_| ok(&body)
    });
    let dir = temp_dir("dl-ok");
    let c = client(config(None));
    let ctl = Control::default();

    let sha1 = Checksum::sha1("2aae6c35c94fcfb415dbe95f408b9ce91ee846ed").unwrap();
    let a = Download::new(server.url("/a"), dir.join("sub/a.bin"))
        .checksum(sha1)
        .size(11);
    let out = c.download(&a, &ctl).unwrap();
    assert_eq!((out.bytes, out.reused), (11, false));
    assert_eq!(std::fs::read(dir.join("sub/a.bin")).unwrap(), body);

    let b = Download::new(server.url("/b"), dir.join("b.bin")).checksum(sha256(&body));
    c.download(&b, &ctl).unwrap();
    assert_eq!(
        leftovers(&dir).len(),
        2,
        "no temporary files remain: {:?}",
        leftovers(&dir)
    );
}

#[test]
fn an_existing_matching_file_is_kept_without_a_request() {
    let server = Server::start(|_| ok(b"new"));
    let dir = temp_dir("dl-reuse");
    let dest = dir.join("f.bin");
    std::fs::write(&dest, b"hello world").unwrap();
    let spec = Download::new(server.url("/f"), &dest).checksum(sha256(b"hello world"));
    let out = client(config(None))
        .download(&spec, &Control::default())
        .unwrap();
    assert!(out.reused);
    assert_eq!(server.requests(), 0);
}

#[test]
fn wrong_hash_fails_after_retries_and_leaves_no_file() {
    let server = Server::start(|_| ok(b"corrupted"));
    let dir = temp_dir("dl-hash");
    let spec = Download::new(server.url("/f"), dir.join("f.bin")).checksum(sha256(b"expected"));
    let err = client(config(None))
        .download(&spec, &Control::default())
        .unwrap_err();
    assert!(
        matches!(
            err,
            NetError::HashMismatch {
                algorithm: "sha256",
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(server.requests(), 3);
    assert!(leftovers(&dir).is_empty(), "{:?}", leftovers(&dir));
}

#[test]
fn an_existing_wrong_file_is_replaced() {
    let server = Server::start(|_| ok(b"fresh"));
    let dir = temp_dir("dl-replace");
    let dest = dir.join("f.bin");
    std::fs::write(&dest, b"stale!").unwrap();
    let spec = Download::new(server.url("/f"), &dest)
        .checksum(sha256(b"fresh"))
        .size(5);
    client(config(None))
        .download(&spec, &Control::default())
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"fresh");
}

#[test]
fn a_dropped_connection_is_retried_and_succeeds() {
    let server = Server::start(|r| {
        if r.n == 0 {
            Reply::Truncated {
                body: b"hel".to_vec(),
                declared: 11,
            }
        } else {
            ok(b"hello world")
        }
    });
    let dir = temp_dir("dl-retry");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    let ctl = Control::default().on_progress(move |p| s.lock().unwrap().push(p.done));
    let spec = Download::new(server.url("/f"), dir.join("f.bin")).size(11);
    let out = client(config(None)).download(&spec, &ctl).unwrap();
    assert_eq!(out.bytes, 11);
    assert_eq!(server.requests(), 2);
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.last(),
        Some(&11),
        "retried bytes are not counted twice: {seen:?}"
    );
    assert!(
        seen.windows(2).any(|w| w[1] < w[0]),
        "the failed attempt was rolled back"
    );
}

#[test]
fn server_errors_are_retried_then_reported() {
    let server = Server::start(|_| status(500));
    let dir = temp_dir("dl-500");
    let spec = Download::new(server.url("/f"), dir.join("f.bin"));
    let err = client(config(None))
        .download(&spec, &Control::default())
        .unwrap_err();
    assert!(matches!(err, NetError::Status { status: 500, .. }));
    assert_eq!(server.requests(), 3);
}

#[test]
fn downloads_over_the_size_are_rejected() {
    let server = Server::start(|r| {
        if r.target == "/len" {
            ok(&[b'x'; 4000])
        } else {
            Reply::NoLength {
                body: vec![b'x'; 4000],
            }
        }
    });
    let dir = temp_dir("dl-big");
    let c = client(config(None));
    for path in ["/len", "/nolen"] {
        let spec = Download::new(server.url(path), dir.join("big.bin")).max_bytes(1000);
        let err = c.download(&spec, &Control::default()).unwrap_err();
        assert!(
            matches!(err, NetError::TooLarge { limit: 1000, .. }),
            "{path}: {err:?}"
        );
    }
    let exact = Download::new(server.url("/nolen"), dir.join("big.bin")).size(10);
    let err = c.download(&exact, &Control::default()).unwrap_err();
    assert!(
        matches!(err, NetError::TooLarge { limit: 10, .. }),
        "{err:?}"
    );
    assert!(leftovers(&dir).is_empty(), "{:?}", leftovers(&dir));
}

#[test]
fn a_declared_size_that_differs_from_the_expected_one_is_rejected() {
    let server = Server::start(|_| ok(b"12345"));
    let dir = temp_dir("dl-size");
    let spec = Download::new(server.url("/f"), dir.join("f.bin")).size(9);
    let err = client(config(None))
        .download(&spec, &Control::default())
        .unwrap_err();
    assert!(
        matches!(
            err,
            NetError::TooLarge { .. } | NetError::SizeMismatch { .. }
        ),
        "{err:?}"
    );
    assert!(!dir.join("f.bin").exists());
}

#[test]
fn cancelling_stops_a_download_and_removes_the_temporary_file() {
    let server = Server::start(|_| Reply::Slow {
        first: vec![b'x'; 1000],
        total: 100_000,
        hold: Duration::from_secs(10),
    });
    let dir = temp_dir("dl-cancel");
    let token = CancelToken::new();
    let t = token.clone();
    let ctl = Control::with_cancel(token).on_progress(move |p| {
        if p.done > 0 {
            t.cancel();
        }
    });
    let spec = Download::new(server.url("/f"), dir.join("f.bin"));
    let started = std::time::Instant::now();
    let err = client(config(None)).download(&spec, &ctl).unwrap_err();
    assert_eq!(err, NetError::Cancelled);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "returned promptly"
    );
    assert!(leftovers(&dir).is_empty(), "{:?}", leftovers(&dir));
}

#[test]
fn a_cancelled_token_prevents_any_request() {
    let server = Server::start(|_| ok(b"x"));
    let token = CancelToken::new();
    token.cancel();
    let dir = temp_dir("dl-precancel");
    let r = client(config(None)).download(
        &Download::new(server.url("/f"), dir.join("f")),
        &Control::with_cancel(token),
    );
    assert_eq!(r.unwrap_err(), NetError::Cancelled);
    assert_eq!(server.requests(), 0);
}

#[test]
fn batches_respect_the_parallelism_limit_and_report_per_file_results() {
    let running = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (r, p) = (running.clone(), peak.clone());
    let server = Server::start(move |req| {
        if req.target == "/missing" {
            return status(404);
        }
        let now = r.fetch_add(1, Ordering::SeqCst) + 1;
        p.fetch_max(now, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(120));
        r.fetch_sub(1, Ordering::SeqCst);
        ok(req.target.as_bytes())
    });
    let dir = temp_dir("dl-batch");
    let mut cfg = config(None);
    cfg.max_parallel = 2;
    let c = client(cfg);

    let mut specs: Vec<Download> = (0..6)
        .map(|i| {
            let path = format!("/f{i}");
            Download::new(server.url(&path), dir.join(format!("f{i}"))).size(path.len() as u64)
        })
        .collect();
    specs.push(Download::new(server.url("/missing"), dir.join("missing")).size(1));

    let seen = Arc::new(Mutex::new(0u64));
    let s = seen.clone();
    let ctl = Control::default().on_progress(move |p| {
        let mut m = s.lock().unwrap();
        *m = (*m).max(p.done);
        assert_eq!(p.total, Some(6 * 3 + 1));
    });
    let results = c.download_all(&specs, &ctl);
    assert_eq!(results.len(), 7);
    assert!(results[..6].iter().all(|r| r.is_ok()));
    assert!(matches!(
        results[6],
        Err(NetError::Status { status: 404, .. })
    ));
    assert_eq!(peak.load(Ordering::SeqCst), 2);
    assert_eq!(*seen.lock().unwrap(), 18);
    assert_eq!(std::fs::read(dir.join("f3")).unwrap(), b"/f3");
}

#[test]
fn the_download_limit_is_shared_by_concurrent_callers() {
    let running = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (r, p) = (running.clone(), peak.clone());
    let server = Server::start(move |_| {
        let now = r.fetch_add(1, Ordering::SeqCst) + 1;
        p.fetch_max(now, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(100));
        r.fetch_sub(1, Ordering::SeqCst);
        ok(b"x")
    });
    let dir = temp_dir("dl-shared");
    let mut cfg = config(None);
    cfg.max_parallel = 2;
    let c = client(cfg);
    thread::scope(|s| {
        for i in 0..5 {
            let (c, url, dest) = (c.clone(), server.url("/f"), dir.join(format!("f{i}")));
            s.spawn(move || {
                c.download(&Download::new(url, dest), &Control::default())
                    .unwrap();
            });
        }
    });
    assert_eq!(peak.load(Ordering::SeqCst), 2);
}

#[test]
fn cancelling_a_batch_cancels_files_not_started() {
    let server = Server::start(|_| Reply::Slow {
        first: vec![b'x'; 10],
        total: 1000,
        hold: Duration::from_secs(10),
    });
    let dir = temp_dir("dl-batch-cancel");
    let mut cfg = config(None);
    cfg.max_parallel = 1;
    let token = CancelToken::new();
    let t = token.clone();
    let ctl = Control::with_cancel(token).on_progress(move |_| t.cancel());
    let specs: Vec<_> = (0..3)
        .map(|i| Download::new(server.url("/f"), dir.join(format!("f{i}"))))
        .collect();
    let results = client(cfg).download_all(&specs, &ctl);
    assert!(
        results.iter().all(|r| *r == Err(NetError::Cancelled)),
        "{results:?}"
    );
    assert!(leftovers(&dir).is_empty());
}

// ---------------------------------------------------------------- the transport seam

struct Fake;

impl vl_net::HttpTransport for Fake {
    fn get(&self, request: &vl_net::HttpRequest) -> Result<vl_net::HttpResponse, NetError> {
        Ok(vl_net::HttpResponse {
            status: 200,
            headers: Vec::new(),
            body: Box::new(std::io::Cursor::new(request.url.clone().into_bytes())),
        })
    }
}

#[test]
fn a_fake_transport_replaces_the_network() {
    let c = Client::with_transport(config(None), Arc::new(Fake));
    let got = c
        .get_bytes(
            "https://meta.test/v.json",
            &GetOptions::default(),
            &Control::default(),
        )
        .unwrap();
    assert_eq!(got.bytes, b"https://meta.test/v.json");
    let dir = temp_dir("fake");
    let spec = Download::new("https://meta.test/f", dir.join("f"));
    c.download(&spec, &Control::default()).unwrap();
    assert_eq!(
        std::fs::read(dir.join("f")).unwrap(),
        b"https://meta.test/f"
    );
}
