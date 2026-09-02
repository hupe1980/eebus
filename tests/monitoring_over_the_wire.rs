//! Monitoring of Power Consumption and of the Grid Connection Point, over SPINE.
//!
//! An energy manager discovers what a heat pump measures, subscribes, and receives its
//! readings; then the same exchange against a grid connection point, which names its two
//! energies from the grid's side. What is checked throughout is that the numbers arriving
//! as bare identifiers come out the other end as quantities with phases and units.

use eebus::model::{
    CmdData, ElectricalConnectionPhaseName as Phase, FeatureType, Function, MeasurementValueState,
    Role, ScopeType,
};
use eebus::usecases::monitoring::{Measurand, Quantity, ReadingState, Readings};

mod common;
use common::MonitoringPair as Link;

/// MPC scenario 1: a heat pump reports what it is drawing, and the energy manager reads
/// it as watts rather than as an identifier.
#[test]
fn power_readings_reach_the_energy_manager_with_their_meaning() {
    let mut link = Link::consuming();
    link.commission();

    link.report(&Measurand::total_power(), 2_300.0);

    assert_eq!(link.readings.total_power(), Some(2_300.0));
    let reading = link
        .readings
        .get(&Measurand::total_power())
        .expect("the total power");
    assert_eq!(reading.measurand.unit(), eebus::model::UnitOfMeasurement::W);
    assert_eq!(reading.state, ReadingState::Normal);
}

/// The phase-specific values are told apart from the total, which is the whole reason the
/// parameter descriptions exist: both are `measurementType = "power"` in watts.
#[test]
fn a_phase_value_is_not_confused_with_the_total() {
    let mut link = Link::consuming();
    link.commission();

    link.report(&Measurand::total_power(), 2_300.0);
    link.report(&Measurand::on(Quantity::Power, Phase::A), 800.0);
    link.report(&Measurand::on(Quantity::Current, Phase::A), 3.5);

    assert_eq!(link.readings.total_power(), Some(2_300.0));
    assert_eq!(
        link.readings
            .value(&Measurand::on(Quantity::Power, Phase::A)),
        Some(800.0)
    );
    assert_eq!(
        link.readings
            .value(&Measurand::on(Quantity::Current, Phase::A)),
        Some(3.5),
        "amperes, not watts"
    );
    assert_eq!(
        link.readings
            .value(&Measurand::on(Quantity::Power, Phase::B)),
        None,
        "a phase the unit does not measure has no value"
    );
}

/// [MPC-003]: a value marked `error` is not to be used, whatever number came with it.
#[test]
fn mpc_003_a_failed_measurement_is_not_reported_as_a_number() {
    let mut link = Link::consuming();
    link.commission();
    link.report(&Measurand::total_power(), 2_300.0);

    link.unit
        .set_state(&Measurand::total_power(), MeasurementValueState::Error);
    let measurement = link.unit_feature(2);
    link.unit
        .notify(&mut link.unit_engine, &measurement, link.now);
    link.exchange();

    let reading = link.readings.get(&Measurand::total_power()).unwrap();
    assert_eq!(reading.state, ReadingState::Error);
    assert_eq!(
        reading.value,
        Some(2_300.0),
        "the number is still on the wire"
    );
    assert_eq!(reading.usable(), None, "but it is not to be used");
    assert_eq!(link.readings.total_power(), None);
}

/// MGCP names its energies from the grid's side. The manager resolves both vocabularies
/// to the same quantity, which is what lets one reader serve both use cases.
#[test]
fn the_grid_connection_point_names_its_energies_from_the_grids_side() {
    let mut link = Link::at_the_grid();
    link.commission();

    link.report(&Measurand::total(Quantity::EnergyConsumed), 12_500.0);
    link.report(&Measurand::total(Quantity::EnergyProduced), 8_400.0);
    link.report(&Measurand::total_power(), -1_200.0);

    // What went on the wire.
    let published = link
        .unit_engine
        .device()
        .resolve(&link.unit_feature(2))
        .unwrap()
        .data(&Function::MeasurementDescriptionListData)
        .unwrap();
    let CmdData::MeasurementDescriptionListData(descriptions) = published else {
        panic!("expected the descriptions");
    };
    let scopes: Vec<&ScopeType> = descriptions
        .measurement_description_data
        .iter()
        .flatten()
        .filter_map(|d| d.scope_type.as_ref())
        .collect();
    assert!(scopes.contains(&&ScopeType::GridConsumption), "[MGCP-041]");
    assert!(scopes.contains(&&ScopeType::GridFeedIn), "[MGCP-031]");
    assert!(
        !scopes.contains(&&ScopeType::AcEnergyConsumed),
        "which is MPC's name for it, not MGCP's"
    );

    // What the manager made of it.
    assert_eq!(
        link.readings
            .value(&Measurand::total(Quantity::EnergyConsumed)),
        Some(12_500.0)
    );
    assert_eq!(
        link.readings
            .value(&Measurand::total(Quantity::EnergyProduced)),
        Some(8_400.0)
    );
    assert_eq!(
        link.readings.total_power(),
        Some(-1_200.0),
        "the load convention: negative means the building is feeding in ([MPC-001])"
    );
}

/// A reading whose description never arrived is dropped rather than guessed at.
#[test]
fn a_measurement_without_a_description_is_not_reported() {
    let mut link = Link::consuming();
    // No commissioning: the manager has read no descriptions.
    link.unit.set(&Measurand::total_power(), 2_300.0, link.now);

    let mut readings = Readings::new();
    assert!(readings.apply(&link.unit.measurements()).is_empty());
    assert_eq!(readings.total_power(), None);

    // With the descriptions, the same payload resolves.
    readings.describe(&link.unit.measurement_descriptions());
    readings.describe(&link.unit.parameter_descriptions());
    assert_eq!(readings.apply(&link.unit.measurements()).len(), 1);
    assert_eq!(readings.total_power(), Some(2_300.0));
}

/// Discovery reports which scenarios the unit plays, which is how a manager knows whether
/// to expect a frequency at all.
#[test]
fn discovery_reports_the_scenarios_each_actor_plays() {
    let mut link = Link::consuming();
    link.commission();

    let address = link.unit_engine.device().address().clone();
    let peer = link.manager.peer(&address).expect("the peer");
    let use_case = peer
        .use_case("monitoringOfPowerConsumption", "MonitoredUnit")
        .expect("the use case");
    for scenario in [1, 2, 3, 5] {
        assert!(use_case.supports_scenario(scenario), "scenario {scenario}");
    }
    assert!(
        !use_case.supports_scenario(4),
        "this unit measures no voltage, and does not claim to"
    );
    assert!(
        peer.feature_for(use_case, &FeatureType::Measurement, Role::Server)
            .is_some()
    );
}

/// A value outside the declared range is flagged, and the range itself is published so
/// the manager can see what "out of range" meant.
#[test]
fn a_value_outside_its_constraints_is_flagged_and_the_range_is_published() {
    let mut link = Link::consuming();
    link.unit
        .set_range(&Measurand::total_power(), 0.0, 11_000.0);
    let ec = link.unit_feature(1);
    let m = link.unit_feature(2);
    link.unit.publish(&mut link.unit_engine, &ec, &m);
    link.commission();

    link.report(&Measurand::total_power(), 2_300.0);
    assert_eq!(
        link.readings.get(&Measurand::total_power()).unwrap().state,
        ReadingState::Normal
    );

    link.report(&Measurand::total_power(), 14_000.0);
    let reading = link.readings.get(&Measurand::total_power()).unwrap();
    assert_eq!(reading.state, ReadingState::OutOfRange);
    assert_eq!(
        reading.usable(),
        Some(14_000.0),
        "out of range is still a reading; only `error` invalidates the number"
    );

    // And the constraints say what the range was.
    let published = link
        .unit_engine
        .device()
        .resolve(&m)
        .unwrap()
        .data(&Function::MeasurementConstraintsListData)
        .expect("the constraints are published");
    let CmdData::MeasurementConstraintsListData(list) = published else {
        panic!("expected the constraints");
    };
    let entry = &list.measurement_constraints_data.as_ref().unwrap()[0];
    assert_eq!(
        entry.value_range_max.as_ref().unwrap().to_f64(),
        Some(11_000.0)
    );
}
