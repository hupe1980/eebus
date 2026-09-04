//! The Energy Guard: the side that sets the limit.
//!
//! An Energy Guard is a grid operator's control box, or an energy manager acting for
//! one. Its job looks small — send a heartbeat, write a limit — and almost all of it is
//! in the rules around those two messages, which the 2026 implementation guides spell
//! out because getting them wrong risks the low-voltage grid:
//!
//! * **Heartbeat first, then the limit** (§2.11). A Controllable System evaluates a
//!   limit only if a heartbeat arrived within the previous sixty seconds, so the order
//!   is not a convention but a precondition.
//! * **Never deactivate on reconnection** (§2.13). The first limit after a connection is
//!   restored is the one the grid currently needs, activated. A heat pump allowed back
//!   to full power may not be able to come down again.
//! * **Never a duration of zero on an activated limit** (§2.2). Zero means "deactivate
//!   now", which is not what an activated limit means.
//! * **A refusal is not a reason to give up** (§2.5). The Controllable System may have
//!   rejected on timing alone; retry, backing off a minute further each time.
//! * **Keep beating regardless** (§2.12). What the Controllable System's own heartbeat
//!   does is its business; ours does not depend on it.
//! * **Do not write more often than every five minutes** (§2.10), retries excepted.
//!
//! The actor holds those rules; the application supplies the one thing only it knows —
//! what limit the grid situation currently requires — through
//! [`require`](EnergyGuardActor::require).
//!
//! Like the Controllable System it talks to, this serves both LPC and LPP: a
//! [`Direction`] is the whole difference.

use alloc::vec;
use alloc::vec::Vec;
use core::time::Duration;

use crate::model::{
    AddressDevice, CmdData, DeviceConfigurationKeyValueData, DeviceConfigurationKeyValueListData,
    DeviceConfigurationKeyValueValue, FeatureAddress, FeatureType, Filter, FilterElements,
    FilterSelectors, Function, LoadControlLimitData, LoadControlLimitDataElements,
    LoadControlLimitId, LoadControlLimitListData, LoadControlLimitListDataSelectors, MsgCounter,
    Role, ScaledNumber, TimePeriod, TimePeriodElements,
};
use crate::spine::{
    Engine, ErrorNumber, HeartbeatMonitor, HeartbeatProducer, LocalFeature, RemoteDevice,
    SpineEvent,
};

use super::Direction;
use super::actor::{NominalMax, PeerIds};
use super::audit::{AuditLog, LimitRecord};
use super::state::{FAILSAFE_DURATION_RANGE, LimitWrite, NackReason, WriteOutcome};

/// The first retry after a refusal, and the step each further retry adds (§2.5).
pub const RETRY_BACKOFF_STEP: Duration = Duration::from_secs(60);

/// How often the guide asks an Energy Guard not to exceed when writing (§2.10).
pub const MIN_WRITE_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// How long a heartbeat this node sent still counts as recent enough to precede a limit.
///
/// The Controllable System's window is sixty seconds (§2.11); half of it leaves room for
/// the write to cross the network and still land inside.
const HEARTBEAT_FRESHNESS: Duration = Duration::from_secs(30);

/// How long to wait for the Controllable System to subscribe to the heartbeat before
/// writing anyway.
///
/// LPC scenario 3 has the Controllable System subscribe, and until it has, a notified
/// heartbeat reaches nobody and the limit that follows is refused. Waiting for the
/// subscription is right; waiting forever is not, because a peer may have kept a
/// subscription across the reconnection and see no reason to ask again.
const SUBSCRIPTION_GRACE: Duration = Duration::from_secs(10);

/// Builds the `Generic` client feature an Energy Guard holds its client side on.
///
/// The LPC implementation guide §3.3 asks an actor to put all of its client
/// functionality on one `Generic` feature rather than mirroring each server feature it
/// talks to — which is what makes "who is the Energy Guard here" a single address.
pub fn client_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::Generic, Role::Client)
}

/// Where one Controllable System's features live.
///
/// Built from the peer's detailed discovery with [`locate`], or by hand for a peer whose
/// addresses are already known.
#[derive(Clone, Debug, PartialEq)]
pub struct ControllableSystemPeer {
    /// The peer's device address.
    pub device: AddressDevice,
    /// Its `LoadControl` feature, which carries the limit.
    pub load_control: FeatureAddress,
    /// Its `DeviceConfiguration` feature, which carries the failsafe values.
    pub device_configuration: FeatureAddress,
    /// Its `DeviceDiagnosis` feature, if it serves one.
    pub device_diagnosis: Option<FeatureAddress>,
    /// Its `ElectricalConnection` feature, if it publishes the constraints of scenario 4.
    pub electrical_connection: Option<FeatureAddress>,
}

/// Finds a peer's limitation features from its detailed discovery and use-case data.
///
/// Returns [`None`] until the peer has announced the use case and the two features it
/// cannot be driven without. Both are read from the entity that announced the actor, as
/// the use-case implementation guide §3.3 requires — which is what makes the answer
/// unambiguous on a device exposing several energy-manager entities.
pub fn locate(remote: &RemoteDevice, direction: Direction) -> Option<ControllableSystemPeer> {
    let use_case = remote.use_case(
        direction.use_case_name(),
        crate::usecases::descriptor::actors::CONTROLLABLE_SYSTEM,
    )?;
    Some(ControllableSystemPeer {
        device: remote.address.clone()?,
        load_control: remote.address_of(use_case, &FeatureType::LoadControl, Role::Server)?,
        device_configuration: remote.address_of(
            use_case,
            &FeatureType::DeviceConfiguration,
            Role::Server,
        )?,
        device_diagnosis: remote.address_of(use_case, &FeatureType::DeviceDiagnosis, Role::Server),
        electrical_connection: remote.address_of(
            use_case,
            &FeatureType::ElectricalConnection,
            Role::Server,
        ),
    })
}

/// What the Energy Guard learned about one Controllable System.
#[derive(Clone, Debug, PartialEq)]
pub enum GuardEvent {
    /// The pre-scenario communication finished: both bindings are held, the peer's own
    /// `limitId` has been read, and the limit can be written.
    Ready {
        /// The peer.
        device: AddressDevice,
    },
    /// The Controllable System accepted a limit ([LPC/LPP-002]).
    ///
    /// Under §14a EnWG this pair — what was sent and what came back — is the evidence
    /// the operator has to be able to produce (implementation guide §4.1.5).
    LimitAccepted {
        /// The peer.
        device: AddressDevice,
        /// What it accepted.
        limit: LimitWrite,
        /// The counter of the write it answers.
        request: MsgCounter,
    },
    /// The Controllable System refused a limit ([LPC/LPP-003]).
    ///
    /// The peer stays in the guard's list and the write is retried; §2.5 is explicit
    /// that a refusal is not grounds for dropping a device.
    LimitRefused {
        /// The peer.
        device: AddressDevice,
        /// What it refused.
        limit: LimitWrite,
        /// What it reported.
        error: ErrorNumber,
        /// When the guard will try again.
        retry_at: Duration,
    },
    /// The Controllable System reported its nominal maximum (scenario 4).
    ///
    /// This is what turns a percentage from the grid operator into watts, so an Energy
    /// Guard that works that way has to wait for it before it can write a limit at all.
    /// Which of the two kinds arrived matters: a device's nameplate ([LPC/LPP-041]) is a
    /// physical ceiling, a contractual maximum ([LPC/LPP-042]) is not.
    ConstraintsLearned {
        /// The peer.
        device: AddressDevice,
        /// What it reported.
        nominal_max: NominalMax,
    },
    /// The Controllable System's own heartbeat stopped.
    ///
    /// Informational only: §2.12 forbids letting it change what this actor does.
    PeerHeartbeatLost {
        /// The peer.
        device: AddressDevice,
    },
    /// The peer answered with a `LoadControl` feature that publishes no limit this guard
    /// can write to, so no limit will be sent.
    ///
    /// The peer's limit description named no `signDependentAbsValueLimit` obligation in
    /// this guard's direction (LPC/LPP Table 22), which is what a Controllable System is
    /// required to publish. Usually one of two installations: a device that implements
    /// the *other* direction — an LPP appliance an LPC guard has attached to — or one
    /// whose actor was built and never installed, which answers discovery and describes
    /// no limit.
    ///
    /// Reported once per attach, and worth surfacing rather than logging: an installation
    /// that stops here looks commissioned from both ends and is not. Nothing on the wire
    /// says so, which is the whole reason this event exists.
    NoLimitPublished {
        /// The peer.
        device: AddressDevice,
    },
    /// A request to this peer went unanswered through the whole of the SPINE
    /// implementation guide §2.6.2 escalation path.
    ///
    /// Not a refusal — that is a completed exchange, and arrives as
    /// [`LimitRefused`](Self::LimitRefused). This is silence, and whatever the request was
    /// holding has been released, so the guard carries on rather than waiting for an
    /// acknowledgement that is not coming.
    ///
    /// Worth surfacing (§2.6.4): under §14a EnWG a control box whose limit writes are being
    /// swallowed has stopped controlling, and looks from the outside exactly like one the
    /// grid is asking nothing of.
    PeerUnresponsive {
        /// The peer.
        device: AddressDevice,
        /// What went unanswered.
        outstanding: Unanswered,
    },
}

/// What a peer failed to answer ([`GuardEvent::PeerUnresponsive`]).
///
/// The three are not equally bad, which is what an installer's one line has to say: a lost
/// limit is a grid instruction that went nowhere, a lost description is an installation
/// that never started, and a lost binding is retried on the next attach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unanswered {
    /// A limit write. The limit has been let go and will be written again.
    Limit,
    /// A description read. Until one arrives, no limit can be addressed at all.
    Description,
    /// A binding request. Without it the peer will accept no write.
    Binding,
}

/// One Controllable System, and where the guard has got to with it.
#[derive(Clone, Debug)]
struct Tracked {
    peer: ControllableSystemPeer,
    bound_load_control: bool,
    bound_configuration: bool,
    /// Whether [`GuardEvent::Ready`] has been reported for this attach.
    ready_reported: bool,
    /// The identifiers this peer chose for the limit and the two failsafe keys.
    ///
    /// Empty until the description reads come back. Nothing may be written before the
    /// limit is in here: the numbers are the peer's, not this crate's.
    ids: PeerIds,
    /// Whether the peer has answered with a limit description that named no usable limit.
    reported_no_limit: bool,
    /// The limit the grid situation requires of this peer.
    required: Option<LimitWrite>,
    /// The limit whose acknowledgement is outstanding.
    outstanding: Option<(MsgCounter, LimitWrite)>,
    /// The limit this peer has acknowledged.
    applied: Option<LimitWrite>,
    /// When the last write went out.
    last_write: Option<Duration>,
    /// When the next limit write may go out, once one is wanted.
    ///
    /// Set the moment the application changes what the grid requires, and pushed back by
    /// whatever is holding the write up — the five-minute ceiling, a refusal's backoff,
    /// or a peer that has not subscribed to the heartbeat yet. Reported through
    /// [`EnergyGuardActor::poll_timeout`], so a limit never waits for the next heartbeat
    /// to be noticed.
    write_due_at: Option<Duration>,
    /// When to try again after a refusal, and how many refusals there have been.
    retry_at: Option<Duration>,
    refusals: u32,
    monitor: HeartbeatMonitor,
    /// The binding requests still outstanding, by counter.
    pending_bindings: Vec<(MsgCounter, FeatureAddress)>,
    /// The description reads still outstanding, by counter.
    ///
    /// These gate every write — without the peer's `limitId` and `keyId`s there is no
    /// address to write to — so an unanswered one is an installation that never starts.
    /// Tracked so a give-up can be told from a read still in flight.
    pending_reads: Vec<(MsgCounter, FeatureAddress)>,
    /// When this peer was attached, for the subscription grace period.
    attached_at: Duration,
    /// Whether the peer has subscribed to this guard's heartbeat (scenario 3).
    watches_heartbeat: bool,
    /// What scenario 4 reported, once the read comes back.
    nominal_max: Option<NominalMax>,
    /// When this peer was last sent a heartbeat it could actually have received.
    ///
    /// Not the same as "when did this guard last beat". A heartbeat is a notification to
    /// *subscribers*, so one emitted before this peer subscribed reached it no more than
    /// if it had never been sent — and implementation guide §2.11 is about what the
    /// Controllable System *received* in the sixty seconds before the write, not about
    /// what the Energy Guard sent. Tracking it per peer is what stops the guard writing a
    /// limit that the peer must then refuse for want of a heartbeat it never got.
    beat_seen_at: Option<Duration>,
    /// Whether the opening limit write of implementation guide §2.11 has gone out.
    ///
    /// Reset by [`EnergyGuardActor::attach`], because a reconnection starts the sequence
    /// again — the Controllable System on the other end has been through `init` or the
    /// failsafe state and is waiting for it.
    opening_write_sent: bool,
}

impl Tracked {
    fn new(peer: ControllableSystemPeer, now: Duration) -> Self {
        Self {
            peer,
            bound_load_control: false,
            bound_configuration: false,
            ready_reported: false,
            ids: PeerIds::new(),
            reported_no_limit: false,
            required: None,
            outstanding: None,
            applied: None,
            last_write: None,
            write_due_at: None,
            retry_at: None,
            refusals: 0,
            monitor: HeartbeatMonitor::new(),
            pending_bindings: Vec::new(),
            pending_reads: Vec::new(),
            attached_at: now,
            watches_heartbeat: false,
            nominal_max: None,
            beat_seen_at: None,
            opening_write_sent: false,
        }
    }

    /// Whether everything a limit write needs is in place.
    ///
    /// Both bindings, and the peer's own `limitId`. The identifier belongs here rather
    /// than beside the write because a guard that is "ready" without one is ready to
    /// address nothing: the alternative to waiting is writing to whatever the peer keeps
    /// under a number this crate chose for itself.
    fn is_ready(&self) -> bool {
        self.bound_load_control && self.bound_configuration && self.ids.can_write_limit()
    }

    /// Whether a limit written now would find the peer listening for the heartbeat that
    /// has to precede it.
    fn can_be_heard(&self, now: Duration) -> bool {
        self.watches_heartbeat || now.saturating_sub(self.attached_at) >= SUBSCRIPTION_GRACE
    }
}

/// The Energy Guard actor of Limitation of Power Consumption or Production.
#[derive(Debug)]
pub struct EnergyGuardActor {
    direction: Direction,
    client: FeatureAddress,
    diagnosis: FeatureAddress,
    heartbeat: HeartbeatProducer,

    peers: Vec<Tracked>,
    /// What the grid requires of a device this guard has not attached to yet.
    ///
    /// A requirement is a fact about the installation, not about how far the pre-scenario
    /// exchange has got. Dropping one because the bindings had not settled would discard a
    /// grid operator's instruction silently — which under §14a EnWG is the worst failure
    /// this crate could have.
    deferred: Vec<(AddressDevice, Option<LimitWrite>)>,
    audit: AuditLog,
}

impl EnergyGuardActor {
    /// An Energy Guard writing from `client` and beating from `diagnosis`.
    ///
    /// `direction` selects the use case: [`Direction::Consumption`] for LPC,
    /// [`Direction::Production`] for LPP.
    pub fn new(
        direction: Direction,
        client: FeatureAddress,
        diagnosis: FeatureAddress,
        now: Duration,
    ) -> Self {
        Self {
            direction,
            client,
            diagnosis,
            // §2.11: a Controllable System evaluates a limit only after a heartbeat, so
            // the first one goes out immediately rather than a minute from now.
            heartbeat: HeartbeatProducer::new(now).due_at(now),
            peers: Vec::new(),
            deferred: Vec::new(),
            audit: AuditLog::new(),
        }
    }

    /// Which of the two limitation use cases this actor plays.
    pub fn direction(&self) -> Direction {
        self.direction
    }

    /// The record of every limit this guard wrote and how it was answered.
    ///
    /// The LPC implementation guide §4.1.5 has an Energy Guard report exactly this to the
    /// grid operator's backend, where the success or failure of each write is kept for
    /// later evaluation. It fills itself; draining it is the application's business.
    pub fn audit(&self) -> &AuditLog {
        &self.audit
    }

    /// The record, for draining it into storage.
    pub fn audit_mut(&mut self) -> &mut AuditLog {
        &mut self.audit
    }

    /// Replaces the record, for a guard that keeps more or less history.
    #[must_use]
    pub fn with_audit_log(mut self, audit: AuditLog) -> Self {
        self.audit = audit;
        self
    }

    /// The peers this guard is controlling.
    pub fn peers(&self) -> impl Iterator<Item = &ControllableSystemPeer> {
        self.peers.iter().map(|t| &t.peer)
    }

    /// Whether a peer's pre-scenario communication has finished: both bindings held and
    /// the peer's own `limitId` known.
    pub fn is_ready(&self, device: &AddressDevice) -> bool {
        self.tracked(device).is_some_and(Tracked::is_ready)
    }

    /// The limit a peer has acknowledged, as far as this guard knows.
    pub fn applied_limit(&self, device: &AddressDevice) -> Option<LimitWrite> {
        self.tracked(device)?.applied
    }

    /// Starts controlling a Controllable System.
    ///
    /// Sends the binding requests the use case needs — `LoadControl` for the limit,
    /// `DeviceConfiguration` for the failsafe values — reads the two description
    /// functions that say how this peer numbers its data, and subscribes to the peer's
    /// heartbeat where it serves one. Calling it again for a peer already tracked
    /// restarts the pre-scenario communication, which is what a reconnection needs.
    ///
    /// The description reads are not optional and not an optimisation. `limitId` and the
    /// failsafe `keyId`s are the peer's own (LPC/LPP Tables 22 and 24 spell them
    /// `<l1#(1..1)>`, `<k1#(1..1)>`, `<k2#(1..1)>`), so until they come back this guard
    /// has no address to write to — see [`PeerIds`](super::PeerIds).
    pub fn attach(&mut self, engine: &mut Engine, peer: ControllableSystemPeer, now: Duration) {
        let device = peer.device.clone();
        self.peers.retain(|t| t.peer.device != device);

        let mut tracked = Tracked::new(peer, now);
        // Anything the grid asked for before this peer was reachable applies now.
        if let Some(index) = self.deferred.iter().position(|(d, _)| d == &device) {
            let (_, limit) = self.deferred.remove(index);
            set_required(&mut tracked, limit, now);
        }
        for feature in [
            tracked.peer.load_control.clone(),
            tracked.peer.device_configuration.clone(),
        ] {
            let counter = engine.request_binding(&self.client, &feature, now);
            tracked.pending_bindings.push((counter, feature));
        }
        // How this peer numbers its data. Reads, not writes, so they need no binding and
        // go out with the binding requests rather than behind them. Their counters are
        // kept: nothing is written until they come back, so an unanswered one has to be
        // something the application is told about rather than a silence.
        for (feature, function) in [
            (
                tracked.peer.load_control.clone(),
                Function::LoadControlLimitDescriptionListData,
            ),
            (
                tracked.peer.device_configuration.clone(),
                Function::DeviceConfigurationKeyValueDescriptionListData,
            ),
        ] {
            let counter = engine.read(&feature, &self.client, function, now);
            tracked.pending_reads.push((counter, feature));
        }
        // The limit itself is worth watching: a Controllable System that leaves the
        // limited state on its own notifies the change.
        engine.request_subscription(&self.client, &tracked.peer.load_control.clone(), now);
        if let Some(diagnosis) = tracked.peer.device_diagnosis.clone() {
            engine.request_subscription(&self.client, &diagnosis, now);
        }
        // Scenario 4 is a one-off read, not a subscription: a nameplate does not change
        // while the device runs, and a contract does not change without someone writing
        // it. The reply is what [`GuardEvent::ConstraintsLearned`] reports.
        if let Some(electrical) = tracked.peer.electrical_connection.clone() {
            engine.read(
                &electrical,
                &self.client,
                Function::ElectricalConnectionCharacteristicListData,
                now,
            );
        }
        self.peers.push(tracked);
    }

    /// The identifiers a peer chose for the data this guard addresses.
    ///
    /// Filled from the description reads [`attach`](Self::attach) sends. Worth showing in
    /// a commissioning log: a peer whose `limit` is [`None`] is one no limit will be
    /// written to, and the reason is not otherwise visible on the wire.
    pub fn peer_ids(&self, device: &AddressDevice) -> Option<PeerIds> {
        Some(self.tracked(device)?.ids)
    }

    /// What a peer reported as its nominal maximum in scenario 4, once it has.
    ///
    /// [`None`] means the peer has not answered yet, or does not implement scenario 4 —
    /// which it need not, the specification marking it `R` for a Controllable System.
    pub fn nominal_max(&self, device: &AddressDevice) -> Option<NominalMax> {
        self.peers
            .iter()
            .find(|t| &t.peer.device == device)
            .and_then(|t| t.nominal_max)
    }

    /// Stops controlling a Controllable System, on disconnection or removal.
    pub fn detach(&mut self, device: &AddressDevice) {
        self.peers.retain(|t| &t.peer.device != device);
    }

    /// Sets the limit the grid situation requires of a peer.
    ///
    /// Nothing goes out from here: the write is *scheduled*, and
    /// [`poll_timeout`](Self::poll_timeout) reports it at once, so the next
    /// [`handle_timeout`](Self::handle_timeout) sends the heartbeat §2.11 requires and
    /// the limit behind it. Scheduling rather than sending is what keeps the ordering in
    /// one place; reporting it at once is what keeps a curtailment from waiting for the
    /// next minute's heartbeat.
    ///
    /// Passing [`None`] means "no limit is required", which is communicated as an
    /// explicit deactivation — and, per §2.13, only when the grid actually permits
    /// unlimited operation.
    /// Order does not matter. A requirement set before the guard has attached to the
    /// device — which is normal, since a grid operator does not wait for a binding to
    /// settle — is held and applied the moment it does.
    pub fn require(&mut self, device: &AddressDevice, limit: Option<LimitWrite>, now: Duration) {
        if let Some(tracked) = self.peers.iter_mut().find(|t| &t.peer.device == device) {
            set_required(tracked, limit, now);
            return;
        }
        self.deferred.retain(|(d, _)| d != device);
        self.deferred.push((device.clone(), limit));
    }

    /// Sets the same limit for every peer, which is what a grid-wide curtailment is.
    ///
    /// Unlike [`require`](Self::require) this names no device, so there is nothing to hold
    /// for a peer that has not appeared: it applies to the peers attached now.
    pub fn require_all(&mut self, limit: Option<LimitWrite>, now: Duration) {
        for tracked in &mut self.peers {
            set_required(tracked, limit, now);
        }
    }

    /// What the grid requires of devices this guard has not attached to yet.
    ///
    /// Empty in a healthy installation. A device that stays here is one that was asked
    /// for something and has never announced the Controllable System — worth surfacing,
    /// because under §14a the operator is owed an answer either way.
    pub fn deferred_requirements(
        &self,
    ) -> impl Iterator<Item = (&AddressDevice, Option<LimitWrite>)> {
        self.deferred.iter().map(|(d, l)| (d, *l))
    }

    /// Writes the failsafe active power limit onto a peer ([LPC/LPP-021]).
    ///
    /// Implementation guide §2.15 makes this mandatory for a Controllable System to
    /// accept: a device stuck on its factory default cannot protect anything.
    ///
    /// [`None`] until the peer's own `keyId` for it is known — the description read
    /// [`attach`](Self::attach) sends is what supplies it. `keyId` is the device's
    /// (`<k1#(1..1)>`, LPC/LPP Table 24) and the fixed element is the `keyName`, so a
    /// write addressed by this crate's own numbering would land on whatever key that peer
    /// keeps at that index. A `DeviceConfiguration` feature holds every configuration key
    /// a device has, not only this use case's two.
    pub fn write_failsafe_limit(
        &mut self,
        engine: &mut Engine,
        device: &AddressDevice,
        watts: f64,
        now: Duration,
    ) -> Option<MsgCounter> {
        let tracked = self.peers.iter().find(|t| &t.peer.device == device)?;
        if !tracked.bound_configuration {
            return None;
        }
        let key = tracked.ids.failsafe_limit?;
        let data =
            CmdData::DeviceConfigurationKeyValueListData(DeviceConfigurationKeyValueListData {
                device_configuration_key_value_data: Some(vec![DeviceConfigurationKeyValueData {
                    key_id: Some(key),
                    value: Some(DeviceConfigurationKeyValueValue {
                        scaled_number: Some(ScaledNumber::from_f64(watts.max(0.0), 0)),
                        ..Default::default()
                    }),
                    ..Default::default()
                }]),
            });
        let target = tracked.peer.device_configuration.clone();
        Some(engine.write(&target, &self.client, data, true, now))
    }

    /// Writes the Failsafe Duration Minimum onto a peer ([LPC/LPP-022]).
    ///
    /// A value outside two to twenty-four hours is refused here rather than on the wire:
    /// the range is the specification's, and a Controllable System would answer with a
    /// NACK the guard would then have to interpret.
    ///
    /// [`None`] until the peer's own `keyId` for it is known, for the reason given on
    /// [`write_failsafe_limit`](Self::write_failsafe_limit).
    pub fn write_failsafe_duration(
        &mut self,
        engine: &mut Engine,
        device: &AddressDevice,
        duration: Duration,
        now: Duration,
    ) -> Option<MsgCounter> {
        if !FAILSAFE_DURATION_RANGE.contains(&duration) {
            return None;
        }
        let tracked = self.peers.iter().find(|t| &t.peer.device == device)?;
        if !tracked.bound_configuration {
            return None;
        }
        let key = tracked.ids.failsafe_duration?;
        let data =
            CmdData::DeviceConfigurationKeyValueListData(DeviceConfigurationKeyValueListData {
                device_configuration_key_value_data: Some(vec![DeviceConfigurationKeyValueData {
                    key_id: Some(key),
                    value: Some(DeviceConfigurationKeyValueValue {
                        duration: Some(crate::model::format_iso8601_duration(duration)),
                        ..Default::default()
                    }),
                    ..Default::default()
                }]),
            });
        let target = tracked.peer.device_configuration.clone();
        Some(engine.write(&target, &self.client, data, true, now))
    }

    /// When [`handle_timeout`](Self::handle_timeout) should next be called.
    ///
    /// The earliest of the heartbeat, a refusal's backoff, and any limit waiting to go
    /// out — an *absolute* instant on the same monotonic scale as the `now` passed in.
    ///
    /// Every deadline reported here advances: a heartbeat moves when it fires, a due write
    /// is cleared when it goes out. A caller sleeps until this value and calls
    /// [`handle_timeout`](Self::handle_timeout), so an instant that could never be reached
    /// again would turn that loop into a spin — and in a loop that also owns a socket, a
    /// spin is a connection that is never read from.
    pub fn poll_timeout(&self) -> Duration {
        let mut next = self.heartbeat.poll_timeout();
        for tracked in &self.peers {
            if let Some(at) = tracked.write_due_at {
                next = next.min(at);
            }
            if let Some(retry) = tracked.retry_at {
                next = next.min(retry);
            }
        }
        next
    }

    /// Sends the heartbeat and any limit that is due.
    ///
    /// The order within one call is the order §2.11 requires: the heartbeat is queued
    /// first, so the Controllable System has seen one by the time the write arrives.
    pub fn handle_timeout(&mut self, engine: &mut Engine, now: Duration) -> Vec<GuardEvent> {
        let mut events = Vec::new();

        // §2.12: our heartbeat does not depend on the peer's.
        let diagnosis = self.diagnosis.clone();
        if self.heartbeat.tick(engine, &diagnosis, now) {
            self.record_beat(now);
        }

        for index in 0..self.peers.len() {
            let device = self.peers[index].peer.device.clone();
            if self.peers[index].monitor.handle_timeout(now) {
                events.push(GuardEvent::PeerHeartbeatLost {
                    device: device.clone(),
                });
            }
            self.write_limit_if_due(engine, index, now);
        }
        events
    }

    /// Feeds one engine event to the actor.
    pub fn handle_event(
        &mut self,
        engine: &mut Engine,
        event: &SpineEvent,
        now: Duration,
    ) -> Option<GuardEvent> {
        match event {
            // A peer that announces the Controllable System is taken up here rather than
            // on request: §2.11 requires the opening limit write as soon as the bindings
            // settle, whether or not the grid is asking for anything, so a guard that
            // waited to be attached would leave a conformant appliance in `init`.
            SpineEvent::DiscoveryUpdated { device } | SpineEvent::UseCasesUpdated { device } => {
                if self.peers.iter().any(|t| &t.peer.device == device) {
                    return None;
                }
                let peer = super::locate(engine.peer(device)?, self.direction)?;
                self.attach(engine, peer, now);
                None
            }
            SpineEvent::DataNotified {
                feature,
                resolved: data,
                ..
            }
            | SpineEvent::ReplyReceived {
                feature,
                resolved: data,
                ..
            } => {
                let index = self.peers.iter().position(|t| {
                    t.peer.device_diagnosis.as_ref() == Some(feature)
                        || &t.peer.load_control == feature
                        || &t.peer.device_configuration == feature
                        || t.peer.electrical_connection.as_ref() == Some(feature)
                })?;
                if self.peers[index].peer.electrical_connection.as_ref() == Some(feature) {
                    let nominal_max = super::actor::read_constraints(data, self.direction)?;
                    let tracked = &mut self.peers[index];
                    if tracked.nominal_max == Some(nominal_max) {
                        return None;
                    }
                    tracked.nominal_max = Some(nominal_max);
                    return Some(GuardEvent::ConstraintsLearned {
                        device: tracked.peer.device.clone(),
                        nominal_max,
                    });
                }
                // How this peer numbers its data, from either description function.
                // Until the `limitId` is here the peer is not ready and nothing is
                // written, so this is what releases the opening write of §2.11.
                if let Some(event) = self.learn_ids(engine, index, feature, data, now) {
                    return Some(event);
                }
                if self.peers[index].monitor.observe_data(data, now) {
                    return None;
                }
                // The Controllable System reports what it is actually limited to. If that
                // has drifted from what the grid requires — a limit whose duration ran
                // out, say — the guard has to notice and say so again: §2.6 asks it to
                // keep the system in the controlled states, and it cannot do that from a
                // record of what it once sent.
                if feature == &self.peers[index].peer.load_control {
                    self.observe_applied(engine, index, data, now);
                }
                None
            }
            SpineEvent::SubscriptionGranted { client, server } => {
                // Scenario 3: the Controllable System is now listening for our
                // heartbeat, which is the precondition every limit write has.
                if server == &self.diagnosis
                    && let Some(device) = client.device.as_ref()
                    && let Some(index) = self.peers.iter().position(|t| &t.peer.device == device)
                {
                    self.peers[index].watches_heartbeat = true;
                    self.write_limit_if_due(engine, index, now);
                }
                None
            }
            SpineEvent::ResultReceived { request, error } => {
                self.resolve(engine, *request, *error, now)
            }
            SpineEvent::RequestTimedOut { request, .. } => self.give_up(*request, now),
            _ => None,
        }
    }

    /// Lets go of a request the peer never answered.
    ///
    /// The engine raises this only once SPINE IG §2.6.2's escalation path is exhausted, so
    /// the peer is absent rather than slow and holding the slot buys nothing. The limit
    /// write is the one that matters: an outstanding one blocks every later write to that
    /// peer.
    fn give_up(&mut self, request: MsgCounter, now: Duration) -> Option<GuardEvent> {
        let index = self.peers.iter().position(|t| {
            t.outstanding.is_some_and(|(c, _)| c == request)
                || t.pending_bindings.iter().any(|(c, _)| *c == request)
                || t.pending_reads.iter().any(|(c, _)| *c == request)
        })?;
        let tracked = &mut self.peers[index];
        let device = tracked.peer.device.clone();

        if let Some((counter, limit)) = tracked.outstanding
            && counter == request
        {
            tracked.outstanding = None;
            // The limit is still required, so it is still owed. §2.5's backoff is for a
            // peer that *refused*; this one said nothing, and the retry is due as soon as
            // the rate limit allows.
            tracked.write_due_at = Some(now);
            // §4.1.5: the record has to show a limit that was never acknowledged, because
            // an unacknowledged limit is one the operator cannot claim was applied.
            self.audit.record(
                LimitRecord::new(
                    now,
                    request,
                    limit,
                    WriteOutcome::Rejected(NackReason::Unstated),
                )
                .with_peer(device.clone())
                .on_basis("unanswered through the whole of SPINE IG §2.6.2"),
            );
            return Some(GuardEvent::PeerUnresponsive {
                device,
                outstanding: Unanswered::Limit,
            });
        }

        if tracked.pending_reads.iter().any(|(c, _)| *c == request) {
            tracked.pending_reads.retain(|(c, _)| *c != request);
            return Some(GuardEvent::PeerUnresponsive {
                device,
                outstanding: Unanswered::Description,
            });
        }

        tracked.pending_bindings.retain(|(c, _)| *c != request);
        Some(GuardEvent::PeerUnresponsive {
            device,
            outstanding: Unanswered::Binding,
        })
    }

    /// Learns a peer's own identifiers from a description payload.
    ///
    /// Returns the event that follows, if any: [`GuardEvent::Ready`] when this was the
    /// last thing the pre-scenario communication was waiting for, or
    /// [`GuardEvent::NoLimitPublished`] when the peer answered the limit description read
    /// with a feature that carries no limit for this direction.
    fn learn_ids(
        &mut self,
        engine: &mut Engine,
        index: usize,
        feature: &FeatureAddress,
        data: &CmdData,
        now: Duration,
    ) -> Option<GuardEvent> {
        let tracked = &self.peers[index];
        if feature != &tracked.peer.load_control && feature != &tracked.peer.device_configuration {
            return None;
        }
        let direction = self.direction;
        let tracked = &mut self.peers[index];
        let learned = tracked.ids.learn(data, direction);
        if learned {
            // Answered, so no longer something a timeout could be about.
            tracked
                .pending_reads
                .retain(|(_, address)| address != feature);
        }

        // A limit description that named nothing usable is a dead installation, and the
        // only place it is visible is here: the read succeeded, so no error is reported,
        // and the guard simply never writes. Said once per attach.
        if matches!(data, CmdData::LoadControlLimitDescriptionListData(_))
            && tracked.ids.limit.is_none()
            && !tracked.reported_no_limit
        {
            tracked.reported_no_limit = true;
            return Some(GuardEvent::NoLimitPublished {
                device: tracked.peer.device.clone(),
            });
        }
        learned
            .then(|| self.become_ready(engine, index, now))
            .flatten()
    }

    /// Reports [`GuardEvent::Ready`] the first time everything a write needs is in place.
    ///
    /// Both the bindings and the description reads can be the last to arrive, so the
    /// check lives here rather than in whichever path happens to complete it.
    fn become_ready(
        &mut self,
        engine: &mut Engine,
        index: usize,
        now: Duration,
    ) -> Option<GuardEvent> {
        let tracked = &mut self.peers[index];
        if !tracked.is_ready() || tracked.ready_reported {
            return None;
        }
        tracked.ready_reported = true;
        let device = tracked.peer.device.clone();
        self.write_limit_if_due(engine, index, now);
        Some(GuardEvent::Ready { device })
    }

    /// Takes what a peer reports about its own limit as the truth about it.
    fn observe_applied(
        &mut self,
        engine: &mut Engine,
        index: usize,
        data: &CmdData,
        now: Duration,
    ) {
        // The peer's own `limitId`. A guard that read this with its own numbering would
        // either see nothing — and keep rewriting a limit already in force — or read some
        // other limit of the peer's as the answer to its own.
        let Some(limit_id) = self.peers[index].ids.limit else {
            return;
        };
        let Some(reported) = super::actor::read_limit_write(data, limit_id) else {
            return;
        };
        let tracked = &mut self.peers[index];
        if tracked.outstanding.is_some() {
            // A write is in flight; its acknowledgement is the authority, not a notify
            // that may have crossed it.
            return;
        }
        if tracked
            .applied
            .is_some_and(|applied| same_limit(&applied, &reported))
        {
            return;
        }
        tracked.applied = Some(reported);
        if Self::wants_write(tracked) {
            tracked.write_due_at = Some(now);
            self.write_limit_if_due(engine, index, now);
        }
    }

    fn resolve(
        &mut self,
        engine: &mut Engine,
        request: MsgCounter,
        error: ErrorNumber,
        now: Duration,
    ) -> Option<GuardEvent> {
        // A binding request coming back.
        if let Some(index) = self
            .peers
            .iter()
            .position(|t| t.pending_bindings.iter().any(|(c, _)| *c == request))
        {
            let tracked = &mut self.peers[index];
            let position = tracked
                .pending_bindings
                .iter()
                .position(|(c, _)| *c == request)?;
            let (_, feature) = tracked.pending_bindings.remove(position);
            if error.is_success() {
                if feature == tracked.peer.load_control {
                    tracked.bound_load_control = true;
                } else if feature == tracked.peer.device_configuration {
                    tracked.bound_configuration = true;
                }
            }
            return self.become_ready(engine, index, now);
        }

        // A limit write coming back.
        let index = self
            .peers
            .iter()
            .position(|t| t.outstanding.is_some_and(|(c, _)| c == request))?;
        let tracked = &mut self.peers[index];
        let (_, limit) = tracked.outstanding.take()?;
        let device = tracked.peer.device.clone();

        if error.is_success() {
            tracked.applied = Some(limit);
            tracked.refusals = 0;
            tracked.retry_at = None;
            self.audit.record(
                LimitRecord::new(now, request, limit, WriteOutcome::Accepted)
                    .with_peer(device.clone()),
            );
            return Some(GuardEvent::LimitAccepted {
                device,
                limit,
                request,
            });
        }

        // §2.5: keep the device, and try again a minute later than last time.
        tracked.refusals = tracked.refusals.saturating_add(1);
        let backoff = RETRY_BACKOFF_STEP.saturating_mul(tracked.refusals);
        let retry_at = now + backoff;
        tracked.retry_at = Some(retry_at);
        tracked.write_due_at = Some(retry_at);
        // §4.1.5: what came back is the evidence, refusals included — a NACK is what the
        // operator has to be able to show when a limitation was *not* honoured.
        self.audit.record(
            LimitRecord::new(
                now,
                request,
                limit,
                WriteOutcome::Rejected(NackReason::Unstated),
            )
            .with_peer(device.clone())
            .on_basis(alloc::format!("errorNumber {} ({error})", error.number())),
        );
        Some(GuardEvent::LimitRefused {
            device,
            limit,
            error,
            retry_at,
        })
    }

    /// Whether this peer's required limit differs from what it has acknowledged.
    fn wants_write(tracked: &Tracked) -> bool {
        // Implementation guide §2.11: "Following a heartbeat, the EG SHALL perform a write
        // operation on the Active Power Consumption Limit within 60 seconds." That is not
        // conditional on the grid needing anything — until the write lands, the
        // Controllable System is not in a controllable state at all, and after two minutes
        // it stops waiting and runs autonomously. So the first write is owed even when
        // the application has asked for nothing, and §2.13 says what it carries: the
        // currently required limit, or a deactivation where the grid genuinely permits
        // unlimited operation.
        if !tracked.opening_write_sent {
            return true;
        }
        let required = tracked.required.or_else(|| {
            // Nothing required and nothing applied means nothing to say; something
            // applied means an explicit deactivation is owed.
            tracked.applied.map(|_| LimitWrite::deactivated())
        });
        match (required, tracked.applied) {
            (None, _) => false,
            (Some(required), Some(applied)) => !same_limit(&required, &applied),
            (Some(_), None) => true,
        }
    }

    fn write_limit_if_due(&mut self, engine: &mut Engine, index: usize, now: Duration) {
        let tracked = &mut self.peers[index];
        if !tracked.is_ready() || tracked.outstanding.is_some() {
            // There is nothing to *wait* for here. What unblocks a write held up by a
            // missing binding is the binding being granted, and what unblocks one held up
            // by a write already in flight is that write's acknowledgement — both events,
            // and both come back through here when they arrive. Leaving a deadline
            // standing would report an instant that can never be reached again, and a
            // caller that sleeps until `poll_timeout` would spin instead of sleeping.
            tracked.write_due_at = None;
            return;
        }
        if !Self::wants_write(tracked) {
            tracked.write_due_at = None;
            return;
        }

        // §2.11: a limit only counts if the peer heard a heartbeat first, and it can only
        // hear one once it is subscribed.
        if !tracked.can_be_heard(now) {
            tracked.write_due_at = Some(tracked.attached_at + SUBSCRIPTION_GRACE);
            return;
        }
        // A refusal holds the write back until its backoff expires.
        if let Some(retry_at) = tracked.retry_at
            && now < retry_at
        {
            tracked.write_due_at = Some(retry_at);
            return;
        }
        // §2.10: no more than one write every five minutes, retries excepted.
        if tracked.retry_at.is_none()
            && let Some(last) = tracked.last_write
            && now.saturating_sub(last) < MIN_WRITE_INTERVAL
        {
            tracked.write_due_at = Some(last + MIN_WRITE_INTERVAL);
            return;
        }

        // §2.11 again: the heartbeat goes first, in the same batch, so the write lands
        // inside the sixty-second window it opens.
        //
        // The question is what *this peer* last received, not when this guard last beat.
        // A heartbeat is a notification to subscribers, so one sent before this peer
        // subscribed reached it no more than if it had never been sent — and a guard that
        // counted it would write a limit the peer then has to refuse for want of a
        // heartbeat it never got. That refusal is indistinguishable, from the operator's
        // side, from a device declining to be limited.
        let stale = self.peers[index]
            .beat_seen_at
            .is_none_or(|at| now.saturating_sub(at) >= HEARTBEAT_FRESHNESS);
        if stale {
            let diagnosis = self.diagnosis.clone();
            self.heartbeat.beat_now(engine, &diagnosis, now);
            self.record_beat(now);
        }

        let limit = self.peers[index]
            .required
            .unwrap_or_else(LimitWrite::deactivated);
        let target = self.peers[index].peer.load_control.clone();
        // `is_ready` is what guarantees this: a peer whose limit description has not come
        // back is not ready, and nothing is written to it.
        let Some(limit_id) = self.peers[index].ids.limit else {
            return;
        };
        let counter = engine.write_filtered(
            &target,
            &self.client,
            limit_payload(&limit, limit_id),
            limit_filters(&limit, limit_id),
            now,
        );

        let tracked = &mut self.peers[index];
        let opening = !tracked.opening_write_sent;
        tracked.outstanding = Some((counter, limit));
        // The opening write does not start the five-minute clock of §2.10. That
        // recommendation is about not churning the limit; the opening write is a protocol
        // obligation the guard owes whether or not the application has decided anything
        // yet, and if it went out as a deactivation, §2.13 is explicit that a device may
        // be unable to come back down again — so the requirement that follows it must not
        // wait five minutes behind it.
        tracked.last_write = (!opening).then_some(now);
        tracked.retry_at = None;
        tracked.write_due_at = None;
        tracked.opening_write_sent = true;
    }

    /// Notes that a heartbeat has gone out, against every peer that could receive it.
    ///
    /// Only the peers that are watching: a notification goes to subscribers, so a peer
    /// that has not subscribed has not been told anything, whatever the wire saw.
    fn record_beat(&mut self, now: Duration) {
        for tracked in &mut self.peers {
            if tracked.watches_heartbeat {
                tracked.beat_seen_at = Some(now);
            }
        }
    }

    fn tracked(&self, device: &AddressDevice) -> Option<&Tracked> {
        self.peers.iter().find(|t| &t.peer.device == device)
    }
}

/// Records what the grid now requires of one peer, and schedules the write.
fn set_required(tracked: &mut Tracked, limit: Option<LimitWrite>, now: Duration) {
    tracked.required = limit.map(normalise);
    // A new decision supersedes the old backoff: the operator changed its mind, and the
    // peer has not refused *this* value.
    tracked.retry_at = None;
    tracked.refusals = 0;
    tracked.write_due_at = Some(now);
}

/// Two limits that mean the same thing to a Controllable System.
fn same_limit(a: &LimitWrite, b: &LimitWrite) -> bool {
    a.is_active == b.is_active
        && a.duration == b.duration
        && (!a.is_active || (a.watts - b.watts).abs() < 0.5)
}

/// Applies §2.2 to a limit the application asked for.
///
/// A duration of zero means "deactivate now", so an *activated* limit must not carry
/// one; the guide forbids sending that combination rather than leaving the Controllable
/// System to reconcile it.
fn normalise(mut limit: LimitWrite) -> LimitWrite {
    if limit.watts < 0.0 {
        limit.watts = 0.0;
    }
    if limit.is_active && limit.duration == Some(Duration::ZERO) {
        limit.duration = None;
    }
    limit
}

/// The filters a limit write carries (LPC/LPP §3.4.1.4).
///
/// Always a `partial` update — a write that replaced the whole function would erase
/// `isLimitChangeable` and whatever else the Controllable System keeps there. And, when
/// the limit has **no** duration, a `delete` filter ahead of it that withdraws the
/// `endTime`.
///
/// That delete is not optional and not cosmetic. Under the partial concept an absent
/// element means *unchanged*, so a guard that follows "4.2 kW for fifteen minutes" with
/// "4.2 kW, open-ended" and simply omits the `timePeriod` leaves the old end time in
/// force: the limit lapses a quarter of an hour later, the household goes back to full
/// draw, and the operator's record says it asked for something indefinite. The
/// specification spells the remedy out and puts both filters in one command, which is
/// also what keeps it atomic — there is no instant in which the peer holds neither the
/// old duration nor the new value.
///
/// It is sent unconditionally rather than only when the peer is known to hold a duration:
/// deleting an element that is not there is a no-op, and the alternative is tracking the
/// peer's stored state well enough to be sure — which, after a reconnection, this side
/// cannot be.
fn limit_filters(limit: &LimitWrite, limit_id: LoadControlLimitId) -> Vec<Filter> {
    if limit.duration.is_some() {
        return alloc::vec![Filter::partial()];
    }
    let withdraw_end_time = Filter::delete()
        .select(FilterSelectors::LoadControlLimitListDataSelectors(
            LoadControlLimitListDataSelectors {
                limit_id: Some(limit_id),
            },
        ))
        .covering(FilterElements::LoadControlLimitDataElements(
            LoadControlLimitDataElements {
                time_period: Some(TimePeriodElements {
                    end_time: Some(crate::codec::ElementTag),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ));
    alloc::vec![withdraw_end_time, Filter::partial()]
}

/// The `loadControlLimitListData` payload of a limit write (LPC/LPP Table 23).
fn limit_payload(limit: &LimitWrite, limit_id: LoadControlLimitId) -> CmdData {
    CmdData::LoadControlLimitListData(LoadControlLimitListData {
        load_control_limit_data: Some(vec![LoadControlLimitData {
            limit_id: Some(limit_id),
            is_limit_active: Some(limit.is_active),
            value: Some(ScaledNumber::from_f64(limit.watts, 0)),
            time_period: limit.duration.map(|d| TimePeriod {
                end_time: Some(crate::model::format_iso8601_duration(d).into()),
                ..Default::default()
            }),
            ..Default::default()
        }]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A peer's identifier, deliberately not the one this crate publishes for itself.
    ///
    /// Every test here addresses the limit by a number the *peer* chose, because that is
    /// the only thing the wire ever carries: a guard that assumed [`super::super::LIMIT_ID`]
    /// would write into whatever that peer keeps under `1`.
    const PEER_LIMIT: LoadControlLimitId = LoadControlLimitId(7);

    /// §3.4.1.4: a limit without a duration carries the delete that withdraws the old
    /// `endTime`, because an omitted element means "unchanged" and would leave the
    /// previous duration in force.
    #[test]
    fn a_limit_without_a_duration_withdraws_the_old_end_time() {
        let filters = limit_filters(&LimitWrite::active(4_200.0), PEER_LIMIT);
        assert_eq!(filters.len(), 2, "one delete, then the partial update");

        let delete = &filters[0];
        assert!(delete.is_delete());
        assert!(
            !delete.is_partial(),
            "the delete and the update are separate filters"
        );
        assert_eq!(
            delete.selectors.as_deref(),
            Some(
                &[FilterSelectors::LoadControlLimitListDataSelectors(
                    LoadControlLimitListDataSelectors {
                        limit_id: Some(PEER_LIMIT),
                    }
                )][..]
            ),
            "the delete names the limit it addresses"
        );
        match &delete.elements {
            Some(FilterElements::LoadControlLimitDataElements(elements)) => {
                assert!(
                    elements
                        .time_period
                        .as_ref()
                        .is_some_and(|period| period.end_time.is_some()),
                    "and the element it withdraws"
                );
                assert!(
                    elements.value.is_none() && elements.is_limit_active.is_none(),
                    "and nothing else: the value and the activation are being written"
                );
            }
            other => panic!("expected load-control elements, saw {other:?}"),
        }
        assert!(filters[1].is_partial());

        // A limit that *has* a duration writes it, so there is nothing to withdraw.
        let with_duration = limit_filters(
            &LimitWrite::active_for(4_200.0, Duration::from_secs(900)),
            PEER_LIMIT,
        );
        assert_eq!(with_duration.len(), 1);
        assert!(with_duration[0].is_partial());
    }

    /// §2.2: an activated limit never goes out with a duration of zero.
    #[test]
    fn an_activated_limit_never_carries_a_zero_duration() {
        let limit = normalise(LimitWrite {
            is_active: true,
            watts: 4_200.0,
            duration: Some(Duration::ZERO),
        });
        assert_eq!(limit.duration, None, "the duration is unset, not zeroed");
        assert!(limit.is_active);

        // A deactivation may carry one: that combination is what the guide permits.
        let off = normalise(LimitWrite {
            is_active: false,
            watts: 0.0,
            duration: Some(Duration::ZERO),
        });
        assert_eq!(off.duration, Some(Duration::ZERO));
    }

    #[test]
    fn a_negative_limit_is_clamped_before_it_reaches_the_wire() {
        let limit = normalise(LimitWrite::active(-1.0));
        assert_eq!(limit.watts, 0.0);
    }

    #[test]
    fn a_limit_payload_carries_the_identifier_and_the_duration() {
        let data = limit_payload(
            &LimitWrite::active_for(3_000.0, Duration::from_secs(900)),
            PEER_LIMIT,
        );
        let CmdData::LoadControlLimitListData(list) = data else {
            panic!("expected the limit list");
        };
        let entry = &list.load_control_limit_data.as_ref().unwrap()[0];
        assert_eq!(entry.limit_id, Some(PEER_LIMIT));
        assert_eq!(entry.is_limit_active, Some(true));
        assert_eq!(
            entry.value.as_ref().and_then(ScaledNumber::to_f64),
            Some(3_000.0)
        );
        assert_eq!(
            entry
                .time_period
                .as_ref()
                .and_then(|p| p.end_time.as_ref())
                .map(|t| t.as_str()),
            Some("PT15M")
        );
    }
}
