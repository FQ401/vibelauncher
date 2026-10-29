//! Sharing a game log on [mclo.gs](https://mclo.gs): preparing the content and uploading it.
//!
//! The API (`https://api.mclo.gs`, version 1): `POST /1/log` with a JSON body
//! `{"content": "<log>", "source": "<software>"}` answers `{"success": true, "id", "url", "raw",
//! ...}` or `{"success": false, "error": "<text>"}`. The service accepts at most 10 MiB and
//! 25 000 lines ([`LIMITS`]); it cuts longer content itself, so the launcher cuts it first and
//! says what it left out. The answer also carries a token that deletes the log; the launcher
//! neither keeps nor shows it.
//!
//! [`prepare`] reads the file without loading more than the limit into memory, masks it with a
//! [`Redactor`], and keeps the first quarter of the allowed lines and bytes from the start of the
//! log (versions, mods, settings) and the rest from its end (the error), with a marker line
//! between them naming the number of lines left out. [`upload`] sends prepared content; nothing
//! is ever sent without it being called, and front ends call it only after the user confirmed.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use serde::{Deserialize, Serialize};
use vl_core::redact::Redactor;
use vl_core::{Error, ErrorKind};
use vl_net::{Client, Control};

/// The address of the API.
pub const API: &str = "https://api.mclo.gs";
/// The name of the service shown to the user.
pub const SERVICE: &str = "mclo.gs";
/// What the service accepts.
pub const LIMITS: Limits = Limits {
    max_bytes: 10 * 1024 * 1024,
    max_lines: 25_000,
};
/// The `source` sent with each log.
const SOURCE: &str = "VibeLauncher";
/// Room left below the byte limit for the marker line and for masks longer than what they hide.
const MARGIN_BYTES: u64 = 64 * 1024;
/// The most bytes read of an answer.
const MAX_ANSWER_BYTES: u64 = 64 * 1024;
/// The longest link accepted from an answer.
const MAX_URL_LEN: usize = 512;

/// How much content a service accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes of content.
    pub max_bytes: u64,
    /// Lines of content (at least 2: a line from each end and the marker do not fit in fewer).
    pub max_lines: usize,
}

/// A log ready to be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    /// The masked content within the limits.
    pub content: String,
    /// The size of the file, bytes.
    pub file_bytes: u64,
    /// The lines of the file.
    pub file_lines: u64,
    /// The lines of `content`, the marker included.
    pub lines: u64,
    /// The lines of the file left out; 0 when everything fits.
    pub omitted_lines: u64,
}

/// Reads `path` and prepares it for upload within `limits`, masked with `redactor`.
///
/// # Errors
///
/// `NotFound` for a missing file, `Io` when it cannot be read.
pub fn prepare(path: &Path, redactor: &Redactor, limits: Limits) -> vl_core::Result<Prepared> {
    read_parts(path, limits)
        .map(|parts| assemble(parts, redactor, limits))
        .map_err(|e| {
            let kind = if e.kind() == io::ErrorKind::NotFound {
                ErrorKind::NotFound
            } else {
                ErrorKind::Io
            };
            Error::new(kind, format!("cannot read {}: {e}", path.display()))
        })
}

/// The start and the end of a file and what lies between them.
struct Parts {
    head: Vec<u8>,
    tail: Vec<u8>,
    /// Lines between `head` and `tail` that were not read.
    gap_lines: u64,
    file_bytes: u64,
    file_lines: u64,
}

fn read_parts(path: &Path, limits: Limits) -> io::Result<Parts> {
    let mut file = File::open(path)?;
    let file_bytes = file.metadata()?.len();
    let budget = limits.max_bytes.saturating_sub(MARGIN_BYTES).max(1);
    if file_bytes <= budget {
        let mut head = Vec::new();
        file.by_ref().take(budget).read_to_end(&mut head)?;
        let file_lines = count_lines(&head);
        return Ok(Parts {
            head,
            tail: Vec::new(),
            gap_lines: 0,
            file_bytes,
            file_lines,
        });
    }
    let head_budget = budget / 4;
    let mut head = Vec::new();
    file.by_ref().take(head_budget).read_to_end(&mut head)?;
    if let Some(end) = head.iter().rposition(|&b| b == b'\n') {
        head.truncate(end + 1);
    }
    let head_end = head.len() as u64;

    let tail_start = file_bytes
        .saturating_sub(budget - head_budget)
        .max(head_end);
    file.seek(SeekFrom::Start(tail_start))?;
    let mut tail = Vec::new();
    file.by_ref().read_to_end(&mut tail)?;
    // Start the tail at a line, unless the previous byte ended one.
    let mut tail_from = tail_start;
    if tail_start > head_end {
        file.seek(SeekFrom::Start(tail_start - 1))?;
        let mut previous = [0u8; 1];
        file.read_exact(&mut previous)?;
        if previous[0] != b'\n'
            && let Some(start) = tail.iter().position(|&b| b == b'\n')
        {
            tail.drain(..=start);
            tail_from = tail_start + start as u64 + 1;
        }
    }

    // Count the lines in between without holding them.
    file.seek(SeekFrom::Start(head_end))?;
    let mut middle = file.by_ref().take(tail_from - head_end);
    let mut gap_lines = 0u64;
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let n = middle.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        gap_lines += buffer.iter().take(n).filter(|&&b| b == b'\n').count() as u64;
    }
    let file_lines = gap_lines + count_lines(&head) + count_lines(&tail);
    Ok(Parts {
        head,
        tail,
        gap_lines,
        file_bytes,
        file_lines,
    })
}

/// Lines of `bytes`: its newlines, plus an unterminated last line.
fn count_lines(bytes: &[u8]) -> u64 {
    let newlines = bytes.iter().filter(|&&b| b == b'\n').count() as u64;
    newlines + u64::from(bytes.last().is_some_and(|&b| b != b'\n'))
}

fn assemble(parts: Parts, redactor: &Redactor, limits: Limits) -> Prepared {
    let head_text = String::from_utf8_lossy(&parts.head);
    let tail_text = String::from_utf8_lossy(&parts.tail);
    let head_text = redactor.redact(&head_text);
    let tail_text = redactor.redact(&tail_text);
    let mut head: Vec<&str> = head_text.lines().collect();
    let mut tail: Vec<&str> = tail_text.lines().collect();
    let mut gap = parts.gap_lines;

    let max_lines = limits.max_lines.max(2);
    let marker = usize::from(gap > 0);
    if head.len() + tail.len() + marker > max_lines {
        let budget = max_lines - 1;
        let keep_head = (budget / 4).min(head.len());
        let keep_tail = budget - keep_head;
        if tail.is_empty() && gap == 0 {
            // One part that is too long: its end becomes the tail.
            let at = head.len().saturating_sub(keep_tail).max(keep_head);
            tail = head.split_off(at);
        } else if tail.len() > keep_tail {
            let drop = tail.len() - keep_tail;
            gap += drop as u64;
            tail.drain(..drop);
        }
        gap += head.len().saturating_sub(keep_head) as u64;
        head.truncate(keep_head);
    }

    // Masks may be longer than what they hide: drop lines after the head until it fits.
    let size = |lines: &[&str]| lines.iter().map(|l| l.len() as u64 + 1).sum::<u64>();
    let mut bytes = size(&head) + size(&tail);
    while gap > 0 || bytes > limits.max_bytes {
        let marker_bytes = marker_line(gap.max(1)).len() as u64 + 1;
        if bytes + marker_bytes <= limits.max_bytes {
            break;
        }
        if tail.is_empty() {
            match head.pop() {
                Some(line) => bytes -= line.len() as u64 + 1,
                None => break,
            }
        } else {
            bytes -= tail[0].len() as u64 + 1;
            tail.remove(0);
        }
        gap += 1;
    }

    let mut content = String::with_capacity(usize::try_from(bytes).unwrap_or(0) + 128);
    let mut lines = 0u64;
    for line in &head {
        content.push_str(line);
        content.push('\n');
        lines += 1;
    }
    if gap > 0 {
        content.push_str(&marker_line(gap));
        content.push('\n');
        lines += 1;
    }
    for line in &tail {
        content.push_str(line);
        content.push('\n');
        lines += 1;
    }
    Prepared {
        content,
        file_bytes: parts.file_bytes,
        file_lines: parts.file_lines,
        lines,
        omitted_lines: gap,
    }
}

fn marker_line(omitted: u64) -> String {
    format!(
        "[VibeLauncher] ... {omitted} lines left out here: the log is longer than {SERVICE} \
         accepts ({} lines, {} MiB) ...",
        LIMITS.max_lines,
        LIMITS.max_bytes >> 20
    )
}

/// A log on the service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uploaded {
    /// Its id.
    pub id: String,
    /// Its page.
    pub url: String,
    /// Its raw content, when the service names it.
    pub raw_url: Option<String>,
}

#[derive(Serialize)]
struct Request<'a> {
    content: &'a str,
    source: &'a str,
}

#[derive(Deserialize)]
struct Answer {
    success: bool,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    raw: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// Uploads `content` to the API at `api` ([`API`], or a test server) and returns the link.
///
/// # Errors
///
/// `Network` when the service cannot be reached or answers an error (its message included),
/// `Integrity` when the answer is damaged or names no `https` link.
pub fn upload(
    client: &Client,
    api: &str,
    content: &str,
    control: &Control,
) -> vl_core::Result<Uploaded> {
    let body = serde_json::to_vec(&Request {
        content,
        source: SOURCE,
    })
    .map_err(|e| Error::internal(format!("cannot encode the upload: {e}")))?;
    let url = format!("{}/1/log", api.trim_end_matches('/'));
    let answer = client
        .post_bytes(
            &url,
            "application/json",
            &body,
            Some(MAX_ANSWER_BYTES),
            control,
            None,
        )
        .map_err(Error::from)?;
    let answer: Answer = serde_json::from_slice(&answer).map_err(|e| {
        Error::new(
            ErrorKind::Integrity,
            format!("{SERVICE} answered something unexpected: {e}"),
        )
    })?;
    if !answer.success {
        let reason = answer.error.as_deref().map_or_else(
            || "no reason given".to_owned(),
            |e| e.chars().filter(|c| !c.is_control()).take(200).collect(),
        );
        return Err(Error::new(
            ErrorKind::Network,
            format!("{SERVICE} refused the log: {reason}"),
        ));
    }
    let link = answer.url.filter(|u| is_link(u)).ok_or_else(|| {
        Error::new(
            ErrorKind::Integrity,
            format!("{SERVICE} answered without an https link"),
        )
    })?;
    Ok(Uploaded {
        id: answer.id.unwrap_or_default(),
        url: link,
        raw_url: answer.raw.filter(|u| is_link(u)),
    })
}

/// An `https` link of a sane length without spaces or control characters.
fn is_link(url: &str) -> bool {
    url.len() <= MAX_URL_LEN
        && url.len() > "https://".len()
        && url
            .get(..8)
            .is_some_and(|s| s.eq_ignore_ascii_case("https://"))
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::thread;

    use vl_net::NetConfig;

    use super::*;

    fn temp_file(name: &str, content: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vl-mclogs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    fn numbered(lines: usize) -> String {
        (0..lines).map(|n| format!("line {n}\n")).collect()
    }

    #[test]
    fn a_small_log_is_sent_whole_and_masked() {
        let text = "[12:00:00] [main/INFO]: Loading for /home/synthetic\n\
                    [12:00:01] [main/INFO]: --accessToken synthetic-token-value\n\
                    last line without newline";
        let path = temp_file("small.log", text.as_bytes());
        let redactor = Redactor::new().with_home("/home/synthetic", false);
        let p = prepare(&path, &redactor, LIMITS).unwrap();
        assert_eq!((p.file_lines, p.lines, p.omitted_lines), (3, 3, 0));
        assert_eq!(p.file_bytes, text.len() as u64);
        assert!(p.content.contains("Loading for ~\n"), "{}", p.content);
        assert!(p.content.contains("--accessToken <redacted>"));
        assert!(!p.content.contains("synthetic-token-value"));
        assert!(p.content.ends_with("last line without newline\n"));
    }

    #[test]
    fn too_many_lines_keep_the_start_and_the_end() {
        let path = temp_file("lines.log", numbered(1000).as_bytes());
        let limits = Limits {
            max_bytes: 10 * 1024 * 1024,
            max_lines: 101,
        };
        let p = prepare(&path, &Redactor::new(), limits).unwrap();
        let lines: Vec<&str> = p.content.lines().collect();
        assert_eq!(lines.len(), 101);
        assert_eq!(p.lines, 101);
        assert_eq!(p.file_lines, 1000);
        assert_eq!(p.omitted_lines, 900);
        assert_eq!(lines[0], "line 0");
        assert_eq!(lines[24], "line 24");
        assert!(lines[25].contains("900 lines left out"), "{}", lines[25]);
        assert_eq!(lines[26], "line 925");
        assert_eq!(lines[100], "line 999");
    }

    #[test]
    fn too_many_bytes_keep_whole_lines_from_both_ends() {
        // 200 000 lines of 100 bytes: about 19 MiB, over the 10 MiB limit.
        let line = format!("{}\n", "y".repeat(99));
        let mut text = String::with_capacity(20 << 20);
        for n in 0..200_000 {
            text.push_str(&format!("{n:06} "));
            text.push_str(&line[7..]);
        }
        let path = temp_file("bytes.log", text.as_bytes());
        let limits = Limits {
            max_bytes: LIMITS.max_bytes,
            max_lines: 1_000_000,
        };
        let p = prepare(&path, &Redactor::new(), limits).unwrap();
        assert!(p.content.len() as u64 <= limits.max_bytes);
        assert_eq!(p.file_lines, 200_000);
        assert_eq!(p.file_bytes, text.len() as u64);
        let lines: Vec<&str> = p.content.lines().collect();
        assert!(lines[0].starts_with("000000 "));
        assert!(lines.last().unwrap().starts_with("199999 "));
        assert!(
            lines
                .iter()
                .all(|l| l.len() == 99 || l.contains("left out"))
        );
        let marker = lines.iter().position(|l| l.contains("left out")).unwrap();
        assert_eq!(p.lines as usize, lines.len());
        assert_eq!(p.omitted_lines + p.lines - 1, p.file_lines);
        // The lines on both sides of the marker are consecutive with the gap between them.
        let before: u64 = lines[marker - 1][..6].parse().unwrap();
        let after: u64 = lines[marker + 1][..6].parse().unwrap();
        assert_eq!(after - before - 1, p.omitted_lines);

        // Both limits at once: the stricter one wins.
        let p = prepare(&path, &Redactor::new(), LIMITS).unwrap();
        assert_eq!(p.lines, 25_000);
        assert!(p.content.len() as u64 <= LIMITS.max_bytes);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_missing_file_is_not_found() {
        let err = prepare(
            Path::new("definitely/missing/file.log"),
            &Redactor::new(),
            LIMITS,
        )
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::NotFound);
    }

    /// A local stand-in for the API: answers one request with `status` and `body` and hands over
    /// the request line, headers and body it received.
    fn mock_api(status: u16, body: &'static str) -> (String, mpsc::Receiver<(String, String)>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut head = String::new();
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
                head.push_str(&line);
            }
            let mut request = vec![0u8; length];
            reader.read_exact(&mut request).unwrap();
            tx.send((head, String::from_utf8(request).unwrap()))
                .unwrap();
            let mut stream = stream;
            write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        (addr, rx)
    }

    fn client() -> Client {
        let mut config = NetConfig::new("0.0.0");
        config.proxy = vl_net::ProxyConfig::None;
        config.retry.attempts = 1;
        Client::new(config).unwrap()
    }

    #[test]
    fn uploads_json_and_returns_the_link() {
        let (api, requests) = mock_api(
            200,
            r#"{"success":true,"id":"Abc123","source":"VibeLauncher","created":1,"expires":2,
               "size":5,"lines":1,"errors":0,"url":"https://mclo.gs/Abc123",
               "raw":"https://api.mclo.gs/1/raw/Abc123","token":"synthetic-delete-token",
               "metadata":[]}"#,
        );
        let uploaded = upload(&client(), &api, "a log\n", &Control::default()).unwrap();
        assert_eq!(
            uploaded,
            Uploaded {
                id: "Abc123".into(),
                url: "https://mclo.gs/Abc123".into(),
                raw_url: Some("https://api.mclo.gs/1/raw/Abc123".into()),
            }
        );
        let (head, body) = requests.recv().unwrap();
        assert!(head.starts_with("POST /1/log HTTP/1.1"), "{head}");
        assert!(
            head.to_ascii_lowercase()
                .contains("content-type: application/json"),
            "{head}"
        );
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["content"], "a log\n");
        assert_eq!(json["source"], "VibeLauncher");
    }

    #[test]
    fn a_refusal_and_a_bad_answer_are_errors() {
        let (api, _requests) = mock_api(
            200,
            r#"{"success":false,"error":"Required field 'content' not found."}"#,
        );
        let err = upload(&client(), &api, "", &Control::default()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Network);
        assert!(err.message.contains("Required field"), "{err}");

        let (api, _requests) = mock_api(200, r#"{"success":true,"url":"http://insecure/x"}"#);
        let err = upload(&client(), &api, "x", &Control::default()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Integrity);

        let (api, _requests) = mock_api(500, "{}");
        let err = upload(&client(), &api, "x", &Control::default()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Network);
    }
}
