//! The Monitoring Appliance: the side that reads.
//!
//! A `measurementListData` notification carries identifiers and numbers and nothing
//! else — `{"measurementId": 3, "value": {"number": 2300}}`. What that means is in two
//! other functions, read once at commissioning. [`Readings`] holds those descriptions and
//! resolves each notification against them, so an application sees a quantity and a
//! phase rather than an integer it has to remember the meaning of.

use alloc::vec::Vec;

use crate::model::{
    AbsoluteOrRelativeTime, CmdData, ElectricalConnectionPhaseName, MeasurementId,
    MeasurementValueState, ScopeType,
};

use super::measurand::{Measurand, Quantity};

/// One resolved value from a Monitored Unit.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    /// What was measured, and on which phases.
    pub measurand: Measurand,
    /// The value, in the measurand's unit. Absent when the unit reported an error.
    pub value: Option<f64>,
    /// Whether the value can be trusted ([MPC-003]).
    pub state: ReadingState,
    /// When the unit says it took the reading, exactly as it sent it.
    ///
    /// Permitted by every measurement use case here and required by none ([MPC-002],
    /// [MDT-002], [MRT-002], [MOT-002]), so [`None`] far more often than not — and what a
    /// missing one means is the reader's decision.
    ///
    /// The obvious substitute is wrong: an appliance subscribes rather than polls, so the
    /// arrival time of a notification is the age of the *change*, and a room holding its
    /// temperature sends nothing at all. Ageing readings by arrival discards the ones most
    /// likely to still be true, and cannot tell that room from a sensor that died an hour
    /// ago. Only the peer knows when it measured.
    ///
    /// Kept as [`AbsoluteOrRelativeTime`] rather than a parsed instant because the element
    /// is exactly that — a time *or* an ISO 8601 duration — and which one arrived is part
    /// of what the peer said.
    pub timestamp: Option<AbsoluteOrRelativeTime>,
}

/// What a measurement's `valueState` says about its value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadingState {
    /// A value within its constraints. `valueState` absent or `normal`.
    Normal,
    /// A value outside the constraints the unit published.
    ///
    /// **To be ignored, exactly as [`Error`](Self::Error) is.** MPC §2.5.2 and MDT §2.5.1
    /// list the three states together and say the same thing about both of the abnormal
    /// ones: "out of range: value is out of range and SHALL be ignored by the Monitoring
    /// Appliance". It is tempting to read `outOfRange` as "high but real" — a meter
    /// reporting 14 kW against an 11 kW constraint looks like a number worth having — and
    /// the specification does not permit it.
    OutOfRange,
    /// The unit could not measure. Any value sent with this is to be ignored.
    Error,
}

impl Reading {
    /// When the unit says it took the reading, where it said.
    ///
    /// See [`timestamp`](Self::timestamp) for why an absent one is not an error and not a
    /// reason to substitute the arrival time.
    pub fn taken_at(&self) -> Option<&AbsoluteOrRelativeTime> {
        self.timestamp.as_ref()
    }

    /// The value, if the unit did not flag it.
    ///
    /// [MPC-003], [MDT-005]: where `valueState` is `error` **or** `outOfRange` the content
    /// of `value` SHALL be ignored, so a caller that wants a number it can act on asks for
    /// it here rather than reading the field. [`value`](Self::value) is still there for a
    /// caller that wants to show the flagged number to a person — which is a different
    /// thing from acting on it.
    pub fn usable(&self) -> Option<f64> {
        match self.state {
            ReadingState::Error | ReadingState::OutOfRange => None,
            ReadingState::Normal => self.value,
        }
    }
}

/// What one `measurementId` is known to mean.
///
/// The quantity comes from either description; the phases come only from the parameter
/// descriptions, so they stay unknown until those are read.
#[derive(Clone, Debug, PartialEq)]
struct Known {
    id: MeasurementId,
    quantity: Quantity,
    /// The phases, once they are known. `Some(None)` is a direct-current measurement,
    /// which *has* no phases; the outer `None` is one whose parameter description has not
    /// arrived yet. A client that treated the two the same would report a per-phase
    /// current as soon as its measurement description arrived, before knowing which phase.
    phases: Option<Option<ElectricalConnectionPhaseName>>,
}

/// What a Monitoring Appliance has learned about one Monitored Unit.
///
/// Feed it the two description functions once, then every `measurementListData` that
/// arrives; ask it for the values you care about by measurand.
#[derive(Clone, Debug, Default)]
pub struct Readings {
    known: Vec<Known>,
    values: Vec<(MeasurementId, Reading)>,
}

impl Readings {
    /// Nothing known yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Learns what each `measurementId` means, from either description function.
    ///
    /// Both carry a `scopeType`, which gives the quantity. Only the parameter
    /// descriptions carry `acMeasuredPhases`, so a measurement stays without phases until
    /// those are read — which is why MPC §3.2.2.2 makes them mandatory. The two may
    /// arrive in either order.
    pub fn describe(&mut self, data: &CmdData) -> bool {
        match data {
            CmdData::MeasurementDescriptionListData(list) => {
                for entry in list.measurement_description_data.iter().flatten() {
                    let (Some(id), Some(scope)) = (entry.measurement_id, entry.scope_type.as_ref())
                    else {
                        continue;
                    };
                    let Some(quantity) = quantity_of(scope) else {
                        continue;
                    };
                    self.learn(id, quantity, phasing_of(scope));
                }
                true
            }
            CmdData::ElectricalConnectionParameterDescriptionListData(list) => {
                for entry in list
                    .electrical_connection_parameter_description_data
                    .iter()
                    .flatten()
                {
                    let (Some(id), Some(scope)) = (entry.measurement_id, entry.scope_type.as_ref())
                    else {
                        continue;
                    };
                    let Some(quantity) = quantity_of(scope) else {
                        continue;
                    };
                    // The parameter description is where `acMeasuredPhases` lives, so its
                    // absence here is authoritative: a direct-current measurement.
                    self.learn(id, quantity, Some(entry.ac_measured_phases.clone()));
                }
                true
            }
            _ => false,
        }
    }

    /// Records what a description said, without unlearning phases already known.
    fn learn(
        &mut self,
        id: MeasurementId,
        quantity: Quantity,
        phases: Option<Option<ElectricalConnectionPhaseName>>,
    ) {
        match self.known.iter_mut().find(|k| k.id == id) {
            Some(known) => {
                known.quantity = quantity;
                if phases.is_some() {
                    known.phases = phases;
                }
            }
            None => self.known.push(Known {
                id,
                quantity,
                phases,
            }),
        }
    }

    /// Applies a `measurementListData`, returning what it resolved to.
    ///
    /// Values whose `measurementId` has no description, or whose phases are not yet
    /// known, are dropped: a number without a meaning is worse than no number, because it
    /// invites a guess.
    ///
    /// **Give it the resolved state, not a fragment.** A notification may be partial, and
    /// an omitted element then means *unchanged* (SPINE IG §3.3) — a measurement whose
    /// `scale` is left out keeps the one already sent, so reading the fragment alone is off
    /// by a power of ten. The same rule is what makes an absent `timestamp` here mean "this
    /// peer sends none" rather than "this update left it out".
    pub fn apply(&mut self, data: &CmdData) -> Vec<Reading> {
        let CmdData::MeasurementListData(list) = data else {
            return Vec::new();
        };
        let mut applied = Vec::new();
        for entry in list.measurement_data.iter().flatten() {
            let Some(id) = entry.measurement_id else {
                continue;
            };
            let Some(measurand) = self.measurand_of(id) else {
                continue;
            };
            let state = match entry.value_state.as_ref() {
                Some(MeasurementValueState::OutOfRange) => ReadingState::OutOfRange,
                Some(MeasurementValueState::Error) => ReadingState::Error,
                _ => ReadingState::Normal,
            };
            let reading = Reading {
                measurand,
                value: entry.value.as_ref().and_then(|v| v.to_f64()),
                state,
                // Only what the peer sent. Give this the *resolved* state and an absent
                // element is a peer that sent no timestamp: a partial update that omits it
                // leaves the stored one in place, which is §7.1.5's rule and not this
                // reader's to second-guess.
                timestamp: entry.timestamp.clone(),
            };
            match self.values.iter_mut().find(|(known, _)| *known == id) {
                Some((_, stored)) => *stored = reading.clone(),
                None => self.values.push((id, reading.clone())),
            }
            applied.push(reading);
        }
        applied
    }

    /// The measurements described so far, whether or not a value has arrived for them.
    ///
    /// A measurement counts as described once its quantity *and* its phases are known —
    /// which takes both description functions, since only the parameter descriptions
    /// carry `acMeasuredPhases`.
    pub fn described(&self) -> impl Iterator<Item = (MeasurementId, Measurand)> + '_ {
        self.known.iter().filter_map(|known| {
            Some((
                known.id,
                Measurand {
                    quantity: known.quantity,
                    phases: known.phases.clone()?,
                },
            ))
        })
    }

    /// What a `measurementId` means, once both its descriptions have been read.
    pub fn measurand_of(&self, id: MeasurementId) -> Option<Measurand> {
        let known = self.known.iter().find(|k| k.id == id)?;
        Some(Measurand {
            quantity: known.quantity,
            phases: known.phases.clone()?,
        })
    }

    /// The latest reading of a measurand.
    pub fn get(&self, measurand: &Measurand) -> Option<Reading> {
        self.latest(measurand).cloned()
    }

    /// The same, without copying it.
    pub fn latest(&self, measurand: &Measurand) -> Option<&Reading> {
        self.values
            .iter()
            .find(|(_, reading)| reading.measurand == *measurand)
            .map(|(_, reading)| reading)
    }

    /// The latest usable value of a measurand, in its unit.
    pub fn value(&self, measurand: &Measurand) -> Option<f64> {
        self.latest(measurand).and_then(Reading::usable)
    }

    /// The same value, with when the unit says it was taken.
    ///
    /// What a consumer that ages its inputs needs — see [`Reading::timestamp`] for why the
    /// arrival time will not do. [`None`] whenever the peer sent none, which is the common
    /// case, and what that means is the caller's decision.
    ///
    /// ```
    /// use eebus::model::UnitOfMeasurement;
    /// use eebus::usecases::hvac::mrt;
    /// use eebus::usecases::monitoring::Readings;
    ///
    /// let mut readings = Readings::new();
    /// readings.describe(&mrt::temperature_description());
    /// readings.apply(&mrt::temperature_at(21.5, "2026-09-05T08:15:00Z".into()));
    ///
    /// let (degrees, taken_at) = readings.read_at(&mrt::MEASURAND).expect("a reading");
    /// assert_eq!(degrees, 21.5);
    /// assert_eq!(taken_at.map(|t| t.as_str()), Some("2026-09-05T08:15:00Z"));
    /// # let _ = UnitOfMeasurement::DegC;
    /// ```
    pub fn read_at(&self, measurand: &Measurand) -> Option<(f64, Option<&AbsoluteOrRelativeTime>)> {
        let reading = self.latest(measurand)?;
        Some((reading.usable()?, reading.taken_at()))
    }

    /// The latest total active power, in watts — scenario 1 of MPC, scenario 2 of MGCP.
    ///
    /// Positive means the unit is drawing power and negative that it is feeding in, per
    /// the load convention both use cases apply ([MPC-001]).
    pub fn total_power(&self) -> Option<f64> {
        self.value(&Measurand::total_power())
    }

    /// Every reading held, in the order the measurements were first described.
    pub fn all(&self) -> impl Iterator<Item = &Reading> {
        self.values.iter().map(|(_, reading)| reading)
    }
}

/// Where one Monitored Unit's measurements live.
///
/// Every feature is optional, and that is the specification's shape rather than
/// looseness. MGCP Table 1 marks each scenario `M`, `R` or `O` **per actor**, and
/// §3.2.2.2.1 then says the feature table's presence indications are "meant relative to
/// the ones of the according Scenario stated in Table 1" — a feature is required only
/// where the scenario that uses it is. Scenario 1 needs `DeviceConfiguration` and nothing
/// else; scenarios 2 to 7 need `Measurement` and `ElectricalConnection`. A peer that
/// serves one group and not the other is describable here, which is what
/// [`locate`] returns and what a DHW circuit located through
/// [`hvac::mdt::locate`](crate::usecases::hvac::mdt::locate) — a tank, with no electrical
/// connection at all — needs.
#[derive(Clone, Debug, PartialEq)]
pub struct MonitoredUnitPeer {
    /// The peer's device address.
    pub device: crate::model::AddressDevice,
    /// Its `ElectricalConnection` feature, which says which measurement covers which
    /// phases.
    ///
    /// [`None`] for a peer that serves no phase-specific measurement: a DHW circuit
    /// ([`hvac::mdt`](crate::usecases::hvac::mdt)), or a Grid Connection Point that serves
    /// scenario 1 alone. Without it a phase-specific AC measurement stays undescribed —
    /// `acMeasuredPhases` lives in the parameter descriptions and nowhere else — which is
    /// why [`Readings`] drops such a value rather than guessing at its phase.
    pub electrical_connection: Option<crate::model::FeatureAddress>,
    /// Its `Measurement` feature, which says what each measurement is and what it reads.
    ///
    /// [`None`] for a Grid Connection Point that serves MGCP scenario 1 alone. Nothing
    /// else in this crate produces such a peer, and a real one is unusual: Table 1 marks
    /// scenarios 2, 3 and 4 mandatory *for the Grid Connection Point*, so a conformant one
    /// always has this. It is optional because an appliance that cannot read a device at
    /// all is worse off than one that reads the half the device does serve.
    pub measurement: Option<crate::model::FeatureAddress>,
    /// Its `DeviceConfiguration` feature, where MGCP scenario 1 keeps the PV feed-in
    /// curtailment factor.
    ///
    /// [`None`] for a peer that serves none — every MPC Monitored Unit, and a Grid
    /// Connection Point that does not implement scenario 1, which MGCP Table 1 leaves
    /// optional. Scenario 1 is the one row of MGCP that is not a measurement.
    pub curtailment: Option<crate::model::FeatureAddress>,
}

pub use crate::usecases::UnitId;

impl MonitoredUnitPeer {
    /// Which unit this is: its device and the entity its features live on.
    pub fn id(&self) -> UnitId {
        UnitId {
            device: self.device.clone(),
            entity: self.entity_path(),
        }
    }

    /// Whether this is the unit `id` names, without building one to compare against.
    ///
    /// The lookup every accessor runs, and it is on the hot path of `handle_event`.
    pub fn is_unit(&self, id: &UnitId) -> bool {
        if self.device != id.device {
            return false;
        }
        match self.entity_feature() {
            Some(feature) => feature
                .entity
                .iter()
                .flatten()
                .map(|e| e.get())
                .eq(id.entity.iter().copied()),
            // A peer with no features at all has no entity, which `id` reports as empty.
            None => id.entity.is_empty(),
        }
    }

    /// Whichever feature says which entity this unit is.
    ///
    /// The three are on one entity — implementation guide §3.3 — so whichever is present
    /// answers for all of them.
    fn entity_feature(&self) -> Option<&crate::model::FeatureAddress> {
        self.measurement
            .as_ref()
            .or(self.curtailment.as_ref())
            .or(self.electrical_connection.as_ref())
    }

    fn entity_path(&self) -> Vec<u32> {
        self.entity_feature()
            .map(crate::spine::entity_path)
            .unwrap_or_default()
    }

    /// Whether `feature` is one of the three this peer serves.
    ///
    /// The device is checked first and separately: [`same_feature`](crate::spine::same_feature)
    /// compares device addresses only where both carry one, which is right for matching a
    /// discovery read against the instance it was aimed at and wrong here — two devices may
    /// well number an entity and a feature the same way.
    fn serves(&self, feature: &crate::model::FeatureAddress) -> bool {
        if feature.device.as_ref().is_none_or(|d| d != &self.device) {
            return false;
        }
        [
            self.measurement.as_ref(),
            self.curtailment.as_ref(),
            self.electrical_connection.as_ref(),
        ]
        .into_iter()
        .flatten()
        .any(|known| crate::spine::same_feature(known, feature))
    }

    /// A peer whose only feature is a `Measurement` server.
    ///
    /// What a use case with no electrical connection to describe locates: MDT's DHW
    /// circuit is the one in this crate.
    pub fn measuring(
        device: crate::model::AddressDevice,
        measurement: crate::model::FeatureAddress,
    ) -> Self {
        Self {
            device,
            electrical_connection: None,
            measurement: Some(measurement),
            curtailment: None,
        }
    }

    /// Whether the peer serves anything this actor can read.
    ///
    /// A peer that announced the use case and serves none of its three features is a peer
    /// there is nothing to attach to — which [`locate`] reports as [`None`] rather than as
    /// an empty peer that would sit in the actor's list producing nothing.
    pub fn is_usable(&self) -> bool {
        self.measurement.is_some() || self.curtailment.is_some()
    }
}

/// Finds a peer's monitoring features from its detailed discovery and use-case data.
///
/// `actor` is [`crate::usecases::descriptor::actors::MONITORED_UNIT`] for MPC and
/// [`crate::usecases::descriptor::actors::GRID_CONNECTION_POINT`] for MGCP — the two use
/// cases publish the same data and differ only in who is publishing it.
///
/// Every feature is looked up and none is required, because MGCP ties each of them to a
/// scenario and Table 1 makes those scenarios `M`, `R` or `O` separately. What *is*
/// required is that the peer serve at least one of them: a peer announcing the use case
/// and none of its features is reported as [`None`], since there would be nothing to read
/// from it. See [`MonitoredUnitPeer`] for which absence means what.
pub fn locate(
    remote: &crate::spine::RemoteDevice,
    use_case: &str,
    actor: &str,
) -> Option<MonitoredUnitPeer> {
    use crate::model::{FeatureType, Role};

    let found = remote.use_case(use_case, actor)?;
    let peer = MonitoredUnitPeer {
        device: remote.address.clone()?,
        electrical_connection: remote.address_of(
            found,
            &FeatureType::ElectricalConnection,
            Role::Server,
        ),
        measurement: remote.address_of(found, &FeatureType::Measurement, Role::Server),
        // Optional, and absent for MPC entirely: a `DeviceConfiguration` server inside
        // this use case is MGCP scenario 1 and nothing else.
        curtailment: remote.address_of(found, &FeatureType::DeviceConfiguration, Role::Server),
    };
    // A peer that announces the use case and serves none of its features has nothing to
    // read. Reporting it as located would put it in the actor's list for good, waiting on
    // notifications from features that are not there.
    peer.is_usable().then_some(peer)
}

/// The Monitoring Appliance, wired to a SPINE engine.
///
/// Its whole job is bookkeeping: read the two description functions once, subscribe to
/// the values, and resolve every notification that arrives against what the descriptions
/// said. The use-case implementation guide §3.2.2 is why it subscribes rather than
/// polling — a poll loop over a dozen devices is what an energy manager cannot afford.
///
/// There is no binding: a Monitoring Appliance never writes.
#[derive(Debug)]
pub struct MonitoringApplianceActor {
    client: crate::model::FeatureAddress,
    peers: Vec<TrackedUnit>,
}

#[derive(Debug)]
struct TrackedUnit {
    peer: MonitoredUnitPeer,
    readings: Readings,
    described: bool,
    /// MGCP scenario 1, where the peer serves it.
    curtailment: crate::usecases::mgcp::Curtailment,
}

/// What a Monitoring Appliance learned.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum MonitoringEvent {
    /// A unit's descriptions arrived, so its values can now be read.
    UnitDescribed {
        /// The unit.
        unit: UnitId,
    },
    /// New values arrived from a unit.
    Measured {
        /// The unit.
        unit: UnitId,
        /// What changed.
        readings: Vec<Reading>,
    },
    /// A Grid Connection Point's PV feed-in curtailment factor changed (MGCP scenario 1).
    ///
    /// The factor is a percentage of the building's cumulated nominal PV peak power, and
    /// only the two together are a number of watts ([MGCP-011]) — which is what a §9 EEG
    /// export ceiling is. Turn it into one with
    /// [`FeedInLimit`](crate::usecases::mgcp::FeedInLimit), or ask the actor for
    /// [`feed_in_limit`](MonitoringApplianceActor::feed_in_limit).
    ///
    /// MPC units never report this: it is the one MGCP scenario that is not a
    /// measurement.
    CurtailmentChanged {
        /// The connection point.
        unit: UnitId,
        /// The factor, as a percentage in `0..=100`.
        factor_percent: f64,
    },
}

impl MonitoringApplianceActor {
    /// An appliance reading from `client`, the actor's single `Generic` client feature.
    ///
    /// One feature for every server it talks to, whatever their types: the LPC
    /// implementation guide §3.3 asks an actor to "use a client Feature with featureType
    /// `Generic` for all its client functionality" rather than mirroring each server
    /// feature it reads. Build it with
    /// [`limitation::client_feature`](crate::usecases::limitation::client_feature), which
    /// is the same one-line constructor an Energy Guard uses. There is deliberately no
    /// per-feature-type client constructor here — a `DeviceConfiguration` client for
    /// MGCP scenario 1 and a `Measurement` client for the rest would make "which feature
    /// is the Monitoring Appliance" a question with two answers.
    ///
    /// The one typed client feature in this crate,
    /// [`limitation::device_diagnosis_client_feature`](crate::usecases::limitation::device_diagnosis_client_feature),
    /// is the exception and has its own reason: a Controllable System holds no `Generic`
    /// client at all, and real devices in `tests/fixtures/devices` carry a
    /// `DeviceDiagnosis` server and client side by side.
    pub fn new(client: crate::model::FeatureAddress) -> Self {
        Self {
            client,
            peers: Vec::new(),
        }
    }

    /// Starts monitoring a unit: reads its descriptions, then subscribes to its values.
    ///
    /// Calling it again for a unit already tracked restarts the exchange, which is what
    /// a reconnection needs — the subscription did not survive it, and the descriptions
    /// may have changed while the connection was down.
    ///
    /// "Already tracked" is by [`UnitId`] — device *and* entity — so several units of one
    /// device sit here side by side. A heat-pump gateway announcing one `HVACRoom` per room
    /// is attached once per room, and each keeps its own [`Readings`].
    pub fn attach(
        &mut self,
        engine: &mut crate::spine::Engine,
        peer: MonitoredUnitPeer,
        now: core::time::Duration,
    ) {
        use crate::model::Function;

        let id = peer.id();
        self.peers.retain(|t| !t.peer.is_unit(&id));

        // Each of the three features is read only where the peer serves it. A read
        // addressed to a feature that is not there is answered with `errorNumber` 7 at
        // best and dropped at worst, and either way it is a question this side already
        // knows the answer to — discovery said so.
        if let Some(electrical) = peer.electrical_connection.clone() {
            for function in [
                Function::ElectricalConnectionDescriptionListData,
                Function::ElectricalConnectionParameterDescriptionListData,
            ] {
                engine.read(&electrical, &self.client, function, now);
            }
        }
        if let Some(measurement) = peer.measurement.clone() {
            for function in [
                Function::MeasurementDescriptionListData,
                Function::MeasurementConstraintsListData,
                Function::MeasurementListData,
            ] {
                engine.read(&measurement, &self.client, function, now);
            }
            engine.request_subscription(&self.client, &measurement, now);
        }

        // MGCP scenario 1, where this peer serves it. The description read is not
        // optional: it is what says which `keyId` carries the factor, and that number is
        // the peer's own (MGCP Table 23 spells it `<k1#(1..1)>`).
        if let Some(configuration) = peer.curtailment.clone() {
            for function in [
                Function::DeviceConfigurationKeyValueDescriptionListData,
                Function::DeviceConfigurationKeyValueListData,
            ] {
                engine.read(&configuration, &self.client, function, now);
            }
            engine.request_subscription(&self.client, &configuration, now);
        }

        self.peers.push(TrackedUnit {
            peer,
            readings: Readings::new(),
            described: false,
            curtailment: crate::usecases::mgcp::Curtailment::new(),
        });
    }

    /// Stops monitoring a unit.
    pub fn detach(&mut self, unit: &UnitId) {
        self.peers.retain(|t| !t.peer.is_unit(unit));
    }

    /// Stops monitoring every unit on a device — what a lost connection removes.
    ///
    /// A device that goes away takes all of its entities with it, and a Monitoring
    /// Appliance holding one room of a gateway and not the other three is worse than one
    /// holding none.
    pub fn detach_device(&mut self, device: &crate::model::AddressDevice) {
        self.peers.retain(|t| &t.peer.device != device);
    }

    /// The units being monitored.
    pub fn units(&self) -> impl Iterator<Item = &MonitoredUnitPeer> {
        self.peers.iter().map(|t| &t.peer)
    }

    /// What is known about one unit's measurements.
    pub fn readings(&self, unit: &UnitId) -> Option<&Readings> {
        self.peers
            .iter()
            .find(|t| t.peer.is_unit(unit))
            .map(|t| &t.readings)
    }

    /// A Grid Connection Point's PV feed-in curtailment factor, as a percentage.
    ///
    /// [`None`] for a peer that does not serve MGCP scenario 1, and for one whose
    /// description and value have not both arrived. Not zero, ever: a factor that has not
    /// been read is not a curtailment, and treating it as one stops a roof exporting.
    pub fn curtailment(&self, unit: &UnitId) -> Option<f64> {
        self.peers
            .iter()
            .find(|t| t.peer.is_unit(unit))?
            .curtailment
            .factor_percent()
    }

    /// The same factor as a feed-in ceiling in watts ([MGCP-011]).
    ///
    /// `nominal_peak_watts` is the cumulated nominal peak power of the building's PV
    /// systems, which no EEBUS message carries — it is a property of the installation and
    /// the caller's to supply.
    pub fn feed_in_limit(
        &self,
        unit: &UnitId,
        nominal_peak_watts: f64,
    ) -> Option<crate::usecases::mgcp::FeedInLimit> {
        self.peers
            .iter()
            .find(|t| t.peer.is_unit(unit))?
            .curtailment
            .limit(nominal_peak_watts)
    }

    /// Feeds one engine event to the actor.
    pub fn handle_event(&mut self, event: &crate::spine::SpineEvent) -> Option<MonitoringEvent> {
        use crate::spine::SpineEvent;

        let (feature, data) = match event {
            // `resolved`, not `data`. A Monitored Unit is asked to notify rather than be
            // polled (SPINE IG §3.2.2), and a notification may be partial — a measurement
            // whose `scale` is omitted keeps the one already sent, so reading the fragment
            // alone is off by a power of ten.
            SpineEvent::ReplyReceived {
                feature, resolved, ..
            }
            | SpineEvent::DataNotified {
                feature, resolved, ..
            } => (feature, resolved),
            _ => return None,
        };
        // By feature, not by device. Two `HVACRoom` entities of one gateway are two units
        // with two `Measurement` features, and a device-wide lookup would resolve both
        // rooms' notifications against whichever was attached first.
        let index = self.peers.iter().position(|t| t.peer.serves(feature))?;
        let tracked = &mut self.peers[index];
        let unit = tracked.peer.id();

        // MGCP scenario 1: a `DeviceConfiguration` payload from the feature this peer
        // serves it from. Neither half is a measurement, so it is settled before the
        // measurement layer sees it.
        if tracked
            .peer
            .curtailment
            .as_ref()
            .is_some_and(|known| crate::spine::same_feature(known, feature))
        {
            let before = tracked.curtailment.factor_percent();
            if tracked.curtailment.describe(data) {
                // A description can complete a value that arrived before it.
                return changed(before, tracked.curtailment.factor_percent()).map(|factor| {
                    MonitoringEvent::CurtailmentChanged {
                        unit,
                        factor_percent: factor,
                    }
                });
            }
            let after = tracked.curtailment.apply(data);
            return changed(before, after).map(|factor| MonitoringEvent::CurtailmentChanged {
                unit,
                factor_percent: factor,
            });
        }

        if tracked.readings.describe(data) {
            // The descriptions are what make a value legible; a unit is "described" once
            // at least one measurement has both a meaning and its phases.
            if !tracked.described && tracked.readings.described().next().is_some() {
                tracked.described = true;
                return Some(MonitoringEvent::UnitDescribed { unit });
            }
            return None;
        }

        let readings = tracked.readings.apply(data);
        if readings.is_empty() {
            return None;
        }
        Some(MonitoringEvent::Measured { unit, readings })
    }
}

/// The new factor, if it is both known and different from what was known before.
///
/// A connection point notifies its whole `DeviceConfiguration` feature, so a change to
/// some other key of its own arrives here too; reporting the same factor again would have
/// an application re-deriving an export ceiling that did not move.
fn changed(before: Option<f64>, after: Option<f64>) -> Option<f64> {
    let after = after?;
    (before != Some(after)).then_some(after)
}

/// Whether a scope is inherently about the whole connection rather than some phases.
///
/// `acPower`, `acCurrent` and `acVoltage` are the phase-specific ones; everything else
/// these two use cases publish is a total.
/// What a measurement description alone says about a measurand's phases.
///
/// Three answers, and the distinction matters. `Some(Some(abc))`: a scope that covers the
/// whole connection names its own phases. `None`: a phase-specific AC scope, whose phase
/// is in the *parameter* description and is not known yet. `Some(None)`: a direct-current
/// scope, which has no phases and never will — waiting for a parameter description that is
/// not coming would leave the measurand undescribed forever.
fn phasing_of(scope: &ScopeType) -> Option<Option<ElectricalConnectionPhaseName>> {
    match scope {
        ScopeType::AcPower | ScopeType::AcCurrent | ScopeType::AcVoltage => None,
        ScopeType::DcPower
        | ScopeType::DcCurrent
        | ScopeType::DcVoltage
        | ScopeType::DcEnergy
        | ScopeType::DcChargeEnergy
        | ScopeType::DcDischargeEnergy
        | ScopeType::Charge
        | ScopeType::StateOfCharge
        | ScopeType::StateOfHealth
        | ScopeType::StateOfEnergy
        | ScopeType::UseableCapacity
        | ScopeType::LoadCycleCount
        | ScopeType::InsulationResistance
        | ScopeType::ComponentTemperature
        | ScopeType::DhwTemperature
        | ScopeType::RoomAirTemperature
        | ScopeType::OutsideAirTemperature
        | ScopeType::TravelRange => Some(None),
        // Apparent and reactive power split into a total and a per-phase scope, like
        // active power: the phase-specific ones wait for the parameter description.
        ScopeType::AcPowerApparent | ScopeType::AcPowerReactive => None,
        _ => Some(Some(ElectricalConnectionPhaseName::Abc)),
    }
}

/// The quantity a scope names, in either use case's vocabulary.
fn quantity_of(scope: &ScopeType) -> Option<Quantity> {
    Some(match scope {
        ScopeType::AcPowerTotal | ScopeType::AcPower => Quantity::Power,
        ScopeType::AcEnergyConsumed | ScopeType::GridConsumption => Quantity::EnergyConsumed,
        ScopeType::AcEnergyProduced | ScopeType::GridFeedIn => Quantity::EnergyProduced,
        ScopeType::AcCurrent => Quantity::Current,
        ScopeType::AcVoltage => Quantity::Voltage,
        ScopeType::AcFrequency => Quantity::Frequency,
        // The e-mobility scopes. `charge` is EVCEM scenario 3; the other three are EVSOC.
        ScopeType::Charge => Quantity::EnergyCharged,
        ScopeType::StateOfCharge => Quantity::StateOfCharge,
        ScopeType::StateOfHealth => Quantity::StateOfHealth,
        ScopeType::TravelRange => Quantity::TravelRange,
        // Inverter, battery and PV-string vocabulary.
        ScopeType::AcPowerApparentTotal | ScopeType::AcPowerApparent => Quantity::ApparentPower,
        ScopeType::AcPowerReactiveTotal | ScopeType::AcPowerReactive => Quantity::ReactivePower,
        ScopeType::AcCosPhi => Quantity::PowerFactor,
        ScopeType::AcYieldDay => Quantity::YieldDay,
        ScopeType::AcYieldMonth => Quantity::YieldMonth,
        ScopeType::AcYieldYear => Quantity::YieldYear,
        ScopeType::AcYieldTotal => Quantity::YieldTotal,
        ScopeType::ComponentTemperature => Quantity::Temperature,
        // The three measurements here that are not electricity, and have no phases: a
        // tank is a tank (MDT Table 7), a room is a room (MRT Table 7), and outdoors is
        // outdoors (MOT Table 7).
        ScopeType::DhwTemperature => Quantity::DhwTemperature,
        ScopeType::RoomAirTemperature => Quantity::RoomTemperature,
        ScopeType::OutsideAirTemperature => Quantity::OutdoorTemperature,
        ScopeType::DcPower => Quantity::DcPower,
        ScopeType::DcCurrent => Quantity::DcCurrent,
        ScopeType::DcVoltage => Quantity::DcVoltage,
        ScopeType::DcEnergy => Quantity::DcEnergy,
        ScopeType::DcChargeEnergy => Quantity::DcChargeEnergy,
        ScopeType::DcDischargeEnergy => Quantity::DcDischargeEnergy,
        ScopeType::StateOfEnergy => Quantity::StateOfEnergy,
        ScopeType::UseableCapacity => Quantity::UsableCapacity,
        ScopeType::LoadCycleCount => Quantity::LoadCycleCount,
        ScopeType::InsulationResistance => Quantity::InsulationResistance,
        _ => return None,
    })
}
