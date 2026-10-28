//! `instances diagnose <instance>`: analyse the newest logs of an instance without launching.

use vl_core::crash::{CrashCause, CrashDiagnosis, DiagnosisLevel, EvidenceSource};
use vl_core::launch::LaunchCommand;
use vl_core::{Command, Facade, InstanceId, Response};

use super::{CliError, Out, USAGE, call, unexpected};

pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    let [instance] = args else {
        return Err(USAGE.into());
    };
    let instance = InstanceId::new(instance).ok_or(CliError::from(USAGE))?;
    match call(
        facade,
        Command::Launch(LaunchCommand::Diagnose { instance }),
    )? {
        Response::Diagnosis(d) => print(&d, out),
        other => Err(unexpected(&other)),
    }
}

fn print(d: &CrashDiagnosis, out: Out<'_>) -> Result<(), CliError> {
    let level = match d.level {
        DiagnosisLevel::Signature => "known problem",
        DiagnosisLevel::SingleMod => "possibly one mod",
        DiagnosisLevel::SeveralMods => "several mods involved",
        DiagnosisLevel::Undetermined => "undetermined",
        _ => "unknown level",
    };
    let cause = match d.cause {
        CrashCause::ModLikely => "mod likely",
        CrashCause::Environment => "environment (Java, memory, driver or game files)",
        CrashCause::Unknown => "unknown",
        _ => "unknown",
    };
    writeln!(out, "result:  {level}")?;
    writeln!(out, "cause:   {cause}")?;
    if let Some(signature) = &d.signature {
        writeln!(out, "signature: {signature}")?;
    }
    writeln!(out, "message: {}", d.message_key)?;
    if let Some(action) = &d.action_key {
        writeln!(out, "action:  {action}")?;
    }
    for (key, value) in &d.args {
        writeln!(out, "  {key} = {value}")?;
    }
    if !d.mods.is_empty() {
        writeln!(out, "mods:    {}", d.mods.join(", "))?;
    }
    if d.loader_reported {
        writeln!(out, "the mod loader named the problem itself")?;
    }
    if d.bisect_useful() {
        writeln!(out, "a mod search (instances bisect start) can help")?;
    }
    if !d.files.is_empty() {
        writeln!(out, "files:   {}", d.files.join(", "))?;
    }
    for line in &d.evidence {
        let source = match line.source {
            EvidenceSource::Log => "log",
            EvidenceSource::CrashReport => "crash report",
            EvidenceSource::HsErr => "JVM error log",
            _ => "file",
        };
        writeln!(out, "  [{source}:{}] {}", line.line, line.text)?;
    }
    Ok(())
}
