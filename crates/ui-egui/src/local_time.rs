//! Dates and times in the system's own locale: the OS timezone and its date/time pattern.
//!
//! The app language never takes part. Someone reading SolidCraft in English may still live in
//! Sweden (ISO-like dates) or Taiwan (`2026/10/18`), and someone reading it in Swedish may be
//! in the US. The locale comes from `LC_TIME` (then `LC_ALL`, `LANG`) on Unix and from the
//! platform's user locale elsewhere; the pattern is the CLDR short date and short time for that
//! locale, and an unknown or region-less tag falls back to ISO 8601 rather than guessing.

use std::sync::OnceLock;

use chrono::{DateTime, Local, Utc};

/// Format a UTC instant (seconds since 1970) in the system's locale and timezone.
pub fn format(unix_secs: u64) -> String {
    format_with(system_tag().as_deref(), unix_secs)
}

/// [`format`] with an explicit locale tag, so tests do not depend on the machine's locale
/// (`None`: no usable system locale, ISO 8601 is shown).
pub fn format_with(tag: Option<&str>, unix_secs: u64) -> String {
    let Ok(secs) = i64::try_from(unix_secs) else { return String::new() };
    let Some(utc) = DateTime::<Utc>::from_timestamp(secs, 0) else { return String::new() };
    let local = utc.with_timezone(&Local);
    match tag.and_then(locale) {
        Some(locale) => local.format_localized("%x %X", locale).to_string(),
        None => local.format("%Y-%m-%d %H:%M").to_string(),
    }
}

/// The system locale tag, read once per process.
fn system_tag() -> Option<String> {
    static TAG: OnceLock<Option<String>> = OnceLock::new();
    TAG.get_or_init(|| {
        #[cfg(not(target_arch = "wasm32"))]
        {
            unix_time_tag().or_else(sys_locale::get_locale)
        }
        #[cfg(target_arch = "wasm32")]
        {
            None
        }
    })
    .clone()
}

/// The time locale on Unix (`C` and `POSIX` mean "no locale", not a region).
#[cfg(not(target_arch = "wasm32"))]
fn unix_time_tag() -> Option<String> {
    for key in ["LC_TIME", "LC_ALL", "LANG"] {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim();
            if !value.is_empty() && value != "C" && value != "POSIX" {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// The locale tag (`sv-SE.UTF-8`, `zh_Hant_TW`, `en_GB@euro`, …) as a chrono locale. Chinese
/// script tags pick the matching region; a tag without a region is not guessed.
fn locale(tag: &str) -> Option<chrono::Locale> {
    let tag = tag.split(['.', '@']).next()?.replace('-', "_");
    let parts: Vec<&str> = tag.split('_').filter(|p| !p.is_empty()).collect();
    let language = *parts.first()?;
    if !language.chars().all(|c| c.is_ascii_alphabetic()) || language.len() < 2 {
        return None;
    }
    let rest = &parts[1..];
    if language.eq_ignore_ascii_case("zh") {
        if rest.iter().any(|p| p.eq_ignore_ascii_case("Hant") || p.eq_ignore_ascii_case("TW") || p.eq_ignore_ascii_case("HK")) {
            return "zh_TW".parse().ok();
        }
        if rest.iter().any(|p| p.eq_ignore_ascii_case("Hans") || p.eq_ignore_ascii_case("CN") || p.eq_ignore_ascii_case("SG")) {
            return "zh_CN".parse().ok();
        }
        return None;
    }
    let region = rest.iter().find(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_alphabetic()))?;
    format!("{language}_{}", region.to_ascii_uppercase()).parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2020-09-13T12:26:40Z, far enough from midnight for the local date to be stable in
    /// every timezone from UTC-12 to UTC+14.
    const NOONISH: u64 = 1_600_000_000;

    #[test]
    fn region_and_script_tags_map_to_the_expected_locale() {
        assert_eq!(locale("sv_SE"), Some(chrono::Locale::sv_SE));
        assert_eq!(locale("zh-TW"), Some(chrono::Locale::zh_TW));
        assert_eq!(locale("zh_Hant_TW"), Some(chrono::Locale::zh_TW));
        assert_eq!(locale("zh_Hans_CN"), Some(chrono::Locale::zh_CN));
        assert_eq!(locale("en_GB.UTF-8"), Some(chrono::Locale::en_GB));
        assert_eq!(locale("de_DE@euro"), Some(chrono::Locale::de_DE));
        assert_eq!(locale("en"), None, "no region: never guess a country");
        assert_eq!(locale("C"), None);
        assert_eq!(locale("not a locale"), None);
    }

    #[test]
    fn the_system_tag_decides_the_pattern_not_the_app_language() {
        // The local hour the test runs in is irrelevant: only the locale's field order and
        // separators are asserted.
        let local = DateTime::<Utc>::from_timestamp(NOONISH as i64, 0).unwrap().with_timezone(&Local);
        // Swe: ISO-like date, year first.
        assert!(format_with(Some("sv_SE"), NOONISH).starts_with(&local.format("%Y-%m-%d").to_string()));
        // Britain is day-first with slashes; the US is month-first and uses a 12-hour clock.
        let britain = format_with(Some("en_GB"), NOONISH);
        let america = format_with(Some("en_US"), NOONISH);
        assert!(britain.starts_with(&local.format("%d/%m/%y").to_string()), "{britain}");
        assert!(america.starts_with(&local.format("%m/%d/%Y").to_string()), "{america}");
        assert!(america.to_ascii_uppercase().contains("AM") || america.to_ascii_uppercase().contains("PM"), "{america}");
        assert_ne!(britain, america);
        // Taiwan is its own format (glibc writes the era and uses CJK date words), and it is
        // neither the Swedish nor the US one.
        let taiwan = format_with(Some("zh_TW"), NOONISH);
        assert_ne!(taiwan, america);
        assert_ne!(taiwan, format_with(Some("sv_SE"), NOONISH));
        assert!(taiwan.contains(&local.format("%Y").to_string()), "{taiwan}");
    }

    #[test]
    fn an_unknown_locale_falls_back_to_iso_8601() {
        let local = DateTime::<Utc>::from_timestamp(NOONISH as i64, 0).unwrap().with_timezone(&Local);
        assert_eq!(format_with(None, NOONISH), local.format("%Y-%m-%d %H:%M").to_string());
        assert_eq!(format_with(Some("xx_YY"), NOONISH), local.format("%Y-%m-%d %H:%M").to_string());
    }
}
