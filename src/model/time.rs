//! Reading the times SPINE carries as strings.
//!
//! SPINE writes every instant and every span as text. `AbsoluteOrRelativeTimeType` is an
//! `xs:union` of `xs:duration` and `xs:dateTime` (Resource Specification, Common Data
//! Types), so one element may arrive as either — `PT2H` or `2026-09-05T08:15:00Z` — and
//! which one arrived is part of what the peer said.
//!
//! Both halves are readable here, and neither is quite the everyday type it looks like:
//!
//! * the **relative** form is a *span*, and on a measurement it is an **age**: the value
//!   was taken `duration` before the message carrying it. [`parse_iso8601_duration`]
//!   reads it; the subtraction from the arrival time is the consumer's, because only the
//!   consumer knows on which clock it is counting.
//! * the **absolute** form is `xs:dateTime`, which is *not* RFC 3339. The offset is
//!   optional, so `2026-09-05T08:15:00` is a perfectly valid timestamp naming a local
//!   time whose instant nobody can compute; the year may be negative and may run past
//!   four digits; and `24:00:00` is midnight ending that day. An RFC 3339 parser rejects
//!   the first and the third, which from the outside looks exactly like a peer that sent
//!   no timestamp at all. [`DateTime`] reads all of it.
//!
//! What stays the consumer's is the **policy**. A household device sets its clock from
//! NTP or from nothing, and how far a peer's clock may be wrong before its timestamps are
//! worth less than the arrival time is not a decision a protocol crate can make. This
//! module parses; it never substitutes.
//!
//! ```
//! use eebus::model::{AbsoluteOrRelativeTime, TimeValue};
//! use core::time::Duration;
//!
//! let sent = AbsoluteOrRelativeTime::from("2026-09-05T08:15:00+02:00");
//! let stamp = sent.as_timestamp().expect("a well-formed xs:dateTime");
//! assert_eq!(stamp.offset_minutes(), Some(120));
//! assert_eq!(stamp.unix_seconds(), Some(1_788_588_900));
//!
//! // The same element, in the other form: an age rather than an instant.
//! let age = AbsoluteOrRelativeTime::from("PT30S");
//! assert!(matches!(age.parse(), Some(TimeValue::Relative(d)) if d == Duration::from_secs(30)));
//! ```

use alloc::format;
use alloc::string::String;

use crate::model::AbsoluteOrRelativeTime;

impl AbsoluteOrRelativeTime {
    /// Reads the value as an ISO 8601 duration, e.g. `PT2H` or `PT60S`.
    ///
    /// Returns [`None`] when the value is a timestamp rather than a duration, or when
    /// the duration carries months or years, whose length depends on a calendar the
    /// protocol does not supply.
    ///
    /// **On a measurement this is an age**, not a deadline: the reading was taken this
    /// long before the message carrying it, so the instant is `arrived − duration`. The
    /// subtraction is the consumer's — see
    /// [`Reading::taken_at_relative_to`](crate::usecases::monitoring::Reading::taken_at_relative_to),
    /// which is that one line against the clock the consumer is already keeping.
    pub fn as_duration(&self) -> Option<core::time::Duration> {
        parse_iso8601_duration(self.as_str())
    }

    /// Formats a duration as ISO 8601, e.g. `Duration::from_secs(7200)` → `PT2H`.
    pub fn from_duration(duration: core::time::Duration) -> Self {
        Self(format_iso8601_duration(duration))
    }

    /// Reads the value as an `xs:dateTime`, the absolute half of the union.
    ///
    /// [`None`] when the value is a duration, and [`None`] when it is a timestamp this
    /// crate could not read — which are different facts, and
    /// [`is_absolute`](Self::is_absolute) is what tells them apart.
    ///
    /// The parsed form is [`DateTime`] rather than an instant because the schema's is:
    /// an `xs:dateTime` without an offset names a wall-clock reading and fixes no point
    /// in time at all. See [`DateTime::unix_seconds`].
    ///
    /// ```
    /// use eebus::model::AbsoluteOrRelativeTime;
    ///
    /// let sent = AbsoluteOrRelativeTime::from("2026-09-05T08:15:00Z");
    /// assert_eq!(sent.as_timestamp().and_then(|t| t.unix_seconds()), Some(1_788_596_100));
    /// assert_eq!(AbsoluteOrRelativeTime::from("PT2H").as_timestamp(), None);
    /// ```
    pub fn as_timestamp(&self) -> Option<DateTime> {
        parse_xs_date_time(self.as_str())
    }

    /// Writes a timestamp in the canonical `xs:dateTime` form.
    ///
    /// The counterpart of [`from_duration`](Self::from_duration), for the sending side:
    /// a device stamping its own measurement builds the [`DateTime`] from its clock —
    /// [`DateTime::from_unix_seconds`] — and puts it on the wire through here.
    pub fn from_timestamp(stamp: DateTime) -> Self {
        use alloc::string::ToString as _;
        Self(stamp.to_string())
    }

    /// Reads whichever half of the union arrived.
    ///
    /// One call for a consumer that has to handle both, and the only way to handle both
    /// without guessing: the two forms are read by different parsers and mean different
    /// things, and nothing in the element says which is coming.
    ///
    /// ```
    /// use eebus::model::{AbsoluteOrRelativeTime, TimeValue};
    /// use core::time::Duration;
    ///
    /// # let arrived = Duration::from_secs(3_600);
    /// let taken_at = match AbsoluteOrRelativeTime::from("PT30S").parse() {
    ///     // A calendar instant: the peer's clock, and the consumer's to trust or not.
    ///     Some(TimeValue::Absolute(stamp)) => stamp.unix_seconds().map(|s| s as u64),
    ///     // An age: counted back from when the message arrived, on the reader's clock.
    ///     Some(TimeValue::Relative(age)) => Some(arrived.saturating_sub(age).as_secs()),
    ///     None => None,
    /// };
    /// assert_eq!(taken_at, Some(3_570));
    /// ```
    pub fn parse(&self) -> Option<TimeValue> {
        if self.is_relative() {
            return self.as_duration().map(TimeValue::Relative);
        }
        self.as_timestamp().map(TimeValue::Absolute)
    }

    /// Which form the peer sent, by the one character that distinguishes them.
    ///
    /// `xs:duration` always starts with `P`, optionally signed; `xs:dateTime` always
    /// starts with a digit or a sign followed by one. So this is a fact about the text
    /// and needs no parse — and it stays true for a *malformed* timestamp, which is the
    /// case that matters: `is_absolute()` and [`as_timestamp`](Self::as_timestamp)
    /// disagreeing is exactly how a consumer learns that a peer sent something it meant
    /// as a timestamp and this crate could not read.
    pub fn is_absolute(&self) -> bool {
        !self.is_relative()
    }

    /// True when the value is the relative half: an ISO 8601 duration.
    pub fn is_relative(&self) -> bool {
        self.as_str().starts_with('P') || self.as_str().starts_with("-P")
    }
}

/// Parses the subset of ISO 8601 durations SPINE uses: `PnDTnHnMnS`, with an optional
/// fractional seconds part.
///
/// Months and years are rejected rather than approximated: LPC's failsafe duration is a
/// safety-relevant timer, and guessing at 30-day months would make it wrong.
///
/// ```
/// use eebus::model::parse_iso8601_duration;
/// use core::time::Duration;
///
/// assert_eq!(parse_iso8601_duration("PT2H"), Some(Duration::from_secs(7200)));
/// assert_eq!(parse_iso8601_duration("PT1M30S"), Some(Duration::from_secs(90)));
/// assert_eq!(parse_iso8601_duration("P1DT12H"), Some(Duration::from_secs(129_600)));
/// assert_eq!(parse_iso8601_duration("P1M"), None, "months have no fixed length");
///
/// // A span pointing backwards has no time left to run, and `Duration` is unsigned.
/// assert_eq!(parse_iso8601_duration("-PT2H"), Some(Duration::ZERO));
/// // At least one component is required; `P` and `PT` say nothing.
/// assert_eq!(parse_iso8601_duration("PT"), None);
/// ```
pub fn parse_iso8601_duration(s: &str) -> Option<core::time::Duration> {
    // A duration pointing backwards, which `Duration` cannot hold, reads as zero rather
    // than as its magnitude: these values are timers, and a span that has elapsed has
    // nothing left to run.
    let (negative, s) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let rest = s.strip_prefix('P')?;
    let (date_part, time_part) = match rest.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (rest, None),
    };

    let mut secs: u64 = 0;
    let mut nanos: u32 = 0;
    // ISO 8601 requires at least one component: reading bare `P` or `PT` as zero would
    // turn a truncated field into a valid instruction.
    let mut components = 0u32;

    let mut number = String::new();
    for c in date_part.chars() {
        match c {
            '0'..='9' | '.' => number.push(c),
            'Y' | 'M' => return None, // calendar-dependent
            'W' => {
                secs = secs.checked_add(take_u64(&mut number)?.checked_mul(604_800)?)?;
                components += 1;
            }
            'D' => {
                secs = secs.checked_add(take_u64(&mut number)?.checked_mul(86_400)?)?;
                components += 1;
            }
            _ => return None,
        }
    }
    if !number.is_empty() {
        return None;
    }

    if let Some(time_part) = time_part {
        for c in time_part.chars() {
            match c {
                '0'..='9' | '.' => number.push(c),
                'H' => {
                    secs = secs.checked_add(take_u64(&mut number)?.checked_mul(3_600)?)?;
                    components += 1;
                }
                'M' => {
                    secs = secs.checked_add(take_u64(&mut number)?.checked_mul(60)?)?;
                    components += 1;
                }
                'S' => {
                    let raw = core::mem::take(&mut number);
                    let (whole, frac) = match raw.split_once('.') {
                        Some((w, f)) => (w, Some(f)),
                        None => (raw.as_str(), None),
                    };
                    if whole.is_empty() && frac.is_none_or(str::is_empty) {
                        return None;
                    }
                    if !whole.is_empty() {
                        secs = secs.checked_add(whole.parse::<u64>().ok()?)?;
                    }
                    if let Some(frac) = frac {
                        let digits: String = frac
                            .chars()
                            .chain(core::iter::repeat('0'))
                            .take(9)
                            .collect();
                        nanos = digits.parse::<u32>().ok()?;
                    }
                    components += 1;
                }
                _ => return None,
            }
        }
        if !number.is_empty() {
            return None;
        }
    }

    if components == 0 {
        return None;
    }
    if negative {
        return Some(core::time::Duration::ZERO);
    }
    Some(core::time::Duration::new(secs, nanos))
}

fn take_u64(buf: &mut String) -> Option<u64> {
    let raw = core::mem::take(buf);
    if raw.is_empty() || raw.contains('.') {
        return None;
    }
    raw.parse().ok()
}

/// Formats a duration in the canonical ISO 8601 form SPINE expects.
///
/// ```
/// use eebus::model::format_iso8601_duration;
/// use core::time::Duration;
///
/// assert_eq!(format_iso8601_duration(Duration::from_secs(7200)), "PT2H");
/// assert_eq!(format_iso8601_duration(Duration::from_secs(90)), "PT1M30S");
/// assert_eq!(format_iso8601_duration(Duration::ZERO), "PT0S");
/// ```
pub fn format_iso8601_duration(duration: core::time::Duration) -> String {
    use core::fmt::Write as _;

    let total = duration.as_secs();
    let nanos = duration.subsec_nanos();
    let days = total / 86_400;
    let hours = (total % 86_400) / 3_600;
    let minutes = (total % 3_600) / 60;
    let seconds = total % 60;

    let mut out = String::from("P");
    if days > 0 {
        let _ = write!(out, "{days}D");
    }
    if hours > 0 || minutes > 0 || seconds > 0 || nanos > 0 || days == 0 {
        out.push('T');
        if hours > 0 {
            let _ = write!(out, "{hours}H");
        }
        if minutes > 0 {
            let _ = write!(out, "{minutes}M");
        }
        if seconds > 0 || nanos > 0 || (hours == 0 && minutes == 0) {
            if nanos > 0 {
                let frac = format!("{nanos:09}");
                let frac = frac.trim_end_matches('0');
                let _ = write!(out, "{seconds}.{frac}S");
            } else {
                let _ = write!(out, "{seconds}S");
            }
        }
    }
    out
}

/// What an [`AbsoluteOrRelativeTime`] turned out to hold.
///
/// The schema unions two types that mean different things, and a reader that guesses
/// which one arrived gets the wrong answer silently: `PT30S` read as an instant is
/// nonsense, and `2026-09-05T08:15:00Z` read as an age is a value from 1970.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeValue {
    /// `xs:dateTime`: an instant the peer named on its own calendar.
    Absolute(DateTime),
    /// `xs:duration`: a span. On a measurement it is an **age** — the value was taken
    /// this long before the message that carried it.
    Relative(core::time::Duration),
}

/// A calendar instant, as `xs:dateTime` writes it.
///
/// The lexical form is `[-]yyyy[y+]-mm-ddThh:mm:ss[.fraction][Z|±hh:mm]`, and three parts
/// of it are why an RFC 3339 parser is the wrong tool:
///
/// * **the offset is optional.** Without one the value names a *local* time on a clock
///   whose zone the protocol never states, so it fixes no instant:
///   [`unix_seconds`](Self::unix_seconds) is [`None`] and
///   [`unix_seconds_at`](Self::unix_seconds_at) makes the caller supply the zone it means
///   to assume. Guessing UTC is how a value lands an hour or two out of place.
/// * **`24:00:00` is legal**, and means midnight ending that day. It is normalised here to
///   `00:00:00` of the next, which is what XML Schema Part 2 §3.2.7 says it denotes.
/// * **the year may be negative and may exceed four digits.** Neither appears in a
///   household message, and both are cheap to accept and expensive to reject: a parser
///   that refuses them looks, from the outside, exactly like a peer that sent nothing.
///
/// Equality is *structural*, not chronological. `2026-09-05T08:15:00Z` and
/// `2026-09-05T10:15:00+02:00` are the same instant and different values here, because
/// what a peer wrote is part of what it said. Compare
/// [`unix_seconds`](Self::unix_seconds) to compare instants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DateTime {
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    nanosecond: u32,
    offset_minutes: Option<i16>,
}

impl DateTime {
    /// The proleptic Gregorian year. Negative for a year before 1 BCE's successor.
    pub const fn year(&self) -> i32 {
        self.year
    }

    /// The month, `1..=12`.
    pub const fn month(&self) -> u8 {
        self.month
    }

    /// The day of the month, `1..=31`.
    pub const fn day(&self) -> u8 {
        self.day
    }

    /// The hour, `0..=23`. A parsed `24:00:00` has already rolled into the next day.
    pub const fn hour(&self) -> u8 {
        self.hour
    }

    /// The minute, `0..=59`.
    pub const fn minute(&self) -> u8 {
        self.minute
    }

    /// The second, `0..=59`. XML Schema has no leap second, so `60` never parses.
    pub const fn second(&self) -> u8 {
        self.second
    }

    /// The fractional second, in nanoseconds. Digits beyond the ninth are dropped.
    pub const fn nanosecond(&self) -> u32 {
        self.nanosecond
    }

    /// Minutes east of UTC, or [`None`] for a timestamp that carried no offset.
    ///
    /// `Some(0)` is `Z` — explicitly UTC — and [`None`] is a local time whose zone the
    /// peer never said. The two are different claims and only the first fixes an instant.
    pub const fn offset_minutes(&self) -> Option<i16> {
        self.offset_minutes
    }

    /// Whether the peer said which zone it meant.
    pub const fn is_zoned(&self) -> bool {
        self.offset_minutes.is_some()
    }

    /// Seconds since the Unix epoch, or [`None`] when the timestamp carried no offset.
    ///
    /// [`None`] is not a parse failure: it is a well-formed `xs:dateTime` that names a
    /// wall-clock reading rather than an instant. Decide what zone it meant and use
    /// [`unix_seconds_at`](Self::unix_seconds_at), or fall back to the arrival time —
    /// but do it deliberately, because the two differ by up to fourteen hours.
    pub const fn unix_seconds(&self) -> Option<i64> {
        match self.offset_minutes {
            Some(offset) => Some(self.unix_seconds_at(offset)),
            None => None,
        }
    }

    /// The same, reading the timestamp as though it carried `offset_minutes`.
    ///
    /// An offset the value *does* carry wins: this resolves the timezone-less case and
    /// leaves a zoned timestamp alone, so a caller can apply its local zone to every
    /// reading without checking each one first.
    pub const fn unix_seconds_at(&self, offset_minutes: i16) -> i64 {
        let offset = match self.offset_minutes {
            Some(carried) => carried,
            None => offset_minutes,
        };
        let days = days_from_civil(self.year, self.month, self.day);
        days * 86_400 + self.hour as i64 * 3_600 + self.minute as i64 * 60 + self.second as i64
            - offset as i64 * 60
    }

    /// The UTC instant `seconds` after the Unix epoch, written with a `Z` offset.
    ///
    /// What a device stamping its own measurement needs: SPINE has no "now", so a
    /// `timestamp` element is built from the sender's own clock.
    ///
    /// ```
    /// use eebus::model::DateTime;
    ///
    /// let stamp = DateTime::from_unix_seconds(1_788_588_900);
    /// assert_eq!(stamp.to_string(), "2026-09-05T06:15:00Z");
    /// ```
    pub const fn from_unix_seconds(seconds: i64) -> Self {
        // Rust's `/` truncates toward zero and `%` follows it, so a pre-epoch instant
        // needs the remainder pulled back into `0..86_400` before the day is counted.
        let mut days = seconds / 86_400;
        let mut rem = seconds % 86_400;
        if rem < 0 {
            rem += 86_400;
            days -= 1;
        }
        let (year, month, day) = civil_from_days(days);
        Self {
            year,
            month,
            day,
            hour: (rem / 3_600) as u8,
            minute: ((rem % 3_600) / 60) as u8,
            second: (rem % 60) as u8,
            nanosecond: 0,
            offset_minutes: Some(0),
        }
    }

    /// The same timestamp, declared to be at `offset_minutes` east of UTC.
    ///
    /// This *labels* a timezone-less reading; it does not shift the clock reading. Use it
    /// where the local zone is known from outside the protocol — a commissioning setting,
    /// say — and the peer omitted the offset.
    #[must_use]
    pub const fn with_offset(mut self, offset_minutes: i16) -> Self {
        self.offset_minutes = Some(offset_minutes);
        self
    }

    /// The instant as a [`SystemTime`](std::time::SystemTime), where one is determined.
    ///
    /// [`None`] for a timestamp with no offset, for the same reason
    /// [`unix_seconds`](Self::unix_seconds) is.
    #[cfg(feature = "std")]
    pub fn to_system_time(&self) -> Option<std::time::SystemTime> {
        let seconds = self.unix_seconds()?;
        let nanos = u64::from(self.nanosecond);
        Some(if seconds >= 0 {
            std::time::SystemTime::UNIX_EPOCH
                + core::time::Duration::new(seconds.unsigned_abs(), 0)
                + core::time::Duration::from_nanos(nanos)
        } else {
            std::time::SystemTime::UNIX_EPOCH - core::time::Duration::new(seconds.unsigned_abs(), 0)
                + core::time::Duration::from_nanos(nanos)
        })
    }
}

impl core::fmt::Display for DateTime {
    /// Writes the canonical `xs:dateTime` lexical form, which is what SPINE puts on the
    /// wire. A parsed value round-trips through this except for the two spellings the
    /// schema calls equivalent: `24:00:00` comes back as the next day's `00:00:00`, and a
    /// fractional second loses its trailing zeroes.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.year < 0 {
            write!(f, "-{:04}", self.year.unsigned_abs())?;
        } else {
            write!(f, "{:04}", self.year)?;
        }
        write!(
            f,
            "-{:02}-{:02}T{:02}:{:02}:{:02}",
            self.month, self.day, self.hour, self.minute, self.second
        )?;
        if self.nanosecond > 0 {
            let mut frac = format!("{:09}", self.nanosecond);
            while frac.ends_with('0') {
                frac.pop();
            }
            write!(f, ".{frac}")?;
        }
        match self.offset_minutes {
            None => Ok(()),
            Some(0) => f.write_str("Z"),
            Some(offset) => {
                let sign = if offset < 0 { '-' } else { '+' };
                let magnitude = offset.unsigned_abs();
                write!(f, "{sign}{:02}:{:02}", magnitude / 60, magnitude % 60)
            }
        }
    }
}

/// Parses the `xs:dateTime` half of [`AbsoluteOrRelativeTime`].
///
/// Strict about everything that carries meaning and lenient about nothing: a month of
/// `13`, a 29 February in a common year, an offset past ±14:00 and a missing `T` are all
/// [`None`], because a timestamp that is wrong is worse than one that is absent — the
/// second is visibly missing and the first is silently believed.
///
/// ```
/// use eebus::model::parse_xs_date_time;
///
/// let utc = parse_xs_date_time("2026-09-05T08:15:00Z").expect("well formed");
/// assert_eq!(utc.unix_seconds(), Some(1_788_596_100));
///
/// // No offset: a wall-clock reading, and no instant at all.
/// let local = parse_xs_date_time("2026-09-05T08:15:00").expect("well formed");
/// assert_eq!(local.unix_seconds(), None);
///
/// // Midnight ending the day, which XML Schema spells `24:00:00`.
/// let rollover = parse_xs_date_time("2026-09-05T24:00:00Z").expect("well formed");
/// assert_eq!(rollover.to_string(), "2026-09-06T00:00:00Z");
///
/// assert_eq!(parse_xs_date_time("2025-02-29T00:00:00Z"), None, "not a leap year");
/// assert_eq!(parse_xs_date_time("PT2H"), None, "that is the other half");
/// ```
pub fn parse_xs_date_time(s: &str) -> Option<DateTime> {
    let (negative_year, rest) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };

    // The year runs to the first `-`, and is at least four digits. More than four is
    // permitted only without a leading zero, which is what stops `00002026` — a spelling
    // the schema forbids and whose acceptance would make two texts one value.
    let (year_text, rest) = rest.split_once('-')?;
    if year_text.len() < 4 || !year_text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if year_text.len() > 4 && year_text.starts_with('0') {
        return None;
    }
    let year: i32 = year_text.parse().ok()?;
    let year = if negative_year { -year } else { year };

    let (month, rest) = take_fixed_digits(rest, 2)?;
    let rest = rest.strip_prefix('-')?;
    let (day, rest) = take_fixed_digits(rest, 2)?;
    let rest = rest.strip_prefix('T')?;
    let (hour, rest) = take_fixed_digits(rest, 2)?;
    let rest = rest.strip_prefix(':')?;
    let (minute, rest) = take_fixed_digits(rest, 2)?;
    let rest = rest.strip_prefix(':')?;
    let (second, rest) = take_fixed_digits(rest, 2)?;

    let (nanosecond, rest) = match rest.strip_prefix('.') {
        Some(rest) => {
            let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            if digits == 0 {
                // `xs:dateTime` requires at least one digit after the point.
                return None;
            }
            (parse_fraction(&rest[..digits]), &rest[digits..])
        }
        None => (0, rest),
    };

    let offset_minutes = parse_offset(rest)?;

    let month = month as u8;
    let day = day as u8;
    if !(1..=12).contains(&month) || day < 1 || u32::from(day) > days_in_month(year, month) {
        return None;
    }
    if minute > 59 || second > 59 {
        return None;
    }

    let mut parsed = DateTime {
        year,
        month,
        day,
        hour: hour as u8,
        minute: minute as u8,
        second: second as u8,
        nanosecond,
        offset_minutes,
    };
    match hour {
        0..=23 => Some(parsed),
        // §3.2.7: `24:00:00` is permitted and denotes the start of the following day.
        // Only exactly midnight — `24:00:01` names no time at all.
        24 if minute == 0 && second == 0 && nanosecond == 0 => {
            let (year, month, day) = civil_from_days(days_from_civil(year, month, day) + 1);
            parsed.year = year;
            parsed.month = month;
            parsed.day = day;
            parsed.hour = 0;
            Some(parsed)
        }
        _ => None,
    }
}

/// Reads exactly `count` ASCII digits, returning them and what follows.
fn take_fixed_digits(s: &str, count: usize) -> Option<(u32, &str)> {
    let head = s.get(..count)?;
    if !head.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((head.parse().ok()?, &s[count..]))
}

/// A fractional-second string as nanoseconds, truncating past the ninth digit.
fn parse_fraction(digits: &str) -> u32 {
    let mut nanos = 0u32;
    for i in 0..9 {
        nanos = nanos * 10 + digits.as_bytes().get(i).map_or(0, |b| u32::from(b - b'0'));
    }
    nanos
}

/// The timezone part: absent, `Z`, or `±hh:mm` within ±14:00.
fn parse_offset(s: &str) -> Option<Option<i16>> {
    if s.is_empty() {
        return Some(None);
    }
    if s == "Z" {
        return Some(Some(0));
    }
    let (sign, rest) = match s.as_bytes().first()? {
        b'+' => (1i16, &s[1..]),
        b'-' => (-1i16, &s[1..]),
        _ => return None,
    };
    let (hours, rest) = take_fixed_digits(rest, 2)?;
    let rest = rest.strip_prefix(':')?;
    let (minutes, rest) = take_fixed_digits(rest, 2)?;
    if !rest.is_empty() || minutes > 59 {
        return None;
    }
    let total = hours as i16 * 60 + minutes as i16;
    // §3.2.7 bounds the offset at ±14:00, which is wider than any zone in use and
    // narrow enough that `+99:00` is refused rather than believed.
    if total > 14 * 60 {
        return None;
    }
    Some(Some(sign * total))
}

/// Days in a month of the proleptic Gregorian calendar.
const fn days_in_month(year: i32, month: u8) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

const fn is_leap_year(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// Days from the Unix epoch to a proleptic Gregorian date, and back.
///
/// Howard Hinnant's `days_from_civil`, which is exact for every year an `i32` holds and
/// needs neither a table nor a loop — so it costs the same on a microcontroller as it
/// does anywhere else. The era is 400 years, the cycle over which the Gregorian calendar
/// repeats exactly (146 097 days), and shifting the year to start in March puts the leap
/// day at the end where it perturbs nothing.
const fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let y = if month <= 2 { year - 1 } else { year } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse of [`days_from_civil`].
const fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    ((y + if m <= 2 { 1 } else { 0 }) as i32, m as u8, d as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::time::Duration;

    #[test]
    fn durations_round_trip() {
        for secs in [0u64, 1, 59, 60, 90, 3_600, 7_200, 86_400, 129_600, 86_399] {
            let text = format_iso8601_duration(Duration::from_secs(secs));
            assert_eq!(
                parse_iso8601_duration(&text),
                Some(Duration::from_secs(secs)),
                "{text} (from {secs}s)"
            );
        }
    }

    #[test]
    fn lpc_timers_parse() {
        // Heartbeat timeout, LPC UC TS §3.2.1.2.2.1: "PT60S" or "PT1M".
        assert_eq!(
            parse_iso8601_duration("PT60S"),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            parse_iso8601_duration("PT1M"),
            Some(Duration::from_secs(60))
        );
        // Failsafe duration minimum, [LPC-022]: 2 h to 24 h.
        assert_eq!(
            parse_iso8601_duration("PT2H"),
            Some(Duration::from_secs(7_200))
        );
        assert_eq!(
            parse_iso8601_duration("PT120M"),
            Some(Duration::from_secs(7_200))
        );
        assert_eq!(
            parse_iso8601_duration("PT24H"),
            Some(Duration::from_secs(86_400))
        );
    }

    #[test]
    fn fractional_seconds_are_kept() {
        assert_eq!(
            parse_iso8601_duration("PT0.5S"),
            Some(Duration::from_millis(500))
        );
        assert_eq!(
            format_iso8601_duration(Duration::from_millis(1_500)),
            "PT1.5S"
        );
    }

    #[test]
    fn calendar_units_are_rejected() {
        assert_eq!(parse_iso8601_duration("P1Y"), None);
        assert_eq!(parse_iso8601_duration("P1M"), None);
        assert_eq!(parse_iso8601_duration("nonsense"), None);
    }

    /// A duration with no components says nothing, and reading it as zero would turn a
    /// truncated field into "deactivate now".
    #[test]
    fn a_duration_without_components_is_rejected() {
        assert_eq!(parse_iso8601_duration("P"), None);
        assert_eq!(parse_iso8601_duration("PT"), None);
        assert_eq!(parse_iso8601_duration("PT.S"), None);
        // Zero itself is still expressible, and round trips.
        assert_eq!(parse_iso8601_duration("PT0S"), Some(Duration::ZERO));
        assert_eq!(format_iso8601_duration(Duration::ZERO), "PT0S");
    }

    /// A span pointing backwards has already elapsed, so nothing is left to run.
    ///
    /// `core::time::Duration` is unsigned, so the only alternatives are zero and the
    /// magnitude — and the magnitude is the dangerous one: it would keep a curtailment
    /// whose `endTime` lies two hours in the past alive for another two hours.
    #[test]
    fn a_negative_duration_collapses_to_zero() {
        assert_eq!(parse_iso8601_duration("-PT2H"), Some(Duration::ZERO));
        assert_eq!(parse_iso8601_duration("-P1DT12H"), Some(Duration::ZERO));
        // Still parsed, so malformed input is still refused rather than zeroed.
        assert_eq!(parse_iso8601_duration("-P1M"), None);
        assert_eq!(parse_iso8601_duration("-PT"), None);
    }

    /// The three shapes an RFC 3339 parser gets wrong, and the offset arithmetic.
    #[test]
    fn timestamps_parse_in_every_form_the_schema_permits() {
        let utc = parse_xs_date_time("2026-09-05T08:15:00Z").expect("well formed");
        assert_eq!(utc.unix_seconds(), Some(1_788_596_100));
        assert_eq!(utc.offset_minutes(), Some(0));

        // An offset east of UTC names an earlier instant for the same clock reading.
        let east = parse_xs_date_time("2026-09-05T08:15:00+02:00").expect("well formed");
        assert_eq!(east.unix_seconds(), Some(1_788_588_900));
        let west = parse_xs_date_time("2026-09-05T08:15:00-05:30").expect("well formed");
        assert_eq!(west.unix_seconds(), Some(1_788_615_900));

        // No offset at all: well formed, and no instant.
        let local = parse_xs_date_time("2026-09-05T08:15:00").expect("well formed");
        assert_eq!(local.offset_minutes(), None);
        assert_eq!(local.unix_seconds(), None);
        assert_eq!(local.unix_seconds_at(0), 1_788_596_100);
        // A carried offset wins over the assumed one, so a caller may apply its zone
        // to every reading without sorting them first.
        assert_eq!(east.unix_seconds_at(0), 1_788_588_900);

        // Fractional seconds, to the nanosecond and no further.
        let fraction = parse_xs_date_time("2026-09-05T08:15:00.123456789012Z").expect("ok");
        assert_eq!(fraction.nanosecond(), 123_456_789);

        // A year outside four digits, and a negative one.
        assert_eq!(
            parse_xs_date_time("12026-09-05T00:00:00Z").map(|t| t.year()),
            Some(12_026)
        );
        assert_eq!(
            parse_xs_date_time("-0044-03-15T00:00:00Z").map(|t| t.year()),
            Some(-44)
        );
    }

    /// `24:00:00` is the start of the following day (XML Schema Part 2 §3.2.7), and the
    /// only hour past 23 that names a time at all.
    #[test]
    fn the_twenty_fourth_hour_rolls_into_the_next_day() {
        let end_of_year = parse_xs_date_time("2026-12-31T24:00:00Z").expect("well formed");
        assert_eq!(end_of_year.to_string(), "2027-01-01T00:00:00Z");
        assert_eq!(parse_xs_date_time("2026-09-05T24:00:01Z"), None);
        assert_eq!(parse_xs_date_time("2026-09-05T24:30:00Z"), None);
        assert_eq!(parse_xs_date_time("2026-09-05T25:00:00Z"), None);
    }

    /// A timestamp that is wrong is worse than one that is absent: the second is visibly
    /// missing and the first is silently believed.
    #[test]
    fn malformed_timestamps_are_refused_rather_than_repaired() {
        for bad in [
            "",
            "2026-09-05",             // a date is not a dateTime
            "2026-9-05T08:15:00Z",    // unpadded month
            "26-09-05T08:15:00Z",     // two-digit year
            "002026-09-05T08:15:00Z", // padded past four digits
            "2026-13-01T00:00:00Z",   // no thirteenth month
            "2026-00-01T00:00:00Z",
            "2026-09-31T00:00:00Z",      // September has thirty days
            "2025-02-29T00:00:00Z",      // not a leap year
            "2026-09-05T08:60:00Z",      // no sixtieth minute
            "2026-09-05T08:15:60Z",      // no leap second in XML Schema
            "2026-09-05 08:15:00Z",      // a space is not a `T`
            "2026-09-05T08:15:00.Z",     // a point with no digits
            "2026-09-05T08:15:00+15:00", // past the ±14:00 bound
            "2026-09-05T08:15:00+02:60",
            "2026-09-05T08:15:00+0200", // the colon is not optional
            "2026-09-05T08:15:00CEST",
            "2026-09-05T08:15:00Z ",
        ] {
            assert_eq!(
                parse_xs_date_time(bad),
                None,
                "{bad:?} is not an xs:dateTime"
            );
        }
        // The leap day itself, in the years that have one.
        assert!(parse_xs_date_time("2024-02-29T00:00:00Z").is_some());
        assert!(
            parse_xs_date_time("2000-02-29T00:00:00Z").is_some(),
            "a 400-year leap"
        );
        assert_eq!(
            parse_xs_date_time("1900-02-29T00:00:00Z"),
            None,
            "a 100-year skip"
        );
    }

    /// The two civil-calendar conversions are inverses, across the epoch and the leap
    /// rules on both sides of it.
    #[test]
    fn the_epoch_conversion_round_trips() {
        for seconds in [
            0i64,
            1_788_596_100,
            -1,
            -86_400,
            -2_208_988_800, // 1900-01-01
            951_782_400,    // 2000-02-29
            4_102_444_800,  // 2100-01-01, which is not a leap year
        ] {
            let stamp = DateTime::from_unix_seconds(seconds);
            assert_eq!(stamp.unix_seconds(), Some(seconds), "{seconds}");
            let text = alloc::string::ToString::to_string(&stamp);
            assert_eq!(parse_xs_date_time(&text), Some(stamp), "{text}");
        }
        assert_eq!(
            DateTime::from_unix_seconds(0).to_string(),
            "1970-01-01T00:00:00Z"
        );
        assert_eq!(
            DateTime::from_unix_seconds(-1).to_string(),
            "1969-12-31T23:59:59Z"
        );
        assert_eq!(
            DateTime::from_unix_seconds(951_782_400).to_string(),
            "2000-02-29T00:00:00Z"
        );
    }

    /// Every day of a four-century era converts and converts back. The era is the cycle
    /// the Gregorian calendar repeats over, so a conversion correct across one is correct
    /// everywhere.
    #[test]
    fn every_day_of_a_gregorian_era_survives_the_round_trip() {
        let mut expected = (1_600, 1u8, 1u8);
        let first = super::days_from_civil(1_600, 1, 1);
        for offset in 0..146_097i64 {
            let (year, month, day) = super::civil_from_days(first + offset);
            assert_eq!((year, month, day), expected, "day {offset} of the era");
            assert_eq!(super::days_from_civil(year, month, day), first + offset);
            expected = next_day(expected);
        }
    }

    fn next_day((year, month, day): (i32, u8, u8)) -> (i32, u8, u8) {
        if u32::from(day) < super::days_in_month(year, month) {
            (year, month, day + 1)
        } else if month == 12 {
            (year + 1, 1, 1)
        } else {
            (year, month + 1, 1)
        }
    }

    /// Which half of the union arrived, and what each half is for.
    #[test]
    fn both_halves_of_the_union_are_readable() {
        let absolute = AbsoluteOrRelativeTime::from("2026-09-05T08:15:00Z");
        assert!(absolute.is_absolute());
        assert!(!absolute.is_relative());
        assert_eq!(
            absolute.parse(),
            Some(TimeValue::Absolute(
                parse_xs_date_time("2026-09-05T08:15:00Z").expect("well formed")
            ))
        );

        let relative = AbsoluteOrRelativeTime::from("PT30S");
        assert!(relative.is_relative());
        assert_eq!(relative.as_timestamp(), None);
        assert_eq!(
            relative.parse(),
            Some(TimeValue::Relative(Duration::from_secs(30)))
        );

        // The bug this closes: a value that says it is absolute and cannot be read is
        // now distinguishable from one that is a duration.
        let broken = AbsoluteOrRelativeTime::from("2026-09-05 08:15:00");
        assert!(broken.is_absolute(), "the peer meant a timestamp");
        assert_eq!(broken.as_timestamp(), None, "and this crate cannot read it");
        assert_eq!(broken.parse(), None);
    }

    /// What a device sends is what a device can read back.
    #[test]
    fn timestamps_round_trip_through_the_wire_type() {
        let stamp = DateTime::from_unix_seconds(1_788_596_100);
        let sent = AbsoluteOrRelativeTime::from_timestamp(stamp);
        assert_eq!(sent.as_str(), "2026-09-05T08:15:00Z");
        assert_eq!(sent.as_timestamp(), Some(stamp));

        // A fraction survives, without its trailing zeroes.
        let fraction = parse_xs_date_time("2026-09-05T08:15:00.500Z").expect("well formed");
        assert_eq!(fraction.to_string(), "2026-09-05T08:15:00.5Z");
        assert_eq!(parse_xs_date_time("2026-09-05T08:15:00.5Z"), Some(fraction));

        // So does an offset, in both directions and to the minute.
        for text in [
            "2026-09-05T08:15:00+02:00",
            "2026-09-05T08:15:00-05:30",
            "2026-09-05T08:15:00+14:00",
            "2026-09-05T08:15:00",
        ] {
            let parsed = parse_xs_date_time(text).expect("well formed");
            assert_eq!(parsed.to_string(), text);
        }
    }

    /// Two spellings of one instant are two values, and the same instant.
    #[test]
    fn equality_is_what_the_peer_wrote_and_not_when_it_meant() {
        let utc = parse_xs_date_time("2026-09-05T08:15:00Z").expect("well formed");
        let berlin = parse_xs_date_time("2026-09-05T10:15:00+02:00").expect("well formed");
        assert_ne!(utc, berlin);
        assert_eq!(utc.unix_seconds(), berlin.unix_seconds());
    }

    #[cfg(feature = "std")]
    #[test]
    fn a_zoned_timestamp_becomes_a_system_time() {
        let stamp = parse_xs_date_time("2026-09-05T08:15:00.25Z").expect("well formed");
        let at = stamp.to_system_time().expect("zoned");
        assert_eq!(
            at.duration_since(std::time::SystemTime::UNIX_EPOCH).ok(),
            Some(Duration::new(1_788_596_100, 250_000_000))
        );
        // Before the epoch, where the sign of the seconds and of the fraction differ.
        let old = parse_xs_date_time("1969-12-31T23:59:59Z").expect("well formed");
        assert_eq!(
            std::time::SystemTime::UNIX_EPOCH
                .duration_since(old.to_system_time().expect("zoned"))
                .ok(),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            parse_xs_date_time("2026-09-05T08:15:00")
                .expect("well formed")
                .to_system_time(),
            None,
            "no offset, no instant"
        );
    }
}
