//! `log upload <instance> [--file <path>] [--yes]`: share a game log on mclo.gs.
//!
//! Prints what would be sent (the file, the size, lines left out to fit the service's limits,
//! what is masked), asks, and only then uploads. `--yes` answers the question; without a
//! terminal and without `--yes` nothing is sent.

use std::path::PathBuf;

use vl_core::console::{ConsoleCommand, LogUploadPreview};
use vl_core::{Command, Facade, InstanceId, Response};

use super::catalog::confirmed_with;
use super::{CliError, Out, USAGE, call, unexpected};

pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    let ["upload", instance, flags @ ..] = args else {
        return Err(USAGE.into());
    };
    let instance = InstanceId::new(instance).ok_or(CliError::from(USAGE))?;
    let mut file = None;
    let mut yes = false;
    let mut it = flags.iter();
    while let Some(flag) = it.next() {
        match *flag {
            "--yes" => yes = true,
            "--file" => file = Some(PathBuf::from(it.next().ok_or(CliError::from(USAGE))?)),
            _ => return Err(USAGE.into()),
        }
    }
    let preview = match call(
        facade,
        Command::Console(ConsoleCommand::PreviewUpload {
            instance: instance.clone(),
            file: file.clone(),
        }),
    )? {
        Response::LogUploadPreview(p) => p,
        other => return Err(unexpected(&other)),
    };
    print_preview(&preview, out)?;
    if !yes && !confirmed_with(out, "upload?")? {
        writeln!(out, "not uploaded; add --yes to upload")?;
        return Ok(());
    }
    match call(
        facade,
        Command::Console(ConsoleCommand::Upload { instance, file }),
    )? {
        Response::LogUploaded(uploaded) => {
            if uploaded.preview.omitted_lines != preview.omitted_lines {
                // The log grew between the preview and the upload.
                writeln!(
                    out,
                    "left out: {} lines at upload",
                    uploaded.preview.omitted_lines
                )?;
            }
            writeln!(out, "uploaded: {}", uploaded.url)?;
            if let Some(raw) = &uploaded.raw_url {
                writeln!(out, "raw:      {raw}")?;
            }
            Ok(())
        }
        other => Err(unexpected(&other)),
    }
}

fn print_preview(p: &LogUploadPreview, out: Out<'_>) -> Result<(), CliError> {
    writeln!(out, "file:     {}", p.file.display())?;
    writeln!(
        out,
        "to:       {} (a public link: anyone who has it can read the log)",
        p.service
    )?;
    writeln!(
        out,
        "content:  {} lines, {} (the file has {} lines, {})",
        p.lines,
        size(p.bytes),
        p.file_lines,
        size(p.file_bytes)
    )?;
    if p.truncated() {
        writeln!(
            out,
            "left out: {} lines between the start and the end of the log: it is longer than {} \
             accepts",
            p.omitted_lines, p.service
        )?;
    }
    let home = if p.home_masked {
        ", your home folder as ~"
    } else {
        ""
    };
    writeln!(out, "masked:   tokens, keys and passwords{home}")?;
    Ok(())
}

fn size(bytes: u64) -> String {
    if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_preview_says_what_is_left_out_and_masked() {
        let p = LogUploadPreview {
            file: PathBuf::from("vl-game.log"),
            service: "mclo.gs".into(),
            file_bytes: 20 << 20,
            file_lines: 200_000,
            bytes: 2_560,
            lines: 25_000,
            omitted_lines: 175_001,
            home_masked: true,
        };
        let mut out = Vec::new();
        print_preview(&p, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("25000 lines, 2.5 KiB (the file has 200000 lines, 20.0 MiB)"));
        assert!(text.contains("left out: 175001 lines"), "{text}");
        assert!(text.contains("home folder as ~"), "{text}");
    }

    #[test]
    fn arguments_are_checked() {
        let facade = vl_mock::MockFacade::new(vl_mock::MockOptions::instant());
        let mut out = Vec::new();
        assert!(run(&facade, &["upload"], &mut out).is_err());
        assert!(run(&facade, &["upload", "survival", "--file"], &mut out).is_err());
        assert!(run(&facade, &["upload", "survival", "--bad"], &mut out).is_err());
        assert!(run(&facade, &["send", "survival"], &mut out).is_err());
    }
}
