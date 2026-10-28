//! The signature table (data in `signatures.toml`) and its matcher.

use crate::{Cause, EvidenceLine, Source, evidence, push_evidence};
use regex::Regex;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::OnceLock;

const TABLE: &str = include_str!("signatures.toml");

/// Lines longer than this are cut before matching.
const MAX_LINE_CHARS: usize = 4000;

#[derive(Deserialize)]
struct Table {
    #[serde(default)]
    signature: Vec<Raw>,
}

#[derive(Deserialize)]
struct Raw {
    id: String,
    message_key: String,
    #[serde(default)]
    action_key: Option<String>,
    #[serde(default)]
    cause: Option<String>,
    #[serde(default)]
    loader_reported: bool,
    #[serde(default)]
    sources: Vec<String>,
    triggers: Vec<String>,
    #[serde(default)]
    collect: Vec<String>,
}

/// One compiled signature.
#[derive(Debug)]
pub struct Signature {
    id: String,
    message_key: String,
    action_key: Option<String>,
    cause: Cause,
    loader_reported: bool,
    sources: Vec<Source>,
    triggers: Vec<Regex>,
    collect: Vec<Regex>,
}

/// A compiled signature set.
#[derive(Debug)]
pub struct Signatures {
    list: Vec<Signature>,
}

/// A matched signature with what it collected.
pub(crate) struct Hit {
    pub id: String,
    pub message_key: String,
    pub action_key: Option<String>,
    pub cause: Cause,
    pub loader_reported: bool,
    pub mods: Vec<String>,
    pub args: BTreeMap<String, String>,
    pub evidence: Vec<EvidenceLine>,
}

impl Signatures {
    /// The signature set shipped with the crate. If the shipped table were invalid (a unit test
    /// guards against it), the set is empty and every log is undetermined.
    pub fn builtin() -> &'static Signatures {
        static BUILTIN: OnceLock<Signatures> = OnceLock::new();
        BUILTIN.get_or_init(|| Signatures::parse(TABLE).unwrap_or(Signatures { list: Vec::new() }))
    }

    /// Compiles a signature table in the format of `signatures.toml`.
    pub fn parse(toml_text: &str) -> Result<Signatures, String> {
        let table: Table = toml::from_str(toml_text).map_err(|e| e.to_string())?;
        let mut list = Vec::new();
        for raw in table.signature {
            let sources = raw
                .sources
                .iter()
                .map(|name| match name.as_str() {
                    "latest_log" => Ok(Source::LatestLog),
                    "crash_report" => Ok(Source::CrashReport),
                    "hs_err" => Ok(Source::HsErr),
                    other => Err(format!("{}: unknown source {other}", raw.id)),
                })
                .collect::<Result<Vec<_>, _>>()?;
            let cause = match raw.cause.as_deref() {
                None | Some("unknown") => Cause::Unknown,
                Some("mod_likely") => Cause::ModLikely,
                Some("environment") => Cause::Environment,
                Some(other) => return Err(format!("{}: unknown cause {other}", raw.id)),
            };
            let compile = |patterns: &[String]| {
                patterns
                    .iter()
                    .map(|p| Regex::new(p).map_err(|e| format!("{}: {e}", raw.id)))
                    .collect::<Result<Vec<_>, _>>()
            };
            if raw.triggers.is_empty() {
                return Err(format!("{}: no triggers", raw.id));
            }
            list.push(Signature {
                triggers: compile(&raw.triggers)?,
                collect: compile(&raw.collect)?,
                id: raw.id,
                message_key: raw.message_key,
                action_key: raw.action_key,
                cause,
                loader_reported: raw.loader_reported,
                sources,
            });
        }
        Ok(Signatures { list })
    }

    /// The ids of the signatures, in evaluation order.
    pub fn ids(&self) -> Vec<&str> {
        self.list.iter().map(|s| s.id.as_str()).collect()
    }

    /// The message and action keys the signatures can produce.
    pub fn keys(&self) -> Vec<&str> {
        let mut keys = Vec::new();
        for s in &self.list {
            keys.push(s.message_key.as_str());
            keys.extend(s.action_key.as_deref());
        }
        keys
    }

    pub(crate) fn find(&self, sources: &[(Source, &str)]) -> Option<Hit> {
        self.list.iter().find_map(|sig| sig.matches(sources))
    }
}

impl Signature {
    fn allows(&self, source: Source) -> bool {
        self.sources.is_empty() || self.sources.contains(&source)
    }

    fn matches(&self, sources: &[(Source, &str)]) -> Option<Hit> {
        let triggered = sources
            .iter()
            .filter(|(source, _)| self.allows(*source))
            .any(|(_, text)| {
                text.lines()
                    .any(|line| self.triggers.iter().any(|re| re.is_match(clip(line))))
            });
        if !triggered {
            return None;
        }
        let mut hit = Hit {
            id: self.id.clone(),
            message_key: self.message_key.clone(),
            action_key: self.action_key.clone(),
            cause: self.cause,
            loader_reported: self.loader_reported,
            mods: Vec::new(),
            args: BTreeMap::new(),
            evidence: Vec::new(),
        };
        for (source, text) in sources.iter().filter(|(s, _)| self.allows(*s)) {
            for (index, line) in text.lines().enumerate() {
                let line = clip(line);
                let mut matched = false;
                for re in self.triggers.iter().chain(&self.collect) {
                    let Some(caps) = re.captures(line) else {
                        continue;
                    };
                    matched = true;
                    for name in re.capture_names().flatten() {
                        let Some(value) = caps.name(name) else {
                            continue;
                        };
                        let value = value.as_str().trim().to_owned();
                        if value.is_empty() {
                            continue;
                        }
                        if name == "mod" {
                            if !hit.mods.contains(&value) {
                                hit.mods.push(value);
                            }
                        } else {
                            hit.args.entry(name.to_owned()).or_insert(value);
                        }
                    }
                }
                if matched {
                    push_evidence(&mut hit.evidence, evidence(*source, index + 1, line));
                }
            }
        }
        Some(hit)
    }
}

fn clip(line: &str) -> &str {
    match line.char_indices().nth(MAX_LINE_CHARS) {
        Some((end, _)) => &line[..end],
        None => line,
    }
}
