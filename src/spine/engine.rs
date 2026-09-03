//! The SPINE engine: routing datagrams to and from the local device.
//!
//! Like the SHIP handshake, this is a state machine with no I/O. You hand it datagrams
//! and the current time; it tells you what to send, when to come back, and what
//! happened. That is what lets a whole conversation — discovery, binding, subscription,
//! a limit write and its acknowledgement — run in a unit test with a virtual clock.
//!
//! What the engine is responsible for:
//!
//! * **Validation.** A datagram whose header is incomplete is discarded in silence,
//!   because the SPINE implementation guide §2.1 says a reply would have nowhere to go.
//! * **Routing.** Addresses resolve to features of the [`LocalDevice`]; an address that
//!   names nothing is answered with `errorNumber` 4.
//! * **Permission.** A write without a binding is refused with `errorNumber` 9.
//! * **Acknowledgement.** Whether a `result` is owed, and what it says, follows the
//!   classifier table of §5.2.4.
//! * **Notification.** A local change is pushed to every subscriber.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;
use core::time::Duration;

use crate::model::rfe::RestrictError;
use crate::model::{
    AddressDevice, BindingId, Cmd, CmdClassifier, CmdData, Datagram, FeatureAddress, Filter,
    FilterSelectors, Function, Header, MsgCounter, NodeManagementBindingData,
    NodeManagementBindingDataBindingEntry, NodeManagementBindingRequestCall,
    NodeManagementSubscriptionData, NodeManagementSubscriptionDataSubscriptionEntry,
    NodeManagementSubscriptionRequestCall, Payload, ResultData, SpecificationVersion,
    SubscriptionId,
};

use super::ack::{DEFAULT_MAX_RESPONSE_DELAY, ErrorNumber, owes_ack};
use super::address::{
    self, is_node_management, node_management, node_management_without_device, same_feature,
};
use super::counter::{MsgCounterSource, MsgCounterTracker};
use super::device::{LocalDevice, WriteApproval};
use super::discovery::{RemoteDevice, SPINE_VERSION, detailed_discovery, use_case_data};
use super::relations::{BindingPolicy, Relations};
use crate::usecases::UseCaseDescriptor;

/// Something the application needs to know about.
#[derive(Clone, Debug, PartialEq)]
pub enum SpineEvent {
    /// A peer's detailed discovery data arrived or changed.
    DiscoveryUpdated {
        /// The peer.
        device: AddressDevice,
    },
    /// A peer's use-case data arrived or changed.
    UseCasesUpdated {
        /// The peer.
        device: AddressDevice,
    },
    /// A deferred write went unanswered long enough that the peer has stopped waiting.
    ///
    /// The application was handed a [`WriteRequest`] and never called
    /// [`accept_write`](Engine::accept_write) or
    /// [`reject_write`](Engine::reject_write). Its token no longer resolves, and nothing
    /// further will go to the peer — §5.2.5's maximum response delay has passed, so an
    /// answer now would arrive after the peer gave up.
    ///
    /// Under §14a EnWG this is worth logging rather than ignoring: a limit that was never
    /// decided is a limit that cannot be shown to have been honoured.
    WriteAbandoned {
        /// The token that will no longer resolve.
        token: WriteToken,
        /// The feature the write addressed.
        feature: FeatureAddress,
        /// The peer that is no longer waiting.
        from: FeatureAddress,
    },
    /// A peer wrote to one of this device's features, and the engine applied it.
    DataWritten {
        /// The feature that was written.
        feature: FeatureAddress,
        /// The peer that wrote it.
        from: FeatureAddress,
        /// The function that changed.
        function: Function,
    },
    /// A peer wrote to a feature whose writes the application decides on.
    ///
    /// Nothing has been stored and nothing has been answered. Call
    /// [`Engine::accept_write`] or [`Engine::reject_write`] with the token; until then
    /// the peer is waiting, and the maximum response delay of ten seconds is running.
    ///
    /// Boxed because it is by far the largest thing an event can carry, and every event
    /// queued behind it would otherwise be sized for it.
    WriteRequested(Box<WriteRequest>),
    /// A peer notified data this device is subscribed to.
    ///
    /// **Read [`resolved`](Self::DataNotified::resolved), not `data`**, for the same
    /// reason [`WriteRequested`](Self::WriteRequested) hands over both: a notification may
    /// be *partial*, and an omitted element then means *unchanged* rather than absent
    /// (SPINE IG §3.3). A measurement notified as a bare `number` with its `scale` left
    /// out is off by whatever the stored scale was.
    DataNotified {
        /// The feature the data came from.
        feature: FeatureAddress,
        /// The payload, exactly as it arrived — a fragment, if the notification was
        /// partial.
        data: CmdData,
        /// What that function now holds: `data` merged into what this peer had sent
        /// before.
        resolved: CmdData,
    },
    /// A reply to one of this device's reads arrived.
    ///
    /// A reply to a *partial* read is a fragment too (§5.3.4.5), so this carries both
    /// payloads for the same reason [`DataNotified`](Self::DataNotified) does.
    ReplyReceived {
        /// The feature that replied.
        feature: FeatureAddress,
        /// The payload, exactly as it arrived.
        data: CmdData,
        /// What that function now holds: `data` merged into what this peer had sent
        /// before.
        resolved: CmdData,
    },
    /// A request this device sent was acknowledged, positively or not.
    ResultReceived {
        /// The counter of the request this answers.
        request: MsgCounter,
        /// What the peer reported.
        error: ErrorNumber,
    },
    /// A peer was granted a binding on one of this device's features.
    ///
    /// The LPC implementation guide §3.8 turns this into a decision point: an energy
    /// manager may expose several entities, and which of them is actually in control is
    /// only settled once the bindings arrive.
    BindingGranted {
        /// The peer feature that asked.
        client: FeatureAddress,
        /// The local feature it may now write.
        server: FeatureAddress,
    },
    /// A peer released a binding.
    BindingReleased {
        /// The peer feature that held it.
        client: FeatureAddress,
        /// The local feature it may no longer write.
        server: FeatureAddress,
    },
    /// A peer was granted a subscription to one of this device's features.
    SubscriptionGranted {
        /// The peer feature that asked.
        client: FeatureAddress,
        /// The local feature it now watches.
        server: FeatureAddress,
    },
    /// A peer released a subscription.
    SubscriptionReleased {
        /// The peer feature that held it.
        client: FeatureAddress,
        /// The local feature it no longer watches.
        server: FeatureAddress,
    },
    /// A request went unanswered for longer than the maximum response delay.
    ///
    /// The SPINE implementation guide §2.6.1 distinguishes this from a refusal: a
    /// refusal is a completed exchange, a timeout is an unresponsive peer, and only the
    /// second one calls for the staggered retry of §2.6.2.
    RequestTimedOut {
        /// The counter of the request that went unanswered.
        request: MsgCounter,
        /// Where it was sent.
        destination: FeatureAddress,
    },
}

/// A peer's write, waiting on the application's decision.
///
/// Carried by [`SpineEvent::WriteRequested`]. Resolve it with [`Engine::accept_write`] or
/// [`Engine::reject_write`], naming the [`token`](Self::token).
#[derive(Clone, Debug, PartialEq)]
pub struct WriteRequest {
    /// Identifies this write when resolving it.
    pub token: WriteToken,
    /// The `msgCounter` of the write, which the acknowledgement will reference.
    ///
    /// Under §14a EnWG the pair — this counter and the answer that names it — is the
    /// operator's evidence that a limit was received and applied (LPC implementation
    /// guide §4.1.5), so an application that has to keep a record needs it here.
    pub request: MsgCounter,
    /// The feature that was written.
    pub feature: FeatureAddress,
    /// The peer that wrote it.
    pub from: FeatureAddress,
    /// What the peer sent, exactly as it arrived.
    ///
    /// This is the record of what was *asked for*, which under §14a EnWG is half of the
    /// evidence, and it is what says *which* entries the write addresses. To decide what
    /// those entries become, use [`resolved`](Self::resolved).
    pub data: CmdData,
    /// What the function would hold if this write were accepted: [`data`](Self::data)
    /// merged into what is stored.
    ///
    /// A partial write carries only what changed and an omitted element means *unchanged*
    /// (SPINE IG §3.3), so `data` alone does not describe the state the peer is asking
    /// for — a limit update that adjusts only `value` leaves `isLimitActive` out, and
    /// reading that as `false` turns a curtailment into a release back to full power.
    /// Decide with both: `data` for the entries addressed, `resolved` for their values.
    pub resolved: CmdData,
    /// Whether the write is partial.
    pub partial: bool,
    /// Whether the write is a delete.
    pub delete: bool,
}

/// Whether a command produced an answer now, or handed the decision to the application.
enum CmdOutcome {
    Answer(ErrorNumber),
    Deferred,
}

/// Identifies a write whose acceptance the application has yet to decide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WriteToken(u64);

/// Why [`Engine::accept_write`] could not do what the application decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WriteError {
    /// The token names no write that is still waiting: it was resolved already, abandoned
    /// after the peer stopped waiting ([`SpineEvent::WriteAbandoned`]), or never issued.
    #[error("the token names no write that is still waiting for a decision")]
    UnknownToken,
    /// The application accepted, and the feature could not store the result. The peer has
    /// been answered with this error rather than with an acknowledgement.
    #[error("the accepted write could not be stored: {0}")]
    NotStored(#[from] super::device::FeatureError),
}

/// A write that has been reported and not yet resolved.
#[derive(Clone, Debug)]
struct DeferredWrite {
    token: WriteToken,
    feature: FeatureAddress,
    peer: FeatureAddress,
    data: CmdData,
    ops: WriteOps,
    reference: MsgCounter,
    ack_request: bool,
    /// When the peer stops waiting for an answer (§5.2.5, the maximum response delay).
    deadline: Duration,
}

/// What one write command asks for, read off its filters.
///
/// A command may carry **several** filters (SPINE §5.3.4.2), and LPC UC TS §3.4.1.4 is
/// built on it: one filter deletes a limit's `endTime`, the next writes the new value —
/// in a single command, so a Controllable System never observes the limit without one or
/// the other. Collapsing that into "is this a delete?" and "is this partial?" answers
/// both yes, and then does only the delete: the limit is removed and the value that was
/// meant to replace it is dropped.
#[derive(Clone, Debug, PartialEq)]
struct WriteOps {
    /// The delete filters, in the order the command gives them.
    deletes: Vec<Filter>,
    /// Whether a filter asked for a partial update.
    partial: bool,
    /// Whether the payload is data to store, rather than only the identity of what a
    /// delete addresses.
    stores: bool,
}

impl WriteOps {
    fn of(cmd: &Cmd) -> Self {
        let deletes: Vec<Filter> = cmd
            .filter
            .iter()
            .flatten()
            .filter(|f| f.is_delete())
            .cloned()
            .collect();
        let partial = cmd.is_partial();
        Self {
            // A command of nothing but deletes carries its payload as identity. Anything
            // else — no filter at all, or one that asks for a partial update — stores it.
            stores: deletes.is_empty() || partial,
            partial,
            deletes,
        }
    }

    /// Runs the deletes, then the update, on `feature`.
    fn apply_to(
        &self,
        feature: &mut super::device::LocalFeature,
        data: CmdData,
    ) -> Result<(), super::device::FeatureError> {
        for filter in &self.deletes {
            feature.delete_filtered(&data, filter)?;
        }
        if self.stores {
            feature.apply(data, self.partial)?;
        }
        Ok(())
    }

    /// The same sequence against a detached copy of the stored data.
    fn resolve(&self, resolved: &mut CmdData, update: &CmdData) -> Result<(), RestrictError> {
        for filter in &self.deletes {
            let elements = filter.elements.as_ref();
            match filter.selectors.as_deref().unwrap_or_default() {
                [] => resolved.delete_restricted(update, None, elements)?,
                many => {
                    for selector in many {
                        resolved.delete_restricted(update, Some(selector), elements)?;
                    }
                }
            }
        }
        if self.stores {
            resolved
                .apply(update.clone(), self.partial)
                .map_err(|_| RestrictError::Mismatch)?;
        }
        Ok(())
    }
}

/// A request this device sent and is still waiting on.
#[derive(Clone, Debug)]
struct Pending {
    counter: MsgCounter,
    destination: FeatureAddress,
    deadline: Duration,
}

/// What this device knows about one peer.
#[derive(Clone, Debug)]
struct Peer {
    remote: RemoteDevice,
    counters: MsgCounterTracker,
    /// The merged state of every function this peer has sent, newest last.
    ///
    /// A partial notification is a fragment, and reading a fragment needs the value it is
    /// a fragment *of*. See [`MAX_REMOTE_FUNCTIONS`].
    data: Vec<(FeatureAddress, CmdData)>,
}

/// How many of a peer's functions one engine keeps the merged state of.
///
/// SPINE lets a server notify a *partial* update, and the implementation guide §3.2.2 asks
/// clients to subscribe rather than poll — so a fragment is the normal shape a measurement
/// arrives in, and an omitted `scale` means "the one you already have". Somebody has to
/// hold that value, and holding it once in the engine is what stops every use case holding
/// it differently.
///
/// The peer chooses how many functions it sends, so the table is bounded and the
/// least-recently-updated entry is evicted. A function whose state has been evicted starts
/// its next merge from empty, which is the same position the very first notification is in;
/// the number is far above what a real device notifies — the largest use case here watches
/// four functions on two features.
pub const MAX_REMOTE_FUNCTIONS: usize = 32;

/// How many peers one engine tracks.
///
/// A SPINE device on a home network talks to a handful of others; the cap exists because
/// a peer's device address arrives in the header of every datagram, and a node that
/// allocated an entry for each one it was told about would grow without bound on the word
/// of whoever is on the wire. SHIP's own connection limit is ten.
pub const MAX_PEERS: usize = 32;

/// How many writes may be waiting on the application at once.
///
/// A deferred write is memory the *peer* allocates: it writes, the engine raises
/// [`SpineEvent::WriteRequested`], and the entry lives until the application answers. An
/// application that is slow — or a peer that writes faster than any application could
/// answer — must not be able to grow that queue without bound on a device with a few
/// kilobytes to spare.
///
/// Sixteen is far more than a use case produces: LPC writes one limit at a time, and the
/// implementation guide's own rate limit is one write per five minutes. Reaching this cap
/// means something is wrong on the wire, and the answer is `errorNumber` 3 — overload —
/// which is exactly what §5.2.5 defines it for.
pub const MAX_DEFERRED_WRITES: usize = 16;

/// The SPINE engine of one device.
#[derive(Debug)]
pub struct Engine {
    device: LocalDevice,
    relations: Relations,
    use_cases: Vec<(Vec<u32>, u32, &'static UseCaseDescriptor, Vec<u32>)>,
    counters: MsgCounterSource,
    peers: Vec<Peer>,
    outbox: VecDeque<Datagram>,
    events: VecDeque<SpineEvent>,
    pending: Vec<Pending>,
    deferred: Vec<DeferredWrite>,
    next_token: u64,
    max_response_delay: Duration,
}

impl Engine {
    /// An engine serving `device`.
    pub fn new(device: LocalDevice) -> Self {
        Self {
            device,
            relations: Relations::new(BindingPolicy::SinglePerFeature),
            use_cases: Vec::new(),
            counters: MsgCounterSource::default(),
            peers: Vec::new(),
            outbox: VecDeque::new(),
            events: VecDeque::new(),
            pending: Vec::new(),
            deferred: Vec::new(),
            next_token: 1,
            max_response_delay: DEFAULT_MAX_RESPONSE_DELAY,
        }
    }

    /// Declares that an entity plays a use case, implementing what it requires.
    ///
    /// The scenarios announced are the ones the specification does not leave optional. A
    /// device that implements optional scenarios too has to say so — see
    /// [`add_use_case_scenarios`](Self::add_use_case_scenarios) — because a peer decides
    /// what to ask for from this list, and will not look for what is not in it.
    pub fn add_use_case(
        &mut self,
        entity: impl Into<Vec<u32>>,
        feature: u32,
        descriptor: &'static UseCaseDescriptor,
    ) {
        let scenarios = descriptor.required_scenarios().collect();
        self.use_cases
            .push((entity.into(), feature, descriptor, scenarios));
    }

    /// Declares that an entity plays a use case, implementing exactly these scenarios.
    ///
    /// Numbers the use case does not define for this actor are dropped rather than
    /// announced: `useCaseScenarioSupport` is what a peer plans against, and a scenario
    /// that does not exist would send it looking for functions nobody serves.
    pub fn add_use_case_scenarios(
        &mut self,
        entity: impl Into<Vec<u32>>,
        feature: u32,
        descriptor: &'static UseCaseDescriptor,
        scenarios: &[u32],
    ) {
        let scenarios = scenarios
            .iter()
            .copied()
            .filter(|s| descriptor.defines_scenario(*s))
            .collect();
        self.use_cases
            .push((entity.into(), feature, descriptor, scenarios));
    }

    /// The local device.
    pub fn device(&self) -> &LocalDevice {
        &self.device
    }

    /// The local device, for publishing data.
    pub fn device_mut(&mut self) -> &mut LocalDevice {
        &mut self.device
    }

    /// The bindings and subscriptions held.
    pub fn relations(&self) -> &Relations {
        &self.relations
    }

    /// Declares that these features belong to one use case and share a binding partner.
    ///
    /// See [`Relations::bind_together`]: it is what stops two energy managers each
    /// winning one half of the pair LPC needs, after which neither can run the use case.
    pub fn bind_features_together(&mut self, features: impl IntoIterator<Item = FeatureAddress>) {
        self.relations.bind_together(features);
    }

    /// Replaces the binding policy, for a device whose features carry entries belonging
    /// to several use cases with different controllers (LPC IG §3.5).
    pub fn set_binding_policy(&mut self, policy: BindingPolicy) {
        self.relations.set_policy(policy);
    }

    /// What this device knows about a peer.
    pub fn peer(&self, device: &AddressDevice) -> Option<&RemoteDevice> {
        self.peers
            .iter()
            .find(|p| p.remote.address.as_ref() == Some(device))
            .map(|p| &p.remote)
    }

    /// Every peer this device has discovered.
    pub fn peers(&self) -> impl Iterator<Item = &RemoteDevice> {
        self.peers.iter().map(|p| &p.remote)
    }

    /// The next datagram to send.
    pub fn poll_transmit(&mut self) -> Option<Datagram> {
        self.outbox.pop_front()
    }

    /// The next thing that happened.
    pub fn poll_event(&mut self) -> Option<SpineEvent> {
        self.events.pop_front()
    }

    /// When [`handle_timeout`](Self::handle_timeout) should next be called.
    ///
    /// Both kinds of deadline are folded in: a request this device is waiting on, and a
    /// write it has put to the application and not been answered about. The second one
    /// matters more than it looks — a device that only ever *serves* writes has no
    /// requests outstanding, so leaving the deferred writes out reported "no deadline",
    /// nothing ever called [`handle_timeout`](Self::handle_timeout), and the queue that
    /// [`MAX_DEFERRED_WRITES`] bounds filled up and stayed full.
    pub fn poll_timeout(&self) -> Option<Duration> {
        self.pending
            .iter()
            .map(|p| p.deadline)
            .chain(self.deferred.iter().map(|w| w.deadline))
            .min()
    }

    /// Expires requests that went unanswered.
    pub fn handle_timeout(&mut self, now: Duration) {
        let (expired, waiting): (Vec<_>, Vec<_>) =
            self.pending.drain(..).partition(|p| now >= p.deadline);
        self.pending = waiting;
        for request in expired {
            self.events.push_back(SpineEvent::RequestTimedOut {
                request: request.counter,
                destination: request.destination,
            });
        }

        // A write the application never decided. The peer stopped waiting a long time
        // ago — §5.2.5's maximum response delay is ten seconds — so holding the entry
        // buys nothing and holding all of them is how a queue grows without bound. The
        // application is told the decision is no longer wanted, and the token it holds
        // stops resolving.
        let (stale, live): (Vec<_>, Vec<_>) =
            self.deferred.drain(..).partition(|w| now >= w.deadline);
        self.deferred = live;
        for write in stale {
            self.events.push_back(SpineEvent::WriteAbandoned {
                token: write.token,
                feature: write.feature,
                from: write.peer,
            });
        }
    }

    // ---- sending ---------------------------------------------------------------

    /// Runs the opening exchange of a fresh connection: who are you, and what do you do?
    ///
    /// Every SHIP connection starts the same way, and it is the same two reads every time:
    /// `nodeManagementDetailedDiscoveryData` for the peer's device, entities and features,
    /// and `nodeManagementUseCaseData` for the actors it plays. Until both have come back
    /// there is nothing to bind to and nothing to subscribe to — the addresses a use case
    /// needs are in the first reply and the use case itself is in the second.
    ///
    /// [`runtime::Hub`](crate::runtime::Hub) calls this for you when a connection opens.
    /// **This is the supported way for an application that owns its own [`Engine`]** —
    /// a driver that has to be testable without a socket, a different transport, a
    /// gateway — to run the same opening exchange. There is nothing else to reproduce:
    /// call it once per connection, and route the two replies back in.
    ///
    /// Both reads are addressed *without* a device part, which the SPINE implementation
    /// guide §2.7 permits for exactly this message and no other: the peer's device address
    /// is what the answer contains, so it cannot be in the question. That makes it
    /// meaningful only on a connection whose far end is a single peer — which is what a
    /// SHIP connection is. Use [`discover`](Self::discover) where the address is already
    /// known, as it is on a reconnection.
    ///
    /// Returns the two `msgCounter`s, in that order, so a caller can tell the replies from
    /// anything else it has outstanding.
    ///
    /// ```
    /// use core::time::Duration;
    /// use eebus::prelude::*;
    ///
    /// let mut device = LocalDevice::new("i:46925", "HeatPump-1", DeviceType::HeatGenerationSystem)?;
    /// device.add_entity(LocalEntity::new([1], EntityType::HeatPumpAppliance))?;
    /// let mut engine = Engine::new(device);
    ///
    /// // A connection just opened. Ask what is on the other end of it.
    /// let [discovery, use_cases] = engine.start_discovery(Duration::ZERO);
    /// assert_ne!(discovery, use_cases);
    ///
    /// // Two datagrams to put on the wire, and nothing else queued.
    /// assert!(engine.poll_transmit().is_some());
    /// assert!(engine.poll_transmit().is_some());
    /// assert!(engine.poll_transmit().is_none());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn start_discovery(&mut self, now: Duration) -> [MsgCounter; 2] {
        self.discovery_reads(&node_management_without_device(), now)
    }

    /// The opening exchange, addressed to a peer whose device address is already known.
    ///
    /// The same two reads as [`start_discovery`](Self::start_discovery), for the case the
    /// guide's §2.7 exception does not cover: re-reading a peer that announced a change,
    /// or a transport that carries more than one peer.
    pub fn discover(&mut self, peer: &AddressDevice, now: Duration) -> [MsgCounter; 2] {
        self.discovery_reads(&node_management(peer), now)
    }

    fn discovery_reads(&mut self, destination: &FeatureAddress, now: Duration) -> [MsgCounter; 2] {
        let source = node_management(self.device().address());
        [
            self.read(
                destination,
                &source,
                Function::NodeManagementDetailedDiscoveryData,
                now,
            ),
            self.read(
                destination,
                &source,
                Function::NodeManagementUseCaseData,
                now,
            ),
        ]
    }

    /// Reads a function from a peer.
    pub fn read(
        &mut self,
        destination: &FeatureAddress,
        source: &FeatureAddress,
        function: Function,
        now: Duration,
    ) -> MsgCounter {
        let counter = self.counters.next();
        self.send(
            Datagram {
                header: Some(self.header(
                    source,
                    destination,
                    counter,
                    CmdClassifier::Read,
                    false,
                    None,
                )),
                payload: Some(Payload {
                    cmd: Some(vec![Cmd::read(function)]),
                }),
            },
            Some((destination.clone(), counter)),
            now,
        );
        counter
    }

    /// Writes to a peer's feature, asking for an acknowledgement.
    ///
    /// A write always requests one: the acknowledgement is the only thing that tells the
    /// sender whether the peer accepted, and under §14a it is also the evidence the
    /// operator has to be able to produce.
    pub fn write(
        &mut self,
        destination: &FeatureAddress,
        source: &FeatureAddress,
        data: CmdData,
        partial: bool,
        now: Duration,
    ) -> MsgCounter {
        let filters = if partial {
            alloc::vec![Filter::partial()]
        } else {
            Vec::new()
        };
        self.write_filtered(destination, source, data, filters, now)
    }

    /// Writes to a peer's feature with filters of the caller's choosing.
    ///
    /// What [`write`](Self::write) cannot express: a command that **withdraws** an
    /// element and writes new data in the same breath. A partial write can only ever set
    /// elements, because an absent element means "unchanged" — so removing one takes a
    /// `delete` filter, and LPC §3.4.1.4 gives the worked example of doing both at once:
    ///
    /// > Special care must be taken by the Actor Energy Guard in case a previous limit
    /// > WITH the Element endTime set is overwritten with a new limit WITHOUT the Element
    /// > endTime. […] the Energy Guard has to delete the old endTime Element with a
    /// > partial delete command (that can be part of the write command that includes the
    /// > new limit).
    ///
    /// Filters are applied in the order given, so the delete comes first and the partial
    /// update after it. One command, one acknowledgement, and no window in which the peer
    /// holds a limit with neither the old duration nor the new value.
    pub fn write_filtered(
        &mut self,
        destination: &FeatureAddress,
        source: &FeatureAddress,
        data: CmdData,
        filters: Vec<Filter>,
        now: Duration,
    ) -> MsgCounter {
        let counter = self.counters.next();
        let mut cmd = Cmd::with_data(data);
        for filter in filters {
            cmd = cmd.with_filter(filter);
        }
        self.send(
            Datagram {
                header: Some(self.header(
                    source,
                    destination,
                    counter,
                    CmdClassifier::Write,
                    true,
                    None,
                )),
                payload: Some(Payload {
                    cmd: Some(vec![cmd]),
                }),
            },
            Some((destination.clone(), counter)),
            now,
        );
        counter
    }

    /// Notifies every subscriber of a local feature that its data changed.
    ///
    /// The SPINE implementation guide §2.4 asks implementations to keep this quiet:
    /// notifying unchanged data, or notifying a volatile measurement every few
    /// milliseconds, can overload an energy manager talking to a dozen devices. The
    /// engine sends what it is told to; the rate limiting belongs to the caller, which
    /// knows what "changed enough" means for its data.
    pub fn notify(&mut self, feature: &FeatureAddress, function: &Function, now: Duration) {
        // NodeManagement's own functions are computed rather than stored — the same
        // rule [`handle_read`](Self::handle_read) follows. Reading only the stored data
        // here made a subscription to NodeManagement unserviceable: it is granted, the
        // peer waits for the entity list to change, and no notification can ever be
        // built for it (§7.1.3, §7.3.2, §7.4.2).
        let computed = is_node_management(feature)
            .then(|| self.node_management_data(function))
            .flatten();
        let Some(data) = computed.or_else(|| {
            self.device
                .resolve(feature)
                .and_then(|f| f.data(function))
                .cloned()
        }) else {
            return;
        };
        let subscribers: Vec<FeatureAddress> =
            self.relations.subscribers_of(feature).cloned().collect();

        for subscriber in subscribers {
            let counter = self.counters.next();
            self.send(
                Datagram {
                    header: Some(self.header(
                        feature,
                        &subscriber,
                        counter,
                        CmdClassifier::Notify,
                        false,
                        None,
                    )),
                    payload: Some(Payload {
                        cmd: Some(vec![Cmd::with_data(data.clone())]),
                    }),
                },
                None,
                now,
            );
        }
    }

    /// Asks a peer for a binding on one of its features.
    pub fn request_binding(
        &mut self,
        client: &FeatureAddress,
        server: &FeatureAddress,
        now: Duration,
    ) -> MsgCounter {
        let call = CmdData::NodeManagementBindingRequestCall(NodeManagementBindingRequestCall {
            binding_request: Some(
                crate::model::NodeManagementBindingRequestCallBindingRequest {
                    client_address: Some(client.clone()),
                    server_address: Some(server.clone()),
                    server_feature_type: self.peer_feature_type(server).or_else(|| {
                        self.device
                            .resolve(server)
                            .map(|f| f.feature_type().clone())
                    }),
                },
            ),
        });
        self.call(call, server, now)
    }

    /// Asks a peer for a subscription to one of its features.
    pub fn request_subscription(
        &mut self,
        client: &FeatureAddress,
        server: &FeatureAddress,
        now: Duration,
    ) -> MsgCounter {
        let call =
            CmdData::NodeManagementSubscriptionRequestCall(NodeManagementSubscriptionRequestCall {
                subscription_request: Some(
                    crate::model::NodeManagementSubscriptionRequestCallSubscriptionRequest {
                        client_address: Some(client.clone()),
                        server_address: Some(server.clone()),
                        server_feature_type: self.peer_feature_type(server).or_else(|| {
                            self.device
                                .resolve(server)
                                .map(|f| f.feature_type().clone())
                        }),
                    },
                ),
            });
        self.call(call, server, now)
    }

    /// Sends a call to the peer's primary NodeManagement instance.
    ///
    /// The SPINE implementation guide §2.3 fixes the addressing: a binding or
    /// subscription request goes to entity 0, feature 0, and should come from ours.
    fn call(&mut self, data: CmdData, server: &FeatureAddress, now: Duration) -> MsgCounter {
        let counter = self.counters.next();
        let source = address::node_management(self.device.address());
        let destination = match &server.device {
            Some(device) => address::node_management(device),
            None => address::node_management_without_device(),
        };
        self.send(
            Datagram {
                header: Some(self.header(
                    &source,
                    &destination,
                    counter,
                    CmdClassifier::Call,
                    true,
                    None,
                )),
                payload: Some(Payload {
                    cmd: Some(vec![Cmd::with_data(data)]),
                }),
            },
            Some((destination, counter)),
            now,
        );
        counter
    }

    fn peer_feature_type(&self, address: &FeatureAddress) -> Option<crate::model::FeatureType> {
        let device = address.device.as_ref()?;
        let remote = self.peer(device)?;
        let path = address::entity_path(address);
        remote
            .entity(&path)?
            .features
            .iter()
            .find(|f| f.address.feature == address.feature)
            .map(|f| f.feature_type.clone())
    }

    fn send(
        &mut self,
        datagram: Datagram,
        awaiting: Option<(FeatureAddress, MsgCounter)>,
        now: Duration,
    ) {
        if let Some((destination, counter)) = awaiting {
            let deadline = now + self.response_delay_of(&destination);
            self.pending.push(Pending {
                counter,
                destination,
                deadline,
            });
        }
        self.outbox.push_back(datagram);
    }

    /// How long to wait for an answer from `destination`.
    ///
    /// §5.2.5.3 lets a feature announce a `maxResponseDelay` longer than the ten-second
    /// default in its detailed discovery, and a client is meant to honour it. Ignoring it
    /// reports a conformant peer as unresponsive — which the implementation guide §2.6.1
    /// distinguishes from a refusal precisely because §2.6.2's staggered retry follows the
    /// first and not the second. So the guess would not merely be wrong; it would retry
    /// against a peer that is answering as fast as it said it would.
    fn response_delay_of(&self, destination: &FeatureAddress) -> Duration {
        destination
            .device
            .as_ref()
            .and_then(|device| self.peer(device))
            .and_then(|remote| remote.feature_at(destination))
            .and_then(|feature| feature.max_response_delay)
            .unwrap_or(self.max_response_delay)
    }

    /// Fills in the device part of a local address.
    ///
    /// A request may leave it out — the SPINE implementation guide §2.7 permits exactly
    /// one message to do so, the opening detailed-discovery read — but a *response* may
    /// not: the sender's own address is how the receiver knows whose data it is looking
    /// at. Echoing the request's destination back unchanged loses that, and a peer that
    /// bootstrapped without knowing our address would file the answer under nobody.
    fn local_address(&self, addressed: &FeatureAddress) -> FeatureAddress {
        FeatureAddress {
            device: Some(self.device.address().clone()),
            ..addressed.clone()
        }
    }

    fn header(
        &self,
        source: &FeatureAddress,
        destination: &FeatureAddress,
        counter: MsgCounter,
        classifier: CmdClassifier,
        ack_request: bool,
        reference: Option<MsgCounter>,
    ) -> Header {
        Header {
            specification_version: Some(SpecificationVersion::from(SPINE_VERSION)),
            address_source: Some(source.clone()),
            address_destination: Some(destination.clone()),
            msg_counter: Some(counter),
            msg_counter_reference: reference,
            cmd_classifier: Some(classifier),
            ack_request: ack_request.then_some(true),
            ..Default::default()
        }
    }

    // ---- receiving -------------------------------------------------------------

    /// Processes a received datagram.
    ///
    /// Returns `false` when the datagram was discarded without a response, which the
    /// implementation guide §2.1 requires for a malformed or incomplete header: there is
    /// no reliable place to send an error, so silence is the only safe answer.
    pub fn handle_datagram(&mut self, datagram: &Datagram, now: Duration) -> bool {
        let Some(header) = &datagram.header else {
            return false;
        };
        let (Some(counter), Some(classifier), Some(source)) = (
            header.msg_counter,
            header.cmd_classifier,
            header.address_source.as_ref(),
        ) else {
            return false;
        };
        let Some(destination) = header.address_destination.as_ref() else {
            return false;
        };

        // An address this node cannot key on is a header it cannot answer: every reply
        // goes back to `source`, and the peer record, the routing and the § 14a audit
        // entry are all filed under it. The implementation guide §2.1 says a header that
        // cannot be trusted is discarded in silence rather than answered, and this is
        // that case: there is nowhere for an error to go.
        if source
            .device
            .as_ref()
            .is_some_and(|device| !address::is_usable_device_address(device.as_str()))
        {
            return false;
        }

        // Implementation guide §2.5: a version string that does not match the pattern,
        // or a major this implementation does not speak, is refused. `TC_SPINE_COMP_006`
        // calls that the recommended behaviour — answering a datagram whose header
        // cannot be trusted is how a peer ends up acting on a misread limit.
        let version =
            super::version::check(header.specification_version.as_ref().map(|v| v.as_str()));
        if !version.is_acceptable() {
            if owes_ack(classifier, true, ErrorNumber::General) {
                self.send_result(source, destination, counter, ErrorNumber::General, now);
            }
            return false;
        }

        // §7.1.1.5: a datagram addressed to another device is not ours to serve.
        // Enhanced-mode routing, where a node forwards on another's behalf, is not
        // implemented, so `errorNumber` 5 — destination unreachable — is the truth.
        if let Some(device) = &destination.device
            && device != self.device.address()
        {
            let counter_ok = source.device.as_ref().is_none_or(|peer| {
                self.peer_entry(peer)
                    .counters
                    .observe(counter)
                    .is_acceptable()
            });
            if counter_ok && owes_ack(classifier, true, ErrorNumber::DestinationUnreachable) {
                self.send_result(
                    source,
                    destination,
                    counter,
                    ErrorNumber::DestinationUnreachable,
                    now,
                );
            }
            return false;
        }

        if let Some(device) = &source.device {
            let peer = self.peer_entry(device);
            if !peer.counters.observe(counter).is_acceptable() {
                // A duplicate. Processing it again would re-apply a write.
                return false;
            }
        }

        let ack_request = header.ack_request.unwrap_or(false);
        let reference = header.msg_counter_reference;
        let mut error = ErrorNumber::None;
        let mut deferred = false;

        // SPINE §5.3.2: "each instance of a payload element SHALL contain exactly one
        // cmd instance". Several are refused whole rather than executed in part, because
        // one acknowledgement cannot report two outcomes; none asked for nothing, and
        // acknowledging that would report a write the peer never made.
        let mut commands = datagram.payload.iter().flat_map(|p| p.cmd.iter().flatten());
        match (commands.next(), commands.next()) {
            (Some(cmd), None) => {
                match self.handle_cmd(
                    cmd,
                    source,
                    destination,
                    classifier,
                    counter,
                    reference,
                    ack_request,
                    now,
                ) {
                    CmdOutcome::Deferred => deferred = true,
                    CmdOutcome::Answer(outcome) if !outcome.is_success() => error = outcome,
                    CmdOutcome::Answer(_) => {}
                }
            }
            (Some(_), Some(_)) => error = ErrorNumber::General,
            (None, _) => {
                if matches!(
                    classifier,
                    CmdClassifier::Read | CmdClassifier::Write | CmdClassifier::Call
                ) {
                    error = ErrorNumber::General;
                }
            }
        }

        // A deferred write is answered when the application decides, not now.
        if !deferred && owes_ack(classifier, ack_request, error) {
            self.send_result(source, destination, counter, error, now);
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_cmd(
        &mut self,
        cmd: &Cmd,
        source: &FeatureAddress,
        destination: &FeatureAddress,
        classifier: CmdClassifier,
        counter: MsgCounter,
        reference: Option<MsgCounter>,
        ack_request: bool,
        now: Duration,
    ) -> CmdOutcome {
        match classifier {
            CmdClassifier::Read => {
                CmdOutcome::Answer(self.handle_read(cmd, source, destination, counter, now))
            }
            CmdClassifier::Write => {
                self.handle_write(cmd, source, destination, counter, ack_request, now)
            }
            CmdClassifier::Call => CmdOutcome::Answer(self.handle_call(cmd, source)),
            CmdClassifier::Reply | CmdClassifier::Notify => {
                self.resolve_pending(reference, source);
                CmdOutcome::Answer(self.handle_incoming_data(cmd, source, classifier))
            }
            CmdClassifier::Result => {
                self.handle_result(cmd, reference, source);
                CmdOutcome::Answer(ErrorNumber::None)
            }
        }
    }

    /// Clears the request a response answers, so it does not later time out.
    ///
    /// `msgCounterReference` is what ties the two together (`TC_SPINE_DATA_004`); a
    /// response without one cannot be matched and leaves the request to expire.
    ///
    /// **And it has to come from the peer the request went to.** `msgCounter` is
    /// allocated per *engine*, not per peer, so a device on the same LAN — paired but
    /// misbehaving, or simply confused — can otherwise name a counter belonging to a
    /// conversation with somebody else and have its answer filed under that request.
    /// Under §14a EnWG that is an acknowledgement attributed to a limit it never
    /// answered. The one address that may be absent is the opening discovery read's,
    /// which §2.7 permits precisely because a SHIP connection has one peer on it.
    fn resolve_pending(
        &mut self,
        reference: Option<MsgCounter>,
        from: &FeatureAddress,
    ) -> Option<Pending> {
        let reference = reference?;
        let index = self.pending.iter().position(|p| {
            p.counter == reference
                && p.destination
                    .device
                    .as_ref()
                    .is_none_or(|peer| Some(peer) == from.device.as_ref())
        })?;
        Some(self.pending.remove(index))
    }

    fn handle_read(
        &mut self,
        cmd: &Cmd,
        source: &FeatureAddress,
        destination: &FeatureAddress,
        counter: MsgCounter,
        now: Duration,
    ) -> ErrorNumber {
        // A read names the function it wants in either of two ways: the explicit `function`
        // element, or an empty instance of the data element. `spine-go`, and therefore most
        // of the deployed base, sends only the second.
        let function = match cmd.function.clone() {
            Some(function) => function,
            None => match &cmd.data {
                Some(data) => Function::from(data.key()),
                None => return ErrorNumber::General,
            },
        };

        // §5.3.4.4: a read considers at most one filter. Two would have to be combined,
        // and the specification does not say how, so the honest answer is number 8.
        let mut filters = cmd.filter.iter().flatten();
        let filter = filters.next();
        if filters.next().is_some() {
            return ErrorNumber::RestrictedExchangeNotSupported;
        }

        // NodeManagement is resolved like any other feature — it *is* one — so a function
        // it does not declare is refused rather than answered with an empty payload. Only
        // its data differs: discovery, the use-case table and the two relation tables are
        // computed rather than stored (§7.1.3, §7.3.2, §7.4.2, §7.5.3).
        let Some(feature) = self.device.resolve(destination) else {
            return ErrorNumber::DestinationUnknown;
        };
        let (stored, partial_offered) = match feature.function(&function) {
            None => return ErrorNumber::CommandNotSupported,
            Some(entry) if !entry.operations.read => {
                return ErrorNumber::CommandNotSupported;
            }
            Some(entry) => {
                let operations = entry.operations;
                let stored = entry.data.clone();
                let computed = if is_node_management(destination) {
                    self.node_management_data(&function)
                } else {
                    None
                };
                (computed.or(stored), operations.read_partial)
            }
        };

        // The function exists but holds nothing yet. An empty payload of the right
        // function is a truthful answer, and it is an answer: a read left unanswered
        // makes the peer wait out its whole response deadline for no reason.
        let Some(data) = stored.or_else(|| CmdData::empty(function.as_str())) else {
            return ErrorNumber::CommandNotSupported;
        };

        let (data, partial) = match filter {
            None => (data, false),
            Some(filter) if !partial_offered => {
                let _ = filter;
                return ErrorNumber::RestrictedExchangeNotSupported;
            }
            Some(filter) => match restrict_for_read(&data, filter) {
                Ok(data) => (data, true),
                Err(error) => return error.error_number(),
            },
        };

        let mut reply = Cmd::with_data(data);
        if partial {
            // §5.3.4.5: a partial reply says so, so the client knows the answer is a
            // subset rather than the whole function.
            reply = reply.with_filter(Filter::partial());
        }

        let reply_counter = self.counters.next();
        let reply_source = self.local_address(destination);
        self.send(
            Datagram {
                header: Some(self.header(
                    &reply_source,
                    source,
                    reply_counter,
                    CmdClassifier::Reply,
                    false,
                    Some(counter),
                )),
                payload: Some(Payload {
                    cmd: Some(vec![reply]),
                }),
            },
            None,
            now,
        );
        ErrorNumber::None
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_write(
        &mut self,
        cmd: &Cmd,
        source: &FeatureAddress,
        destination: &FeatureAddress,
        counter: MsgCounter,
        ack_request: bool,
        now: Duration,
    ) -> CmdOutcome {
        let Some(data) = cmd.data.clone() else {
            return CmdOutcome::Answer(ErrorNumber::General);
        };
        let Some(feature) = self.device.resolve(destination) else {
            return CmdOutcome::Answer(ErrorNumber::DestinationUnknown);
        };
        // `TC_SPINE_BIND_002`: a write without a binding is refused.
        if !self.relations.is_bound(source, destination) {
            return CmdOutcome::Answer(ErrorNumber::BindingRequired);
        }

        let function = Function::from(data.key());
        let ops = WriteOps::of(cmd);
        let (delete, partial) = (!ops.deletes.is_empty(), ops.partial);

        // A write the feature would refuse regardless is refused now, rather than put to
        // the application only to be turned down. A delete is a restricted exchange as
        // much as a partial update is, and `possibleOperations` has one flag for both.
        if let Err(e) = feature.check_write(&data, partial || delete) {
            return CmdOutcome::Answer(e.error_number());
        }

        if feature.write_approval() == WriteApproval::Deferred {
            // The queue is the peer's to fill and the application's to drain. A peer that
            // writes faster than the application answers is told it is being turned away
            // rather than silently costing memory.
            if self.deferred.len() >= MAX_DEFERRED_WRITES {
                return CmdOutcome::Answer(ErrorNumber::Overload);
            }
            // The peer waits as long as this feature announced it might take, which is
            // the same figure §5.2.5.3 put in discovery — so a feature that said it needs
            // a minute is not abandoned after ten seconds.
            let decision_window = feature
                .max_response_delay()
                .unwrap_or(self.max_response_delay);
            let resolved = resolve_write(feature.data(&function), &data, &ops);
            let token = WriteToken(self.next_token);
            self.next_token += 1;
            self.deferred.push(DeferredWrite {
                token,
                feature: destination.clone(),
                peer: source.clone(),
                data: data.clone(),
                ops: ops.clone(),
                reference: counter,
                ack_request,
                deadline: now + decision_window,
            });
            self.events
                .push_back(SpineEvent::WriteRequested(Box::new(WriteRequest {
                    token,
                    request: counter,
                    feature: destination.clone(),
                    from: source.clone(),
                    data,
                    resolved,
                    partial,
                    delete,
                })));
            return CmdOutcome::Deferred;
        }

        let feature = self
            .device
            .resolve_mut(destination)
            .expect("resolved a moment ago");
        let outcome = ops.apply_to(feature, data);

        match outcome {
            Ok(()) => {
                self.events.push_back(SpineEvent::DataWritten {
                    feature: destination.clone(),
                    from: source.clone(),
                    function: function.clone(),
                });
                // §7.4: a subscriber asked to be told when this feature's data changes,
                // and a peer's write is a change. Without this a client that subscribed
                // instead of polling — which the use-case implementation guide §3.2.2
                // asks it to do — would never learn what the write did.
                self.notify(destination, &function, now);
                CmdOutcome::Answer(ErrorNumber::None)
            }
            Err(e) => CmdOutcome::Answer(e.error_number()),
        }
    }

    /// Applies a deferred write and acknowledges it.
    ///
    /// ```ignore
    /// SpineEvent::WriteRequested(write) => {
    ///     match use_case.decide(&write.resolved) {
    ///         Accept => engine.accept_write(write.token, now),
    ///         Reject => engine.reject_write(write.token, ErrorNumber::CommandRejected, now),
    ///     }
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// The application's *decision* is to accept; whether the engine can then *store* the
    /// write is a second question, and the two can disagree — the merged list would exceed
    /// [`MAX_LIST_ENTRIES`](super::MAX_LIST_ENTRIES), say. When they do, the peer has been
    /// answered with the error, **not** with an acknowledgement, and this says so: a use
    /// case that had already moved its state machine and written "accepted" into a §14a
    /// record would otherwise be holding evidence of an acknowledgement that never went
    /// out. An unknown token — already resolved, abandoned, or never issued — is
    /// [`WriteError::UnknownToken`].
    pub fn accept_write(&mut self, token: WriteToken, now: Duration) -> Result<(), WriteError> {
        let Some(write) = self.take_deferred(token) else {
            return Err(WriteError::UnknownToken);
        };
        let function = Function::from(write.data.key());
        let outcome = match self.device.resolve_mut(&write.feature) {
            None => Err(super::device::FeatureError::UnknownFunction),
            Some(feature) => write.ops.apply_to(feature, write.data),
        };

        let error = match &outcome {
            Ok(()) => {
                self.events.push_back(SpineEvent::DataWritten {
                    feature: write.feature.clone(),
                    from: write.peer.clone(),
                    function: function.clone(),
                });
                self.notify(&write.feature, &function, now);
                ErrorNumber::None
            }
            Err(e) => e.error_number(),
        };
        self.answer_deferred(
            &write.peer,
            &write.feature,
            write.reference,
            write.ack_request,
            error,
            now,
        );
        outcome.map_err(WriteError::NotStored)
    }

    /// Refuses a deferred write, storing nothing and reporting `error`.
    ///
    /// `ErrorNumber::CommandRejected` is the one a use case means when it declines a
    /// value it cannot follow — LPC's NACK.
    ///
    /// Returns whether the token named a write still waiting; a refusal of a write that was
    /// already resolved or abandoned changes nothing.
    pub fn reject_write(&mut self, token: WriteToken, error: ErrorNumber, now: Duration) -> bool {
        let Some(write) = self.take_deferred(token) else {
            return false;
        };
        let error = if error.is_success() {
            // A refusal that reports success would tell the peer its limit was applied.
            ErrorNumber::CommandRejected
        } else {
            error
        };
        self.answer_deferred(
            &write.peer,
            &write.feature,
            write.reference,
            write.ack_request,
            error,
            now,
        );
        true
    }

    fn take_deferred(&mut self, token: WriteToken) -> Option<DeferredWrite> {
        let index = self.deferred.iter().position(|w| w.token == token)?;
        Some(self.deferred.remove(index))
    }

    fn answer_deferred(
        &mut self,
        peer: &FeatureAddress,
        feature: &FeatureAddress,
        reference: MsgCounter,
        ack_request: bool,
        error: ErrorNumber,
        now: Duration,
    ) {
        if owes_ack(CmdClassifier::Write, ack_request, error) {
            self.send_result(peer, feature, reference, error, now);
        }
    }

    /// The client address a binding or subscription call names, as this node will file it.
    ///
    /// The address is the *peer's* to name and the payload is where it names it — but the
    /// peer that sent the datagram is the one SHIP authenticated, and the two have to
    /// agree. A call naming a client on another device would let one paired peer grant
    /// itself a relation in another's name, or release another's binding, and §7.3 gives a
    /// binding's client no authority over anybody else's. A call that leaves the device
    /// part out is completed from the header, which is what `spine-go` does too.
    fn client_of_caller(
        named: &FeatureAddress,
        source: &FeatureAddress,
    ) -> Result<FeatureAddress, ErrorNumber> {
        match (&named.device, &source.device) {
            (Some(named), Some(sender)) if named != sender => Err(ErrorNumber::CommandRejected),
            (None, Some(sender)) => Ok(FeatureAddress {
                device: Some(sender.clone()),
                ..named.clone()
            }),
            _ => Ok(named.clone()),
        }
    }

    fn handle_call(&mut self, cmd: &Cmd, source: &FeatureAddress) -> ErrorNumber {
        match &cmd.data {
            Some(CmdData::NodeManagementBindingRequestCall(call)) => {
                let Some(request) = &call.binding_request else {
                    return ErrorNumber::General;
                };
                let (Some(client), Some(server)) =
                    (&request.client_address, &request.server_address)
                else {
                    return ErrorNumber::General;
                };
                let client = match Self::client_of_caller(client, source) {
                    Ok(client) => client,
                    Err(error) => return error,
                };
                let client = &client;
                if self.device.resolve(server).is_none() {
                    return ErrorNumber::DestinationUnknown;
                }
                match self.relations.add_binding(client, server) {
                    Ok(_) => {
                        self.events.push_back(SpineEvent::BindingGranted {
                            client: client.clone(),
                            server: server.clone(),
                        });
                        ErrorNumber::None
                    }
                    Err(e) => e,
                }
            }
            Some(CmdData::NodeManagementSubscriptionRequestCall(call)) => {
                let Some(request) = &call.subscription_request else {
                    return ErrorNumber::General;
                };
                let (Some(client), Some(server)) =
                    (&request.client_address, &request.server_address)
                else {
                    return ErrorNumber::General;
                };
                let client = match Self::client_of_caller(client, source) {
                    Ok(client) => client,
                    Err(error) => return error,
                };
                let client = &client;
                // NodeManagement itself is subscribable, so it is resolved separately
                // from the ordinary features.
                if !is_node_management(server) && self.device.resolve(server).is_none() {
                    return ErrorNumber::DestinationUnknown;
                }
                match self.relations.add_subscription(client, server) {
                    Ok(_) => {
                        self.events.push_back(SpineEvent::SubscriptionGranted {
                            client: client.clone(),
                            server: server.clone(),
                        });
                        ErrorNumber::None
                    }
                    Err(e) => e,
                }
            }
            Some(CmdData::NodeManagementBindingDeleteCall(call)) => {
                if let Some(delete) = &call.binding_delete
                    && let (Some(client), Some(server)) =
                        (&delete.client_address, &delete.server_address)
                {
                    let client = match Self::client_of_caller(client, source) {
                        Ok(client) => client,
                        Err(error) => return error,
                    };
                    self.relations.remove_binding(&client, server);
                    self.events.push_back(SpineEvent::BindingReleased {
                        client,
                        server: server.clone(),
                    });
                }
                ErrorNumber::None
            }
            Some(CmdData::NodeManagementSubscriptionDeleteCall(call)) => {
                if let Some(delete) = &call.subscription_delete
                    && let (Some(client), Some(server)) =
                        (&delete.client_address, &delete.server_address)
                {
                    let client = match Self::client_of_caller(client, source) {
                        Ok(client) => client,
                        Err(error) => return error,
                    };
                    self.relations.remove_subscription(&client, server);
                    self.events.push_back(SpineEvent::SubscriptionReleased {
                        client,
                        server: server.clone(),
                    });
                }
                ErrorNumber::None
            }
            _ => ErrorNumber::CommandNotSupported,
        }
    }

    fn handle_incoming_data(
        &mut self,
        cmd: &Cmd,
        source: &FeatureAddress,
        classifier: CmdClassifier,
    ) -> ErrorNumber {
        let Some(data) = cmd.data.clone() else {
            return ErrorNumber::General;
        };

        // The peer may have sent a *fragment*: a partial notification, a partial reply, or
        // a delete. What the application needs is the value that fragment is a fragment
        // of, and computing it once here is the same rule the write path follows — the
        // reference implementations pass the fragment alone, and every consumer then
        // reimplements the merge.
        let resolved = self.resolve_remote(source, &data, &WriteOps::of(cmd));

        // Discovery data updates what we know about the peer — from the merged document,
        // since §7.1.5's re-send may itself be partial.
        match &resolved {
            CmdData::NodeManagementDetailedDiscoveryData(discovery) => {
                // Filed under the address the *header* carries, which is the one SHIP
                // authenticated and the one every reply goes back to. The payload names
                // one too, and a peer whose payload disagreed with its header would
                // otherwise have its discovery filed under a device that never sent it.
                // The payload is the fallback for the one message §2.7 lets through
                // without a device part.
                let device = source.device.clone().or_else(|| {
                    discovery
                        .device_information
                        .as_ref()
                        .and_then(|d| d.description.as_ref())
                        .and_then(|d| d.device_address.as_ref())
                        .and_then(|a| a.device.clone())
                });
                if let Some(device) = device {
                    let peer = self.peer_entry(&device);
                    peer.remote.apply_detailed_discovery(discovery);
                    self.events
                        .push_back(SpineEvent::DiscoveryUpdated { device });
                }
            }
            CmdData::NodeManagementUseCaseData(use_cases) => {
                if let Some(device) = source.device.clone() {
                    let peer = self.peer_entry(&device);
                    peer.remote.apply_use_case_data(use_cases);
                    self.events
                        .push_back(SpineEvent::UseCasesUpdated { device });
                }
            }
            _ => {}
        }

        self.events.push_back(match classifier {
            CmdClassifier::Reply => SpineEvent::ReplyReceived {
                feature: source.clone(),
                data,
                resolved,
            },
            _ => SpineEvent::DataNotified {
                feature: source.clone(),
                data,
                resolved,
            },
        });
        ErrorNumber::None
    }

    /// Merges an incoming payload into what this peer has sent before, and answers with
    /// the result.
    ///
    /// The table is per peer and bounded by [`MAX_REMOTE_FUNCTIONS`], with the
    /// least-recently-updated entry evicted — the peer decides how many functions it
    /// sends, so this is memory it would otherwise allocate on this device.
    fn resolve_remote(
        &mut self,
        source: &FeatureAddress,
        data: &CmdData,
        ops: &WriteOps,
    ) -> CmdData {
        let Some(device) = source.device.clone() else {
            // Nothing to file it under. The opening discovery read is the one message
            // §2.7 lets through without a device part, and it is never partial.
            return data.clone();
        };
        let key = data.key();
        let peer = self.peer_entry(&device);

        let position = peer
            .data
            .iter()
            .position(|(feature, held)| same_feature(feature, source) && held.key() == key);

        let mut resolved = match position {
            Some(index) => peer.data.remove(index).1,
            None => match CmdData::empty(key) {
                Some(empty) => empty,
                // A function with no empty value cannot be merged into; the fragment is
                // all there is to report.
                None => return data.clone(),
            },
        };
        if ops.resolve(&mut resolved, data).is_err() {
            return data.clone();
        }
        // A peer that notifies a list can grow it one entry at a time, exactly as a peer
        // with a binding can grow a stored one, so the same bound applies.
        if resolved.entry_count() > super::device::MAX_LIST_ENTRIES {
            return data.clone();
        }

        if peer.data.len() >= MAX_REMOTE_FUNCTIONS {
            peer.data.remove(0);
        }
        peer.data.push((source.clone(), resolved.clone()));
        resolved
    }

    /// The merged state of one of a peer's functions, as far as this engine has seen it.
    ///
    /// This is what [`SpineEvent::DataNotified`] reports as `resolved`, kept so that an
    /// application can ask again rather than having to hold the last event.
    pub fn remote_data(&self, feature: &FeatureAddress, function: &Function) -> Option<&CmdData> {
        let device = feature.device.as_ref()?;
        self.peers
            .iter()
            .find(|p| p.remote.address.as_ref() == Some(device))?
            .data
            .iter()
            .find(|(held, data)| {
                same_feature(held, feature) && &Function::from(data.key()) == function
            })
            .map(|(_, data)| data)
    }

    fn handle_result(&mut self, cmd: &Cmd, reference: Option<MsgCounter>, from: &FeatureAddress) {
        let error = match &cmd.data {
            Some(CmdData::ResultData(ResultData { error_number, .. })) => {
                error_number.unwrap_or(ErrorNumber::None)
            }
            _ => ErrorNumber::General,
        };
        // `msgCounterReference` says which request this answers. A result that names
        // none cannot be attributed — and under §14a, an acknowledgement that cannot be
        // attributed to a limit is no evidence at all — so it is dropped.
        let Some(request) = self.resolve_pending(reference, from) else {
            return;
        };
        self.events.push_back(SpineEvent::ResultReceived {
            request: request.counter,
            error,
        });
    }

    fn send_result(
        &mut self,
        source: &FeatureAddress,
        destination: &FeatureAddress,
        reference: MsgCounter,
        error: ErrorNumber,
        now: Duration,
    ) {
        let counter = self.counters.next();
        let result_source = self.local_address(destination);
        self.send(
            Datagram {
                header: Some(self.header(
                    &result_source,
                    source,
                    counter,
                    CmdClassifier::Result,
                    false,
                    Some(reference),
                )),
                payload: Some(Payload {
                    cmd: Some(vec![Cmd::with_data(CmdData::ResultData(ResultData {
                        error_number: Some(error),
                        description: None,
                    }))]),
                }),
            },
            None,
            now,
        );
    }

    /// The NodeManagement functions, which are computed rather than stored.
    fn node_management_data(&self, function: &Function) -> Option<CmdData> {
        match function {
            Function::NodeManagementDetailedDiscoveryData => Some(
                CmdData::NodeManagementDetailedDiscoveryData(detailed_discovery(&self.device)),
            ),
            Function::NodeManagementUseCaseData => {
                let entries: Vec<_> = self
                    .use_cases
                    .iter()
                    .map(|(entity, feature, descriptor, scenarios)| {
                        (entity.clone(), *feature, *descriptor, scenarios.clone())
                    })
                    .collect();
                Some(CmdData::NodeManagementUseCaseData(use_case_data(
                    &self.device,
                    &entries,
                )))
            }
            // §7.3.2 and §7.4.2: the tables report the relations actually held, not a
            // stored copy that would answer an empty list.
            Function::NodeManagementBindingData => Some(CmdData::NodeManagementBindingData(
                NodeManagementBindingData {
                    binding_entry: Some(
                        self.relations
                            .bindings()
                            .iter()
                            .map(|relation| NodeManagementBindingDataBindingEntry {
                                binding_id: Some(BindingId(relation.id)),
                                client_address: Some(relation.client.clone()),
                                server_address: Some(relation.server.clone()),
                                ..Default::default()
                            })
                            .collect(),
                    ),
                },
            )),
            Function::NodeManagementSubscriptionData => Some(
                CmdData::NodeManagementSubscriptionData(NodeManagementSubscriptionData {
                    subscription_entry: Some(
                        self.relations
                            .subscriptions()
                            .iter()
                            .map(|relation| NodeManagementSubscriptionDataSubscriptionEntry {
                                subscription_id: Some(SubscriptionId(relation.id)),
                                client_address: Some(relation.client.clone()),
                                server_address: Some(relation.server.clone()),
                                ..Default::default()
                            })
                            .collect(),
                    ),
                }),
            ),
            _ => None,
        }
    }

    fn peer_entry(&mut self, device: &AddressDevice) -> &mut Peer {
        if let Some(index) = self
            .peers
            .iter()
            .position(|p| p.remote.address.as_ref() == Some(device))
        {
            return &mut self.peers[index];
        }
        if self.peers.len() >= MAX_PEERS {
            // Drop the one that has told us least: a peer that never completed discovery
            // is either gone or was never real, and keeping it would let whoever is
            // sending addresses evict the peers that matter. Everything else filed under
            // it goes too — its relations, the requests outstanding against it and the
            // writes it has waiting — because an entry that outlives its peer record is
            // an entry nothing can ever clean up.
            let index = self
                .peers
                .iter()
                .position(|p| p.remote.entities.is_empty() && p.remote.use_cases.is_empty())
                .unwrap_or(0);
            if let Some(evicted) = self.peers[index].remote.address.clone() {
                self.remove_peer(&evicted);
            } else {
                self.peers.remove(index);
            }
        }
        self.peers.push(Peer {
            remote: RemoteDevice {
                address: Some(device.clone()),
                ..RemoteDevice::default()
            },
            counters: MsgCounterTracker::new(),
            data: Vec::new(),
        });
        self.peers.last_mut().expect("just pushed")
    }

    /// Forgets a peer and everything that belonged to it.
    ///
    /// A write that peer had put to the application goes with it: the answer had nowhere
    /// to go the moment the connection did, and holding the slot would spend one of
    /// [`MAX_DEFERRED_WRITES`] on a peer that is gone. The application is told, so a
    /// decision it is still holding a token for is not simply lost — under §14a EnWG a
    /// limit that was never decided is worth a line in the record.
    pub fn remove_peer(&mut self, device: &AddressDevice) {
        self.relations.remove_device(device);
        self.peers
            .retain(|p| p.remote.address.as_ref() != Some(device));
        self.pending
            .retain(|p| p.destination.device.as_ref() != Some(device));

        let (gone, live): (Vec<_>, Vec<_>) = self
            .deferred
            .drain(..)
            .partition(|w| w.peer.device.as_ref() == Some(device));
        self.deferred = live;
        for write in gone {
            self.events.push_back(SpineEvent::WriteAbandoned {
                token: write.token,
                feature: write.feature,
                from: write.peer,
            });
        }
    }

    /// Grants a binding locally, without a request having arrived.
    ///
    /// For tests and for a device that persists its bindings across a restart, which
    /// §7.3 rule 4 recommends.
    pub fn insert_binding(&mut self, client: &FeatureAddress, server: &FeatureAddress) {
        let _ = self.relations.add_binding(client, server);
    }

    /// Records a subscription locally.
    pub fn insert_subscription(&mut self, client: &FeatureAddress, server: &FeatureAddress) {
        let _ = self.relations.add_subscription(client, server);
    }
}

/// What a function would hold if a write were applied to it.
///
/// A partial write says only what changed, so the state the peer is asking for is that
/// update merged into what is stored (SPINE IG §3.3) — an application deciding whether it
/// can follow the request has to see that, not the fragment that arrived.
fn resolve_write(stored: Option<&CmdData>, update: &CmdData, ops: &WriteOps) -> CmdData {
    let mut resolved = match stored {
        Some(stored) => stored.clone(),
        // Nothing stored yet: the merge starts from this function's empty value, so a
        // partial write adds what it names and a delete removes nothing.
        None => match CmdData::empty(update.key()) {
            Some(empty) => empty,
            None => return update.clone(),
        },
    };
    match ops.resolve(&mut resolved, update) {
        Ok(()) => resolved,
        // The payload names a different function than the stored data, or a filter this
        // node cannot serve. The write is refused either way, so report what arrived.
        Err(_) => update.clone(),
    }
}

/// Answers a partial read: the entries the filter's selectors pick, cut down to the
/// elements it keeps.
///
/// Two selectors in one filter address two different sets of entries, so the answer is
/// their union — merged by identifier, since both sets come from the same stored list
/// and an entry named by both must appear once.
fn restrict_for_read(data: &CmdData, filter: &Filter) -> Result<CmdData, RestrictError> {
    let elements = filter.elements.as_ref();
    let selectors = filter.selectors.as_deref().unwrap_or_default();

    // Detailed discovery holds three parallel lists rather than one, so its selectors do
    // not fit the generic shape and are applied by hand.
    if let CmdData::NodeManagementDetailedDiscoveryData(discovery) = data {
        let mut narrowed = discovery.clone();
        for selector in selectors {
            let FilterSelectors::NodeManagementDetailedDiscoveryDataSelectors(selector) = selector
            else {
                return Err(RestrictError::Mismatch);
            };
            narrowed = super::discovery::restrict_detailed_discovery(&narrowed, selector);
        }
        let narrowed = CmdData::NodeManagementDetailedDiscoveryData(narrowed);
        return match elements {
            None => Ok(narrowed),
            Some(elements) => narrowed.restrict(None, Some(elements)),
        };
    }

    match selectors {
        [] => data.restrict(None, elements),
        [only] => data.restrict(Some(only), elements),
        many => {
            let mut union: Option<CmdData> = None;
            for selector in many {
                let part = data.restrict(Some(selector), elements)?;
                match &mut union {
                    None => union = Some(part),
                    Some(acc) => acc.apply(part, true).map_err(|_| RestrictError::Mismatch)?,
                }
            }
            Ok(union.expect("`many` holds at least two selectors"))
        }
    }
}
