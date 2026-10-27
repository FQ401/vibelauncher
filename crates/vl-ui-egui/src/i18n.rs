//! Localization with Fluent. English is built in; another language is added as `.ftl` text by
//! the host app (this crate reads no files).

use fluent_bundle::{FluentArgs, FluentResource, FluentValue, bundle::FluentBundle};
use intl_memoizer::concurrent::IntlLangMemoizer;
use unic_langid::LanguageIdentifier;

const EN: &str = include_str!("../locales/en/vibelauncher.ftl");

type Bundle = FluentBundle<FluentResource, IntlLangMemoizer>;

/// Message lookup: the chosen language first, then English, then the message id itself.
pub struct I18n {
    primary: Option<Bundle>,
    fallback: Bundle,
}

fn bundle(lang: &str, source: &str) -> Option<Bundle> {
    let id: LanguageIdentifier = lang.parse().ok()?;
    // A resource with errors still yields every message that parsed.
    let resource = match FluentResource::try_new(source.to_owned()) {
        Ok(r) => r,
        Err((r, _)) => r,
    };
    let mut bundle = FluentBundle::new_concurrent(vec![id]);
    bundle.set_use_isolating(false);
    // Duplicate ids are ignored: the first definition wins.
    let _ = bundle.add_resource(resource);
    Some(bundle)
}

impl Default for I18n {
    fn default() -> Self {
        let fallback = bundle("en", EN).unwrap_or_else(|| FluentBundle::new_concurrent(Vec::new()));
        Self {
            primary: None,
            fallback,
        }
    }
}

impl I18n {
    /// Uses `ftl` (Fluent source) for `lang` (a BCP 47 tag such as `ru`); English still
    /// answers for messages it lacks. Returns whether the tag was valid.
    pub fn set_language(&mut self, lang: &str, ftl: &str) -> bool {
        self.primary = bundle(lang, ftl);
        self.primary.is_some()
    }

    /// The text of message `id`.
    pub fn t(&self, id: &str) -> String {
        self.format(id, None)
    }

    /// The text of message `id` with variables.
    pub fn t_args(&self, id: &str, args: &[(&str, FluentValue<'_>)]) -> String {
        let mut fluent_args = FluentArgs::new();
        for (name, value) in args {
            fluent_args.set(*name, value.clone());
        }
        self.format(id, Some(&fluent_args))
    }

    fn format(&self, id: &str, args: Option<&FluentArgs<'_>>) -> String {
        for bundle in self.primary.iter().chain(std::iter::once(&self.fallback)) {
            let Some(pattern) = bundle.get_message(id).and_then(|m| m.value()) else {
                continue;
            };
            let mut errors = Vec::new();
            return bundle
                .format_pattern(pattern, args, &mut errors)
                .into_owned();
        }
        id.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_answers_and_unknown_ids_echo() {
        let i18n = I18n::default();
        assert_eq!(i18n.t("action-play"), "Play");
        assert_eq!(i18n.t("no-such-message"), "no-such-message");
        assert_eq!(
            i18n.t_args(
                "time-hours-minutes",
                &[("hours", 2.into()), ("minutes", 5.into())]
            ),
            "2 h 5 min"
        );
    }

    #[test]
    fn another_language_overrides_and_falls_back() {
        let mut i18n = I18n::default();
        assert!(i18n.set_language("xx", "action-play = Go\n"));
        assert_eq!(i18n.t("action-play"), "Go");
        assert_eq!(i18n.t("action-stop"), "Stop");
    }
}
