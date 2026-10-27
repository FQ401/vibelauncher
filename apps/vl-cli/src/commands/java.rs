//! `java …`.

use vl_core::java::JavaCommand;
use vl_core::platform::Arch;
use vl_core::{Command, Facade, Response};

use super::{CliError, Out, USAGE, call, unexpected};

pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    match args {
        ["list"] => match call(facade, Command::Java(JavaCommand::List))? {
            Response::Javas(javas) => {
                if javas.is_empty() {
                    writeln!(out, "no Java runtimes found")?;
                }
                for j in javas {
                    writeln!(
                        out,
                        "{:<3} {:<14} {:<8} {:<24} {}{}",
                        j.major,
                        j.version,
                        arch_name(j.arch),
                        j.vendor,
                        j.path.display(),
                        if j.managed { "  [managed]" } else { "" }
                    )?;
                }
                Ok(())
            }
            other => Err(unexpected(&other)),
        },
        _ => Err(USAGE.into()),
    }
}

fn arch_name(arch: Arch) -> &'static str {
    match arch {
        Arch::X86 => "x86",
        Arch::X86_64 => "x86_64",
        Arch::Aarch64 => "aarch64",
        Arch::Arm => "arm",
        _ => "other",
    }
}
