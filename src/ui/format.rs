//! Formatted values shown in the UI: sizes, transfer speeds, time remaining,
//! clock times and day headings.
//!
//! Every helper returns an owned [`String`] built in one buffer (no
//! intermediate `format!` chains) because these run for every row and every
//! progress tick. Units are binary (1 KB = 1024 B), as file managers and
//! LocalSend's own clients show them.
//!
//! Only the day heading depends on the language: [`day_label_in`] takes the
//! app's [`Language`] and returns a symbolic [`DayLabel`] (`Today`,
//! `Yesterday`, or a date). Screens localize the symbolic variants themselves
//! through their i18n keys, so a language switch re-renders them.

use crate::i18n::Language;
use chrono::{Datelike, Local, NaiveDate, TimeZone, Timelike};
use nsh_core::element::SharedString;
use std::fmt::Write;

/// Bytes in a binary kilobyte.
const KB: f64 = 1024.0;

/// The binary unit names, smallest first.
const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];

/// A byte count as a person reads it: `812 B`, `24 MB`, `1.2 GB`.
pub fn bytes(size: u64) -> String {
    let mut out = String::with_capacity(16);
    write_size(&mut out, size as f64);
    out
}

/// A transfer rate: `812 B/s`, `24.7 MB/s`.
pub fn speed(bytes_per_second: f64) -> String {
    let mut out = String::with_capacity(20);
    write_size(&mut out, bytes_per_second.max(0.0));
    out.push_str("/s");
    out
}

/// Time remaining as a clock: `00:42`, or `1:02:03` past an hour.
pub fn eta(remaining_seconds: f64) -> String {
    let mut out = String::with_capacity(8);
    let seconds = if remaining_seconds.is_finite() && remaining_seconds > 0.0 {
        remaining_seconds.round() as u64
    } else {
        0
    };
    if seconds >= 3600 {
        let _ = write!(
            out,
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        );
    } else {
        let _ = write!(out, "{:02}:{:02}", seconds / 60, seconds % 60);
    }
    out
}

/// The local wall-clock time of a Unix millisecond timestamp, 24-hour:
/// `14:03`.
pub fn clock(ms: u64) -> String {
    let mut out = String::with_capacity(5);
    if let Some(time) = local(ms) {
        let _ = write!(out, "{:02}:{:02}", time.hour(), time.minute());
    }
    out
}

/// The day heading of a Unix millisecond timestamp: the symbolic
/// [`DayLabel::Today`] / [`DayLabel::Yesterday`], or a date like `Mar 8`
/// whose month names follow `language`.
pub fn day_label_in(ms: u64, language: Language) -> DayLabel {
    let Some(date) = local_date(ms) else {
        return DayLabel::Date(SharedString::default());
    };
    let today = Local::now().date_naive();
    if date == today {
        return DayLabel::Today;
    }
    if today.pred_opt() == Some(date) {
        return DayLabel::Yesterday;
    }
    let months = match language {
        Language::En => MONTHS_EN,
        // Chinese writes the month as a number with 月.
        Language::Zh => MONTHS_ZH,
    };
    let month = months
        .get(date.month0() as usize)
        .copied()
        .unwrap_or(MONTHS_EN[0]);
    let mut out = String::with_capacity(8);
    match language {
        Language::En => {
            let _ = write!(out, "{month} {}", date.day());
        }
        Language::Zh => {
            let _ = write!(out, "{month}{}日", date.day());
        }
    }
    DayLabel::Date(out.into())
}

/// A day heading as the history screen groups transfers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DayLabel {
    /// The current local day.
    Today,
    /// The local day before it.
    Yesterday,
    /// Any older day, already formatted in the requested language.
    Date(SharedString),
}

impl DayLabel {
    /// The i18n key of a symbolic heading (`None` for a [`DayLabel::Date`],
    /// which is already text).
    pub fn key(&self) -> Option<&'static str> {
        match self {
            DayLabel::Today => Some("history.today"),
            DayLabel::Yesterday => Some("history.yesterday"),
            DayLabel::Date(_) => None,
        }
    }

    /// The date of a [`DayLabel::Date`] heading, or `None` for a symbolic
    /// one (the screen translates those through [`Self::key`]).
    pub fn date(&self) -> Option<SharedString> {
        match self {
            DayLabel::Date(date) => Some(date.clone()),
            DayLabel::Today | DayLabel::Yesterday => None,
        }
    }

    /// The heading text in `language`: a symbolic heading is translated
    /// through the i18n table, a date is already formatted in it.
    pub fn label_in(&self, language: Language) -> SharedString {
        match self {
            DayLabel::Today => crate::i18n::text(language, "history.today").into(),
            DayLabel::Yesterday => crate::i18n::text(language, "history.yesterday").into(),
            DayLabel::Date(date) => date.clone(),
        }
    }
}

/// English month abbreviations.
const MONTHS_EN: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Numeric months with the Chinese suffix.
const MONTHS_ZH: [&str; 12] = [
    "1月", "2月", "3月", "4月", "5月", "6月", "7月", "8月", "9月", "10月", "11月", "12月",
];

/// `size` in its largest binary unit, one decimal below 10 (`1.2 GB`,
/// `24 MB`, `812 B`).
fn write_size(out: &mut String, size: f64) {
    let size = size.max(0.0);
    if size < KB {
        let _ = write!(out, "{} B", size.round() as u64);
        return;
    }
    let mut value = size;
    let mut unit = 0;
    while value >= KB && unit + 1 < UNITS.len() {
        value /= KB;
        unit += 1;
    }
    // 1 MiB - 1 B is 1023.999 KB, which rounds to "1024 KB" below: promote it
    // to the next unit ("1.0 MB") as Finder and the official clients do.
    if value.round() >= KB && unit + 1 < UNITS.len() {
        value /= KB;
        unit += 1;
    }
    let name = UNITS[unit];
    if value < 9.95 {
        let _ = write!(out, "{value:.1} {name}");
    } else {
        let _ = write!(out, "{} {name}", value.round() as u64);
    }
}

/// The current local date as a day number, for caches that key "today".
///
/// A day heading depends on the wall clock, not only on the transfers it
/// labels: a list cached at 23:59 shows "Today" after midnight unless its
/// cache key carries the date.
pub fn local_day() -> i32 {
    Local::now().date_naive().num_days_from_ce()
}

/// The local date of a Unix millisecond timestamp.
fn local_date(ms: u64) -> Option<NaiveDate> {
    local(ms).map(|time| time.date_naive())
}

/// The local date and time of a Unix millisecond timestamp.
fn local(ms: u64) -> Option<chrono::DateTime<Local>> {
    let ms = i64::try_from(ms).ok()?;
    Local.timestamp_millis_opt(ms).single()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_like_the_finder() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(812), "812 B");
        assert_eq!(bytes(1024), "1.0 KB");
        assert_eq!(bytes(2_400_000), "2.3 MB");
        assert_eq!(bytes(1_200_000_000), "1.1 GB");
        // Just under a unit boundary: rounded, never "1024 KB".
        assert_eq!(bytes(1_048_575), "1.0 MB");
        assert_eq!(bytes(1_073_741_823), "1.0 GB");
        assert_eq!(bytes(1_048_576), "1.0 MB");
        assert_eq!(speed(24_700_000.0), "24 MB/s");
        assert_eq!(speed(999.0), "999 B/s");
    }

    #[test]
    fn eta_is_a_clock() {
        assert_eq!(eta(0.0), "00:00");
        assert_eq!(eta(42.4), "00:42");
        assert_eq!(eta(3_723.0), "1:02:03");
        assert_eq!(eta(-4.0), "00:00");
    }

    #[test]
    fn today_is_symbolic() {
        let now = Local::now().timestamp_millis().max(0) as u64;
        assert_eq!(day_label_in(now, Language::En), DayLabel::Today);
    }

    #[test]
    fn day_headings_follow_the_language() {
        let now = Local::now().timestamp_millis().max(0) as u64;
        assert_eq!(
            day_label_in(now, Language::En).label_in(Language::En),
            "Today"
        );
        assert_eq!(
            day_label_in(now, Language::Zh).label_in(Language::Zh),
            "今天"
        );
        // A date is already formatted in the language it was built with.
        let old = day_label_in(0, Language::Zh);
        let Some(date) = old.date() else {
            panic!("a 1970 timestamp is not today");
        };
        assert!(date.as_str().ends_with('日'));
    }

    #[test]
    fn clock_has_two_digit_fields() {
        let text = clock(1_700_000_000_000);
        assert_eq!(text.len(), 5);
        assert_eq!(text.as_bytes()[2], b':');
    }
}
