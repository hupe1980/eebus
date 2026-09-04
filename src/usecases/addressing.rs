//! Addressing a peer's data by what it means, rather than by where this crate keeps it.
//!
//! Almost every list in SPINE is keyed by an identifier the *device* chooses. The
//! specifications write them as placeholders — `<l1#(1..1)>` for a load-control limit,
//! `<k1#(1..1)>` for a configuration key, `<p1#(1..1)>` for an electrical-connection
//! parameter — and say only "SHALL be used as the primary identifier", which is a promise
//! that the device keeps its own number stable. It is not a number any peer may assume.
//!
//! What the specifications fix instead is what each entry *describes*: a key's `keyName`,
//! a parameter's `scopeType` and phases, a limit's type and category and direction. Every
//! list that is addressed by a chosen identifier has a description function beside it that
//! gives each entry its meaning, and reading that description is the only way to address
//! the peer rather than a coincidence.
//!
//! Getting this wrong is quiet in a way that few protocol errors are. The write is
//! well-formed. It names a real entry of the peer's. The peer applies it and acknowledges
//! it. Nothing is logged anywhere, and the value has gone somewhere else.
//!
//! This module holds the two resolvers that are not specific to one use case.
//! [`KeyIds`] does `DeviceConfiguration`, [`ParameterIds`] does `ElectricalConnection`;
//! the ones that are specific live with their use case —
//! [`limitation::PeerIds`](crate::usecases::limitation::PeerIds),
//! [`mgcp::Curtailment`](crate::usecases::mgcp::Curtailment) and
//! [`charging::PhaseLimits`](crate::usecases::emobility::charging::PhaseLimits).

use alloc::vec::Vec;

use crate::model::{
    CmdData, DeviceConfigurationKeyId, DeviceConfigurationKeyName, ElectricalConnectionId,
    ElectricalConnectionParameterId, ElectricalConnectionPhaseName, MeasurementId, ScopeType,
};

/// Finds the `keyId` a peer publishes a named `DeviceConfiguration` key under.
///
/// Give it that peer's `deviceConfigurationKeyValueDescriptionListData`. The `keyName` is
/// what the specification fixes; the `keyId` beside it is the device's own, and a
/// `DeviceConfiguration` feature carries every configuration key its device has — not
/// just the ones a given use case is interested in.
///
/// [`KeyIds`] is the same thing kept across payloads, which is what a reader of *values*
/// needs: a value list carries identifiers and no names.
pub fn find_key_id(
    data: &CmdData,
    name: &DeviceConfigurationKeyName,
) -> Option<DeviceConfigurationKeyId> {
    let CmdData::DeviceConfigurationKeyValueDescriptionListData(list) = data else {
        return None;
    };
    list.device_configuration_key_value_description_data
        .iter()
        .flatten()
        .find(|entry| entry.key_name.as_ref() == Some(name))
        .and_then(|entry| entry.key_id)
}

/// What a peer's `DeviceConfiguration` keys are called, by identifier and by name.
///
/// Feed it every `deviceConfigurationKeyValueDescriptionListData` that arrives from one
/// feature. Then [`get`](Self::get) turns a name into the identifier to write to, and
/// [`name_of`](Self::name_of) turns an identifier that arrived in a value list back into
/// the name that says what it is — which is the direction a reader needs, because a
/// `deviceConfigurationKeyValueListData` carries no names at all.
///
/// ```
/// use eebus::model::DeviceConfigurationKeyName;
/// use eebus::usecases::addressing::KeyIds;
/// use eebus::usecases::mgcp;
///
/// let mut keys = KeyIds::new();
/// keys.learn(&mgcp::curtailment_description());
///
/// let factor = DeviceConfigurationKeyName::PvCurtailmentLimitFactor;
/// assert_eq!(keys.get(&factor), Some(mgcp::CURTAILMENT_KEY));
/// assert_eq!(keys.name_of(mgcp::CURTAILMENT_KEY), Some(&factor));
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeyIds {
    known: Vec<(DeviceConfigurationKeyId, DeviceConfigurationKeyName)>,
}

impl KeyIds {
    /// Nothing known yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records what one description payload says, and reports whether it was one.
    ///
    /// Anything else is ignored rather than refused: a caller hands it every payload that
    /// arrives from the feature and uses the answer to tell a description from a value
    /// list.
    pub fn learn(&mut self, data: &CmdData) -> bool {
        let CmdData::DeviceConfigurationKeyValueDescriptionListData(list) = data else {
            return false;
        };
        for entry in list
            .device_configuration_key_value_description_data
            .iter()
            .flatten()
        {
            let (Some(id), Some(name)) = (entry.key_id, entry.key_name.clone()) else {
                continue;
            };
            match self.known.iter_mut().find(|(known, _)| *known == id) {
                Some((_, stored)) => *stored = name,
                None => self.known.push((id, name)),
            }
        }
        true
    }

    /// The identifier a named key is published under, once described.
    pub fn get(&self, name: &DeviceConfigurationKeyName) -> Option<DeviceConfigurationKeyId> {
        self.known
            .iter()
            .find(|(_, known)| known == name)
            .map(|(id, _)| *id)
    }

    /// What the key with this identifier is called, once described.
    pub fn name_of(&self, id: DeviceConfigurationKeyId) -> Option<&DeviceConfigurationKeyName> {
        self.known
            .iter()
            .find(|(known, _)| *known == id)
            .map(|(_, name)| name)
    }

    /// Whether any description has arrived.
    pub fn is_empty(&self) -> bool {
        self.known.is_empty()
    }
}

/// One `ElectricalConnection` parameter, as its description gave it.
#[derive(Clone, Debug, PartialEq)]
pub struct Parameter {
    /// The connection it belongs to.
    pub connection: Option<ElectricalConnectionId>,
    /// The parameter's own identifier — the one a permitted value set is addressed by.
    pub parameter: ElectricalConnectionParameterId,
    /// The measurement it is tied to, where it names one.
    pub measurement: Option<MeasurementId>,
    /// What it describes.
    pub scope: Option<ScopeType>,
    /// The phases it covers, where it is phase-specific.
    pub phases: Option<ElectricalConnectionPhaseName>,
}

/// What a peer's `ElectricalConnection` parameters describe, by identifier.
///
/// `electricalConnectionPermittedValueSetListData` and
/// `electricalConnectionCharacteristicListData` are both addressed by `parameterId`, and
/// nothing in either says what the parameter is. The parameter *descriptions* do —
/// `scopeType` for the quantity, `acMeasuredPhases` for the phase — so a reader that takes
/// the first entry, or assumes parameter `1`, is reading whichever quantity the peer
/// happened to describe first. Clamping a charging current to a range that turned out to
/// be in watts is the shape of that failure.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParameterIds {
    known: Vec<Parameter>,
}

impl ParameterIds {
    /// Nothing known yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records what one description payload says, and reports whether it was one.
    pub fn learn(&mut self, data: &CmdData) -> bool {
        let CmdData::ElectricalConnectionParameterDescriptionListData(list) = data else {
            return false;
        };
        for entry in list
            .electrical_connection_parameter_description_data
            .iter()
            .flatten()
        {
            let Some(parameter) = entry.parameter_id else {
                continue;
            };
            let found = Parameter {
                connection: entry.electrical_connection_id,
                parameter,
                measurement: entry.measurement_id,
                scope: entry.scope_type.clone(),
                phases: entry.ac_measured_phases.clone(),
            };
            match self
                .known
                .iter_mut()
                .find(|known| known.parameter == parameter)
            {
                Some(stored) => *stored = found,
                None => self.known.push(found),
            }
        }
        true
    }

    /// Every parameter described so far.
    pub fn all(&self) -> impl Iterator<Item = &Parameter> {
        self.known.iter()
    }

    /// The parameter describing one quantity, where the peer publishes exactly one.
    ///
    /// [`None`] when none matches **or when several do** — an ambiguous answer is the one
    /// case where guessing is worst, because both candidates are real and only one is
    /// meant. A caller that can narrow it further should use [`all`](Self::all).
    pub fn by_scope(&self, scope: &ScopeType) -> Option<ElectricalConnectionParameterId> {
        let mut matching = self
            .known
            .iter()
            .filter(|known| known.scope.as_ref() == Some(scope));
        let first = matching.next()?;
        matching.next().is_none().then_some(first.parameter)
    }

    /// The parameter covering one phase, where the peer publishes exactly one.
    pub fn by_phase(
        &self,
        phase: &ElectricalConnectionPhaseName,
    ) -> Option<ElectricalConnectionParameterId> {
        let mut matching = self
            .known
            .iter()
            .filter(|known| known.phases.as_ref() == Some(phase));
        let first = matching.next()?;
        matching.next().is_none().then_some(first.parameter)
    }

    /// The phase a `measurementId` covers, where a parameter tied the two together.
    pub fn phase_of_measurement(
        &self,
        measurement: MeasurementId,
    ) -> Option<&ElectricalConnectionPhaseName> {
        self.known
            .iter()
            .find(|known| known.measurement == Some(measurement))?
            .phases
            .as_ref()
    }

    /// Whether any description has arrived.
    pub fn is_empty(&self) -> bool {
        self.known.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        DeviceConfigurationKeyValueDescriptionData, DeviceConfigurationKeyValueDescriptionListData,
        ElectricalConnectionParameterDescriptionData,
        ElectricalConnectionParameterDescriptionListData,
    };
    use alloc::vec;

    fn described(id: u32, name: DeviceConfigurationKeyName) -> CmdData {
        CmdData::DeviceConfigurationKeyValueDescriptionListData(
            DeviceConfigurationKeyValueDescriptionListData {
                device_configuration_key_value_description_data: Some(vec![
                    DeviceConfigurationKeyValueDescriptionData {
                        key_id: Some(DeviceConfigurationKeyId(id)),
                        key_name: Some(name),
                        ..Default::default()
                    },
                ]),
            },
        )
    }

    /// Descriptions arrive in several payloads, and later ones do not erase earlier ones.
    #[test]
    fn keys_accumulate_across_payloads() {
        let mut keys = KeyIds::new();
        keys.learn(&described(
            7,
            DeviceConfigurationKeyName::PeakPowerOfPvSystem,
        ));
        keys.learn(&described(
            9,
            DeviceConfigurationKeyName::PvCurtailmentLimitFactor,
        ));

        assert_eq!(
            keys.get(&DeviceConfigurationKeyName::PeakPowerOfPvSystem),
            Some(DeviceConfigurationKeyId(7))
        );
        assert_eq!(
            keys.name_of(DeviceConfigurationKeyId(9)),
            Some(&DeviceConfigurationKeyName::PvCurtailmentLimitFactor)
        );
        assert_eq!(
            keys.get(&DeviceConfigurationKeyName::FailsafeDurationMinimum),
            None,
            "a key that was never described is not invented"
        );
    }

    /// Two parameters with the same scope is an answer a caller has to disambiguate.
    ///
    /// Returning either would be a coin toss between two real parameters of the peer's,
    /// and the wrong one is a number in the wrong unit.
    #[test]
    fn an_ambiguous_scope_has_no_answer() {
        let descriptions = CmdData::ElectricalConnectionParameterDescriptionListData(
            ElectricalConnectionParameterDescriptionListData {
                electrical_connection_parameter_description_data: Some(vec![
                    ElectricalConnectionParameterDescriptionData {
                        parameter_id: Some(ElectricalConnectionParameterId(1)),
                        scope_type: Some(ScopeType::AcCurrent),
                        ac_measured_phases: Some(ElectricalConnectionPhaseName::A),
                        ..Default::default()
                    },
                    ElectricalConnectionParameterDescriptionData {
                        parameter_id: Some(ElectricalConnectionParameterId(2)),
                        scope_type: Some(ScopeType::AcCurrent),
                        ac_measured_phases: Some(ElectricalConnectionPhaseName::B),
                        ..Default::default()
                    },
                ]),
            },
        );
        let mut parameters = ParameterIds::new();
        assert!(parameters.learn(&descriptions));

        assert_eq!(parameters.by_scope(&ScopeType::AcCurrent), None);
        assert_eq!(
            parameters.by_phase(&ElectricalConnectionPhaseName::B),
            Some(ElectricalConnectionParameterId(2)),
            "but the phase tells them apart"
        );
    }
}
