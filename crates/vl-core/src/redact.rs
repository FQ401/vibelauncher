//! Masking of secrets and personal details in text that leaves the launcher's memory: the
//! launcher log files and game logs shared on a paste service.
//!
//! [`Redactor::redact`] replaces, with [`MASK`]:
//!
//! - JSON Web Tokens (`eyJ…​.eyJ…​.…`, up to five segments), which is the form of Minecraft access
//!   tokens and Xbox Live tokens;
//! - the credentials of `Bearer`, `Basic` and `XBL3.0 x=…;…` authorization values;
//! - the value after a secret-looking name: `access_token`, `refreshToken`, `id_token`,
//!   `api_key`, `x-api-key`, `client_secret`, `password`, `token` and the like, followed by `=`
//!   or `:` (JSON and query string forms included), and the value after `--accessToken` or
//!   `--session` on a command line;
//! - CurseForge API keys (`$2a$10$…`) and Microsoft account tokens (`M.C…_…​.…`, `M.R3_…​.…`);
//! - the exact secrets it was given ([`Redactor::with_secret`]), such as the session's access token;
//! - optionally the user's home folder ([`Redactor::with_home`]), replaced by `~`.
//!
//! Masking is best effort over known forms: code still never puts a secret into a message on
//! purpose (see [`Secret`](crate::secret::Secret)).

use std::borrow::Cow;
use std::sync::OnceLock;

use regex::{Captures, Regex};

/// What a masked value is replaced with.
pub const MASK: &str = "<redacted>";

/// The shortest exact secret that is masked; shorter values would mask ordinary words.
const MIN_SECRET_LEN: usize = 8;

/// A pattern and how a match is replaced (`$name` refers to its groups).
struct Rule {
    pattern: &'static str,
    replacement: &'static str,
}

const RULES: &[Rule] = &[
    // JSON Web Tokens and the five-segment JWE form of XSTS tokens.
    Rule {
        pattern: r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}(?:\.[A-Za-z0-9_-]*){0,3}",
        replacement: MASK,
    },
    // `XBL3.0 x=<user hash>;<token>`.
    Rule {
        pattern: r"(?i)\bXBL3\.0\s+x=[^\s;]+;[^\s,'`]+",
        replacement: "XBL3.0 x=<redacted>",
    },
    // `Bearer <token>`.
    Rule {
        pattern: r"(?i)\b(?P<scheme>bearer)\s+[A-Za-z0-9._~+/=-]{8,}",
        replacement: "$scheme <redacted>",
    },
    // `Authorization: Basic <credentials>` ("basic" alone is an ordinary word).
    Rule {
        pattern: r#"(?i)(?P<head>\bauthorization["']?\s*[:=]\s*["']?basic)\s+[A-Za-z0-9+/=]{4,}"#,
        replacement: "$head <redacted>",
    },
    // `name=value`, `name: value`, `"name":"value"`.
    Rule {
        pattern: r#"(?i)(?P<name>\b(?:(?:access|refresh|id|auth|session|client|bearer|user)[_-]?token|token|api[_-]?key|x-api-key|client[_-]?secret|secret|password|passwd))(?P<sep>["']?\s*[:=]\s*["']?)[^\s"'&,;<>{}\[\]]{4,}"#,
        replacement: "$name$sep<redacted>",
    },
    // `--accessToken <token>`, `--session <token>` on a game command line.
    Rule {
        pattern: r"(?i)(?P<flag>--(?:accessToken|session))\s+[^\s<]\S*",
        replacement: "$flag <redacted>",
    },
    // CurseForge API keys.
    Rule {
        pattern: r"\$2[aby]\$\d{2}\$[./A-Za-z0-9]{53}",
        replacement: MASK,
    },
    // Microsoft account access and refresh tokens.
    Rule {
        pattern: r"\bM\.[A-Z][A-Za-z0-9]*_[A-Za-z0-9]+\.[A-Za-z0-9!*$._~-]{16,}",
        replacement: MASK,
    },
];

fn rules() -> &'static [(Regex, &'static str)] {
    static COMPILED: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    COMPILED.get_or_init(|| {
        RULES
            .iter()
            .filter_map(|r| Regex::new(r.pattern).ok().map(|re| (re, r.replacement)))
            .collect()
    })
}

/// Masks secrets, and optionally the home folder, in text. Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    secrets: Vec<String>,
    home: Option<Regex>,
}

impl Redactor {
    /// A redactor of the known secret forms only.
    pub fn new() -> Self {
        Self::default()
    }

    /// Also masks `secret` wherever it appears; values shorter than eight characters are ignored.
    #[must_use]
    pub fn with_secret(mut self, secret: &str) -> Self {
        if secret.len() >= MIN_SECRET_LEN && !self.secrets.iter().any(|s| s == secret) {
            self.secrets.push(secret.to_owned());
        }
        self
    }

    /// Also replaces the home folder `home` with `~`: as written, with the other kind of
    /// separator, and with backslashes doubled as in JSON. `ignore_case` matches it in any case
    /// (Windows and macOS file systems). A path of fewer than three characters is ignored, and
    /// a longer folder that only starts with the same characters (`/home/al` in `/home/alice`) is
    /// not touched.
    #[must_use]
    pub fn with_home(mut self, home: &str, ignore_case: bool) -> Self {
        let home = home.trim_end_matches(['/', '\\']);
        if home.chars().count() < 3 {
            return self;
        }
        let mut forms = vec![
            home.to_owned(),
            home.replace('\\', "/"),
            home.replace('/', "\\"),
            home.replace('\\', "\\\\"),
            home.replace('/', "\\\\"),
        ];
        forms.sort();
        forms.dedup();
        // Longer forms first, so the doubled-backslash form wins over its prefix.
        forms.sort_by_key(|f| std::cmp::Reverse(f.len()));
        let alternatives: Vec<String> = forms.iter().map(|f| regex::escape(f)).collect();
        let flags = if ignore_case { "(?i)" } else { "" };
        let pattern = format!(
            r"{flags}(?:{})(?P<end>[^A-Za-z0-9_.\-]|$)",
            alternatives.join("|")
        );
        self.home = Regex::new(&pattern).ok();
        self
    }

    /// Whether the home folder is masked.
    pub fn masks_home(&self) -> bool {
        self.home.is_some()
    }

    /// `text` with every secret masked; borrowed when nothing matched.
    pub fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let mut out = Cow::Borrowed(text);
        for secret in &self.secrets {
            if out.contains(secret.as_str()) {
                out = Cow::Owned(out.replace(secret.as_str(), MASK));
            }
        }
        for (re, replacement) in rules() {
            if let Cow::Owned(replaced) = re.replace_all(&out, *replacement) {
                out = Cow::Owned(replaced);
            }
        }
        if let Some(home) = &self.home
            && let Cow::Owned(replaced) =
                home.replace_all(&out, |c: &Captures<'_>| format!("~{}", &c["end"]))
        {
            out = Cow::Owned(replaced);
        }
        out
    }
}

/// `text` with the known secret forms masked (no exact secrets, home folder kept).
pub fn redact(text: &str) -> Cow<'_, str> {
    static PLAIN: OnceLock<Redactor> = OnceLock::new();
    PLAIN.get_or_init(Redactor::new).redact(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic values only: shaped like real tokens, never issued by anyone.
    const JWT: &str = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJzeW50aGV0aWMifQ.c3ludGhldGljLXNpZ25hdHVyZQ";
    const CF_KEY: &str = "$2a$10$abcdefghijklmnopqrstuvABCDEFGHIJKLMNOPQRSTUVWXYZ01234";

    fn masked(text: &str) -> String {
        redact(text).into_owned()
    }

    #[test]
    fn the_known_rule_patterns_all_compile() {
        assert_eq!(rules().len(), RULES.len());
    }

    #[test]
    fn tokens_are_masked_in_every_known_form() {
        let cases = [
            (format!("token {JWT} expired"), "token <redacted> expired"),
            (
                format!("Authorization: Bearer {JWT}"),
                "Authorization: Bearer <redacted>",
            ),
            (
                "authorization: bearer abc.DEF-ghi_jkl".to_owned(),
                "authorization: bearer <redacted>",
            ),
            (
                "Authorization: XBL3.0 x=1234567890;synthetic.token-value".to_owned(),
                "Authorization: XBL3.0 x=<redacted>",
            ),
            (
                r#"{"access_token":"synthetic-access-value","expires_in":3600}"#.to_owned(),
                r#"{"access_token":"<redacted>","expires_in":3600}"#,
            ),
            (
                r#"{"refreshToken": "synthetic-refresh"}"#.to_owned(),
                r#"{"refreshToken": "<redacted>"}"#,
            ),
            (
                "grant_type=refresh_token&refresh_token=M.C123_BAY.synthetic&client_id=x"
                    .to_owned(),
                "grant_type=refresh_token&refresh_token=<redacted>&client_id=x",
            ),
            (
                "Authorization: Basic c3ludGhldGljOnZhbHVl".to_owned(),
                "Authorization: Basic <redacted>",
            ),
            (
                "x-api-key: synthetic-key-value".to_owned(),
                "x-api-key: <redacted>",
            ),
            ("API_KEY=abcd1234".to_owned(), "API_KEY=<redacted>"),
            (format!("key {CF_KEY} set"), "key <redacted> set"),
            (
                "refresh M.R3_BAY.-CRsyntheticRefreshValue!* done".to_owned(),
                "refresh <redacted> done",
            ),
            (
                "--username Steve --accessToken synthetic123 --version 1.20".to_owned(),
                "--username Steve --accessToken <redacted> --version 1.20",
            ),
            ("password=hunter2hunter2".to_owned(), "password=<redacted>"),
        ];
        for (input, expected) in cases {
            assert_eq!(masked(&input), expected, "input: {input}");
        }
    }

    #[test]
    fn ordinary_text_is_left_alone() {
        for text in [
            "[12:00:00] [Render thread/INFO]: Setting user: Steve",
            "Loading 120 mods: fabric-api 0.92.0, sodium 0.5.3",
            "tokenizer=fast, tokens: 12",
            "Basic configuration done",
            "the session ended",
            "java.lang.NullPointerException: Cannot invoke \"Object.toString()\"",
            "",
        ] {
            assert!(
                matches!(redact(text), Cow::Borrowed(_)),
                "changed: {text} -> {}",
                redact(text)
            );
        }
    }

    #[test]
    fn exact_secrets_are_masked_and_short_ones_ignored() {
        let r = Redactor::new()
            .with_secret("plain-synthetic-secret")
            .with_secret("short");
        assert_eq!(
            r.redact("a plain-synthetic-secret b short"),
            "a <redacted> b short"
        );
    }

    #[test]
    fn the_home_folder_becomes_a_tilde_in_every_spelling() {
        let r = Redactor::new().with_home(r"C:\Users\Alice", true);
        assert!(r.masks_home());
        assert_eq!(
            r.redact(r"C:\Users\Alice\AppData\x.log and c:/users/alice/y and C:\\Users\\Alice\\z"),
            r"~\AppData\x.log and ~/y and ~\\z"
        );
        assert_eq!(r.redact(r"C:\Users\Alice"), "~");
        assert_eq!(r.redact(r"C:\Users\Alicette\x"), r"C:\Users\Alicette\x");

        let unix = Redactor::new().with_home("/home/al/", false);
        assert_eq!(
            unix.redact("/home/al/.minecraft /home/alice /HOME/al/x"),
            "~/.minecraft /home/alice /HOME/al/x"
        );
        assert!(!Redactor::new().with_home("/", false).masks_home());
    }
}
