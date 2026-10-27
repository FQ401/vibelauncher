//! OS, architecture and feature rules, evaluated for a platform.
//!
//! This is the stable rule API shared by libraries, natives and arguments. Rules appear in a
//! version JSON as `"rules": [{"action": "allow", "os": {...}, "features": {...}}, ...]`.
//!
//! # Semantics
//!
//! [`allows`] follows the official launcher:
//!
//! - An empty rule list allows.
//! - Otherwise the result starts as *disallowed*; every rule that [applies](Rule::applies), in
//!   order, sets it to its action; the last applicable rule wins.
//! - A rule applies when all its conditions hold. Absent conditions hold.
//! - `os.name` compares with the platform's OS (`windows`, `osx`, `linux`); an unknown name never
//!   matches. `os.arch` compares with the platform's architecture (`x86` is 32-bit x86 only).
//! - `os.version` is a regular expression over the OS version; it holds only when
//!   [`RuleContext::os_version`] is known and the (small, documented) pattern subset matches it.
//!   A pattern outside the subset never matches.
//! - `features` holds when every listed feature has, in [`RuleContext::features`], exactly the
//!   listed boolean (an unset feature is `false`).
//! - A rule with an unknown `action` never applies.
//!
//! # Use
//!
//! ```
//! use vl_core::platform::{Arch, Os, Platform};
//! use vl_meta::rules::{allows, Features, Rule, RuleContext};
//!
//! let rules: Vec<Rule> = serde_json::from_str(
//!     r#"[{"action":"allow"},{"action":"disallow","os":{"name":"osx"}}]"#).unwrap();
//! let features = Features::new();
//! let mac = RuleContext::new(Platform { os: Os::MacOs, arch: Arch::Aarch64 }, &features);
//! let linux = RuleContext::new(Platform { os: Os::Linux, arch: Arch::X86_64 }, &features);
//! assert!(!allows(&rules, &mac));
//! assert!(allows(&rules, &linux));
//! ```

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use vl_core::platform::{Arch, Os, Platform};

/// Feature names used by Mojang rules.
pub mod feature {
    /// The demo mode.
    pub const DEMO: &str = "is_demo_user";
    /// A custom window resolution is set (`--width`, `--height`).
    pub const CUSTOM_RESOLUTION: &str = "has_custom_resolution";
    /// The game version supports quick play (`--quickPlay*` arguments).
    pub const QUICK_PLAYS_SUPPORT: &str = "has_quick_plays_support";
    /// Quick play joins a world.
    pub const QUICK_PLAY_SINGLEPLAYER: &str = "is_quick_play_singleplayer";
    /// Quick play joins a server.
    pub const QUICK_PLAY_MULTIPLAYER: &str = "is_quick_play_multiplayer";
    /// Quick play joins a Realm.
    pub const QUICK_PLAY_REALMS: &str = "is_quick_play_realms";
}

/// The set of enabled features. Features not in the set are `false`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Features(BTreeSet<String>);

impl Features {
    /// No feature enabled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enables `name` (see [`feature`]); returns `self` for chaining.
    #[must_use]
    pub fn with(mut self, name: &str) -> Self {
        self.0.insert(name.to_owned());
        self
    }

    /// Whether `name` is enabled.
    pub fn is_enabled(&self, name: &str) -> bool {
        self.0.contains(name)
    }
}

/// Everything a rule is evaluated against.
#[derive(Debug, Clone, Copy)]
pub struct RuleContext<'a> {
    /// The OS and architecture the game runs on.
    pub platform: Platform,
    /// The OS version string (e.g. `10.0`, `14.5`), when known.
    pub os_version: Option<&'a str>,
    /// The enabled features.
    pub features: &'a Features,
}

impl<'a> RuleContext<'a> {
    /// A context with an unknown OS version.
    pub fn new(platform: Platform, features: &'a Features) -> Self {
        Self {
            platform,
            os_version: None,
            features,
        }
    }

    /// The same context with a known OS version.
    #[must_use]
    pub fn with_os_version(mut self, version: &'a str) -> Self {
        self.os_version = Some(version);
        self
    }
}

/// What a rule does when it applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Allows.
    Allow,
    /// Disallows.
    Disallow,
    /// An action this launcher does not know; the rule never applies.
    #[serde(other)]
    Unknown,
}

/// The `os` condition of a rule. Fields are kept as written upstream.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsCondition {
    /// `windows`, `osx` or `linux`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `x86`, `x86_64`, `arm64`, ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    /// A regular expression over the OS version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// One rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// The action when the rule applies.
    pub action: Action,
    /// The OS condition, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<OsCondition>,
    /// Required feature values, if any.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub features: BTreeMap<String, bool>,
}

impl OsCondition {
    /// Whether the condition holds in `ctx`.
    pub fn matches(&self, ctx: &RuleContext<'_>) -> bool {
        if let Some(name) = &self.name {
            match Os::from_mojang_name(name) {
                Some(os) if os == ctx.platform.os => {}
                _ => return false,
            }
        }
        if let Some(arch) = &self.arch {
            match Arch::from_mojang_name(arch) {
                Some(a) if a == ctx.platform.arch => {}
                _ => return false,
            }
        }
        if let Some(pattern) = &self.version {
            match ctx.os_version {
                Some(v) if regex_find(pattern, v) => {}
                _ => return false,
            }
        }
        true
    }
}

impl Rule {
    /// Whether all conditions of the rule hold in `ctx`.
    pub fn applies(&self, ctx: &RuleContext<'_>) -> bool {
        if self.action == Action::Unknown {
            return false;
        }
        if let Some(os) = &self.os
            && !os.matches(ctx)
        {
            return false;
        }
        self.features
            .iter()
            .all(|(name, want)| ctx.features.is_enabled(name) == *want)
    }
}

/// Evaluates a rule list: whether the guarded library or argument is used in `ctx`.
/// See the module documentation for the exact semantics.
pub fn allows(rules: &[Rule], ctx: &RuleContext<'_>) -> bool {
    if rules.is_empty() {
        return true;
    }
    let mut allowed = false;
    for rule in rules {
        if rule.applies(ctx) {
            allowed = rule.action == Action::Allow;
        }
    }
    allowed
}

#[derive(Clone, Copy)]
enum Atom {
    Any,
    Digit,
    Lit(char),
}

impl Atom {
    fn accepts(self, c: char) -> bool {
        match self {
            Atom::Any => true,
            Atom::Digit => c.is_ascii_digit(),
            Atom::Lit(l) => l == c,
        }
    }
}

/// Searches `text` for `pattern`, a subset of regular expressions: literals, `.`, `\d`, `\.`
/// (and other escaped punctuation), the quantifiers `*`, `+`, `?` after a single atom, and the
/// anchors `^` and `$`. Anything else (classes, groups, alternation, braces) never matches.
fn regex_find(pattern: &str, text: &str) -> bool {
    let mut items: Vec<(Atom, usize, usize)> = Vec::new(); // atom, min, max repeats
    let mut anchored_start = false;
    let mut anchored_end = false;
    let mut chars = pattern.chars().peekable();
    let mut first = true;
    while let Some(c) = chars.next() {
        let atom = match c {
            '^' if first => {
                anchored_start = true;
                first = false;
                continue;
            }
            '$' if chars.peek().is_none() => {
                anchored_end = true;
                continue;
            }
            '.' => Atom::Any,
            '\\' => match chars.next() {
                Some('d') => Atom::Digit,
                Some(e) if !e.is_ascii_alphanumeric() => Atom::Lit(e),
                _ => return false,
            },
            '[' | ']' | '(' | ')' | '{' | '}' | '|' | '*' | '+' | '?' | '^' | '$' => return false,
            other => Atom::Lit(other),
        };
        first = false;
        let (min, max) = match chars.peek() {
            Some('*') => (0, usize::MAX),
            Some('+') => (1, usize::MAX),
            Some('?') => (0, 1),
            _ => (1, 1),
        };
        if (min, max) != (1, 1) {
            chars.next();
        }
        items.push((atom, min, max));
    }

    let text: Vec<char> = text.chars().collect();
    if anchored_start {
        return match_here(&items, &text, anchored_end);
    }
    (0..=text.len()).any(|i| match_here(&items, text.get(i..).unwrap_or(&[]), anchored_end))
}

/// Backtracking match of `items` at the start of `text`.
fn match_here(items: &[(Atom, usize, usize)], text: &[char], end: bool) -> bool {
    let Some((&(atom, min, max), rest)) = items.split_first() else {
        return !end || text.is_empty();
    };
    let run = text.iter().take_while(|c| atom.accepts(**c)).count();
    let top = run.min(max);
    (min..=top)
        .rev()
        .any(|n| match_here(rest, text.get(n..).unwrap_or(&[]), end))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIN: Platform = Platform {
        os: Os::Windows,
        arch: Arch::X86_64,
    };
    const MAC_ARM: Platform = Platform {
        os: Os::MacOs,
        arch: Arch::Aarch64,
    };
    const LINUX: Platform = Platform {
        os: Os::Linux,
        arch: Arch::X86_64,
    };

    fn rules(json: &str) -> Vec<Rule> {
        serde_json::from_str(json).unwrap()
    }

    fn eval(json: &str, platform: Platform) -> bool {
        let f = Features::new();
        allows(&rules(json), &RuleContext::new(platform, &f))
    }

    #[test]
    fn empty_allows() {
        assert!(eval("[]", LINUX));
    }

    #[test]
    fn allow_everywhere_except_osx() {
        let r = r#"[{"action":"allow"},{"action":"disallow","os":{"name":"osx"}}]"#;
        assert!(eval(r, WIN));
        assert!(eval(r, LINUX));
        assert!(!eval(r, MAC_ARM));
    }

    #[test]
    fn allow_only_on_osx() {
        let r = r#"[{"action":"allow","os":{"name":"osx"}}]"#;
        assert!(eval(r, MAC_ARM));
        assert!(!eval(r, WIN));
        assert!(!eval(r, LINUX));
    }

    #[test]
    fn arch_conditions() {
        let x86 = r#"[{"action":"allow","os":{"arch":"x86"}}]"#;
        assert!(!eval(x86, WIN));
        let win32 = Platform {
            os: Os::Windows,
            arch: Arch::X86,
        };
        assert!(eval(x86, win32));
        let arm = r#"[{"action":"allow","os":{"name":"osx","arch":"arm64"}}]"#;
        assert!(eval(arm, MAC_ARM));
        let mac_intel = Platform {
            os: Os::MacOs,
            arch: Arch::X86_64,
        };
        assert!(!eval(arm, mac_intel));
    }

    #[test]
    fn last_applicable_rule_wins() {
        let r = r#"[{"action":"disallow"},{"action":"allow","os":{"name":"linux"}}]"#;
        assert!(eval(r, LINUX));
        assert!(!eval(r, WIN));
    }

    #[test]
    fn unknown_action_and_os_never_apply() {
        assert!(!eval(r#"[{"action":"maybe"}]"#, LINUX));
        assert!(!eval(
            r#"[{"action":"allow","os":{"name":"solaris"}}]"#,
            LINUX
        ));
        assert!(!eval(
            r#"[{"action":"allow","os":{"arch":"riscv"}}]"#,
            LINUX
        ));
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let r = r#"[{"action":"allow","comment":"x","os":{"name":"linux","extra":1}}]"#;
        assert!(eval(r, LINUX));
    }

    #[test]
    fn features() {
        let r = rules(r#"[{"action":"allow","features":{"is_demo_user":true}}]"#);
        let off = Features::new();
        let on = Features::new().with(feature::DEMO);
        assert!(!allows(&r, &RuleContext::new(WIN, &off)));
        assert!(allows(&r, &RuleContext::new(WIN, &on)));
        let neg = rules(r#"[{"action":"allow","features":{"is_demo_user":false}}]"#);
        assert!(allows(&neg, &RuleContext::new(WIN, &off)));
        assert!(!allows(&neg, &RuleContext::new(WIN, &on)));
        let both = rules(
            r#"[{"action":"allow","features":{"has_custom_resolution":true,"is_demo_user":true}}]"#,
        );
        let one = Features::new().with(feature::CUSTOM_RESOLUTION);
        assert!(!allows(&both, &RuleContext::new(WIN, &one)));
    }

    #[test]
    fn os_version_regex() {
        let r = rules(r#"[{"action":"allow","os":{"name":"windows","version":"^10\\."}}]"#);
        let f = Features::new();
        let ctx = RuleContext::new(WIN, &f);
        assert!(!allows(&r, &ctx), "unknown version does not match");
        assert!(allows(&r, &ctx.with_os_version("10.0")));
        assert!(!allows(&r, &ctx.with_os_version("6.1")));
    }

    #[test]
    fn regex_subset() {
        assert!(regex_find(r"^10\.", "10.0.1"));
        assert!(!regex_find(r"^10\.", "110.0"));
        assert!(regex_find(r"^10\.5\.\d$", "10.5.8"));
        assert!(!regex_find(r"^10\.5\.\d$", "10.5.88"));
        assert!(regex_find(r"^10\.\d+$", "10.12"));
        assert!(regex_find("0", "10"));
        assert!(regex_find(r"^a.*z$", "abcz"));
        assert!(regex_find(r"^ab?c$", "ac"));
        assert!(!regex_find("[0-9]", "5"));
        assert!(!regex_find("(a|b)", "a"));
    }

    #[test]
    fn serde_round_trip() {
        let r = rules(r#"[{"action":"allow","os":{"name":"osx"},"features":{"x":true}}]"#);
        let back: Vec<Rule> = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(r, back);
    }
}
