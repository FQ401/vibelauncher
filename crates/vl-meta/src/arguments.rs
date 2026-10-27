//! Game and JVM arguments: `minecraftArguments` (legacy) and `arguments` with rules.
//!
//! Both upstream formats normalize to [`Arguments`]: a list of [`Argument`]s for the game and a
//! list for the JVM. A legacy `minecraftArguments` string becomes plain game arguments plus the
//! default JVM arguments ([`default_jvm`]), since old version JSONs carry none.
//! [`resolve`] applies the rules of a [`RuleContext`] and substitutes `${variables}`.

use std::collections::BTreeMap;

use serde::de::{self, Deserializer};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};

use crate::rules::{Rule, RuleContext, allows};

/// One argument entry: one or more values, used only when its rules allow.
///
/// Upstream writes either a plain string or `{"rules": [...], "value": "x" | ["x", ...]}`; both
/// parse to this type, which serializes back to the same shapes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Argument {
    /// The rules guarding the values; empty means always used.
    pub rules: Vec<Rule>,
    /// The values, in order. May contain `${variables}`.
    pub values: Vec<String>,
}

impl Argument {
    /// An unconditional single-value argument.
    pub fn plain(value: impl Into<String>) -> Self {
        Self {
            rules: Vec::new(),
            values: vec![value.into()],
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawArgument {
    Plain(String),
    Ruled {
        #[serde(default)]
        rules: Vec<Rule>,
        value: OneOrMany,
    },
}

impl<'de> Deserialize<'de> for Argument {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match RawArgument::deserialize(d).map_err(|_| {
            de::Error::custom("an argument must be a string or an object with `rules` and `value`")
        })? {
            RawArgument::Plain(s) => Ok(Self::plain(s)),
            RawArgument::Ruled { rules, value } => Ok(Self {
                rules,
                values: match value {
                    OneOrMany::One(s) => vec![s],
                    OneOrMany::Many(v) => v,
                },
            }),
        }
    }
}

impl Serialize for Argument {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Ruled<'a> {
            rules: &'a [Rule],
            value: &'a [String],
        }
        match (self.rules.is_empty(), self.values.as_slice()) {
            (true, [one]) => s.serialize_str(one),
            _ => Ruled {
                rules: &self.rules,
                value: &self.values,
            }
            .serialize(s),
        }
    }
}

/// The game and JVM arguments of a version.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Arguments {
    /// Arguments passed to the game after the main class.
    #[serde(default)]
    pub game: Vec<Argument>,
    /// Arguments passed to the JVM before the main class.
    #[serde(default)]
    pub jvm: Vec<Argument>,
}

impl Arguments {
    /// Arguments from a legacy `minecraftArguments` string (split on whitespace), with the
    /// [`default_jvm`] arguments.
    pub fn from_legacy(minecraft_arguments: &str) -> Self {
        Self {
            game: minecraft_arguments
                .split_whitespace()
                .map(Argument::plain)
                .collect(),
            jvm: default_jvm(),
        }
    }

    /// Appends `other`'s arguments after these (used when a version inherits another).
    pub fn extend(&mut self, other: Arguments) {
        self.game.extend(other.game);
        self.jvm.extend(other.jvm);
    }
}

/// The JVM arguments every launch needs when the version JSON has none (legacy format):
/// natives path, launcher brand and version, class path.
pub fn default_jvm() -> Vec<Argument> {
    [
        "-Djava.library.path=${natives_directory}",
        "-Dminecraft.launcher.brand=${launcher_name}",
        "-Dminecraft.launcher.version=${launcher_version}",
        "-cp",
        "${classpath}",
    ]
    .into_iter()
    .map(Argument::plain)
    .collect()
}

/// Values for `${name}` placeholders.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Variables(BTreeMap<String, String>);

impl Variables {
    /// No variables.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets `name` to `value`; returns `self` for chaining.
    #[must_use]
    pub fn with(mut self, name: &str, value: impl Into<String>) -> Self {
        self.0.insert(name.to_owned(), value.into());
        self
    }

    /// The value of `name`.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }
}

/// Replaces every `${name}` in `template` with its value from `vars`.
///
/// A placeholder with no value, or one that is not closed, stays as written. Substituted values
/// are not scanned again, so a value containing `${...}` is never expanded.
pub fn substitute(template: &str, vars: &Variables) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("${") {
        out.push_str(rest.get(..start).unwrap_or_default());
        let after = rest.get(start + 2..).unwrap_or_default();
        match after.find('}') {
            Some(end) => {
                let name = after.get(..end).unwrap_or_default();
                match vars.get(name) {
                    Some(value) => out.push_str(value),
                    None => {
                        out.push_str("${");
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = after.get(end + 1..).unwrap_or_default();
            }
            None => {
                out.push_str(rest.get(start..).unwrap_or_default());
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// The final argument strings: entries whose rules [allow](allows) in `ctx`, in order, with
/// `${variables}` substituted.
pub fn resolve(args: &[Argument], ctx: &RuleContext<'_>, vars: &Variables) -> Vec<String> {
    args.iter()
        .filter(|a| allows(&a.rules, ctx))
        .flat_map(|a| a.values.iter().map(|v| substitute(v, vars)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{Features, feature};
    use vl_core::platform::{Arch, Os, Platform};

    const WIN: Platform = Platform {
        os: Os::Windows,
        arch: Arch::X86_64,
    };
    const MAC_ARM: Platform = Platform {
        os: Os::MacOs,
        arch: Arch::Aarch64,
    };

    fn parse(json: &str) -> Arguments {
        serde_json::from_str(json).unwrap()
    }

    const MODERN: &str = r#"{
      "game": [
        "--username", "${auth_player_name}",
        {"rules": [{"action": "allow", "features": {"is_demo_user": true}}], "value": "--demo"},
        {"rules": [{"action": "allow", "features": {"has_custom_resolution": true}}],
         "value": ["--width", "${resolution_width}", "--height", "${resolution_height}"]}
      ],
      "jvm": [
        {"rules": [{"action": "allow", "os": {"name": "osx"}}], "value": ["-XstartOnFirstThread"]},
        {"rules": [{"action": "allow", "os": {"name": "windows"}}],
         "value": "-XX:HeapDumpPath=MojangTricksIntelDriversForPerformance_javaw.exe_minecraft.exe.heapdump"},
        "-Djava.library.path=${natives_directory}",
        "-cp", "${classpath}"
      ],
      "futureField": 1
    }"#;

    fn vars() -> Variables {
        Variables::new()
            .with("auth_player_name", "Steve")
            .with("resolution_width", "854")
            .with("resolution_height", "480")
            .with("natives_directory", "/n")
            .with("classpath", "a.jar:b.jar")
    }

    #[test]
    fn modern_format_per_platform() {
        let a = parse(MODERN);
        let off = Features::new();
        assert_eq!(
            resolve(&a.game, &RuleContext::new(WIN, &off), &vars()),
            ["--username", "Steve"]
        );
        let on = Features::new()
            .with(feature::DEMO)
            .with(feature::CUSTOM_RESOLUTION);
        assert_eq!(
            resolve(&a.game, &RuleContext::new(WIN, &on), &vars()),
            [
                "--username",
                "Steve",
                "--demo",
                "--width",
                "854",
                "--height",
                "480"
            ]
        );
        let mac = resolve(&a.jvm, &RuleContext::new(MAC_ARM, &off), &vars());
        assert_eq!(
            mac,
            [
                "-XstartOnFirstThread",
                "-Djava.library.path=/n",
                "-cp",
                "a.jar:b.jar"
            ]
        );
        let win = resolve(&a.jvm, &RuleContext::new(WIN, &off), &vars());
        assert_eq!(win.len(), 4);
        assert!(win[0].starts_with("-XX:HeapDumpPath"));
    }

    #[test]
    fn legacy_format() {
        let a = Arguments::from_legacy(
            "--username ${auth_player_name}  --version ${version_name}\n--gameDir ${game_directory}",
        );
        assert_eq!(a.game.len(), 6);
        let f = Features::new();
        let v = vars().with("version_name", "1.12.2");
        let game = resolve(&a.game, &RuleContext::new(WIN, &f), &v);
        assert_eq!(game[..4], ["--username", "Steve", "--version", "1.12.2"]);
        assert_eq!(game[5], "${game_directory}", "unknown variable stays");
        let jvm = resolve(&a.jvm, &RuleContext::new(WIN, &f), &v);
        assert!(jvm.contains(&"-Djava.library.path=/n".to_owned()));
        assert!(jvm.contains(&"a.jar:b.jar".to_owned()));
    }

    #[test]
    fn substitution_edge_cases() {
        let v = Variables::new().with("a", "${b}").with("b", "X");
        assert_eq!(substitute("${a}-${b}", &v), "${b}-X", "no rescanning");
        assert_eq!(substitute("pre${a", &v), "pre${a");
        assert_eq!(substitute("${}", &v), "${}");
        assert_eq!(substitute("é${b}é", &v), "éXé");
        assert_eq!(substitute("$${b}", &v), "$X");
    }

    #[test]
    fn serde_round_trip_keeps_shapes() {
        let a = parse(MODERN);
        let back: Arguments = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        assert_eq!(a, back);
        let json = serde_json::to_value(&a).unwrap();
        assert_eq!(json["game"][0], "--username");
    }

    #[test]
    fn malformed_argument_is_an_error() {
        assert!(serde_json::from_str::<Arguments>(r#"{"game":[1]}"#).is_err());
        assert!(serde_json::from_str::<Arguments>(r#"{"game":[{"rules":[]}]}"#).is_err());
        assert!(serde_json::from_str::<Arguments>(r#"{"game":"x"}"#).is_err());
    }
}
