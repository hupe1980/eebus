//! Runtime signals: the values a certification laboratory has to be able to read.
//!
//! The High-Level Test Specifications keep saying the same thing in a footnote: *"the
//! manufacturer must specify conditions on how the test case can be tested (e.g. via
//! debug interface)"*. A tester driving `ATC_LPC_COM_PT_CSTransition6_001` has to see
//! that the limit's duration expired and the system left `limited`; it cannot see that
//! from the wire, because nothing is sent when a timer fires. The parameter sheet then
//! asks the manufacturer to write down where those values can be read.
//!
//! This module is that interface, as a shape rather than a transport. A use-case actor
//! answers [`signals`](Signals::signals) with a flat list of named values, and the
//! application decides where they go: a log line, a debug HTTP endpoint, a serial
//! console, a test harness's assertions. The names follow the convention the EEBUS Tester
//! uses — the use case's abbreviation, a colon, and the data point in `lowerCamelCase`:
//!
//! ```text
//! lpc:state             limited
//! lpc:limit             3000 W
//! lpc:duration          -
//! lpc:isActive          false
//! lpc:failsafeLimit     4200 W
//! lpc:failsafeDuration  7200 s
//! ```
//!
//! Nothing here is on the wire, and nothing here is required to run the protocol. It
//! exists so that "how do we test this?" has an answer that is written down and does not
//! drift from the implementation.
//!
//! ```
//! use core::time::Duration;
//! use eebus::usecases::limitation::{ControllableSystem, CsConfig, LimitWrite, LocalDecision};
//! use eebus::usecases::lpc;
//! use eebus::usecases::signals::Signals;
//!
//! let mut cs = ControllableSystem::new(
//!     CsConfig::new(4_200.0, Duration::from_secs(7_200)),
//!     Duration::ZERO,
//! );
//! cs.on_heartbeat(Duration::from_secs(1));
//! cs.on_limit_write(&LimitWrite::active(3_000.0), LocalDecision::Apply, Duration::from_secs(2));
//!
//! let signals = cs.signals(lpc::DIRECTION);
//! assert_eq!(signals.get("lpc:limit").and_then(|v| v.as_f64()), Some(3_000.0));
//! assert_eq!(signals.get("lpc:isActive").and_then(|v| v.as_bool()), Some(true));
//! ```

use alloc::borrow::Cow;
use alloc::vec::Vec;

/// What a runtime signal is worth.
///
/// Deliberately narrow. A tester reads these to decide a verdict, so every variant has to
/// have one obvious rendering and no room for interpretation; anything richer belongs in
/// the protocol, not in a debug interface.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(untagged)]
pub enum SignalValue {
    /// A flag, such as `isLimitActive`.
    Bool(bool),
    /// A physical quantity, with its unit in the signal's name.
    Number(f64),
    /// A span in seconds.
    Seconds(f64),
    /// A state or an enumerated value, spelled as the specification spells it.
    Text(Cow<'static, str>),
    /// The value is not currently set — a limit with no duration, a nameplate a device
    /// was never told.
    ///
    /// Distinct from zero, which a tester would read as a real limit of nought watts.
    Absent,
}

impl SignalValue {
    /// The value as a number, for a [`Number`](Self::Number) or [`Seconds`](Self::Seconds).
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            SignalValue::Number(value) | SignalValue::Seconds(value) => Some(*value),
            _ => None,
        }
    }

    /// The value as a flag.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            SignalValue::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// The value as text, for a [`Text`](Self::Text).
    pub fn as_str(&self) -> Option<&str> {
        match self {
            SignalValue::Text(value) => Some(value),
            _ => None,
        }
    }

    /// Whether the value is set at all.
    pub fn is_absent(&self) -> bool {
        matches!(self, SignalValue::Absent)
    }

    /// A span, or [`Absent`](Self::Absent) for [`None`].
    pub fn seconds(value: Option<core::time::Duration>) -> Self {
        match value {
            Some(span) => SignalValue::Seconds(span.as_secs_f64()),
            None => SignalValue::Absent,
        }
    }

    /// A quantity, or [`Absent`](Self::Absent) for [`None`].
    pub fn number(value: Option<f64>) -> Self {
        match value {
            Some(number) => SignalValue::Number(number),
            None => SignalValue::Absent,
        }
    }
}

impl core::fmt::Display for SignalValue {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SignalValue::Bool(value) => write!(f, "{value}"),
            SignalValue::Number(value) => write!(f, "{value}"),
            SignalValue::Seconds(value) => write!(f, "{value} s"),
            SignalValue::Text(value) => f.write_str(value),
            SignalValue::Absent => f.write_str("-"),
        }
    }
}

/// One named runtime value.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Signal {
    /// The name, as `<use case>:<dataPoint>` — `lpc:limit`, `mpc:totalActivePower`.
    pub name: Cow<'static, str>,
    /// The value.
    pub value: SignalValue,
    /// The unit, where the value has one: `W`, `s`, `%`.
    ///
    /// Borrowed for the units this crate names itself, owned for one that came off the
    /// wire — SPINE's unit enumeration is extensible, so a peer may name one this crate
    /// has never heard of and a debug interface should still print it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<Cow<'static, str>>,
}

impl Signal {
    /// A signal with no unit.
    pub fn new(name: impl Into<Cow<'static, str>>, value: SignalValue) -> Self {
        Self {
            name: name.into(),
            value,
            unit: None,
        }
    }

    /// Names the unit the value is in.
    #[must_use]
    pub fn in_unit(mut self, unit: impl Into<Cow<'static, str>>) -> Self {
        self.unit = Some(unit.into());
        self
    }
}

impl core::fmt::Display for Signal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} {}", self.name, self.value)?;
        match (self.unit.as_deref(), &self.value) {
            // `Seconds` already prints its unit, and `Absent` has none to print.
            (Some(unit), SignalValue::Bool(_) | SignalValue::Number(_) | SignalValue::Text(_)) => {
                write!(f, " {unit}")
            }
            _ => Ok(()),
        }
    }
}

/// A use-case actor's runtime signals.
///
/// A flat list rather than a map: it is small, it has a meaningful order — the one the
/// parameter sheet lists the data points in — and an ordered list renders the same way
/// every time, which matters when a laboratory is comparing two runs.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct SignalSet(Vec<Signal>);

impl SignalSet {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a signal.
    #[must_use]
    pub fn with(mut self, signal: Signal) -> Self {
        self.0.push(signal);
        self
    }

    /// The signals, in the order the actor reported them.
    pub fn iter(&self) -> impl Iterator<Item = &Signal> {
        self.0.iter()
    }

    /// How many there are.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Looks one up by name.
    pub fn get(&self, name: &str) -> Option<&SignalValue> {
        self.0
            .iter()
            .find(|signal| signal.name == name)
            .map(|signal| &signal.value)
    }
}

impl core::fmt::Display for SignalSet {
    /// One signal per line, which is what a debug console wants.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for (index, signal) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str("\n")?;
            }
            write!(f, "{signal}")?;
        }
        Ok(())
    }
}

impl IntoIterator for SignalSet {
    type Item = Signal;
    type IntoIter = alloc::vec::IntoIter<Signal>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl FromIterator<Signal> for SignalSet {
    fn from_iter<T: IntoIterator<Item = Signal>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}

/// An actor that can report what it currently holds.
///
/// `Context` is whatever the actor needs to name its signals — for the limitation use
/// cases, the [`Direction`](crate::usecases::limitation::Direction), which decides
/// whether they are called `lpc:` or `lpp:`.
pub trait Signals<Context = ()> {
    /// The actor's current runtime values.
    fn signals(&self, context: Context) -> SignalSet;
}
