//! Everything between "launch this instance" and "start the process".

use std::fs;
use std::path::Path;

use vl_core::java::JavaRuntime;
use vl_core::settings::Settings;
use vl_core::{Error, ErrorKind, Result};

use crate::command::{self, CommandInput, CommandLine};
use crate::install::{Installed, Installer};
use crate::java::{self, JavaChoice};
use crate::job::Job;
use crate::memory;
use crate::session::LaunchSession;

/// What a launch needs from the caller.
#[derive(Debug, Clone)]
pub struct PrepareInput<'a> {
    /// The game version id to install and launch.
    pub version: &'a str,
    /// The instance's game directory (created when missing).
    pub game_dir: &'a Path,
    /// The player's session. It is a parameter on purpose: the caller decides where it comes
    /// from.
    pub session: &'a LaunchSession,
    /// The effective settings of the instance.
    pub settings: &'a Settings,
    /// The runtimes found on this machine.
    pub runtimes: &'a [JavaRuntime],
    /// A runtime the user fixed (`settings.java.path`, already checked by the caller).
    pub fixed_java: Option<&'a JavaRuntime>,
    /// `${launcher_name}`.
    pub launcher_name: &'a str,
    /// `${launcher_version}`.
    pub launcher_version: &'a str,
}

/// A launch that is ready to start.
#[derive(Debug, Clone)]
pub struct Prepared {
    /// The command line.
    pub command: CommandLine,
    /// The chosen Java.
    pub java: JavaChoice,
    /// The installed version.
    pub installed: Installed,
    /// Warnings for the user: memory, an unexpected Java version, platform notes.
    pub warnings: Vec<String>,
}

/// Installs the version, chooses Java, checks the memory and builds the command line.
///
/// # Errors
///
/// Everything [`Installer::install`] reports, `Java` when no runtime fits, `Io` when the game
/// directory cannot be created, `InvalidInput` for unusable settings or session.
pub fn prepare(installer: &Installer, input: &PrepareInput<'_>, job: &Job) -> Result<Prepared> {
    let installed = installer.install(input.version, Some(input.game_dir), job)?;
    job.check()?;
    fs::create_dir_all(input.game_dir).map_err(|e| {
        Error::new(
            ErrorKind::Io,
            format!("{}: {e}", input.game_dir.display()),
        )
    })?;
    let required = installed.version.java_version_or_default().major_version;
    let arch = installed.platform.arch;
    let java = match input.fixed_java {
        Some(runtime) => java::check_fixed(runtime, required, arch)?,
        None => java::choose(input.runtimes, required, arch)?,
    };
    let mut warnings = installed.warnings.clone();
    if !java.exact {
        warnings.push(format!(
            "the game asks for Java {required}, Java {} is used",
            java.runtime.major
        ));
    }
    if let Some(warning) = memory::check(input.settings.memory.max_mib, memory::status()) {
        warnings.push(warning.to_string());
    }
    let command = command::build(&CommandInput {
        installed: &installed,
        layout: installer.layout(),
        java: &java.runtime.path,
        game_dir: input.game_dir,
        session: input.session,
        memory: input.settings.memory,
        window: input.settings.window,
        extra_jvm_args: &input.settings.java.extra_args,
        launcher_name: input.launcher_name,
        launcher_version: input.launcher_version,
    })?;
    Ok(Prepared {
        command,
        java,
        installed,
        warnings,
    })
}
