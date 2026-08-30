//! Working with SPINE's scalar value types.
//!
//! The generated model mirrors the schemas exactly, which leaves two everyday jobs to
//! hand-written code: turning a [`ScaledNumber`] into a number you can compute with,
//! and reading the ISO 8601 durations and timestamps that SPINE carries as strings.

use alloc::format;
use alloc::string::String;

use crate::model::{AbsoluteOrRelativeTime, Number, Scale, ScaledNumber};

/// The largest power of ten a [`ScaledNumber`] can represent exactly in `f64`.
const MAX_ABS_SCALE: i32 = 308;

impl ScaledNumber {
    /// A scaled number with both elements set.
    ///
    /// The SPINE implementation guide §3.2.1 makes `scale` mandatory on the wire — an
    /// erratum to the Resource Specification, which used to let it default to zero and
    /// which "severe misinterpretations" in the field made untenable. Constructing
    /// values through this function keeps messages on the right side of that rule.
    ///
    /// ```
    /// use eebus::model::ScaledNumber;
    ///
    /// let watts = ScaledNumber::new(42, 2); // 42 · 10² = 4200 W
    /// assert_eq!(watts.to_f64(), Some(4200.0));
    /// assert_eq!(serde_json::to_string(&watts).unwrap(), r#"[{"number":42},{"scale":2}]"#);
    /// ```
    pub fn new(number: i64, scale: i16) -> Self {
        Self {
            number: Some(Number(number)),
            scale: Some(Scale(scale)),
        }
    }

    /// A scaled number with an implicit scale of zero, written out explicitly.
    pub fn whole(number: i64) -> Self {
        Self::new(number, 0)
    }

    /// The value as `number × 10^scale`, or [`None`] if `number` is absent.
    ///
    /// An absent `scale` is read as zero, which is what the Resource Specification says
    /// and what peers written against it send.
    pub fn to_f64(&self) -> Option<f64> {
        let number = self.number?.get() as f64;
        let scale = i32::from(self.scale.unwrap_or(Scale(0)).get());
        if abs_i32(scale) > MAX_ABS_SCALE {
            return None;
        }
        Some(number * pow10(scale))
    }

    /// Approximates `value` with at most `max_decimals` decimal places.
    ///
    /// The scale is chosen as the smallest that represents `value` without loss up to
    /// that limit, so whole numbers come out as `scale = 0` rather than as a needlessly
    /// precise fraction.
    ///
    /// ```
    /// use eebus::model::ScaledNumber;
    ///
    /// assert_eq!(ScaledNumber::from_f64(4200.0, 2), ScaledNumber::new(4200, 0));
    /// assert_eq!(ScaledNumber::from_f64(23.5, 2), ScaledNumber::new(235, -1));
    /// ```
    pub fn from_f64(value: f64, max_decimals: u8) -> Self {
        let max_decimals = i16::from(max_decimals.min(9));
        for decimals in 0..=max_decimals {
            let scaled = value * pow10(i32::from(decimals));
            let rounded = round_half_away(scaled);
            if abs_f64(scaled - rounded) < 1e-9 {
                return Self::new(rounded as i64, -decimals);
            }
        }
        let scaled = value * pow10(i32::from(max_decimals));
        Self::new(round_half_away(scaled) as i64, -max_decimals)
    }

    /// True when both elements are present, as the implementation guide requires for a
    /// message that is not a partial (Restricted Function Exchange) update.
    pub fn is_complete(&self) -> bool {
        self.number.is_some() && self.scale.is_some()
    }

    /// Fills in an absent `scale` with zero.
    ///
    /// Call this before sending a full — non-partial — message. In a partial message an
    /// omitted element means "unchanged", so normalising there would overwrite the
    /// peer's stored scale; the SPINE engine therefore applies this only where the
    /// implementation guide's table says a missing element would be invalid.
    #[must_use]
    pub fn normalized(mut self) -> Self {
        if self.number.is_some() && self.scale.is_none() {
            self.scale = Some(Scale(0));
        }
        self
    }
}

impl core::fmt::Display for ScaledNumber {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.to_f64() {
            Some(v) => write!(f, "{v}"),
            None => f.write_str("<unset>"),
        }
    }
}

impl AbsoluteOrRelativeTime {
    /// Reads the value as an ISO 8601 duration, e.g. `PT2H` or `PT60S`.
    ///
    /// Returns [`None`] when the value is a timestamp rather than a duration, or when
    /// the duration carries months or years, whose length depends on a calendar the
    /// protocol does not supply.
    pub fn as_duration(&self) -> Option<core::time::Duration> {
        parse_iso8601_duration(self.as_str())
    }

    /// Formats a duration as ISO 8601, e.g. `Duration::from_secs(7200)` → `PT2H`.
    pub fn from_duration(duration: core::time::Duration) -> Self {
        Self(format_iso8601_duration(duration))
    }

    /// True when the value looks like an absolute timestamp rather than a duration.
    pub fn is_absolute(&self) -> bool {
        !self.as_str().starts_with('P') && !self.as_str().starts_with("-P")
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
/// ```
pub fn parse_iso8601_duration(s: &str) -> Option<core::time::Duration> {
    let s = s.strip_prefix('-').map_or(s, |rest| rest); // negative durations clamp to 0 below
    let rest = s.strip_prefix('P')?;
    let (date_part, time_part) = match rest.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (rest, None),
    };

    let mut secs: u64 = 0;
    let mut nanos: u32 = 0;

    let mut number = String::new();
    for c in date_part.chars() {
        match c {
            '0'..='9' | '.' => number.push(c),
            'Y' | 'M' => return None, // calendar-dependent
            'W' => {
                secs = secs.checked_add(take_u64(&mut number)?.checked_mul(604_800)?)?;
            }
            'D' => {
                secs = secs.checked_add(take_u64(&mut number)?.checked_mul(86_400)?)?;
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
                'H' => secs = secs.checked_add(take_u64(&mut number)?.checked_mul(3_600)?)?,
                'M' => secs = secs.checked_add(take_u64(&mut number)?.checked_mul(60)?)?,
                'S' => {
                    let raw = core::mem::take(&mut number);
                    let (whole, frac) = match raw.split_once('.') {
                        Some((w, f)) => (w, Some(f)),
                        None => (raw.as_str(), None),
                    };
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
                }
                _ => return None,
            }
        }
        if !number.is_empty() {
            return None;
        }
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

/// `10^exp`, computed by repeated multiplication.
///
/// `f64::powi` lives in `std`; this crate's protocol core also builds for bare-metal
/// targets, where only `core` is available. Scales in SPINE are small, so a bounded
/// loop costs nothing and avoids a dependency on a maths library.
fn pow10(exp: i32) -> f64 {
    let mut out = 1.0f64;
    if exp >= 0 {
        for _ in 0..exp.min(MAX_ABS_SCALE) {
            out *= 10.0;
        }
    } else {
        for _ in 0..(-exp).min(MAX_ABS_SCALE) {
            out /= 10.0;
        }
    }
    out
}

fn abs_f64(v: f64) -> f64 {
    if v < 0.0 { -v } else { v }
}

fn abs_i32(v: i32) -> i32 {
    if v < 0 { -v } else { v }
}

/// Rounds halfway cases away from zero, as `f64::round` does.
fn round_half_away(v: f64) -> f64 {
    let shifted = if v >= 0.0 { v + 0.5 } else { v - 0.5 };
    // A cast to `i64` truncates toward zero, which is what turns the shift into a round.
    (shifted as i64) as f64
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
        assert_eq!(parse_iso8601_duration("PT"), Some(Duration::ZERO));
    }

    #[test]
    fn scaled_numbers_convert_both_ways() {
        assert_eq!(ScaledNumber::new(42, 2).to_f64(), Some(4_200.0));
        assert_eq!(ScaledNumber::new(235, -1).to_f64(), Some(23.5));
        assert_eq!(
            ScaledNumber::from_f64(4_200.0, 2),
            ScaledNumber::new(4_200, 0)
        );
        assert_eq!(ScaledNumber::from_f64(23.5, 2), ScaledNumber::new(235, -1));
        assert_eq!(ScaledNumber::from_f64(0.125, 3), ScaledNumber::new(125, -3));
    }

    #[test]
    fn an_absent_scale_reads_as_zero_but_is_incomplete() {
        let partial = ScaledNumber {
            number: Some(Number(7)),
            scale: None,
        };
        assert_eq!(partial.to_f64(), Some(7.0));
        assert!(!partial.is_complete());
        assert!(partial.normalized().is_complete());
    }
}
