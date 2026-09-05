//! OHPCF, both actors, over real datagrams.
//!
//! The one exchange in this crate that asks an appliance to consume **more**. A heat pump
//! compressor announces that it could run; an energy manager watching the roof export
//! decides that it should; the compressor starts at the time it was given, is paused when
//! a cloud arrives, resumes, and finishes when the tank is full.
//!
//! What this file is really testing is the two places the shape of the use case makes it
//! easy to get wrong:
//!
//! * every control message is a **partial write** into a document the compressor owns, so
//!   an accepted write leaves the feature holding the manager's fragment until the
//!   compressor publishes its own document again — `Flexibility::notify` is that step, and
//!   the test reads the feature back to prove it happened;
//! * a state change the compressor makes **by itself** — starting at the scheduled time,
//!   completing — reaches the manager only through the subscription §3.3.4 asks for.

use core::time::Duration;

use eebus::model::{CmdData, DeviceType, EntityType, Function, PowerSequenceState};
use eebus::spine::{Engine, ErrorNumber, LocalDevice, LocalEntity, SpineEvent, node_management};
use eebus::usecases::limitation;
use eebus::usecases::ohpcf::{
    self, CompressorOffer, Durations, Flexibility, Interrupt, Refused, Request,
};

/// A heat pump with a compressor inside it, and the energy manager driving it.
struct Link {
    pump: Engine,
    manager: Engine,
    /// The compressor's own state, which is what the feature publishes.
    compressor: Flexibility,
    flexibility: eebus::model::FeatureAddress,
    client: eebus::model::FeatureAddress,
    /// What the manager has learned, updated from every reply and notification.
    offer: Option<CompressorOffer>,
    /// Every write the compressor was asked to make, and what it decided.
    decided: Vec<Result<Request, Refused>>,
    /// Every acknowledgement the manager got back.
    answers: Vec<ErrorNumber>,
    now: Duration,
}

impl Link {
    fn new() -> Self {
        let now = Duration::ZERO;

        // §3.2.2.1: the Compressor is a sub-entity of the HeatPumpAppliance.
        let mut device =
            LocalDevice::new("i:46925", "HeatPump-1", DeviceType::HeatGenerationSystem).unwrap();
        device
            .add_entity(LocalEntity::new([1], EntityType::HeatPumpAppliance))
            .unwrap();
        device
            .add_entity(
                LocalEntity::new([1, 1], EntityType::Compressor)
                    .with_feature(ohpcf::flexibility_feature(1)),
            )
            .unwrap();
        let flexibility = device.address_of(&[1, 1], 1);
        let mut pump = Engine::new(device);
        pump.add_use_case([1, 1], 1, &ohpcf::COMPRESSOR);

        let compressor = Flexibility::offered(2_400.0)
            .interruptible(Interrupt::Either)
            .lasting(
                Durations::new()
                    .at_least(Duration::from_secs(20 * 60))
                    .resting(Duration::from_secs(10 * 60)),
            );
        compressor.publish(&mut pump, &flexibility);

        let mut manager_device =
            LocalDevice::new("i:46925", "CEM-1", DeviceType::EnergyManagementSystem).unwrap();
        manager_device
            .add_entity(
                LocalEntity::new([1], EntityType::CEM).with_feature(limitation::client_feature(1)),
            )
            .unwrap();
        let client = manager_device.address_of(&[1], 1);
        let mut manager = Engine::new(manager_device);
        manager.add_use_case([1], 1, &ohpcf::CEM);

        Self {
            pump,
            manager,
            compressor,
            flexibility,
            client,
            offer: None,
            decided: Vec::new(),
            answers: Vec::new(),
            now,
        }
    }

    /// Discovery, the binding a write needs, and the subscription scenario 1 runs on.
    fn commission(&mut self) {
        let theirs = node_management(self.pump.device().address());
        let ours = node_management(self.manager.device().address());
        for function in [
            Function::NodeManagementDetailedDiscoveryData,
            Function::NodeManagementUseCaseData,
        ] {
            self.manager.read(&theirs, &ours, function, self.now);
        }
        self.settle();

        let device = self.pump.device().address().clone();
        let remote = self.manager.peer(&device).expect("the heat pump");
        let peer = ohpcf::locate(remote).expect("a compressor");
        assert_eq!(
            peer.flexibility, self.flexibility,
            "the feature is on the compressor, not on the appliance around it"
        );

        // The binding scenario 2 needs, the subscription scenario 1 runs on, and the
        // initial read — one call, in the order §3.4.1.1 and §3.4.2 put them.
        let pending = peer.follow(&mut self.manager, &self.client.clone(), self.now);
        self.settle();
        assert!(
            self.answers.contains(&ErrorNumber::None),
            "the binding and the subscription were granted: {:?}",
            self.answers
        );
        let _ = pending;
    }

    /// The manager writes, partially, as Table 10 requires.
    fn write(&mut self, data: CmdData) -> eebus::model::MsgCounter {
        let counter = self.manager.write(
            &self.flexibility.clone(),
            &self.client,
            data,
            true,
            self.now,
        );
        self.settle();
        counter
    }

    fn settle(&mut self) {
        for _ in 0..64 {
            let mut moved = false;
            while let Some(datagram) = self.manager.poll_transmit() {
                self.pump.handle_datagram(&datagram, self.now);
                moved = true;
            }
            while let Some(datagram) = self.pump.poll_transmit() {
                self.manager.handle_datagram(&datagram, self.now);
                moved = true;
            }

            let events: Vec<SpineEvent> = core::iter::from_fn(|| self.pump.poll_event()).collect();
            for event in events {
                moved = true;
                let SpineEvent::WriteRequested(write) = event else {
                    continue;
                };
                // `resolved`, not `data`: every write here is partial.
                let decision = self.compressor.apply(&write.resolved);
                match &decision {
                    Ok(_) => {
                        // `accept_write_with`, not `accept_write`: the document the write
                        // *results in* is the compressor's, and storing the manager's
                        // fragment instead would notify every subscriber of a compressor
                        // that had just withdrawn its power value.
                        self.pump
                            .accept_write_with(write.token, self.compressor.data(), self.now)
                            .expect("the feature can store it");
                    }
                    Err(refused) => {
                        self.pump
                            .reject_write(write.token, refused.error_number(), self.now);
                    }
                }
                self.decided.push(decision);
            }

            let events: Vec<SpineEvent> =
                core::iter::from_fn(|| self.manager.poll_event()).collect();
            for event in &events {
                moved = true;
                match event {
                    SpineEvent::ReplyReceived { resolved, .. }
                    | SpineEvent::DataNotified { resolved, .. } => {
                        if let Some(offer) = CompressorOffer::read(resolved) {
                            self.offer = Some(offer);
                        }
                    }
                    SpineEvent::ResultReceived { error, .. } => self.answers.push(*error),
                    _ => {}
                }
            }
            if !moved {
                return;
            }
        }
        panic!("the exchange did not settle");
    }

    /// What the compressor's feature actually holds, as a peer reading it would see.
    fn published(&self) -> CompressorOffer {
        let data = self
            .pump
            .device()
            .resolve(&self.flexibility)
            .and_then(|feature| feature.data(&Function::SmartEnergyManagementPsData))
            .cloned()
            .expect("the feature holds a document");
        CompressorOffer::read(&data).expect("a readable document")
    }

    fn offer(&self) -> &CompressorOffer {
        self.offer.as_ref().expect("the manager has read it")
    }
}

/// The whole of the use case: an offer, a schedule, a pause, a resume, a completion.
#[test]
fn the_manager_takes_up_the_offer_and_drives_it_to_the_end() {
    let mut link = Link::new();
    link.commission();

    // Phase A. The compressor is offering, and says how it may be interrupted and what it
    // needs of any plan.
    let offer = link.offer();
    assert!(offer.is_available(), "inactive and unscheduled: on offer");
    assert_eq!(offer.power_watts, Some(2_400.0));
    assert_eq!(offer.interrupt(), Some(Interrupt::Either));
    assert_eq!(
        offer.active_duration_min,
        Some(Duration::from_secs(20 * 60)),
        "[OHPCF-008]: a plan that stops it sooner is one it cannot follow"
    );
    assert_eq!(offer.pause_duration_min, Some(Duration::from_secs(10 * 60)));
    let sequence = offer.sequence;

    // Phase B. The roof is exporting; run it now.
    link.write(ohpcf::activate(sequence, "2026-09-04T13:00:00Z"));
    assert_eq!(link.decided.len(), 1);
    assert_eq!(
        link.decided[0],
        Ok(Request::Schedule {
            start_time: "2026-09-04T13:00:00Z".into()
        })
    );
    assert_eq!(link.offer().state, PowerSequenceState::Scheduled);
    assert_eq!(
        link.offer().start_time.as_deref(),
        Some("2026-09-04T13:00:00Z")
    );
    // And the feature holds the compressor's whole document, not the fragment written
    // into it: the power value and the interrupt flags are still there.
    let published = link.published();
    assert_eq!(published.power_watts, Some(2_400.0));
    assert_eq!(published.interrupt(), Some(Interrupt::Either));

    // [OHPCF-012/4]: the compressor starts by itself, and the subscription carries it.
    link.compressor.start();
    let feature = link.flexibility.clone();
    let now = link.now;
    link.compressor.notify(&mut link.pump, &feature, now);
    link.settle();
    assert_eq!(link.offer().state, PowerSequenceState::Running);
    assert!(!link.offer().is_available() && link.offer().is_active());

    // A cloud. Pause, then resume.
    link.write(ohpcf::pause(sequence));
    assert_eq!(link.decided.last(), Some(&Ok(Request::Pause)));
    assert_eq!(link.offer().state, PowerSequenceState::Paused);

    link.write(ohpcf::resume(sequence));
    assert_eq!(link.decided.last(), Some(&Ok(Request::Resume)));
    assert_eq!(link.offer().state, PowerSequenceState::Running);

    // The tank fills. [OHPCF-006/3]: a completion is announced explicitly.
    link.compressor.complete();
    let feature = link.flexibility.clone();
    let now = link.now;
    link.compressor.notify(&mut link.pump, &feature, now);
    link.settle();
    assert!(link.offer().has_ended());
    assert_eq!(link.offer().state, PowerSequenceState::Completed);
}

/// Every notification a subscriber sees is the compressor's document, never the fragment.
///
/// §7.4.1: "a subscribed client is as well notified if it caused the change by a 'write'
/// operation". So the CEM's own partial write comes back to it as a notification — and if
/// what comes back is the *fragment* the engine stored, the CEM reads a compressor that has
/// just withdrawn its power value and both of its interrupt options.
#[test]
fn a_subscriber_never_sees_the_fragment_it_wrote() {
    let mut link = Link::new();
    link.commission();
    let sequence = link.offer().sequence;

    let mut seen: Vec<Option<f64>> = Vec::new();
    let write = ohpcf::activate(sequence, "PT0S");
    link.manager.write(
        &link.flexibility.clone(),
        &link.client.clone(),
        write,
        true,
        link.now,
    );
    for _ in 0..64 {
        let mut moved = false;
        while let Some(datagram) = link.manager.poll_transmit() {
            moved = true;
            link.pump.handle_datagram(&datagram, link.now);
        }
        while let Some(datagram) = link.pump.poll_transmit() {
            moved = true;
            link.manager.handle_datagram(&datagram, link.now);
        }
        let events: Vec<SpineEvent> = core::iter::from_fn(|| link.pump.poll_event()).collect();
        for event in events {
            moved = true;
            if let SpineEvent::WriteRequested(w) = event {
                let decision = link.compressor.apply(&w.resolved);
                if decision.is_ok() {
                    link.pump
                        .accept_write_with(w.token, link.compressor.data(), link.now)
                        .unwrap();
                }
                link.decided.push(decision);
            }
        }
        while let Some(event) = link.manager.poll_event() {
            moved = true;
            if let SpineEvent::DataNotified { resolved, .. } = event
                && let Some(offer) = CompressorOffer::read(&resolved)
            {
                seen.push(offer.power_watts);
            }
        }
        if !moved {
            break;
        }
    }

    assert!(!seen.is_empty(), "the subscription delivered something");
    assert!(
        seen.iter().all(|power| *power == Some(2_400.0)),
        "one of these notifications carried the manager's own fragment rather than the \
         compressor's document: {seen:?}"
    );
}

/// A request the compressor will not honour comes back as a rejection, not a silent no-op.
///
/// The shape LPC's Controllable System established: the decision comes before the
/// acknowledgement, so the manager learns that its plan did not happen rather than
/// believing it did.
#[test]
fn a_pause_on_a_process_that_is_not_running_is_rejected_on_the_wire() {
    let mut link = Link::new();
    link.commission();
    let sequence = link.offer().sequence;

    link.write(ohpcf::activate(sequence, "PT0S"));
    assert_eq!(link.offer().state, PowerSequenceState::Scheduled);

    link.write(ohpcf::pause(sequence));
    assert_eq!(link.decided.last(), Some(&Err(Refused::NotRunning)));

    assert_eq!(
        link.answers.last(),
        Some(&ErrorNumber::CommandRejected),
        "the manager was told, with an errorNumber: {:?}",
        link.answers
    );
    assert_eq!(
        link.offer().state,
        PowerSequenceState::Scheduled,
        "and nothing moved"
    );
}

/// A stop the compressor never offered is refused, and the process carries on.
#[test]
fn an_interrupt_that_was_never_offered_is_refused_over_the_wire() {
    let mut link = Link::new();
    // This compressor may only be paused, which [OHPCF-011/7] permits.
    link.compressor = Flexibility::offered(2_400.0).interruptible(Interrupt::Pausable);
    let feature = link.flexibility.clone();
    link.compressor.publish(&mut link.pump, &feature);
    link.commission();

    let offer = link.offer();
    assert!(offer.is_pausable && !offer.is_stoppable);
    let sequence = offer.sequence;

    link.write(ohpcf::activate(sequence, "PT0S"));
    link.compressor.start();
    let now = link.now;
    link.compressor.notify(&mut link.pump, &feature, now);
    link.settle();

    link.write(ohpcf::stop(sequence));
    assert_eq!(link.decided.last(), Some(&Err(Refused::NotStoppable)));
    assert_eq!(
        link.offer().state,
        PowerSequenceState::Running,
        "the compressor kept running, which is what it said it would do"
    );

    // The interrupt it *did* offer works.
    link.write(ohpcf::pause(sequence));
    assert_eq!(link.decided.last(), Some(&Ok(Request::Pause)));
}

/// Phase D: the compressor withdraws, and the manager sees an absence rather than a stale
/// offer.
#[test]
fn a_withdrawn_offer_is_reported_as_an_absence() {
    let mut link = Link::new();
    link.commission();
    assert!(link.offer().is_available());

    // [OHPCF-006/2]: an abortion may be announced as the absence of a process.
    link.compressor.withdraw();
    let feature = link.flexibility.clone();
    let now = link.now;
    link.compressor.notify(&mut link.pump, &feature, now);

    // The notification carries no alternatives, so nothing updates `offer` — which is the
    // point: a reader that took silence for "unchanged" would go on believing the offer
    // stands. `is_absent` is how the manager sees it.
    let mut absent = false;
    for _ in 0..16 {
        while let Some(datagram) = link.pump.poll_transmit() {
            link.manager.handle_datagram(&datagram, link.now);
        }
        while let Some(datagram) = link.manager.poll_transmit() {
            link.pump.handle_datagram(&datagram, link.now);
        }
        while let Some(event) = link.manager.poll_event() {
            if let SpineEvent::DataNotified { resolved, .. } = event {
                absent |= ohpcf::is_absent(&resolved);
            }
        }
        while link.pump.poll_event().is_some() {}
    }
    assert!(absent, "[OHPCF-003]: there is no process");

    // And a write into the absence is refused rather than quietly starting something.
    let sequence = link.offer().sequence;
    link.write(ohpcf::activate(sequence, "PT0S"));
    assert_eq!(link.decided.last(), Some(&Err(Refused::NothingOffered)));
}

/// [TC_SPINE_BIND_002]: a CEM that subscribed and did not bind can watch the offer and
/// cannot take it up.
///
/// The failure a controller hits first, and the one nothing on the wire warns about: the
/// offer arrives, `CompressorOffer::read` says it is available, the write goes out
/// well-formed against the sequence the compressor itself named — and comes back
/// `BindingRequired`, before the compressor's own state machine has seen it. Every other
/// EEBUS use case a monitoring-shaped controller has met needs no binding, so it is the one
/// step that gets left out.
///
/// `CompressorPeer::follow` is the fix, and this test is both halves: without it, refused;
/// with it, the same write is accepted.
#[test]
fn a_write_without_a_binding_is_refused_and_follow_is_what_grants_it() {
    let mut link = Link::new();

    // Discovery and a subscription — scenario 1, and nothing more. §3.4.1.1 even says
    // binding "SHOULD NOT be used for this Scenario", which is exactly how a CEM ends up
    // here.
    let theirs = node_management(link.pump.device().address());
    let ours = node_management(link.manager.device().address());
    for function in [
        Function::NodeManagementDetailedDiscoveryData,
        Function::NodeManagementUseCaseData,
    ] {
        link.manager.read(&theirs, &ours, function, link.now);
    }
    link.settle();

    let device = link.pump.device().address().clone();
    let peer =
        ohpcf::locate(link.manager.peer(&device).expect("the heat pump")).expect("a compressor");
    link.manager
        .request_subscription(&link.client, &peer.flexibility.clone(), link.now);
    link.manager.read(
        &peer.flexibility.clone(),
        &link.client,
        Function::SmartEnergyManagementPsData,
        link.now,
    );
    link.settle();

    let offer = link.offer.clone().expect("the offer arrived");
    assert!(offer.is_available(), "and it is on the table");

    link.answers.clear();
    link.write(ohpcf::activate(offer.sequence, "PT0S"));
    assert_eq!(
        link.answers,
        [ErrorNumber::BindingRequired],
        "the offer was readable and the write was not"
    );
    assert!(
        link.decided.is_empty(),
        "and the compressor's own state machine never saw it: the binding check is \
         before the payload"
    );
    assert_eq!(
        link.compressor.state(),
        PowerSequenceState::Inactive,
        "so nothing was scheduled"
    );

    // The one call that was missing.
    link.answers.clear();
    peer.follow(&mut link.manager, &link.client.clone(), link.now);
    link.settle();

    link.answers.clear();
    link.write(ohpcf::activate(offer.sequence, "PT0S"));
    assert_eq!(
        link.answers,
        [ErrorNumber::None],
        "the same write, now that the compressor has a binding partner"
    );
    assert_eq!(link.compressor.state(), PowerSequenceState::Scheduled);
}
