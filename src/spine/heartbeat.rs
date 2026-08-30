//! The `DeviceDiagnosis` heartbeat: sending one on schedule, and noticing when one stops.
//!
//! A heartbeat is how two SPINE nodes tell each other the *application* is still alive,
//! which a live TCP connection does not. LPC and LPP are built on it: a Controllable
//! System that stops hearing from its Energy Guard falls back to the failsafe limit after
//! 120 seconds, and under §14a EnWG that fallback is what protects the grid connection
//! when an energy manager hangs.
//!
//! Both halves are here, and both are sans-IO like everything else in [`crate::spine`]:
//! [`HeartbeatProducer`] says when the next one is due and puts it on the wire,
//! [`HeartbeatMonitor`] says when the last one arrived and when to give up on the peer.
//!
//! ```
//! use core::time::Duration;
//! use eebus::spine::{HeartbeatMonitor, HeartbeatProducer};
//!
//! let mut producer = HeartbeatProducer::new(Duration::ZERO);
//! let mut monitor = HeartbeatMonitor::new();
//!
//! // Nothing is due until the period elapses.
//! assert_eq!(producer.poll_timeout(), Duration::from_secs(60));
//!
//! let beat = producer.beat(Duration::from_secs(60));
//! assert_eq!(beat.heartbeat_counter, Some(1));
//! monitor.observe(Duration::from_secs(60));
//!
//! // Two minutes of silence is what LPC treats as a lost Energy Guard.
//! assert!(monitor.is_alive(Duration::from_secs(120)));
//! assert!(!monitor.is_alive(Duration::from_secs(181)));
//! ```

use core::time::Duration;

use crate::model::{CmdData, DeviceDiagnosisHeartbeatData, FeatureAddress, Function};

use super::engine::Engine;

/// How often a heartbeat is sent, and the `heartbeatTimeout` it announces.
///
/// LPC and LPP Table 26 fix both at sixty seconds. A use case that needs a faster beat
/// may shorten the period — the value announced is a maximum, and a peer that runs
/// several use cases over one connection takes the shortest.
pub const DEFAULT_HEARTBEAT_PERIOD: Duration = Duration::from_secs(60);

/// How long a peer may be silent before its heartbeat counts as lost.
///
/// Twice the period, which is what LPC/LPP §2.3 uses: one missed beat is a hiccup, two
/// is an absence.
pub const DEFAULT_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(120);

/// Produces `deviceDiagnosisHeartbeatData` on schedule.
///
/// The counter increases with every heartbeat *sent as a notification* and not with a
/// reply to a read, which is what the specification asks for: a client counting replies
/// would see it stand still and conclude the peer had stopped.
#[derive(Clone, Debug)]
pub struct HeartbeatProducer {
    period: Duration,
    timeout: Duration,
    counter: u64,
    next: Duration,
}

impl HeartbeatProducer {
    /// A producer beating every [`DEFAULT_HEARTBEAT_PERIOD`], starting at `now`.
    pub fn new(now: Duration) -> Self {
        Self {
            period: DEFAULT_HEARTBEAT_PERIOD,
            timeout: DEFAULT_HEARTBEAT_PERIOD,
            counter: 0,
            next: now + DEFAULT_HEARTBEAT_PERIOD,
        }
    }

    /// Makes the next heartbeat due at `at` rather than a whole period from now.
    ///
    /// An Energy Guard opening a connection needs this: §2.11 evaluates a limit only
    /// after a heartbeat, so waiting a minute before the first one would leave the
    /// Controllable System in its failsafe state for that minute.
    #[must_use]
    pub fn due_at(mut self, at: Duration) -> Self {
        self.next = at;
        self
    }

    /// Beats faster than once a minute, for a use case that asks for it.
    ///
    /// The announced `heartbeatTimeout` follows the period, because a peer plans its own
    /// supervision against the figure it is told.
    #[must_use]
    pub fn with_period(mut self, period: Duration, now: Duration) -> Self {
        self.period = period;
        self.timeout = period;
        self.next = now + period;
        self
    }

    /// The counter of the last heartbeat produced.
    pub fn counter(&self) -> u64 {
        self.counter
    }

    /// The period between heartbeats.
    pub fn period(&self) -> Duration {
        self.period
    }

    /// When the next heartbeat is due.
    pub fn poll_timeout(&self) -> Duration {
        self.next
    }

    /// Whether a heartbeat is due at `now`.
    pub fn is_due(&self, now: Duration) -> bool {
        now >= self.next
    }

    /// Takes the next heartbeat, advancing the counter and the schedule.
    ///
    /// `timestamp` is absent because nothing in this crate reads a clock. A device with
    /// a real-time clock adds it with [`with_timestamp`]; the
    /// implementation guide §3.7 asks a server to set it, and equally says a client need
    /// not check it.
    pub fn beat(&mut self, now: Duration) -> DeviceDiagnosisHeartbeatData {
        self.counter = self.counter.wrapping_add(1);
        self.next = now + self.period;
        DeviceDiagnosisHeartbeatData {
            timestamp: None,
            heartbeat_counter: Some(self.counter),
            heartbeat_timeout: Some(crate::model::format_iso8601_duration(self.timeout)),
        }
    }

    /// Publishes the next heartbeat on `feature` and notifies its subscribers.
    ///
    /// Returns `false` when none was due, so a caller driving several timers can pass
    /// every wake-up through without checking first.
    pub fn tick(&mut self, engine: &mut Engine, feature: &FeatureAddress, now: Duration) -> bool {
        if !self.is_due(now) {
            return false;
        }
        self.beat_now(engine, feature, now);
        true
    }

    /// Publishes a heartbeat whether or not one was due, and restarts the schedule.
    ///
    /// An Energy Guard needs this: the implementation guide §2.11 evaluates a limit only
    /// if a heartbeat arrived in the sixty seconds before it, so a limit that has just
    /// become necessary is preceded by a beat rather than waiting for the next one.
    pub fn beat_now(&mut self, engine: &mut Engine, feature: &FeatureAddress, now: Duration) {
        let beat = self.beat(now);
        let address = feature.clone();
        if let Some(feature) = engine.device_mut().resolve_mut(&address) {
            // The counter advances with every beat, so this is always a change — which
            // is the point of a heartbeat, and why §2.4's "do not notify unchanged data"
            // does not apply to it.
            let _ = feature.set_data(CmdData::DeviceDiagnosisHeartbeatData(beat));
        }
        engine.notify(&address, &Function::DeviceDiagnosisHeartbeatData, now);
    }
}

/// Adds a wall-clock timestamp to a heartbeat.
///
/// Kept separate from [`HeartbeatProducer::beat`] because the producer has no clock: a
/// device that has one formats the current time as ISO 8601 and passes it through here.
pub fn with_timestamp(
    mut beat: DeviceDiagnosisHeartbeatData,
    timestamp: impl Into<alloc::string::String>,
) -> DeviceDiagnosisHeartbeatData {
    beat.timestamp = Some(crate::model::AbsoluteOrRelativeTime::from(timestamp.into()));
    beat
}

/// Watches a peer's heartbeats and reports when they stop.
#[derive(Clone, Debug)]
pub struct HeartbeatMonitor {
    timeout: Duration,
    last: Option<Duration>,
    counter: Option<u64>,
    reported_lost: bool,
}

impl Default for HeartbeatMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl HeartbeatMonitor {
    /// A monitor with the [`DEFAULT_HEARTBEAT_TIMEOUT`], having seen nothing yet.
    pub fn new() -> Self {
        Self {
            timeout: DEFAULT_HEARTBEAT_TIMEOUT,
            last: None,
            counter: None,
            reported_lost: false,
        }
    }

    /// A monitor that gives up after `timeout` of silence.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Records a heartbeat received at `now`.
    pub fn observe(&mut self, now: Duration) {
        self.last = Some(now);
        self.reported_lost = false;
    }

    /// Records a heartbeat payload, if that is what `data` is.
    ///
    /// Returns `true` when it was, so an event loop can pass everything through and let
    /// the monitor pick out what belongs to it.
    pub fn observe_data(&mut self, data: &CmdData, now: Duration) -> bool {
        let CmdData::DeviceDiagnosisHeartbeatData(beat) = data else {
            return false;
        };
        self.counter = beat.heartbeat_counter.or(self.counter);
        self.observe(now);
        true
    }

    /// When the peer's silence becomes an absence, if a heartbeat has ever arrived.
    pub fn poll_timeout(&self) -> Option<Duration> {
        self.last.map(|last| last + self.timeout)
    }

    /// Whether the peer has been heard from recently enough.
    pub fn is_alive(&self, now: Duration) -> bool {
        self.last
            .is_some_and(|last| now.saturating_sub(last) <= self.timeout)
    }

    /// Reports the loss once, the first time the deadline passes.
    ///
    /// Returns `true` exactly once per outage, so a caller can act on it — LPC's
    /// Controllable System enters the failsafe state — without acting again on every
    /// later tick.
    pub fn handle_timeout(&mut self, now: Duration) -> bool {
        if self.reported_lost || self.is_alive(now) || self.last.is_none() {
            return false;
        }
        self.reported_lost = true;
        true
    }

    /// The last `heartbeatCounter` seen, where the peer sent one.
    pub fn counter(&self) -> Option<u64> {
        self.counter
    }

    /// When the last heartbeat arrived.
    pub fn last_seen(&self) -> Option<Duration> {
        self.last
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_counter_advances_with_every_beat() {
        let mut producer = HeartbeatProducer::new(Duration::ZERO);
        assert_eq!(producer.counter(), 0);
        assert_eq!(
            producer.beat(Duration::from_secs(60)).heartbeat_counter,
            Some(1)
        );
        assert_eq!(
            producer.beat(Duration::from_secs(120)).heartbeat_counter,
            Some(2)
        );
        assert_eq!(producer.poll_timeout(), Duration::from_secs(180));
    }

    /// LPC/LPP Table 26: the announced timeout is sixty seconds.
    #[test]
    fn the_announced_timeout_matches_the_period() {
        let mut producer = HeartbeatProducer::new(Duration::ZERO);
        let beat = producer.beat(Duration::from_secs(60));
        assert_eq!(beat.heartbeat_timeout.as_deref(), Some("PT1M"));

        let mut fast = HeartbeatProducer::new(Duration::ZERO)
            .with_period(Duration::from_secs(15), Duration::ZERO);
        assert_eq!(
            fast.beat(Duration::from_secs(15))
                .heartbeat_timeout
                .as_deref(),
            Some("PT15S")
        );
    }

    #[test]
    fn a_lost_peer_is_reported_once() {
        let mut monitor = HeartbeatMonitor::new();
        assert!(
            !monitor.handle_timeout(Duration::from_secs(1_000)),
            "nothing seen yet"
        );

        monitor.observe(Duration::from_secs(10));
        assert!(!monitor.handle_timeout(Duration::from_secs(100)));
        assert!(monitor.handle_timeout(Duration::from_secs(200)));
        assert!(
            !monitor.handle_timeout(Duration::from_secs(300)),
            "the same outage is not reported twice"
        );

        monitor.observe(Duration::from_secs(310));
        assert!(monitor.is_alive(Duration::from_secs(320)));
        assert!(
            monitor.handle_timeout(Duration::from_secs(500)),
            "a new outage is"
        );
    }

    #[test]
    fn only_a_heartbeat_payload_counts() {
        let mut monitor = HeartbeatMonitor::new();
        assert!(!monitor.observe_data(
            &CmdData::LoadControlLimitListData(Default::default()),
            Duration::ZERO
        ));
        assert!(monitor.observe_data(
            &CmdData::DeviceDiagnosisHeartbeatData(DeviceDiagnosisHeartbeatData {
                heartbeat_counter: Some(7),
                ..Default::default()
            }),
            Duration::from_secs(5)
        ));
        assert_eq!(monitor.counter(), Some(7));
        assert_eq!(monitor.last_seen(), Some(Duration::from_secs(5)));
    }
}
