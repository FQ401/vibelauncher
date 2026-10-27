//! Turning the game's output into levelled lines.
//!
//! Two shapes exist. Newer versions can write log4j XML events (`<log4j:Event ...>`, possibly
//! over several lines); older ones and everything on the error stream are plain text such as
//! `[12:34:56] [Render thread/INFO]: text`, `[12:34:56 WARN]: text` or
//! `2013-01-01 12:00:00 [INFO] text`. One [`LineParser`] per stream consumes raw lines and
//! produces [`Parsed`] lines; an XML event becomes a header line plus its message and stack trace
//! lines. Anything that is not recognized stays a line with an unknown level, so nothing is lost.

use vl_core::log::LogLevel;

/// The longest XML event kept together; a longer one is handed out as raw lines.
const MAX_EVENT: usize = 256 * 1024;
const EVENT_START: &str = "<log4j:Event";
const EVENT_END: &str = "</log4j:Event>";

/// One output line with its level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    /// The level, `Unknown` when the line has none.
    pub level: LogLevel,
    /// The text, without the line ending.
    pub text: String,
}

/// A stateful parser for one output stream.
#[derive(Debug)]
pub struct LineParser {
    /// The lines of an XML event that has not ended yet.
    pending: Option<String>,
    /// The level of the previous line, inherited by stack trace lines.
    last: LogLevel,
}

impl Default for LineParser {
    fn default() -> Self {
        Self::new()
    }
}

impl LineParser {
    /// A parser with no state.
    pub fn new() -> Self {
        Self {
            pending: None,
            last: LogLevel::Unknown,
        }
    }

    /// Consumes one raw line and appends the lines it completes to `out`.
    pub fn push(&mut self, line: &str, out: &mut Vec<Parsed>) {
        let starts_event = line.trim_start().starts_with(EVENT_START);
        if let Some(buf) = &mut self.pending {
            if !starts_event {
                buf.push('\n');
                buf.push_str(line);
                if buf.contains(EVENT_END) || buf.len() > MAX_EVENT {
                    self.finish_event(out);
                }
                return;
            }
            // The earlier event never ended: hand it out as it is.
            self.flush(out);
        }
        if starts_event {
            self.pending = Some(line.to_owned());
            if line.contains(EVENT_END) {
                self.finish_event(out);
            }
            return;
        }
        let level = self.plain_level(line);
        self.last = level;
        out.push(Parsed {
            level,
            text: line.to_owned(),
        });
    }

    /// Hands out what is still buffered; call when the stream ended.
    pub fn flush(&mut self, out: &mut Vec<Parsed>) {
        if let Some(buf) = self.pending.take() {
            raw_lines(&buf, out);
        }
        self.last = LogLevel::Unknown;
    }

    fn plain_level(&self, line: &str) -> LogLevel {
        if let Some(level) = plain_line_level(line) {
            return level;
        }
        if is_continuation(line) {
            return self.last;
        }
        if line.starts_with("Exception in thread ") {
            return LogLevel::Error;
        }
        LogLevel::Unknown
    }

    fn finish_event(&mut self, out: &mut Vec<Parsed>) {
        let Some(raw) = self.pending.take() else {
            return;
        };
        match parse_event(&raw) {
            Some(lines) => {
                if let Some(first) = lines.first() {
                    self.last = first.level;
                }
                out.extend(lines);
            }
            None => raw_lines(&raw, out),
        }
    }
}

fn raw_lines(text: &str, out: &mut Vec<Parsed>) {
    out.extend(text.split('\n').map(|line| Parsed {
        level: LogLevel::Unknown,
        text: line.to_owned(),
    }));
}

/// Whether `line` continues the previous entry: a stack trace line.
fn is_continuation(line: &str) -> bool {
    line.starts_with('\t')
        || line.starts_with("    at ")
        || line.starts_with("at ")
        || line.starts_with("Caused by: ")
        || line.starts_with("Suppressed: ")
        || line.starts_with("... ")
        || is_throwable_header(line)
}

/// `java.lang.IllegalStateException: text`: the first line of a printed throwable.
fn is_throwable_header(line: &str) -> bool {
    let name = line.split([':', ' ']).next().unwrap_or_default();
    name.contains('.')
        && !name.starts_with('[')
        && ["Exception", "Error", "Throwable"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

/// The level of a plain-text line, from its leading bracket groups.
pub fn plain_line_level(line: &str) -> Option<LogLevel> {
    let mut rest = skip_date(line.trim_start());
    for _ in 0..4 {
        rest = rest.trim_start();
        let inner = rest.strip_prefix('[')?;
        let end = inner.find(']')?;
        let group = &inner[..end];
        if let Some(level) = group
            .split(|c: char| c.is_whitespace() || c == '/')
            .find_map(|word| level_name(word.trim_matches(':')))
        {
            return Some(level);
        }
        rest = &inner[end + 1..];
    }
    None
}

/// Skips a leading `YYYY-MM-DD HH:MM:SS` stamp.
fn skip_date(line: &str) -> &str {
    let stamp = line
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || matches!(c, '-' | ':' | '.' | ',' | ' ')))
        .map_or(line.len(), |(i, _)| i);
    if stamp >= 8 && line.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        &line[stamp..]
    } else {
        line
    }
}

/// A level name, in any case; log4j, java.util.logging and Minecraft spellings.
pub fn level_name(name: &str) -> Option<LogLevel> {
    Some(match name.to_ascii_uppercase().as_str() {
        "FATAL" | "SEVERE" | "ERROR" => LogLevel::Error,
        "WARN" | "WARNING" => LogLevel::Warn,
        "INFO" | "CONFIG" => LogLevel::Info,
        "DEBUG" | "FINE" | "FINER" => LogLevel::Debug,
        "TRACE" | "FINEST" | "ALL" => LogLevel::Trace,
        _ => return None,
    })
}

/// Parses one complete XML event into a header line plus message and throwable lines.
fn parse_event(raw: &str) -> Option<Vec<Parsed>> {
    let start = raw.find(EVENT_START)?;
    let tag_end = tag_end(&raw[start..])? + start;
    let attrs = &raw[start + EVENT_START.len()..tag_end];
    let level_text = attr(attrs, "level");
    let level = level_text
        .as_deref()
        .and_then(level_name)
        .unwrap_or(LogLevel::Unknown);
    let logger = attr(attrs, "logger").unwrap_or_default();
    let thread = attr(attrs, "thread").unwrap_or_default();
    let body = &raw[tag_end + 1..];
    let message = element_text(body, "log4j:Message").unwrap_or_default();
    let throwable = element_text(body, "log4j:Throwable");

    let mut head = String::new();
    if !thread.is_empty() {
        head.push('[');
        head.push_str(&thread);
        if let Some(name) = &level_text {
            head.push('/');
            head.push_str(name);
        }
        head.push_str("] ");
    }
    if !logger.is_empty() {
        head.push('[');
        head.push_str(&logger);
        head.push_str("]: ");
    }
    let mut lines = Vec::new();
    let mut message_lines = message.split('\n').map(|l| l.trim_end_matches('\r'));
    let first = message_lines.next().unwrap_or_default();
    lines.push(Parsed {
        level,
        text: format!("{head}{first}"),
    });
    lines.extend(message_lines.map(|l| Parsed {
        level,
        text: l.to_owned(),
    }));
    if let Some(trace) = throwable {
        lines.extend(
            trace
                .split('\n')
                .map(|l| l.trim_end_matches('\r'))
                .filter(|l| !l.is_empty())
                .map(|l| Parsed {
                    level,
                    text: l.to_owned(),
                }),
        );
    }
    Some(lines)
}

/// The index of the `>` that ends the start tag at the beginning of `s`, outside quotes.
fn tag_end(s: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '>') => return Some(i),
            _ => {}
        }
    }
    None
}

/// The value of attribute `name` in a start tag's attribute text, unescaped.
fn attr(attrs: &str, name: &str) -> Option<String> {
    let mut offset = 0;
    while let Some(found) = attrs[offset..].find(name) {
        let pos = offset + found;
        let before_ok = attrs[..pos].chars().last().is_none_or(char::is_whitespace);
        let after = attrs[pos + name.len()..].trim_start();
        if before_ok && let Some(value) = after.strip_prefix('=') {
            let value = value.trim_start();
            let quote = value.chars().next().filter(|c| matches!(c, '"' | '\''))?;
            let inner = &value[1..];
            let end = inner.find(quote)?;
            return Some(unescape(&inner[..end]));
        }
        offset = pos + name.len();
    }
    None
}

/// The text of element `tag` in `body`: CDATA sections and escaped text, concatenated.
fn element_text(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let start = body.find(&open)?;
    let content_start = tag_end(&body[start..])? + start + 1;
    let end = body[content_start..].find(&close)? + content_start;
    let mut content = &body[content_start..end];
    let mut text = String::new();
    while !content.is_empty() {
        if let Some(rest) = content.strip_prefix("<![CDATA[") {
            let stop = rest.find("]]>").unwrap_or(rest.len());
            text.push_str(&rest[..stop]);
            content = rest.get(stop + 3..).unwrap_or_default();
        } else {
            let stop = content.find("<![CDATA[").unwrap_or(content.len());
            text.push_str(&unescape(&content[..stop]));
            content = &content[stop..];
        }
    }
    Some(text)
}

fn unescape(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(lines: &[&str]) -> Vec<Parsed> {
        let mut parser = LineParser::new();
        let mut out = Vec::new();
        for line in lines {
            parser.push(line, &mut out);
        }
        parser.flush(&mut out);
        out
    }

    fn levels(lines: &[&str]) -> Vec<LogLevel> {
        parse_all(lines).iter().map(|p| p.level).collect()
    }

    #[test]
    fn plain_formats_across_versions() {
        use LogLevel::*;
        assert_eq!(
            levels(&[
                "[12:34:56] [Render thread/INFO]: Setting user: Steve",
                "[12:34:56 WARN]: Can't keep up!",
                "2013-01-01 12:00:00 [SEVERE] Failed to load",
                "[12:34:56] [main/ERROR] [net.minecraft.Foo]: bad",
                "[12:34:56] [main/DEBUG]: details",
                "[12:34:56] [main/FATAL]: boom",
                "[12:34:56] [main/TRACE]: noisy",
                "just text",
                "",
            ]),
            [
                Info, Warn, Error, Error, Debug, Error, Trace, Unknown, Unknown
            ]
        );
    }

    #[test]
    fn brackets_without_a_level_are_not_levels() {
        assert_eq!(plain_line_level("[12:34:56] [main/Mojang]: hi"), None);
        assert_eq!(plain_line_level("[Informational] x"), None);
    }

    #[test]
    fn stack_traces_inherit_the_level_of_their_entry() {
        let out = parse_all(&[
            "[12:00:00] [main/ERROR]: Crash",
            "java.lang.IllegalStateException: no",
            "\tat net.minecraft.Foo.bar(Foo.java:10)",
            "Caused by: java.io.IOException: disk",
            "\t... 5 more",
            "[12:00:01] [main/INFO]: next",
            "\tat orphan.Line(X.java:1)",
        ]);
        let levels: Vec<_> = out.iter().map(|p| p.level).collect();
        assert_eq!(
            levels,
            [
                LogLevel::Error,
                LogLevel::Error,
                LogLevel::Error,
                LogLevel::Error,
                LogLevel::Error,
                LogLevel::Info,
                LogLevel::Info,
            ]
        );
    }

    #[test]
    fn exception_in_thread_is_an_error() {
        assert_eq!(
            levels(&["Exception in thread \"main\" java.lang.Error"]),
            [LogLevel::Error]
        );
    }

    #[test]
    fn xml_event_on_one_line() {
        let out = parse_all(&[
            r#"<log4j:Event logger="net.minecraft.client.Minecraft" timestamp="1700000000000" level="INFO" thread="Render thread"><log4j:Message><![CDATA[Setting user: Steve]]></log4j:Message></log4j:Event>"#,
        ]);
        assert_eq!(
            out,
            [Parsed {
                level: LogLevel::Info,
                text: "[Render thread/INFO] [net.minecraft.client.Minecraft]: Setting user: Steve"
                    .to_owned()
            }]
        );
    }

    #[test]
    fn xml_event_over_lines_with_a_throwable() {
        let out = parse_all(&[
            r#"<log4j:Event logger="Mod" timestamp="1" level="ERROR" thread="main">"#,
            "<log4j:Message><![CDATA[Failed",
            "second line]]></log4j:Message>",
            "<log4j:Throwable><![CDATA[java.lang.RuntimeException: x",
            "\tat a.B.c(B.java:1)",
            "]]></log4j:Throwable>",
            "</log4j:Event>",
            "[12:00:00] [main/INFO]: after",
        ]);
        let texts: Vec<_> = out.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "[main/ERROR] [Mod]: Failed",
                "second line",
                "java.lang.RuntimeException: x",
                "\tat a.B.c(B.java:1)",
                "[12:00:00] [main/INFO]: after",
            ]
        );
        assert!(out[..4].iter().all(|p| p.level == LogLevel::Error));
        assert_eq!(out[4].level, LogLevel::Info);
    }

    #[test]
    fn xml_escapes_split_cdata_and_greater_than_in_attributes() {
        let out = parse_all(&[
            r#"<log4j:Event logger="a&gt;b" level="warn" thread="t"><log4j:Message>x &lt; y <![CDATA[1]]]]><![CDATA[>2]]></log4j:Message></log4j:Event>"#,
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].level, LogLevel::Warn);
        assert_eq!(out[0].text, "[t/warn] [a>b]: x < y 1]]>2");
    }

    #[test]
    fn an_unterminated_event_comes_out_raw() {
        let out = parse_all(&[
            r#"<log4j:Event logger="a" level="INFO" thread="t">"#,
            "<log4j:Message>half",
        ]);
        let texts: Vec<_> = out.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                r#"<log4j:Event logger="a" level="INFO" thread="t">"#,
                "<log4j:Message>half"
            ]
        );
        assert!(out.iter().all(|p| p.level == LogLevel::Unknown));
    }

    #[test]
    fn a_new_event_start_releases_the_unterminated_one() {
        let out = parse_all(&[
            r#"<log4j:Event logger="a" level="INFO" thread="t">"#,
            r#"<log4j:Event logger="b" level="INFO" thread="t"><log4j:Message>ok</log4j:Message></log4j:Event>"#,
        ]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].text, "[t/INFO] [b]: ok");
    }

    #[test]
    fn an_oversized_event_is_released() {
        let mut parser = LineParser::new();
        let mut out = Vec::new();
        parser.push(
            r#"<log4j:Event logger="a" level="INFO" thread="t">"#,
            &mut out,
        );
        let chunk = "x".repeat(60 * 1024);
        for _ in 0..6 {
            parser.push(&chunk, &mut out);
        }
        assert!(!out.is_empty(), "released after the size cap");
        parser.push("[12:00:00] [main/INFO]: fine", &mut out);
        assert_eq!(out.last().map(|p| p.level), Some(LogLevel::Info));
    }
}
