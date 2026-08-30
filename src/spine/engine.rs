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

use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;
use core::time::Duration;

use crate::model::{
    AddressDevice, Cmd, CmdClassifier, CmdData, Datagram, FeatureAddress, Filter, Function, Header,
    MsgCounter, NodeManagementBindingRequestCall, NodeManagementSubscriptionRequestCall, Payload,
    ResultData, SpecificationVersion,
};

use super::ack::{DEFAULT_MAX_RESPONSE_DELAY, ErrorNumber, owes_ack};
use super::address::{self, is_node_management, same_feature};
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
    WriteRequested {
        /// Identifies this write when resolving it.
        token: WriteToken,
        /// The feature that was written.
        feature: FeatureAddress,
        /// The peer that wrote it.
        from: FeatureAddress,
        /// What the peer sent.
        data: CmdData,
        /// Whether the write is partial.
        partial: bool,
        /// Whether the write is a delete.
        delete: bool,
    },
    /// A peer notified data this device is subscribed to.
    DataNotified {
        /// The feature the data came from.
        feature: FeatureAddress,
        /// The payload.
        data: CmdData,
    },
    /// A reply to one of this device's reads arrived.
    ReplyReceived {
        /// The feature that replied.
        feature: FeatureAddress,
        /// The payload.
        data: CmdData,
    },
    /// A request this device sent was acknowledged, positively or not.
    ResultReceived {
        /// The counter of the request this answers.
        request: MsgCounter,
        /// What the peer reported.
        error: ErrorNumber,
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

/// Whether a command produced an answer now, or handed the decision to the application.
enum CmdOutcome {
    Answer(ErrorNumber),
    Deferred,
}

/// Identifies a write whose acceptance the application has yet to decide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WriteToken(u64);

/// A write that has been reported and not yet resolved.
#[derive(Clone, Debug)]
struct DeferredWrite {
    token: WriteToken,
    feature: FeatureAddress,
    peer: FeatureAddress,
    data: CmdData,
    partial: bool,
    delete: bool,
    reference: MsgCounter,
    ack_request: bool,
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
}

/// The SPINE engine of one device.
#[derive(Debug)]
pub struct Engine {
    device: LocalDevice,
    relations: Relations,
    use_cases: Vec<(Vec<u32>, u32, &'static UseCaseDescriptor)>,
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

    /// Declares that an entity plays a use case, for use-case discovery.
    pub fn add_use_case(
        &mut self,
        entity: impl Into<Vec<u32>>,
        feature: u32,
        descriptor: &'static UseCaseDescriptor,
    ) {
        self.use_cases.push((entity.into(), feature, descriptor));
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
    pub fn poll_timeout(&self) -> Option<Duration> {
        self.pending.iter().map(|p| p.deadline).min()
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
    }

    // ---- sending ---------------------------------------------------------------

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
        let counter = self.counters.next();
        let mut cmd = Cmd::with_data(data);
        if partial {
            cmd = cmd.with_filter(Filter::partial());
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
        let Some(data) = self
            .device
            .resolve(feature)
            .and_then(|f| f.data(function))
            .cloned()
        else {
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
            self.pending.push(Pending {
                counter,
                destination,
                deadline: now + self.max_response_delay,
            });
        }
        self.outbox.push_back(datagram);
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

        for cmd in datagram.payload.iter().flat_map(|p| p.cmd.iter().flatten()) {
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
                self.handle_write(cmd, source, destination, counter, ack_request)
            }
            CmdClassifier::Call => CmdOutcome::Answer(self.handle_call(cmd, source)),
            CmdClassifier::Reply | CmdClassifier::Notify => {
                self.resolve_pending(reference);
                CmdOutcome::Answer(self.handle_incoming_data(cmd, source, classifier))
            }
            CmdClassifier::Result => {
                self.handle_result(cmd, reference);
                CmdOutcome::Answer(ErrorNumber::None)
            }
        }
    }

    /// Clears the request a response answers, so it does not later time out.
    ///
    /// `msgCounterReference` is what ties the two together (`TC_SPINE_DATA_004`); a
    /// response without one cannot be matched and leaves the request to expire.
    fn resolve_pending(&mut self, reference: Option<MsgCounter>) -> Option<Pending> {
        let reference = reference?;
        let index = self.pending.iter().position(|p| p.counter == reference)?;
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
        let Some(function) = cmd.function.clone() else {
            return ErrorNumber::General;
        };

        // A read carrying a filter asks for a partial reply. This node does not announce
        // partial reads, so serving one would be answering a question that was not
        // asked; §5.3.4 provides `errorNumber` 8 for exactly that.
        if cmd.filter.iter().flatten().count() > 0 {
            return ErrorNumber::RestrictedExchangeNotSupported;
        }

        let data = if is_node_management(destination) {
            self.node_management_data(&function)
        } else {
            let Some(feature) = self.device.resolve(destination) else {
                return ErrorNumber::DestinationUnknown;
            };
            match feature.function(&function) {
                None => return ErrorNumber::CommandNotSupported,
                Some(entry) if !entry.operations.read => {
                    return ErrorNumber::CommandNotSupported;
                }
                Some(entry) => entry.data.clone(),
            }
        };

        let Some(data) = data else {
            // The function exists but holds nothing yet. An empty payload of the right
            // function is a truthful answer; an error would suggest it is unsupported.
            return ErrorNumber::None;
        };

        let reply_counter = self.counters.next();
        self.send(
            Datagram {
                header: Some(self.header(
                    destination,
                    source,
                    reply_counter,
                    CmdClassifier::Reply,
                    false,
                    Some(counter),
                )),
                payload: Some(Payload {
                    cmd: Some(vec![Cmd::with_data(data)]),
                }),
            },
            None,
            now,
        );
        ErrorNumber::None
    }

    fn handle_write(
        &mut self,
        cmd: &Cmd,
        source: &FeatureAddress,
        destination: &FeatureAddress,
        counter: MsgCounter,
        ack_request: bool,
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
        let delete = cmd.is_delete();
        let partial = cmd.is_partial();

        // A write the feature would refuse regardless is refused now, rather than put to
        // the application only to be turned down.
        if let Err(e) = feature.check_write(&data, partial) {
            return CmdOutcome::Answer(e.error_number());
        }

        if feature.write_approval() == WriteApproval::Deferred {
            let token = WriteToken(self.next_token);
            self.next_token += 1;
            self.deferred.push(DeferredWrite {
                token,
                feature: destination.clone(),
                peer: source.clone(),
                data: data.clone(),
                partial,
                delete,
                reference: counter,
                ack_request,
            });
            self.events.push_back(SpineEvent::WriteRequested {
                token,
                feature: destination.clone(),
                from: source.clone(),
                data,
                partial,
                delete,
            });
            return CmdOutcome::Deferred;
        }

        let feature = self
            .device
            .resolve_mut(destination)
            .expect("resolved a moment ago");
        let outcome = if delete {
            feature.delete(&data)
        } else {
            feature.apply(data, partial)
        };

        match outcome {
            Ok(()) => {
                self.events.push_back(SpineEvent::DataWritten {
                    feature: destination.clone(),
                    from: source.clone(),
                    function,
                });
                CmdOutcome::Answer(ErrorNumber::None)
            }
            Err(e) => CmdOutcome::Answer(e.error_number()),
        }
    }

    /// Applies a deferred write and acknowledges it.
    ///
    /// ```ignore
    /// SpineEvent::WriteRequested { token, data, .. } => {
    ///     match use_case.decide(&data) {
    ///         Accept => engine.accept_write(token, now),
    ///         Reject => engine.reject_write(token, ErrorNumber::CommandRejected, now),
    ///     }
    /// }
    /// ```
    pub fn accept_write(&mut self, token: WriteToken, now: Duration) {
        let Some(write) = self.take_deferred(token) else {
            return;
        };
        let function = Function::from(write.data.key());
        let outcome = match self.device.resolve_mut(&write.feature) {
            None => Err(super::device::FeatureError::UnknownFunction),
            Some(feature) if write.delete => feature.delete(&write.data),
            Some(feature) => feature.apply(write.data, write.partial),
        };

        let error = match outcome {
            Ok(()) => {
                self.events.push_back(SpineEvent::DataWritten {
                    feature: write.feature.clone(),
                    from: write.peer.clone(),
                    function,
                });
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
    }

    /// Refuses a deferred write, storing nothing and reporting `error`.
    ///
    /// `ErrorNumber::CommandRejected` is the one a use case means when it declines a
    /// value it cannot follow — LPC's NACK.
    pub fn reject_write(&mut self, token: WriteToken, error: ErrorNumber, now: Duration) {
        let Some(write) = self.take_deferred(token) else {
            return;
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
                if self.device.resolve(server).is_none() {
                    return ErrorNumber::DestinationUnknown;
                }
                match self.relations.add_binding(client, server) {
                    Ok(_) => ErrorNumber::None,
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
                // NodeManagement itself is subscribable, so it is resolved separately
                // from the ordinary features.
                if !is_node_management(server) && self.device.resolve(server).is_none() {
                    return ErrorNumber::DestinationUnknown;
                }
                match self.relations.add_subscription(client, server) {
                    Ok(_) => ErrorNumber::None,
                    Err(e) => e,
                }
            }
            Some(CmdData::NodeManagementBindingDeleteCall(call)) => {
                if let Some(delete) = &call.binding_delete
                    && let (Some(client), Some(server)) =
                        (&delete.client_address, &delete.server_address)
                {
                    self.relations.remove_binding(client, server);
                }
                ErrorNumber::None
            }
            Some(CmdData::NodeManagementSubscriptionDeleteCall(call)) => {
                if let Some(delete) = &call.subscription_delete
                    && let (Some(client), Some(server)) =
                        (&delete.client_address, &delete.server_address)
                {
                    self.relations.remove_subscription(client, server);
                }
                ErrorNumber::None
            }
            _ => {
                let _ = source;
                ErrorNumber::CommandNotSupported
            }
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

        // Discovery data updates what we know about the peer.
        match &data {
            CmdData::NodeManagementDetailedDiscoveryData(discovery) => {
                let device = discovery
                    .device_information
                    .as_ref()
                    .and_then(|d| d.description.as_ref())
                    .and_then(|d| d.device_address.as_ref())
                    .and_then(|a| a.device.clone())
                    .or_else(|| source.device.clone());
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
            },
            _ => SpineEvent::DataNotified {
                feature: source.clone(),
                data,
            },
        });
        ErrorNumber::None
    }

    fn handle_result(&mut self, cmd: &Cmd, reference: Option<MsgCounter>) {
        let error = match &cmd.data {
            Some(CmdData::ResultData(ResultData { error_number, .. })) => {
                error_number.unwrap_or(ErrorNumber::None)
            }
            _ => ErrorNumber::General,
        };
        // `msgCounterReference` says which request this answers. A result that names
        // none cannot be attributed — and under §14a, an acknowledgement that cannot be
        // attributed to a limit is no evidence at all — so it is dropped.
        let Some(request) = self.resolve_pending(reference) else {
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
        self.send(
            Datagram {
                header: Some(self.header(
                    destination,
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
                    .map(|(entity, feature, descriptor)| (entity.clone(), *feature, *descriptor))
                    .collect();
                Some(CmdData::NodeManagementUseCaseData(use_case_data(
                    &self.device,
                    &entries,
                )))
            }
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
        self.peers.push(Peer {
            remote: RemoteDevice {
                address: Some(device.clone()),
                ..RemoteDevice::default()
            },
            counters: MsgCounterTracker::new(),
        });
        self.peers.last_mut().expect("just pushed")
    }

    /// Forgets a peer and everything that belonged to it.
    pub fn remove_peer(&mut self, device: &AddressDevice) {
        self.relations.remove_device(device);
        self.peers
            .retain(|p| p.remote.address.as_ref() != Some(device));
        self.pending
            .retain(|p| p.destination.device.as_ref() != Some(device));
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

/// True when `address` names the local device's own NodeManagement instance.
pub fn addresses_node_management(address: &FeatureAddress) -> bool {
    is_node_management(address)
}

/// True when two feature addresses are the same, ignoring an absent device part.
pub fn addresses_match(a: &FeatureAddress, b: &FeatureAddress) -> bool {
    same_feature(a, b)
}
