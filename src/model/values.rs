//! Working with SPINE's scalar value types.
//!
//! The generated model mirrors the schemas exactly, which leaves two everyday jobs to
//! hand-written code: turning a [`ScaledNumber`] into a number you can compute with,
//! and reading the ISO 8601 durations and timestamps that SPINE carries as strings.

use alloc::format;
use alloc::string::String;

use crate::model::{AbsoluteOrRelativeTime, Number, Scale, ScaledNumber};

/// The powers of ten that are exactly representable in `f64`.
///
/// `10^n` is exact up to `n = 22`; beyond that the nearest `f64` is not a power of ten at
/// all. Scaling by an entry of this table rather than by a magnitude built up from
/// repeated multiplication is what keeps [`ScaledNumber::to_f64`] correctly rounded over
/// the whole range a `scale` can name — see [`scaled_to_f64`] and [`pow10`].
const POW10: [f64; 23] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
];

/// The largest exponent [`POW10`] holds, and so the largest step that scales a value by
/// an exact power of ten.
const MAX_EXACT_POW10: i32 = POW10.len() as i32 - 1;

/// The first exponent for which `10^exp` is not a finite `f64`.
///
/// `f64::MAX` is about `1.798 · 10^308`, so `10^308` is the last one that fits and
/// everything from here up is infinity — the correctly rounded answer for a product that
/// large, and zero for the reciprocal.
const POW10_OVERFLOWS_AT: u32 = 309;

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

    /// The value as `number × 10^scale`, or [`None`] if it is not a finite number.
    ///
    /// An absent `scale` is read as zero, which is what the Resource Specification says
    /// and what peers written against it send.
    ///
    /// A product that overflows `f64` is [`None`] rather than infinity: `scale` is signed
    /// 16-bit, so a `number` near [`i64::MAX`] reaches infinity in one well-formed
    /// message, and an unrepresentable value is not a value. Saying so lets the caller
    /// refuse the message instead of acting on a number nobody sent.
    ///
    /// ```
    /// use eebus::model::ScaledNumber;
    ///
    /// assert_eq!(ScaledNumber::new(42, 2).to_f64(), Some(4200.0));
    /// assert_eq!(ScaledNumber::new(i64::MAX, 308).to_f64(), None, "overflows f64");
    /// ```
    pub fn to_f64(&self) -> Option<f64> {
        let number = self.number?.get();
        let scale = self.scale.unwrap_or(Scale(0)).get();
        scaled_to_f64(number, scale)
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
        // Neither infinity nor NaN is a quantity, and the alternative to zero here is a
        // saturated `i64::MAX` on the wire.
        if !value.is_finite() {
            return Self::new(0, 0);
        }
        // A value too large for `i64` at scale 0 is still perfectly representable — that
        // is what the scale is for. Raising it until the mantissa fits is the honest
        // answer; `value as i64` would saturate to `i64::MAX` and put a number nobody
        // asked for on the wire.
        let (value, base_scale) = fit_to_i64(value);
        if base_scale > 0 {
            return Self::new(round_half_away(value) as i64, base_scale);
        }

        let max_decimals = i16::from(max_decimals.min(9));
        for decimals in 0..=max_decimals {
            let scaled = shift_decimal(value, decimals);
            let rounded = round_half_away(scaled);
            // The tolerance is relative: an absolute 1e-9 is meaningless beside a
            // megawatt and impossibly strict beside a microamp.
            if abs_f64(scaled - rounded) <= abs_f64(scaled) * 1e-12 && fits_i64(rounded) {
                return Self::new(rounded as i64, -decimals);
            }
        }
        let scaled = shift_decimal(value, max_decimals);
        let (scaled, extra) = fit_to_i64(scaled);
        Self::new(round_half_away(scaled) as i64, extra - max_decimals)
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

/// The largest magnitude that survives `as i64` without saturating: `2^63`, less one
/// step, so every `f64` below it converts exactly.
const I64_LIMIT: f64 = 9.223_372_036_854_775e18;

/// `number × 10^scale`, correctly rounded, or [`None`] when that is not a finite `f64`.
///
/// **Multiplying by a negative power of ten is not the same as dividing by a positive
/// one, and only the second is correct.** No negative power of ten is representable in
/// binary, so `10^-4` is already wrong before the multiplication, and the product rounds
/// a second time: `12345 × 10⁻⁴` came out as `1.2345000000000002` rather than `1.2345`.
/// Dividing by the exact `1e4` rounds once, which is the correctly rounded answer.
///
/// **A scale beyond the exact table is applied in steps, never through a built-up power
/// of ten.** Only the first twenty-three powers of ten are `f64` values; `10^23` is not,
/// so scaling by it in one multiplication rounds the magnitude *and* the product. Taking
/// `10^22` and then `10^1` — each an exact factor — rounds once per step and gets `10^23`
/// exactly right. `scale` is a signed 16-bit integer, so a peer reaches that seam in one
/// schema-valid message.
///
/// The remaining inexactness is inherent to the return type rather than to this
/// function: a `number` above 2⁵³ does not fit an `f64` mantissa. SPINE carries
/// quantities, not counters, so that bound is far outside anything a device sends — but
/// it is the reason this returns `f64` rather than claiming to be exact.
fn scaled_to_f64(number: i64, scale: i16) -> Option<f64> {
    if number == 0 {
        return Some(0.0);
    }
    let mut value = number as f64;
    let mut remaining = i32::from(scale);
    while remaining != 0 {
        // Every step scales by an entry of the exact table, so it rounds once. `scale`
        // has one sign, so the steps all pull the same way and no intermediate result
        // overshoots a final answer that would have been representable.
        let step = remaining.clamp(-MAX_EXACT_POW10, MAX_EXACT_POW10);
        let magnitude = POW10[step.unsigned_abs() as usize];
        value = if step > 0 {
            value * magnitude
        } else {
            value / magnitude
        };
        if !value.is_finite() {
            // Overflowed on the way up. An unrepresentable value is not a value, and
            // saying so lets the caller refuse the message.
            return None;
        }
        if value == 0.0 {
            // Underflowed on the way down, which is the correctly rounded answer for a
            // quantity that small rather than a failure.
            return Some(0.0);
        }
        remaining -= step;
    }
    Some(value)
}

/// `10^exp` for a non-negative exponent, exact wherever `f64` allows it.
///
/// Up to `10^22` the value is read from [`POW10`], where each entry is the exact power.
/// Above that no `f64` is a power of ten, so the rest is assembled *one exact factor of
/// `10^22` at a time* rather than by multiplying by ten `exp - 22` times.
///
/// The difference is not cosmetic. Each multiplication rounds, so a chain of them
/// accumulates: building `10^308` a decade at a time lands 3·10¹⁷ ulps from the true
/// value, while multiplying by the largest exact power lands within 2 — the bound
/// `every_power_of_ten_is_within_two_ulps_of_the_correctly_rounded_value` holds it to.
/// `f64::powi` would do the same and lives in `std`, which the bare-metal build does not
/// have.
fn pow10(exp: u32) -> f64 {
    if let Some(exact) = POW10.get(exp as usize) {
        return *exact;
    }
    if exp >= POW10_OVERFLOWS_AT {
        return f64::INFINITY;
    }
    POW10[MAX_EXACT_POW10 as usize] * pow10(exp - MAX_EXACT_POW10 as u32)
}

/// `value × 10^decimals`, using the exact table so the shift rounds once.
fn shift_decimal(value: f64, decimals: i16) -> f64 {
    value * pow10(decimals.unsigned_abs().into())
}

/// True when `value` converts to `i64` without saturating.
fn fits_i64(value: f64) -> bool {
    abs_f64(value) < I64_LIMIT
}

/// Divides `value` down by tens until it fits an `i64`, returning the scale that buys.
///
/// `1e30` becomes `(1.0, 30)` — `number = 1, scale = 30` — rather than a saturated
/// `i64::MAX` at scale 0.
fn fit_to_i64(value: f64) -> (f64, i16) {
    if fits_i64(value) {
        return (value, 0);
    }
    // Each attempt divides the *original* by one exact power of ten, so the result
    // rounds once. Dividing by ten repeatedly would round once per step, and thirty
    // steps turned `1e30` into `1.0000000000000004e30`.
    let mut scale = 1i16;
    while scale < 300 {
        let shifted = value / pow10(u32::from(scale.unsigned_abs()));
        if fits_i64(shifted) {
            return (shifted, scale);
        }
        scale += 1;
    }
    (value / pow10(300), 300)
}

fn abs_f64(v: f64) -> f64 {
    if v < 0.0 { -v } else { v }
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

    /// A negative scale divides by an exact power of ten rather than multiplying by an
    /// inexact one.
    ///
    /// `10⁻⁴` is not representable, so multiplying by it rounds twice. Both spellings of
    /// the same quantity have to agree, or a limit changes when a peer chooses a different
    /// scale for it.
    #[test]
    fn a_negative_scale_is_correctly_rounded() {
        assert_eq!(ScaledNumber::new(12_345, -4).to_f64(), Some(1.2345));
        assert_eq!(ScaledNumber::new(1, -7).to_f64(), Some(1e-7));
        assert_eq!(ScaledNumber::new(235, -1).to_f64(), Some(23.5));
        assert_eq!(ScaledNumber::new(4_200_000, -3).to_f64(), Some(4200.0));

        // The same number written at four different scales is the same number.
        for scale in 0..=6i16 {
            let number = 42 * 10i64.pow(u32::from(scale as u16));
            assert_eq!(
                ScaledNumber::new(number, -scale).to_f64(),
                Some(42.0),
                "42 written at scale -{scale}"
            );
        }
    }

    /// A value too large for `i64` raises the scale instead of saturating.
    ///
    /// `1e30 as i64` is `i64::MAX`, which is a different number by a factor of nine —
    /// and it would reach the wire as a limit nobody asked for.
    #[test]
    fn a_huge_value_raises_the_scale_rather_than_saturating() {
        let big = ScaledNumber::from_f64(1e30, 2);
        assert_eq!(big.to_f64(), Some(1e30));
        assert!(
            big.number.expect("a number").get() < i64::MAX,
            "saturated instead of scaling: {big:?}"
        );

        let negative = ScaledNumber::from_f64(-1e25, 2);
        assert_eq!(negative.to_f64(), Some(-1e25));
    }

    /// Every quantity a device sends survives `f64` and comes back unchanged.
    ///
    /// This is the round trip the use-case layer depends on: it works in watts and
    /// amperes as `f64`, and the wire is exact, so the two only agree if the conversion
    /// is a bijection over the domain that can occur.
    #[test]
    fn realistic_quantities_round_trip_exactly() {
        let watts = [0.0, 1.0, 4200.0, 11_000.0, 23.5, 0.001, 16.0, 6.0, 3_333.33];
        for value in watts {
            let round_tripped = ScaledNumber::from_f64(value, 3)
                .to_f64()
                .expect("a finite quantity");
            assert_eq!(round_tripped, value, "{value} did not survive the wire");
        }
    }

    /// How far `a` is from `b`, counted in representable `f64` values between them.
    fn ulps_apart(a: f64, b: f64) -> i64 {
        (a.to_bits() as i64 - b.to_bits() as i64).abs()
    }

    /// Every power of ten `f64` can hold, against the correctly rounded answer.
    ///
    /// The oracle is the decimal-to-binary conversion in `str::parse`, which is correctly
    /// rounded by definition. Building the powers past the exact table a decade at a time
    /// put `10^308` 3·10¹⁷ ulps out, and — worse, because it is reachable — put `10^23`
    /// out by a whole factor of ten.
    #[test]
    fn every_power_of_ten_is_within_two_ulps_of_the_correctly_rounded_value() {
        for exp in 0..=308u32 {
            let exact: f64 = format!("1e{exp}").parse().expect("a finite power of ten");
            let ours = pow10(exp);
            let ulps = ulps_apart(exact, ours);
            assert!(
                ulps <= 2,
                "10^{exp}: {ours:e} is {ulps} ulps from {exact:e}"
            );
        }
        assert_eq!(pow10(309), f64::INFINITY, "past what an f64 can hold");
        assert_eq!(pow10(u32::MAX), f64::INFINITY);
    }

    /// The seam in the exact table is not a seam on the wire.
    ///
    /// `scale` is a signed 16-bit integer, so a peer crosses `10^22` in one schema-valid
    /// message. Scaling in exact steps keeps every scale that multiplies exact, and holds
    /// the ones that divide to a single rounding — where building `10^23` first and
    /// dividing by that lost a decimal digit as well as an ulp.
    #[test]
    fn a_scale_past_the_exact_table_is_still_that_scale() {
        for exp in 0..=30i16 {
            let exact: f64 = format!("1e{exp}").parse().expect("finite");
            assert_eq!(
                ScaledNumber::new(1, exp).to_f64(),
                Some(exact),
                "scale {exp} multiplies exactly"
            );
        }
        for exp in 0..=30i16 {
            let exact: f64 = format!("1e-{exp}").parse().expect("finite");
            let ours = ScaledNumber::new(1, -exp).to_f64().expect("finite");
            assert!(
                ulps_apart(exact, ours) <= 1,
                "scale -{exp}: {ours:e} against {exact:e}"
            );
        }
    }

    /// An overflowing scale is not a value, and a vanishing one is zero.
    #[test]
    fn unrepresentable_scales_are_reported_rather_than_approximated() {
        assert_eq!(ScaledNumber::new(i64::MAX, 308).to_f64(), None, "overflows");
        assert_eq!(ScaledNumber::new(i64::MAX, i16::MAX).to_f64(), None);
        // Below the smallest subnormal the answer really is zero, not "unreadable".
        assert_eq!(ScaledNumber::new(1, i16::MIN).to_f64(), Some(0.0));
        assert_eq!(ScaledNumber::new(0, i16::MAX).to_f64(), Some(0.0));
    }
}
