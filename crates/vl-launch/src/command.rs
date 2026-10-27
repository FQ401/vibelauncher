//! Building the game's command line.
//!
//! Order of arguments: `-Xms`/`-Xmx` from the memory settings, the user's JVM arguments (later
//! flags win in the JVM, so they can override the heap), the version's JVM arguments (natives
//! path, class path, platform flags), the logging configuration, the main class, the version's
//! game arguments. `${variables}` are substituted from the session, the installed version and
//! the settings. The program is started with an argument array, never through a shell.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use vl_core::settings::{MemorySettings, WindowSettings};
use vl_core::{Error, ErrorKind, Result};
use vl_meta::arguments::{Variables, default_jvm, resolve, substitute};
use vl_meta::assets::AssetLayout;
use vl_meta::libraries::classpath_separator;
use vl_meta::rules::{Features, RuleContext, feature};

use crate::install::Installed;
use crate::layout::Layout;
use crate::session::LaunchSession;

/// Everything the command line is built from.
#[derive(Debug, Clone)]
pub struct CommandInput<'a> {
    /// The installed version.
    pub installed: &'a Installed,
    /// The shared game files.
    pub layout: &'a Layout,
    /// The `java` executable.
    pub java: &'a Path,
    /// The working directory and `${game_directory}`.
    pub game_dir: &'a Path,
    /// The player.
    pub session: &'a LaunchSession,
    /// Heap size.
    pub memory: MemorySettings,
    /// Window size.
    pub window: WindowSettings,
    /// The user's extra JVM arguments.
    pub extra_jvm_args: &'a [String],
    /// Environment variables to set for the game, on top of the launcher's own.
    pub env: &'a BTreeMap<String, String>,
    /// `${launcher_name}`.
    pub launcher_name: &'a str,
    /// `${launcher_version}`.
    pub launcher_version: &'a str,
}

/// A program, its arguments and where to run it.
#[derive(Clone, PartialEq, Eq)]
pub struct CommandLine {
    /// The `java` executable.
    pub program: PathBuf,
    /// The arguments, exactly as the process receives them.
    pub args: Vec<String>,
    /// The working directory.
    pub cwd: PathBuf,
    /// Environment variables added to the launcher's own.
    pub env: BTreeMap<String, String>,
    secrets: Vec<String>,
}

impl CommandLine {
    /// A command line from parts. `secrets` are values to hide in [`redact`](Self::redact).
    pub fn new(program: PathBuf, args: Vec<String>, cwd: PathBuf, secrets: Vec<String>) -> Self {
        Self {
            program,
            args,
            cwd,
            env: BTreeMap::new(),
            secrets,
        }
    }

    /// `text` with every secret of this command line replaced by `***`.
    pub fn redact(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for secret in self.secrets.iter().filter(|s| !s.is_empty()) {
            out = out.replace(secret.as_str(), "***");
        }
        out
    }

    /// The secrets to hide in the game's output.
    pub fn secrets(&self) -> &[String] {
        &self.secrets
    }

    /// The arguments with secrets hidden: safe to log.
    pub fn redacted_args(&self) -> Vec<String> {
        self.args.iter().map(|a| self.redact(a)).collect()
    }

    /// One printable line, secrets hidden. Arguments with spaces are quoted.
    pub fn display_redacted(&self) -> String {
        let mut line = quote(&self.program.to_string_lossy());
        for arg in self.redacted_args() {
            line.push(' ');
            line.push_str(&quote(&arg));
        }
        line
    }
}

impl std::fmt::Debug for CommandLine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandLine")
            .field("program", &self.program)
            .field("args", &self.redacted_args())
            .field("cwd", &self.cwd)
            .finish()
    }
}

fn quote(arg: &str) -> String {
    if arg.is_empty() || arg.contains([' ', '"', '\t']) {
        format!("\"{}\"", arg.replace('"', "\\\""))
    } else {
        arg.to_owned()
    }
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// The `${variable}` values of a launch.
fn variables(input: &CommandInput<'_>, classpath: &str) -> Variables {
    let installed = input.installed;
    let session = input.session;
    let assets_root = input.layout.assets_dir();
    let game_assets = match installed.asset_layout {
        AssetLayout::Virtual => assets_root.join("virtual").join(&installed.assets_index_id),
        AssetLayout::Resources => input.game_dir.join("resources"),
        AssetLayout::Objects => assets_root.clone(),
    };
    let token = session.access_token.expose();
    let platform = installed.platform;
    let mut vars = Variables::new()
        .with("auth_player_name", &session.player_name)
        .with("auth_uuid", session.uuid_compact())
        .with("auth_access_token", token)
        .with(
            "auth_session",
            format!("token:{token}:{}", session.uuid_compact()),
        )
        .with("user_type", &session.user_type)
        .with("user_properties", "{}")
        .with("profile_name", &session.player_name)
        .with("version_name", &installed.id)
        .with("version_type", &installed.version.kind)
        .with("game_directory", text(input.game_dir))
        .with("assets_root", text(&assets_root))
        .with("game_assets", text(&game_assets))
        .with("assets_index_name", &installed.assets_index_id)
        .with("natives_directory", text(&installed.natives_dir))
        .with("library_directory", text(&input.layout.libraries_dir()))
        .with("classpath", classpath)
        .with(
            "classpath_separator",
            classpath_separator(platform.os).to_string(),
        )
        .with("primary_jar", text(&installed.client_jar))
        .with("launcher_name", input.launcher_name)
        .with("launcher_version", input.launcher_version)
        .with("resolution_width", input.window.width.to_string())
        .with("resolution_height", input.window.height.to_string());
    if let Some(xuid) = &session.xuid {
        vars = vars.with("auth_xuid", xuid);
    }
    if let Some(client_id) = &session.client_id {
        vars = vars.with("clientid", client_id);
    }
    vars
}

fn classpath(input: &CommandInput<'_>) -> String {
    let installed = input.installed;
    let libraries = input.layout.libraries_dir();
    let mut parts: Vec<String> = installed
        .libraries
        .classpath
        .iter()
        .map(|p| text(&crate::layout::join_relative(&libraries, p)))
        .collect();
    parts.push(text(&installed.client_jar));
    parts.join(&classpath_separator(installed.platform.os).to_string())
}

/// Builds the command line.
///
/// # Errors
///
/// `InvalidInput` for an empty player name or a heap smaller than 64 MiB.
pub fn build(input: &CommandInput<'_>) -> Result<CommandLine> {
    if input.session.player_name.trim().is_empty() {
        return Err(Error::invalid_input("the player name is empty"));
    }
    let MemorySettings {
        min_mib, max_mib, ..
    } = input.memory;
    if max_mib < 64 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("maximum memory of {max_mib} MiB is too small"),
        ));
    }
    let installed = input.installed;
    let classpath = classpath(input);
    let vars = variables(input, &classpath);
    let features = Features::new().with(feature::CUSTOM_RESOLUTION);
    let ctx = RuleContext::new(installed.platform, &features);

    let mut args = vec![
        format!("-Xms{}M", min_mib.min(max_mib)),
        format!("-Xmx{max_mib}M"),
    ];
    args.extend(input.extra_jvm_args.iter().cloned());
    let version_args = &installed.version.arguments;
    let jvm_source = if version_args.jvm.is_empty() {
        default_jvm()
    } else {
        version_args.jvm.clone()
    };
    let jvm = resolve(&jvm_source, &ctx, &vars);
    let has_classpath = jvm.iter().any(|a| a == "-cp" || a == "-classpath");
    let has_natives = jvm.iter().any(|a| a.contains("java.library.path"));
    args.extend(jvm);
    if !has_natives {
        args.push(format!(
            "-Djava.library.path={}",
            text(&installed.natives_dir)
        ));
    }
    if !has_classpath {
        args.push("-cp".to_owned());
        args.push(classpath.clone());
    }
    if let Some(logging) = &installed.logging {
        let path_vars = Variables::new().with("path", text(&logging.path));
        args.push(substitute(&logging.argument, &path_vars));
    }
    args.push(installed.version.main_class.clone());
    args.extend(resolve(&version_args.game, &ctx, &vars));

    let secrets = if input.session.access_token.is_empty() {
        Vec::new()
    } else {
        vec![input.session.access_token.expose().to_owned()]
    };
    Ok(CommandLine {
        program: input.java.to_path_buf(),
        args,
        cwd: input.game_dir.to_path_buf(),
        env: input.env.clone(),
        secrets,
    })
}
