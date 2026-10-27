//! Timestamps as `okf.py` reads them: `parse_ts` is
//! `datetime.fromisoformat(s.replace("Z", "+00:00"))`, and `isoformat()`
//! writes them back.
//!
//! [`parse_iso`] ports CPython 3.14's C parser (`_datetimemodule.c`), quirks
//! included, because `okf.py`'s `oldest` calls `parse_ts` on any `at:` string:
//! the date is `YYYY-MM-DD` or `YYYYMMDD`, the separator any one character,
//! the time `HH[:MM[:SS[{.,}f+]]]` or `HH[MM[SS[f+]]]`, and the offset
//! `±HH[:MM[:SS[.f+]]]` (or `Z`). One stray character between the time and the
//! offset is ignored (`10:00:00x+05:00` parses), offset fields are not range
//! checked one by one (`+0099` is `+01:39`), and only the whole offset must be
//! strictly inside ±24 hours. `24:00` moves the date to the next day before
//! the date is checked, so `2026-10-00T24:00` is October 1st.
//!
//! # Known divergences from `okf.py`
//!
//! * ISO week dates (`2026-W43-1`) and offsets with a fraction of a second
//!   (`+05:30:15.5`) are rejected; Python accepts them. Neither occurs in a
//!   catalog, and `lint`'s `ISO_RE` rejects both before parsing anyway.
//!   (Whole-second offsets such as `+05:30:15` are accepted, as in Python.)
//! * Python returns a *naive* datetime when there is no offset, and `okf.py`
//!   then crashes comparing it with an aware one. Callers here treat
//!   [`PyDateTime::Naive`] as "not a usable timestamp" instead.

use std::borrow::Cow;
// `as _` imports the trait only for its methods (`write!` on a `String`).
use std::fmt::Write as _;
use std::sync::LazyLock;

// `Datelike` and `Timelike` are chrono traits; importing them brings their
// methods (`year()`, `hour()`, ...) into scope, the way a Python mixin would.
use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use regex::Regex;

/// Python's `datetime.MAXYEAR`; `MINYEAR` is 1.
const MAX_YEAR: u32 = 9999;

/// Seconds in a day: a UTC offset must be strictly inside `±SECONDS_PER_DAY`.
const SECONDS_PER_DAY: u32 = 86_400;

/// The result of Python's `datetime.fromisoformat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PyDateTime {
    /// With a UTC offset (`Z` or `+HH:MM`): comparable with "now".
    Aware(DateTime<FixedOffset>),
    /// Without one (`2026-10-26`, `2026-10-26T10:00`): not comparable.
    Naive(NaiveDateTime),
}

impl PyDateTime {
    /// Python's `isoformat()`.
    ///
    /// # Returns
    /// `YYYY-MM-DDTHH:MM:SS`, then `.ffffff` when the microseconds are not
    /// zero, then (aware values only) the offset as `+HH:MM`, with `:SS` added
    /// when the offset has seconds.
    pub fn isoformat(&self) -> String {
        match self {
            PyDateTime::Aware(t) => t.to_string_iso(),
            PyDateTime::Naive(t) => {
                let mut out = String::with_capacity(26);
                push_naive(&mut out, t);
                out
            }
        }
    }

    /// The aware value, or `None` for a naive one.
    ///
    /// Takes `self` by value: `PyDateTime` is `Copy`, so this copies rather
    /// than consuming the caller's variable.
    pub fn aware(self) -> Option<DateTime<FixedOffset>> {
        match self {
            PyDateTime::Aware(t) => Some(t),
            PyDateTime::Naive(_) => None,
        }
    }
}

/// Python's `isoformat()` for an aware datetime.
///
/// A trait adds a method to chrono's type, which Rust only allows through a
/// trait defined in this crate (the "orphan rule"). Callers `use` the trait to
/// call the method.
pub trait IsoFormat {
    /// The `isoformat()` text, e.g. `2026-10-26T10:00:00.500000+05:30`.
    ///
    /// UTC is written `+00:00`, as Python does; `okf.py` turns it into `Z`
    /// with `.replace("+00:00", "Z")` itself, so that is the caller's job.
    fn to_string_iso(&self) -> String;
}

impl IsoFormat for DateTime<FixedOffset> {
    fn to_string_iso(&self) -> String {
        // 26 for the local time with microseconds, 9 for `+HH:MM:SS`.
        let mut out = String::with_capacity(35);
        push_naive(&mut out, &self.naive_local());
        let offset = self.offset().local_minus_utc();
        let sign = if offset < 0 { '-' } else { '+' };
        let seconds = offset.unsigned_abs();
        // `write!` into a `String` cannot fail; `let _ =` discards the
        // `fmt::Result` it returns anyway.
        let _ = write!(out, "{sign}{:02}:{:02}", seconds / 3600, seconds / 60 % 60);
        if !seconds.is_multiple_of(60) {
            let _ = write!(out, ":{:02}", seconds % 60);
        }
        out
    }
}

/// Appends `YYYY-MM-DDTHH:MM:SS[.ffffff]` (Python's naive `isoformat()`).
fn push_naive(out: &mut String, t: &NaiveDateTime) {
    let _ = write!(
        out,
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        t.year(),
        t.month(),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    );
    let micros = t.nanosecond() / 1000;
    if micros != 0 {
        let _ = write!(out, ".{micros:06}");
    }
}

/// Does `text` match `okf.py`'s `ISO_RE`
/// (`^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$`)?
///
/// `\d` is Unicode in both engines (so `２０２６-...` matches here and is
/// rejected later by [`parse_iso`], as in `okf.py`). Python's `$` also
/// matches before a final `\n`, spelled out here as `\n?\z`.
///
/// # Arguments
/// * `text` - the frontmatter value to check.
///
/// # Returns
/// `true` when `re.match(ISO_RE, text)` would succeed.
pub fn is_iso_timestamp(text: &str) -> bool {
    // `LazyLock` compiles the regex once, on first use, like a module-level
    // `re.compile` in Python.
    static ISO: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\A\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})\n?\z")
            .expect("ISO regex is valid")
    });
    ISO.is_match(text)
}

/// `okf.py`'s `parse_ts`: `datetime.fromisoformat(text.replace("Z", "+00:00"))`.
///
/// # Arguments
/// * `text` - the timestamp text, e.g. `2026-10-26T10:00:00Z`.
///
/// # Returns
/// [`PyDateTime::Aware`] when the text has an offset, [`PyDateTime::Naive`]
/// when it has none, and `None` where Python raises `ValueError` (or where
/// the port diverges; see the module docs).
///
/// # Examples
/// ```
/// use okf_mcp::okf::timestamp::{parse_iso, PyDateTime};
///
/// let parsed = parse_iso("2026-10-26T10:00Z").expect("valid");
/// assert_eq!(parsed.isoformat(), "2026-10-26T10:00:00+00:00");
/// assert!(matches!(parse_iso("2026-10-26"), Some(PyDateTime::Naive(_))));
/// assert!(parse_iso("2026-02-29T10:00:00Z").is_none());
/// ```
pub fn parse_iso(text: &str) -> Option<PyDateTime> {
    // `Cow` ("clone on write") holds either the borrowed input or a new
    // `String`, so text without a `Z` is not copied.
    let text: Cow<'_, str> = if text.contains('Z') {
        Cow::Owned(text.replace('Z', "+00:00"))
    } else {
        Cow::Borrowed(text)
    };
    let (date, rest) = parse_date(&text)?;
    if rest.is_empty() {
        return Some(PyDateTime::Naive(date.validate()?.and_hms_opt(0, 0, 0)?));
    }
    // Any single character separates the date from the time (CPython
    // replaces a non-ASCII separator with `T` before parsing).
    let mut chars = rest.chars();
    chars.next()?;
    let time = chars.as_str();
    if time.is_empty() {
        return None;
    }
    // CPython looks for the offset at the first `+` or `-` (a `Z` has been
    // replaced already). Byte offsets are safe: both are ASCII.
    let (clock, offset) = match time.find(['+', '-']) {
        Some(at) => (&time[..at], Some(&time[at..])),
        None => (time, None),
    };
    let (fields, clock_trailing) = parse_fields(clock.as_bytes())?;
    let offset = match offset {
        // Without an offset, leftover characters are an error.
        None if clock_trailing => return None,
        None => None,
        // With one, CPython ignores the clock's leftover character.
        Some(offset) => Some(parse_offset(offset)?),
    };
    let naive = local_datetime(date, fields)?;
    Some(match offset {
        None => PyDateTime::Naive(naive),
        // A fixed offset maps every local time to exactly one instant, so
        // `single()` (which is `None` only for ambiguous or skipped local
        // times in real time zones) always has a value here.
        Some(offset) => PyDateTime::Aware(naive.and_local_timezone(offset).single()?),
    })
}

/// A date as written, before range checks: CPython checks it only after
/// `24:00` has moved it to the next day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RawDate {
    year: u32,
    month: u32,
    day: u32,
}

impl RawDate {
    /// Python's `date(year, month, day)` checks: year 1..=9999, a real day.
    fn validate(self) -> Option<NaiveDate> {
        if !(1..=MAX_YEAR).contains(&self.year) {
            return None;
        }
        // `i32::try_from` cannot fail for a 4-digit number; `.ok()?` keeps it
        // panic-free (`?` on an `Option` returns `None` early, like a guard).
        NaiveDate::from_ymd_opt(i32::try_from(self.year).ok()?, self.month, self.day)
    }

    /// The day after, as CPython computes it for `24:00` *before* validating:
    /// it advances only when `month <= 12` and `day` is at most the month's
    /// length (month 0 has length 0), so `2026-10-00T24:00` is October 1st,
    /// `2026-00-00T24:00` is January 1st and `0000-12-31T24:00` is year 1.
    fn next_day_unchecked(self) -> Self {
        let RawDate { year, month, day } = self;
        if month > 12 || day > days_in_month(year, month) {
            return self;
        }
        match (day + 1 > days_in_month(year, month), month == 12) {
            (false, _) => RawDate {
                day: day + 1,
                ..self
            },
            (true, false) => RawDate {
                year,
                month: month + 1,
                day: 1,
            },
            (true, true) => RawDate {
                year: year + 1,
                month: 1,
                day: 1,
            },
        }
    }
}

/// CPython's `days_in_month` table, indexed by month 0..=12 (0 for month 0).
fn days_in_month(year: u32, month: u32) -> u32 {
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        0 => 0,
        _ => 31,
    }
}

/// `YYYY-MM-DD` or `YYYYMMDD`, followed by the rest of `text`.
///
/// CPython decides the date's length from its fifth character: `-` means the
/// extended form (10 characters), anything else the basic form (8). A `W`
/// there (an ISO week date) is a documented divergence and is rejected.
fn parse_date(text: &str) -> Option<(RawDate, &str)> {
    let bytes = text.as_bytes();
    let extended = match bytes.get(4)? {
        b'-' if bytes.get(5) == Some(&b'W') => return None,
        b'-' => true,
        b'W' => return None,
        _ => false,
    };
    // Byte positions of month and day, and where the date ends.
    let (month_at, day_at, end) = if extended { (5, 8, 10) } else { (4, 6, 8) };
    if extended && bytes.get(7) != Some(&b'-') {
        return None;
    }
    let date = RawDate {
        year: digits(bytes, 0, 4)?,
        month: digits(bytes, month_at, 2)?,
        day: digits(bytes, day_at, 2)?,
    };
    // `end` is past ASCII digits only, so it is a character boundary.
    Some((date, &text[end..]))
}

/// The `n` ASCII digits at `bytes[at..at + n]` as a number, or `None` when
/// any is missing or not an ASCII digit (CPython checks bytes, so
/// `\u{ff11}` and other Unicode digits are rejected).
fn digits(bytes: &[u8], at: usize, n: usize) -> Option<u32> {
    bytes.get(at..at + n)?.iter().try_fold(0u32, |acc, &b| {
        b.is_ascii_digit().then(|| acc * 10 + u32::from(b - b'0'))
    })
}

/// Hour, minute, second and microsecond, before range checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fields {
    hour: u32,
    minute: u32,
    second: u32,
    micros: u32,
}

/// CPython's `parse_hh_mm_ss_ff`: `HH[:MM[:SS[{.,}f+]]]` or
/// `HH[MM[SS[[{.,}]f+]]]`, with no range checks. Fraction digits past the
/// sixth are dropped (truncated, not rounded).
///
/// Works on bytes, as CPython does on the UTF-8 text, so one non-ASCII
/// character counts as several (`10\u{e9}+05:00` is rejected, as in Python).
///
/// # Returns
/// The fields, and whether one character or more was left over (CPython's
/// return value 1, which its callers treat as an error only where no offset
/// follows). `None` for a malformed string (CPython's negative codes).
fn parse_fields(bytes: &[u8]) -> Option<(Fields, bool)> {
    let mut values = [0u32; 3];
    let mut pos = 0;
    let mut has_separator = false;
    // `Some(leftover)` when the text ends before a fraction.
    let mut ended = None;
    // `iter_mut().enumerate()` is Python's `enumerate(values)`, except that
    // each item is a mutable reference, so `*value = ...` writes into `values`.
    for (index, value) in values.iter_mut().enumerate() {
        *value = digits(bytes, pos, 2)?;
        pos += 2;
        // `let ... else` binds on a match and runs the `else` block otherwise.
        let Some(&next) = bytes.get(pos) else {
            ended = Some(false);
            break;
        };
        pos += 1;
        if index == 0 {
            has_separator = next == b':';
        }
        // A single character after a field is "left over", whatever it is.
        if pos == bytes.len() {
            ended = Some(true);
            break;
        }
        match next {
            // `if` on a match arm is a guard: the arm applies only when it holds.
            b':' if has_separator && index == 2 => return None,
            b':' if has_separator => {}
            b'.' | b',' if index < 2 => return None,
            b'.' | b',' => break,
            // Basic format: the character was the next field's first digit.
            _ if !has_separator => pos -= 1,
            _ => return None,
        }
    }
    let [hour, minute, second] = values;
    let mut fields = Fields {
        hour,
        minute,
        second,
        micros: 0,
    };
    if let Some(leftover) = ended {
        return Some((fields, leftover));
    }
    // The loop finished on the seconds, with at least one byte to go.
    let kept = (bytes.len() - pos).min(6);
    let kept_exponent = u32::try_from(6 - kept).ok()?;
    fields.micros = digits(bytes, pos, kept)? * 10u32.pow(kept_exponent);
    let dropped = bytes[pos + kept..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    Some((fields, pos + kept + dropped < bytes.len()))
}

/// The offset after the time: a sign, then [`parse_fields`]'s format with
/// nothing left over, strictly inside ±24 hours.
///
/// A zero offset is UTC even with a fraction (`+00:00:00.5`), as CPython
/// does; any other offset with a fraction is rejected (documented divergence:
/// chrono's `FixedOffset` has whole seconds only).
fn parse_offset(text: &str) -> Option<FixedOffset> {
    let negative = text.starts_with('-');
    // The first byte is the ASCII sign.
    let (fields, leftover) = parse_fields(text.as_bytes().get(1..)?)?;
    if leftover {
        return None;
    }
    let seconds = fields.hour * 3600 + fields.minute * 60 + fields.second;
    if seconds >= SECONDS_PER_DAY || (seconds != 0 && fields.micros != 0) {
        return None;
    }
    let seconds = i32::try_from(seconds).ok()?;
    FixedOffset::east_opt(if negative { -seconds } else { seconds })
}

/// The local date and time, with Python's range checks: hour 0..=23,
/// minute and second 0..=59 (no leap second 60), and `24:00:00` as the next
/// day's midnight (see [`RawDate::next_day_unchecked`]).
fn local_datetime(date: RawDate, fields: Fields) -> Option<NaiveDateTime> {
    if fields.hour == 24 {
        if (fields.minute, fields.second, fields.micros) != (0, 0, 0) {
            return None;
        }
        return date.next_day_unchecked().validate()?.and_hms_opt(0, 0, 0);
    }
    // `from_hms_micro_opt` rejects second 60 (chrono models a leap second only
    // as microseconds >= 1_000_000, which `micros` never reaches).
    let time =
        NaiveTime::from_hms_micro_opt(fields.hour, fields.minute, fields.second, fields.micros)?;
    Some(NaiveDateTime::new(date.validate()?, time))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aware(text: &str) -> DateTime<FixedOffset> {
        match parse_iso(text) {
            Some(PyDateTime::Aware(t)) => t,
            other => panic!("{text}: expected aware, got {other:?}"),
        }
    }

    /// Asserts `parse_iso(input).isoformat()` for each pair, where the
    /// expected text is Python 3.14's
    /// `datetime.fromisoformat(input.replace("Z", "+00:00")).isoformat()`.
    fn assert_python_output(cases: &[(&str, &str)]) {
        for (input, python) in cases {
            let parsed = parse_iso(input).unwrap_or_else(|| panic!("{input:?} should parse"));
            assert_eq!(parsed.isoformat(), *python, "{input:?}");
        }
    }

    #[test]
    fn accepts_what_python_fromisoformat_accepts() {
        // Arrange: expected isoformat() output from Python 3.14 (verified fact 3).
        let cases = [
            ("2026-10-26T10:00:00Z", "2026-10-26T10:00:00+00:00"),
            ("2026-10-26T10:00Z", "2026-10-26T10:00:00+00:00"),
            ("2026-10-26 10:00:00Z", "2026-10-26T10:00:00+00:00"),
            ("2026-10-26X10:00:00Z", "2026-10-26T10:00:00+00:00"),
            ("2026-10-26\u{e9}10:00", "2026-10-26T10:00:00"),
            ("20261026T100000Z", "2026-10-26T10:00:00+00:00"),
            ("20261026 1000", "2026-10-26T10:00:00"),
            (
                "2026-10-26T10:00:00.5+0530",
                "2026-10-26T10:00:00.500000+05:30",
            ),
            ("2026-10-26T10:00:00,5Z", "2026-10-26T10:00:00.500000+00:00"),
            (
                "2026-10-26T10:00:00.1234567Z",
                "2026-10-26T10:00:00.123456+00:00",
            ),
            ("2026-10-26T24:00:00Z", "2026-10-27T00:00:00+00:00"),
            ("2026-12-31T24:00", "2027-01-01T00:00:00"),
            ("2026-10-26T24:00:00.0000001", "2026-10-27T00:00:00"),
            ("2026-10-26T10:00:00+23:59", "2026-10-26T10:00:00+23:59"),
            ("2026-10-26T10:00:00-04:00", "2026-10-26T10:00:00-04:00"),
            ("2026-10-26T10:00:00-0000", "2026-10-26T10:00:00+00:00"),
            ("2026-10-26T10+05", "2026-10-26T10:00:00+05:00"),
            ("2026-10-26T10", "2026-10-26T10:00:00"),
            ("2026-10-26", "2026-10-26T00:00:00"),
            ("0001-01-01T00:00:00+05:00", "0001-01-01T00:00:00+05:00"),
        ];

        // Act and assert.
        assert_python_output(&cases);
        assert!(matches!(
            parse_iso("2026-10-26"),
            Some(PyDateTime::Naive(_))
        ));
        assert!(matches!(
            parse_iso("2026-10-26T10:00"),
            Some(PyDateTime::Naive(_))
        ));
    }

    #[test]
    fn follows_cpython_quirks() {
        // Arrange: each was run through Python 3.14.4's C `fromisoformat`.
        let cases = [
            // One stray character between the time and the offset is ignored.
            ("2026-10-26T10:00:00x+05:00", "2026-10-26T10:00:00+05:00"),
            ("2026-10-26T10:00:00 +05:00", "2026-10-26T10:00:00+05:00"),
            ("2026-10-26T10x+05:00", "2026-10-26T10:00:00+05:00"),
            ("2026-10-26T10:+05:00", "2026-10-26T10:00:00+05:00"),
            ("2026-10-26T10:00:00.Z", "2026-10-26T10:00:00+00:00"),
            ("2026-10-26T1000001+05:00", "2026-10-26T10:00:00+05:00"),
            // So are fraction digits past the sixth, then one more character.
            (
                "2026-10-26T10:00:00.1234567x+01:00",
                "2026-10-26T10:00:00.123456+01:00",
            ),
            // Basic format takes a fraction without a decimal mark.
            ("2026-10-26T10000012", "2026-10-26T10:00:00.120000"),
            ("2026-10-26T100000.5", "2026-10-26T10:00:00.500000"),
            ("2026-10-26T1000", "2026-10-26T10:00:00"),
            // Offset fields are only checked as a whole.
            ("2026-10-26T10:00:00+0099", "2026-10-26T10:00:00+01:39"),
            ("2026-10-26T10:00:00+01:60", "2026-10-26T10:00:00+02:00"),
            // Whole-second offsets, and a zero offset's fraction is dropped.
            (
                "2026-10-26T10:00:00-05:30:15",
                "2026-10-26T10:00:00-05:30:15",
            ),
            ("2026-10-26T10:00:00+050015", "2026-10-26T10:00:00+05:00:15"),
            ("2026-10-26T10:00:00+05:00:00", "2026-10-26T10:00:00+05:00"),
            (
                "2026-10-26T10:00:00+00:00:00.5",
                "2026-10-26T10:00:00+00:00",
            ),
        ];

        // Act and assert.
        assert_python_output(&cases);
    }

    #[test]
    fn rejects_what_python_rejects() {
        for input in [
            "2026-10-26T23:59:60Z",
            "2026-10-26T10:00:00+24:00",
            "2026-10-26T10:00:00+23:60",
            "2026-10-26T10:00:00+99",
            "2026-02-29T10:00:00Z",
            "2026-02-30T24:00:00",
            "9999-12-31T24:00:00",
            "2026-10-00T23:00",
            "2026-00-31T24:00",
            "2026-13-01T24:00",
            "2026-10-32T24:00",
            "0000-12-30T24:00",
            "\u{ff12}\u{ff10}\u{ff12}\u{ff16}-10-26T10:00:00Z",
            "2026-10-26T\u{ff11}\u{ff10}",
            "2026-10-26T10:00:00.\u{665}",
            "2026-10-26T24:00:01Z",
            "2026-10-26T25:00",
            "2026-10-26T10:60",
            "0000-01-01T00:00:00Z",
            "2026-10",
            "2026-10-2",
            "2026102",
            "2026-1026",
            "202610-26",
            "2026-10-26 ",
            "2026-10-26T",
            "2026-10-26TT",
            "2026-10-2610:00",
            "2026-10-26T1",
            "2026-10-26T10:",
            "2026-10-26T10:0",
            "2026-10-26T10:00:",
            "2026-10-26T10:0000",
            "2026-10-26T1000:00",
            "2026-10-26T10:00:00:00",
            "2026-10-26T10.5",
            "2026-10-26T1000.5",
            "2026-10-26T10:00.5",
            "2026-10-26T10:00x",
            "2026-10-26T10:00:00 ",
            "2026-10-26T10:00:00.",
            "2026-10-26T10:00:00,",
            "2026-10-26T10:00:00.5.5",
            "2026-10-26T1000005",
            "2026-10-26T10:00:00.1234567x",
            "2026-10-26T10:00:00xx+05:00",
            "2026-10-26T10:00:00.12x+01:00",
            "2026-10-26T10\u{e9}+05:00",
            "2026-10-26T-05:00",
            "2026-10-26T10:00:00+",
            "2026-10-26T10:00:00+5",
            "2026-10-26T10:00:00+05:",
            "2026-10-26T10:00:00+05x",
            "2026-10-26T10:00:00+05:3",
            "2026-10-26T10:00:00+0530x",
            "2026-10-26T10:00:00+05:30.5",
            "2026-10-26T10:00:00+05:00:00:00",
            "2026-10-26T10:00:00+05:00 ",
            "2026-10-26T10:00:00--05:00",
            "2026-10-26T10:00:00Zx",
            "2026-10-26T10:00:00ZZ",
            "not a date",
            "",
        ] {
            assert!(parse_iso(input).is_none(), "{input:?} should be rejected");
        }
    }

    #[test]
    fn rejects_the_documented_divergences() {
        // Python accepts these (2026-10-19T10:00:00 and +05:00:00.500000);
        // the port does not (module docs).
        for input in [
            "2026-W43-1T10",
            "2026W431",
            "2026-10-26T10:00:00+05:00:00.5",
        ] {
            assert!(
                parse_iso(input).is_none(),
                "{input:?} is a documented divergence"
            );
        }
    }

    #[test]
    fn iso_shape_matches_okf_py_iso_re() {
        assert!(is_iso_timestamp("2026-10-26T10:00:00Z"));
        assert!(is_iso_timestamp("2026-10-26T10:00:00.5-04:00"));
        assert!(
            is_iso_timestamp("2026-10-26T10:00:00Z\n"),
            "Python's $ allows one trailing newline"
        );
        assert!(
            is_iso_timestamp("\u{ff12}\u{ff10}\u{ff12}\u{ff16}-10-26T10:00:00Z"),
            "\\d is Unicode"
        );
        assert!(!is_iso_timestamp("2026-10-26T10:00Z"));
        assert!(!is_iso_timestamp("2026-10-26T10:00:00+0400"));
        assert!(!is_iso_timestamp("2026-10-26 10:00:00Z"));
        assert!(!is_iso_timestamp("2026-10-26T10:00:00Z\n\n"));
        assert!(!is_iso_timestamp(" 2026-10-26T10:00:00Z"));
    }

    #[test]
    fn comparisons_use_the_instant() {
        assert_eq!(
            aware("2026-10-26T10:00:00Z"),
            aware("2026-10-26T06:00:00-04:00")
        );
        assert!(aware("2026-10-26T10:00:01Z") > aware("2026-10-26T06:00:00-04:00"));
    }

    #[test]
    fn aware_drops_naive_values() {
        assert_eq!(parse_iso("2026-10-26").and_then(PyDateTime::aware), None);
        assert_eq!(
            parse_iso("2026-10-26T10:00:00Z").and_then(PyDateTime::aware),
            Some(aware("2026-10-26T10:00:00Z"))
        );
    }

    #[test]
    fn isoformat_uses_z_only_through_the_caller() {
        // okf.py writes `.isoformat().replace("+00:00", "Z")`; that replace is
        // the caller's job (see `oldest`), so isoformat keeps `+00:00`.
        assert_eq!(
            aware("2026-10-26T10:00:00Z").to_string_iso(),
            "2026-10-26T10:00:00+00:00"
        );
    }

    #[hegel::test]
    fn isoformat_round_trips(tc: hegel::TestCase) {
        use hegel::generators;
        // Arrange: instants whose local time stays in years 1..=9999 for any
        // offset (0001-01-02 to 9999-12-30 UTC), and offsets to the second.
        let seconds: i64 = tc.draw(
            generators::integers::<i64>()
                .min_value(-62_135_510_400)
                .max_value(253_402_128_000),
        );
        let micros: u32 = tc.draw(generators::integers::<u32>().max_value(999_999));
        let offset_seconds: i32 = tc.draw(
            generators::integers::<i32>()
                .min_value(-86_399)
                .max_value(86_399),
        );
        let offset = FixedOffset::east_opt(offset_seconds).expect("in range");
        let instant = DateTime::from_timestamp(seconds, micros * 1000)
            .expect("in range")
            .with_timezone(&offset);

        // Act.
        let text = instant.to_string_iso();
        let parsed = aware(&text);

        // Assert: the same instant, offset and text come back.
        assert_eq!(parsed, instant);
        assert_eq!(parsed.offset(), instant.offset());
        assert_eq!(parsed.to_string_iso(), text);
    }

    #[hegel::test]
    fn parse_iso_never_panics(tc: hegel::TestCase) {
        use hegel::generators;
        let alphabet = [
            "2", "0", "1", "9", "-", ":", "T", " ", ".", ",", "+", "Z", "W", "\u{e9}",
        ];
        let pieces: Vec<&str> =
            tc.draw(generators::vecs(generators::sampled_from(&alphabet)).max_size(30));
        let _ = parse_iso(&pieces.concat());
    }
}
