//! Everything between "launch this instance" and "start the process".

use std::fs;
use std::path::Path;

use vl_core::java::JavaRuntime;
use vl_core::settings::Settings;
use vl_core::{Error, ErrorKind, Result};
use vl_meta::loader::LoaderProfile;

use crate::command::{self, CommandInput, CommandLine};
use crate::install::{Installed, Installer};
use crate::java::{self, JavaChoice};
use crate::job::Job;
use crate::memory;
use crate::session::LaunchSession;

/// The largest heap a 32-bit JVM starts with reliably.
const MAX_HEAP_32_BIT_MIB: u32 = 1024;

/// What a launch needs from the caller.
#[derive(Debug, Clone)]
pub struct PrepareInput<'a> {
    /// The game version id to install and launch.
    pub version: &'a str,
    /// The mod loader's profile (for `version`), if the instance has one.
    pub loader: Option<&'a LoaderProfile>,
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
    let installed =
        installer.install_with(input.version, input.loader, Some(input.game_dir), job)?;
    job.check()?;
    fs::create_dir_all(input.game_dir)
        .map_err(|e| Error::new(ErrorKind::Io, format!("{}: {e}", input.game_dir.display())))?;
    let required = installed.version.java_version_or_default().major_version;
    let arch = installed.platform.arch;
    let java = match input.fixed_java {
        Some(runtime) => java::check_fixed(runtime, required, arch)?,
        None => java::choose(input.runtimes, required, max_java_major(&installed), arch)?,
    };
    let mut warnings = installed.warnings.clone();
    if java.runtime.arch != arch {
        warnings.push(format!(
            "the Java in use is {:?} while this machine runs {arch:?}",
            java.runtime.arch
        ));
    }
    if !java.exact {
        warnings.push(format!(
            "the game asks for Java {required}, Java {} is used",
            java.runtime.major
        ));
    }
    let mut memory_settings = input.settings.memory.clone();
    if java.runtime.arch.bits() == Some(32) && memory_settings.max_mib > MAX_HEAP_32_BIT_MIB {
        warnings.push(format!(
            "a 32-bit Java cannot use {} MiB; the heap is limited to {MAX_HEAP_32_BIT_MIB} MiB",
            memory_settings.max_mib
        ));
        memory_settings.max_mib = MAX_HEAP_32_BIT_MIB;
        memory_settings.min_mib = memory_settings.min_mib.min(MAX_HEAP_32_BIT_MIB);
    }
    if let Some(warning) = memory::check(memory_settings.max_mib, memory::status()) {
        warnings.push(warning.to_string());
    }
    let command = command::build(&CommandInput {
        installed: &installed,
        layout: installer.layout(),
        java: &java.runtime.path,
        game_dir: input.game_dir,
        session: input.session,
        memory: memory_settings,
        window: input.settings.window.clone(),
        extra_jvm_args: &input.settings.java.extra_args,
        env: &input.settings.env.vars,
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

/// The newest Java a version runs on, when it has a limit: games started through LaunchWrapper
/// (versions up to 1.12.2 and old loaders) cast the system class loader to `URLClassLoader`,
/// which Java 9 and later do not allow.
fn max_java_major(installed: &Installed) -> Option<u32> {
    (installed.version.main_class == "net.minecraft.launchwrapper.Launch").then_some(8)
}
