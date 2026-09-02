//! The Monitoring Appliance: the side that reads.
//!
//! A `measurementListData` notification carries identifiers and numbers and nothing
//! else — `{"measurementId": 3, "value": {"number": 2300}}`. What that means is in two
//! other functions, read once at commissioning. [`Readings`] holds those descriptions and
//! resolves each notification against them, so an application sees a quantity and a
//! phase rather than an integer it has to remember the meaning of.

use alloc::vec::Vec;

use crate::model::{
    CmdData, ElectricalConnectionPhaseName, MeasurementId, MeasurementValueState, ScopeType,
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
}

/// What a measurement's `valueState` says about its value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadingState {
    /// A value within its constraints. `valueState` absent or `normal`.
    Normal,
    /// A value outside the constraints the unit published.
    OutOfRange,
    /// The unit could not measure. Any value sent with this is to be ignored.
    Error,
}

impl Reading {
    /// The value, unless the unit flagged it as an error.
    ///
    /// [MPC-003]: where `valueState` is `error` the content of `value` `SHALL` be
    /// ignored, so a caller that wants a number it can act on should ask for it here
    /// rather than reading the field.
    pub fn usable(&self) -> Option<f64> {
        match self.state {
            ReadingState::Error => None,
            _ => self.value,
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
        self.values
            .iter()
            .find(|(_, reading)| reading.measurand == *measurand)
            .map(|(_, reading)| reading.clone())
    }

    /// The latest usable value of a measurand, in its unit.
    pub fn value(&self, measurand: &Measurand) -> Option<f64> {
        self.get(measurand).and_then(|r| r.usable())
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
#[derive(Clone, Debug, PartialEq)]
pub struct MonitoredUnitPeer {
    /// The peer's device address.
    pub device: crate::model::AddressDevice,
    /// Its `ElectricalConnection` feature, which says which measurement covers which
    /// phases.
    pub electrical_connection: crate::model::FeatureAddress,
    /// Its `Measurement` feature, which says what each measurement is and what it reads.
    pub measurement: crate::model::FeatureAddress,
}

/// Finds a peer's monitoring features from its detailed discovery and use-case data.
///
/// `actor` is [`crate::usecases::descriptor::actors::MONITORED_UNIT`] for MPC and
/// [`crate::usecases::descriptor::actors::GRID_CONNECTION_POINT`] for MGCP — the two use
/// cases publish the same data and differ only in who is publishing it.
pub fn locate(
    remote: &crate::spine::RemoteDevice,
    use_case: &str,
    actor: &str,
) -> Option<MonitoredUnitPeer> {
    use crate::model::{FeatureType, Role};

    let found = remote.use_case(use_case, actor)?;
    Some(MonitoredUnitPeer {
        device: remote.address.clone()?,
        electrical_connection: remote.address_of(
            found,
            &FeatureType::ElectricalConnection,
            Role::Server,
        )?,
        measurement: remote.address_of(found, &FeatureType::Measurement, Role::Server)?,
    })
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
}

/// What a Monitoring Appliance learned.
#[derive(Clone, Debug, PartialEq)]
pub enum MonitoringEvent {
    /// A unit's descriptions arrived, so its values can now be read.
    UnitDescribed {
        /// The unit.
        device: crate::model::AddressDevice,
    },
    /// New values arrived from a unit.
    Measured {
        /// The unit.
        device: crate::model::AddressDevice,
        /// What changed.
        readings: Vec<Reading>,
    },
}

impl MonitoringApplianceActor {
    /// An appliance reading from `client`, the actor's single `Generic` client feature.
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
    pub fn attach(
        &mut self,
        engine: &mut crate::spine::Engine,
        peer: MonitoredUnitPeer,
        now: core::time::Duration,
    ) {
        use crate::model::Function;

        let device = peer.device.clone();
        self.peers.retain(|t| t.peer.device != device);

        for function in [
            Function::ElectricalConnectionDescriptionListData,
            Function::ElectricalConnectionParameterDescriptionListData,
        ] {
            engine.read(&peer.electrical_connection, &self.client, function, now);
        }
        for function in [
            Function::MeasurementDescriptionListData,
            Function::MeasurementConstraintsListData,
            Function::MeasurementListData,
        ] {
            engine.read(&peer.measurement, &self.client, function, now);
        }
        engine.request_subscription(&self.client, &peer.measurement, now);

        self.peers.push(TrackedUnit {
            peer,
            readings: Readings::new(),
            described: false,
        });
    }

    /// Stops monitoring a unit.
    pub fn detach(&mut self, device: &crate::model::AddressDevice) {
        self.peers.retain(|t| &t.peer.device != device);
    }

    /// The units being monitored.
    pub fn units(&self) -> impl Iterator<Item = &MonitoredUnitPeer> {
        self.peers.iter().map(|t| &t.peer)
    }

    /// What is known about one unit's measurements.
    pub fn readings(&self, device: &crate::model::AddressDevice) -> Option<&Readings> {
        self.peers
            .iter()
            .find(|t| &t.peer.device == device)
            .map(|t| &t.readings)
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
        let device = feature.device.as_ref()?;
        let index = self.peers.iter().position(|t| &t.peer.device == device)?;
        let tracked = &mut self.peers[index];

        if tracked.readings.describe(data) {
            // The descriptions are what make a value legible; a unit is "described" once
            // at least one measurement has both a meaning and its phases.
            if !tracked.described && tracked.readings.described().next().is_some() {
                tracked.described = true;
                return Some(MonitoringEvent::UnitDescribed {
                    device: device.clone(),
                });
            }
            return None;
        }

        let readings = tracked.readings.apply(data);
        if readings.is_empty() {
            return None;
        }
        Some(MonitoringEvent::Measured {
            device: device.clone(),
            readings,
        })
    }
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
