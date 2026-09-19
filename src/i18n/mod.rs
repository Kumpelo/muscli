//! Translation.
//!
//! English is the base catalogue and the fallback: a key missing from another
//! language falls back to English rather than showing the raw key, so a partial
//! translation degrades to a readable mix instead of nonsense.
//!
//! Catalogues are plain TOML compiled in with `include_str!`, parsed once on
//! first use. That keeps translations editable as data without a runtime file
//! to locate, and means a malformed catalogue fails the build's tests rather
//! than a user's session.

use std::{
    collections::HashMap,
    sync::{
        OnceLock,
        atomic::{AtomicU8, Ordering},
    },
};

mod detect;

pub use detect::detect_language;

const ENGLISH: &str = include_str!("en.toml");
const SPANISH: &str = include_str!("es.toml");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Language {
    #[default]
    English,
    Spanish,
}

/// The values the `language` setting accepts, in the order the settings view
/// cycles through them. `auto` follows the system locale.
pub const LANGUAGE_CHOICES: [&str; 3] = ["auto", "en", "es"];

impl Language {
    pub fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Spanish => "es",
        }
    }

    /// The language's own name for itself, which is what a language picker
    /// should show: someone looking for Spanish is looking for "Español", not
    /// for whatever the current interface language calls it.
    pub fn label(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Spanish => "Español",
        }
    }

    fn ordinal(self) -> u8 {
        match self {
            Self::English => 0,
            Self::Spanish => 1,
        }
    }

    fn from_ordinal(value: u8) -> Self {
        match value {
            1 => Self::Spanish,
            _ => Self::English,
        }
    }

    /// Parse a language tag such as `es`, `es_ES.UTF-8` or `es-419`.
    pub fn from_tag(tag: &str) -> Option<Self> {
        let primary = tag
            .split(['_', '-', '.', '@'])
            .next()
            .unwrap_or(tag)
            .to_ascii_lowercase();
        match primary.as_str() {
            "en" => Some(Self::English),
            "es" => Some(Self::Spanish),
            _ => None,
        }
    }
}

struct Catalogues {
    english: HashMap<String, String>,
    spanish: HashMap<String, String>,
}

static CATALOGUES: OnceLock<Catalogues> = OnceLock::new();
static ACTIVE: AtomicU8 = AtomicU8::new(0);

fn parse(source: &str, name: &str) -> HashMap<String, String> {
    toml::from_str(source)
        .unwrap_or_else(|error| panic!("the {name} catalogue is malformed: {error}"))
}

fn catalogues() -> &'static Catalogues {
    CATALOGUES.get_or_init(|| Catalogues {
        english: parse(ENGLISH, "English"),
        spanish: parse(SPANISH, "Spanish"),
    })
}

/// Set the interface language.
///
/// Called during start-up, and again whenever the setting is changed in the
/// settings view. Every string is looked up as it is drawn, so a change takes
/// effect on the next frame; the caller marks the screen dirty so that frame
/// comes immediately.
pub fn set_language(language: Language) {
    ACTIVE.store(language.ordinal(), Ordering::Relaxed);
}

pub fn language() -> Language {
    Language::from_ordinal(ACTIVE.load(Ordering::Relaxed))
}

/// Pick the interface language.
///
/// The `--lang` flag wins over the configured language, which wins over the
/// system locale. An unrecognised name is not worth refusing to start over; it
/// falls through to detection, and then to English.
pub fn resolve_language(flag: Option<&str>, configured: &str) -> Language {
    flag.and_then(Language::from_tag)
        .or_else(|| Language::from_tag(configured))
        .or_else(detect_language)
        .unwrap_or_default()
}

/// The translation for `key`, falling back to English and then to the key.
///
/// Returning the key itself is deliberate: a missing string shows up as
/// `status.scanning` in the interface, which is obvious in a screenshot and
/// searchable, rather than silently blank.
pub fn lookup(key: &str) -> &'static str {
    let catalogues = catalogues();
    let from_active = match language() {
        Language::English => None,
        Language::Spanish => catalogues.spanish.get(key),
    };
    from_active
        .or_else(|| catalogues.english.get(key))
        .map(String::as_str)
        .unwrap_or_else(|| leak_missing(key))
}

/// Keys are `&'static str` literals everywhere in the code, but `lookup` takes
/// a `&str`, so an unknown key has to be given a lasting home. This only
/// happens for a key that is missing from every catalogue, which the catalogue
/// tests are there to prevent.
fn leak_missing(key: &str) -> &'static str {
    Box::leak(key.to_owned().into_boxed_str())
}

/// Substitute `{name}` placeholders.
///
/// Deliberately minimal: no escaping, no format specifiers. Translators only
/// ever need to move placeholders around, and anything richer belongs in the
/// code that builds the value.
pub fn format(key: &str, arguments: &[(&str, &dyn std::fmt::Display)]) -> String {
    format_template(lookup(key), arguments)
}

/// The substitution itself, separated so it can be tested without depending on
/// any particular catalogue entry.
fn format_template(template: &str, arguments: &[(&str, &dyn std::fmt::Display)]) -> String {
    let mut output = String::with_capacity(template.len() + 16 * arguments.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let Some(length) = rest[start..].find('}') else {
            break;
        };
        let name = &rest[start + 1..start + length];
        output.push_str(&rest[..start]);
        match arguments.iter().find(|(argument, _)| *argument == name) {
            Some((_, value)) => output.push_str(&value.to_string()),
            // An unknown placeholder is left as written, so a mistranslation
            // shows what it was asking for.
            None => output.push_str(&rest[start..=start + length]),
        }
        rest = &rest[start + length + 1..];
    }
    output.push_str(rest);
    output
}

/// Pick the singular or plural form of `key`, by way of `key.one` and
/// `key.other`.
pub fn plural(key: &str, count: u64) -> &'static str {
    let suffix = if count == 1 { "one" } else { "other" };
    lookup(&format!("{key}.{suffix}"))
}

/// Look up a translation, optionally substituting `{name}` placeholders.
#[macro_export]
macro_rules! t {
    ($key:expr) => {
        $crate::i18n::lookup($key)
    };
    ($key:expr, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::i18n::format(
            $key,
            &[$((stringify!($name), &$value as &dyn std::fmt::Display)),+],
        )
    };
}

/// As `t!`, choosing a plural form by `count`, which is also available to the
/// template as `{count}`.
#[macro_export]
macro_rules! tn {
    ($key:expr, $count:expr) => {
        $crate::i18n::format(
            &format!(
                "{}.{}",
                $key,
                if $count == 1 { "one" } else { "other" },
            ),
            &[("count", &$count as &dyn std::fmt::Display)],
        )
    };
    ($key:expr, $count:expr, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::i18n::format(
            &format!(
                "{}.{}",
                $key,
                if $count == 1 { "one" } else { "other" },
            ),
            &[
                ("count", &$count as &dyn std::fmt::Display),
                $((stringify!($name), &$value as &dyn std::fmt::Display)),+
            ],
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_tags_are_parsed_leniently() {
        // Locale environment variables carry territory and encoding suffixes.
        for tag in ["es", "ES", "es_ES.UTF-8", "es-419", "es@valencia"] {
            assert_eq!(Language::from_tag(tag), Some(Language::Spanish), "{tag}");
        }
        assert_eq!(Language::from_tag("en_GB.UTF-8"), Some(Language::English));
        assert_eq!(Language::from_tag("fr_FR"), None);
        assert_eq!(Language::from_tag("C"), None);
    }

    #[test]
    fn the_flag_wins_over_the_configuration() {
        assert_eq!(resolve_language(Some("es"), "en"), Language::Spanish);
        assert_eq!(resolve_language(Some("en"), "es"), Language::English);
    }

    #[test]
    fn the_configuration_is_used_when_no_flag_is_given() {
        assert_eq!(resolve_language(None, "es"), Language::Spanish);
    }

    #[test]
    fn auto_and_nonsense_fall_through_to_detection() {
        // "auto" is the default setting, and a typo should not be fatal; both
        // land on detection, which ends at English when nothing matches.
        for configured in ["auto", "klingon", ""] {
            let resolved = resolve_language(None, configured);
            assert!(
                matches!(resolved, Language::English | Language::Spanish),
                "{configured}"
            );
        }
    }

    #[test]
    fn every_explicit_choice_names_a_language_muscli_speaks() {
        // The settings view cycles through these, so a choice that parses to
        // nothing would silently leave the language where it was.
        for choice in LANGUAGE_CHOICES.iter().filter(|choice| **choice != "auto") {
            assert!(Language::from_tag(choice).is_some(), "{choice}");
        }
    }

    #[test]
    fn both_catalogues_parse() {
        let catalogues = catalogues();
        assert!(!catalogues.english.is_empty());
        assert!(!catalogues.spanish.is_empty());
    }

    #[test]
    fn every_english_key_is_translated_with_the_same_placeholders() {
        // A translated string that drops or renames a placeholder silently
        // loses the value it was meant to show.
        let catalogues = catalogues();
        let mut missing = Vec::new();
        let mut mismatched = Vec::new();
        for (key, english) in &catalogues.english {
            let Some(spanish) = catalogues.spanish.get(key) else {
                missing.push(key.clone());
                continue;
            };
            if placeholders(english) != placeholders(spanish) {
                mismatched.push(key.clone());
            }
        }
        missing.sort();
        mismatched.sort();
        assert!(missing.is_empty(), "untranslated keys: {missing:?}");
        assert!(
            mismatched.is_empty(),
            "placeholders differ between languages: {mismatched:?}"
        );
    }

    #[test]
    fn no_translation_exists_without_an_english_original() {
        let catalogues = catalogues();
        let mut orphans: Vec<&String> = catalogues
            .spanish
            .keys()
            .filter(|key| !catalogues.english.contains_key(*key))
            .collect();
        orphans.sort();
        assert!(
            orphans.is_empty(),
            "translations with no English key: {orphans:?}"
        );
    }

    #[test]
    fn plural_forms_come_in_pairs() {
        let catalogues = catalogues();
        for key in catalogues.english.keys() {
            if let Some(stem) = key.strip_suffix(".one") {
                assert!(
                    catalogues.english.contains_key(&format!("{stem}.other")),
                    "{key} has no plural form"
                );
            }
            if let Some(stem) = key.strip_suffix(".other") {
                assert!(
                    catalogues.english.contains_key(&format!("{stem}.one")),
                    "{key} has no singular form"
                );
            }
        }
    }

    #[test]
    fn placeholders_are_substituted() {
        assert_eq!(
            format_template(
                "Scanning {label}: {done}/{total}",
                &[("done", &3), ("total", &7), ("label", &"USB")]
            ),
            "Scanning USB: 3/7"
        );
    }

    #[test]
    fn an_unknown_placeholder_is_left_visible() {
        // Better a literal {missing} in the interface than a silently dropped
        // value nobody notices.
        assert_eq!(
            format_template("before {missing} after", &[]),
            "before {missing} after"
        );
    }

    #[test]
    fn a_template_without_placeholders_is_returned_as_is() {
        assert_eq!(
            format_template("plain text", &[("unused", &1)]),
            "plain text"
        );
    }

    #[test]
    fn an_unclosed_brace_does_not_swallow_the_rest() {
        assert_eq!(format_template("open { brace", &[]), "open { brace");
    }

    #[test]
    fn a_missing_key_shows_itself() {
        assert_eq!(lookup("no.such.key.exists"), "no.such.key.exists");
    }

    fn placeholders(template: &str) -> Vec<&str> {
        let mut names = Vec::new();
        let mut rest = template;
        while let Some(start) = rest.find('{') {
            let Some(length) = rest[start..].find('}') else {
                break;
            };
            names.push(&rest[start + 1..start + length]);
            rest = &rest[start + length + 1..];
        }
        names.sort_unstable();
        names
    }
}
