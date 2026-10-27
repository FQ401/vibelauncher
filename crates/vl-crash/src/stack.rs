//! Levels 2-4: suspects from stack traces, when no signature matched.

use crate::{
    ClassIndex, CrashInput, Diagnosis, EvidenceLine, Level, Source, evidence, push_evidence,
};
use std::collections::BTreeMap;

/// Packages that belong to the game, the loaders and common libraries, never to a mod.
const NOT_MODS: &[&str] = &[
    "java.",
    "javax.",
    "jdk.",
    "sun.",
    "com.sun.",
    "net.minecraft.",
    "com.mojang.",
    "net.minecraftforge.",
    "net.neoforged.",
    "net.fabricmc.",
    "org.quiltmc.",
    "cpw.mods.",
    "org.spongepowered.",
    "org.lwjgl.",
    "io.netty.",
    "com.google.",
    "org.apache.",
    "it.unimi.",
    "org.objectweb.",
    "org.slf4j.",
    "joptsimple.",
];

type Frame<'a> = (usize, String, &'a str);

pub(crate) fn suspects<I: ClassIndex + ?Sized>(input: &CrashInput<'_>, mods: &I) -> Diagnosis {
    let (source, text) = if input.crash_report.is_empty() {
        (Source::LatestLog, input.latest_log)
    } else {
        (Source::CrashReport, input.crash_report)
    };
    let found = frames(text, source == Source::LatestLog);

    let mut culprits: Vec<String> = Vec::new();
    let mut lines: Vec<EvidenceLine> = Vec::new();
    for (number, class, raw) in &found {
        if NOT_MODS.iter().any(|prefix| class.starts_with(prefix)) {
            continue;
        }
        let owners = mods.owners(class);
        if owners.is_empty() {
            continue;
        }
        for owner in owners {
            if !culprits.contains(&owner) {
                culprits.push(owner);
            }
        }
        push_evidence(&mut lines, evidence(source, *number, raw));
    }

    let (level, message_key, action_key) = match culprits.len() {
        0 => (
            Level::Undetermined,
            "crash-undetermined",
            "crash-action-export-report",
        ),
        1 => (
            Level::SingleMod,
            "crash-suspect-single",
            "crash-action-disable-mod",
        ),
        _ => (
            Level::SeveralMods,
            "crash-suspect-several",
            "crash-action-find-culprit",
        ),
    };
    Diagnosis {
        level,
        signature: None,
        culprit_mods: culprits,
        message_key: message_key.to_owned(),
        action_key: Some(action_key.to_owned()),
        args: BTreeMap::new(),
        evidence_lines: lines,
    }
}

/// Stack frames as (line number, class name, raw line). For a log only the last trace is used,
/// because a log holds many unrelated, harmless traces.
fn frames(text: &str, last_block_only: bool) -> Vec<Frame<'_>> {
    let mut blocks: Vec<Vec<Frame<'_>>> = Vec::new();
    let mut in_block = false;
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("at ") {
            if !in_block {
                blocks.push(Vec::new());
                in_block = true;
            }
            if let (Some(class), Some(block)) = (class_of(rest), blocks.last_mut()) {
                block.push((index + 1, class, line));
            }
        } else if in_block && !is_trace_continuation(trimmed) {
            in_block = false;
        }
    }
    if last_block_only {
        blocks.pop().unwrap_or_default()
    } else {
        blocks.into_iter().flatten().collect()
    }
}

fn is_trace_continuation(trimmed: &str) -> bool {
    trimmed.starts_with("Caused by:")
        || trimmed.starts_with("Suppressed:")
        || (trimmed.starts_with("...") && trimmed.contains("more"))
}

/// The class of a frame `[loader/]pkg.Class.method(File.java:1)`.
fn class_of(rest: &str) -> Option<String> {
    let call = rest.split('(').next()?;
    // Drop module and class loader prefixes such as `java.base/` or `TRANSFORMER/mod@1.0/`.
    let call = call.rsplit('/').next()?;
    let (class, _method) = call.rsplit_once('.')?;
    (!class.is_empty()).then(|| class.to_owned())
}
