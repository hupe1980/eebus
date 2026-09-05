//! What a consumer may conclude from silence.
//!
//! A value that has not arrived for ten minutes is either a peer that has stopped
//! answering or a peer with nothing to say, and the two call for opposite responses:
//! drop the unit from the site, or leave it exactly where it is. Nothing in a
//! `measurementListData` says which, so the answer has to come from the use case — and
//! it does, in one line that every UC TS repeats per scenario: "Actors SHALL create a
//! subscription for each server Feature that is relevant for the corresponding Actor
//! within this Scenario" (§3.4.n.1), with polling named in §3.3.4 only as what to do
//! when a subscription request is *refused*.
//!
//! So every scenario here is notification-driven, and the distinction that matters is
//! the other one: whether the notification comes on a **clock**. Exactly one family of
//! functions does — the heartbeats — and this file pins that down across every
//! descriptor the crate ships, so that a consumer ageing its inputs can ask the
//! descriptor rather than keep a list of its own.

use core::time::Duration;

use eebus::model::{DeviceType, EntityType, FeatureType, Function, Role};
use eebus::spine::{Engine, LocalDevice, LocalEntity, LocalFeature};
use eebus::usecases::descriptor::{Delivery, UseCaseDescriptor};
use eebus::usecases::emobility::charging::{EvPeer, OverloadGuardActor, Purpose};
use eebus::usecases::limitation::{ControllableSystemPeer, EnergyGuardActor};

use eebus::usecases::monitoring::{MonitoredUnitPeer, MonitoringApplianceActor};
use eebus::usecases::{cob, emobility, hvac, lpc, lpp, mgcp, mob, moi, mpc, mps, ohpcf};

/// Every actor of every use case this crate describes.
///
/// Written out rather than derived: the point of the sweep is to catch a *new* use case
/// that claims a cadence it has no right to, and a list that grew by itself would not.
fn all_descriptors() -> Vec<(&'static str, &'static UseCaseDescriptor)> {
    vec![
        ("lpc::ENERGY_GUARD", &lpc::ENERGY_GUARD),
        ("lpc::CONTROLLABLE_SYSTEM", &lpc::CONTROLLABLE_SYSTEM),
        ("lpp::ENERGY_GUARD", &lpp::ENERGY_GUARD),
        ("lpp::CONTROLLABLE_SYSTEM", &lpp::CONTROLLABLE_SYSTEM),
        ("mpc::MONITORING_APPLIANCE", &mpc::MONITORING_APPLIANCE),
        ("mpc::MONITORED_UNIT", &mpc::MONITORED_UNIT),
        ("mgcp::MONITORING_APPLIANCE", &mgcp::MONITORING_APPLIANCE),
        ("mgcp::GRID_CONNECTION_POINT", &mgcp::GRID_CONNECTION_POINT),
        ("mob::BATTERY", &mob::BATTERY),
        ("mob::MONITORING_APPLIANCE", &mob::MONITORING_APPLIANCE),
        ("moi::INVERTER", &moi::INVERTER),
        ("moi::MONITORING_APPLIANCE", &moi::MONITORING_APPLIANCE),
        ("mps::PV_STRING", &mps::PV_STRING),
        ("mps::MONITORING_APPLIANCE", &mps::MONITORING_APPLIANCE),
        ("cob::INVERTER", &cob::INVERTER),
        ("cob::CEM", &cob::CEM),
        ("ohpcf::COMPRESSOR", &ohpcf::COMPRESSOR),
        ("ohpcf::CEM", &ohpcf::CEM),
        ("emobility::opev::EV", &emobility::opev::EV),
        (
            "emobility::opev::ENERGY_GUARD",
            &emobility::opev::ENERGY_GUARD,
        ),
        ("emobility::oscev::EV", &emobility::oscev::EV),
        ("emobility::oscev::CEM", &emobility::oscev::CEM),
        ("emobility::evcc::EV", &emobility::evcc::EV),
        ("emobility::evcc::CEM", &emobility::evcc::CEM),
        ("emobility::evcem::EV", &emobility::evcem::EV),
        (
            "emobility::evcem::ENERGY_GUARD",
            &emobility::evcem::ENERGY_GUARD,
        ),
        ("emobility::evsoc::EV", &emobility::evsoc::EV),
        (
            "emobility::evsoc::MONITORING_APPLIANCE",
            &emobility::evsoc::MONITORING_APPLIANCE,
        ),
        ("emobility::evsecc::EVSE", &emobility::evsecc::EVSE),
        ("emobility::evsecc::CEM", &emobility::evsecc::CEM),
        ("emobility::evcs::EVSE", &emobility::evcs::EVSE),
        (
            "emobility::evcs::ENERGY_BROKER",
            &emobility::evcs::ENERGY_BROKER,
        ),
        ("emobility::evcs::CEM", &emobility::evcs::CEM),
        ("hvac::cdsf::DHW_CIRCUIT", &hvac::cdsf::DHW_CIRCUIT),
        (
            "hvac::cdsf::CONFIGURATION_APPLIANCE",
            &hvac::cdsf::CONFIGURATION_APPLIANCE,
        ),
        ("hvac::mdsf::DHW_CIRCUIT", &hvac::mdsf::DHW_CIRCUIT),
        (
            "hvac::mdsf::MONITORING_APPLIANCE",
            &hvac::mdsf::MONITORING_APPLIANCE,
        ),
        ("hvac::crhsf::HVAC_ROOM", &hvac::crhsf::HVAC_ROOM),
        (
            "hvac::crhsf::CONFIGURATION_APPLIANCE",
            &hvac::crhsf::CONFIGURATION_APPLIANCE,
        ),
        ("hvac::mrhsf::HVAC_ROOM", &hvac::mrhsf::HVAC_ROOM),
        (
            "hvac::mrhsf::MONITORING_APPLIANCE",
            &hvac::mrhsf::MONITORING_APPLIANCE,
        ),
        ("hvac::crcsf::HVAC_ROOM", &hvac::crcsf::HVAC_ROOM),
        (
            "hvac::crcsf::CONFIGURATION_APPLIANCE",
            &hvac::crcsf::CONFIGURATION_APPLIANCE,
        ),
        ("hvac::mrcsf::HVAC_ROOM", &hvac::mrcsf::HVAC_ROOM),
        (
            "hvac::mrcsf::MONITORING_APPLIANCE",
            &hvac::mrcsf::MONITORING_APPLIANCE,
        ),
        ("hvac::cdt::DHW_CIRCUIT", &hvac::cdt::DHW_CIRCUIT),
        (
            "hvac::cdt::CONFIGURATION_APPLIANCE",
            &hvac::cdt::CONFIGURATION_APPLIANCE,
        ),
        ("hvac::mdt::DHW_CIRCUIT", &hvac::mdt::DHW_CIRCUIT),
        (
            "hvac::mdt::MONITORING_APPLIANCE",
            &hvac::mdt::MONITORING_APPLIANCE,
        ),
        ("hvac::crht::HVAC_ROOM", &hvac::crht::HVAC_ROOM),
        (
            "hvac::crht::CONFIGURATION_APPLIANCE",
            &hvac::crht::CONFIGURATION_APPLIANCE,
        ),
        ("hvac::crct::HVAC_ROOM", &hvac::crct::HVAC_ROOM),
        (
            "hvac::crct::CONFIGURATION_APPLIANCE",
            &hvac::crct::CONFIGURATION_APPLIANCE,
        ),
        ("hvac::mrt::HVAC_ROOM", &hvac::mrt::HVAC_ROOM),
        (
            "hvac::mrt::MONITORING_APPLIANCE",
            &hvac::mrt::MONITORING_APPLIANCE,
        ),
        (
            "hvac::mot::OUTDOOR_TEMPERATURE_SENSOR",
            &hvac::mot::OUTDOOR_TEMPERATURE_SENSOR,
        ),
        (
            "hvac::mot::MONITORING_APPLIANCE",
            &hvac::mot::MONITORING_APPLIANCE,
        ),
    ]
}

/// The sweep above is only as good as its list, so the list is checked against the source.
///
/// A use case added without a line in `all_descriptors` would be swept by nothing, and the
/// failure would be silent — the tests would still pass. This is the same trick
/// `tests/generated_model_size.rs` uses to hold the documented counts to the generator.
#[test]
fn the_sweep_covers_every_descriptor_the_crate_ships() {
    fn declared(dir: &std::path::Path) -> usize {
        let mut total = 0;
        for entry in std::fs::read_dir(dir).expect("the use-case modules are committed") {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                total += declared(&path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("readable");
            total += source
                .lines()
                .filter(|line| {
                    line.starts_with("pub static") && line.contains(": UseCaseDescriptor")
                })
                .count();
        }
        total
    }

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/usecases");
    assert_eq!(
        all_descriptors().len(),
        declared(&root),
        "a use case was added without a line in `all_descriptors` in this file, so nothing \
         is checking what it claims about delivery"
    );
}

/// The heartbeat is the only thing any of these specifications puts a clock on.
///
/// Everything else — a temperature, a limit, a mode, a state of charge — is sent when it
/// changes, so its age is a fact about the *world* and not about the peer. A consumer
/// that times out on it drops a room that is simply holding its temperature.
#[test]
fn only_the_heartbeat_arrives_on_a_clock() {
    for (name, descriptor) in all_descriptors() {
        for (feature, function, period) in descriptor.periodic_functions() {
            assert_eq!(
                (feature, function),
                (
                    &FeatureType::DeviceDiagnosis,
                    &Function::DeviceDiagnosisHeartbeatData
                ),
                "{name} claims a cadence for something that is not a heartbeat"
            );
            assert!(
                period == Duration::from_secs(60) || period == Duration::from_secs(4),
                "{name}: {period:?} is neither the 60 s of LPC, LPP and COB nor the 4 s \
                 of OPEV and OSCEV"
            );
        }
    }
}

/// The two cadences the specifications fix, at the actors that carry them.
#[test]
fn each_heartbeat_runs_at_the_period_its_use_case_fixes() {
    let sixty = Delivery::Periodic(Duration::from_secs(60));
    let four = Delivery::Periodic(Duration::from_secs(4));

    // [LPC-005], [LPC-006] and their LPP twins: both directions, both actors.
    for descriptor in [
        &lpc::ENERGY_GUARD,
        &lpc::CONTROLLABLE_SYSTEM,
        &lpp::ENERGY_GUARD,
        &lpp::CONTROLLABLE_SYSTEM,
    ] {
        assert_eq!(descriptor.scenario(3).expect("heartbeat").delivery(), sixty);
    }
    // [COB-008].
    for descriptor in [&cob::INVERTER, &cob::CEM] {
        assert_eq!(descriptor.scenario(5).expect("heartbeat").delivery(), sixty);
    }
    // [OPEV-005], [OSCEV-005]: four seconds, because a car follows a current at once.
    for descriptor in [
        &emobility::opev::EV,
        &emobility::opev::ENERGY_GUARD,
        &emobility::oscev::EV,
        &emobility::oscev::CEM,
    ] {
        assert_eq!(descriptor.scenario(2).expect("heartbeat").delivery(), four);
    }

    // And the measurement that pairs with the limit is not on a clock at all, which is
    // the whole point of asking.
    assert_eq!(
        mpc::MONITORING_APPLIANCE
            .delivery_of(&FeatureType::Measurement, &Function::MeasurementListData),
        Some(Delivery::OnChange),
    );
    assert_eq!(
        hvac::mrt::MONITORING_APPLIANCE
            .delivery_of(&FeatureType::Measurement, &Function::MeasurementListData),
        Some(Delivery::OnChange),
        "a room holding its temperature is the protocol working"
    );
    assert_eq!(
        mpc::MONITORING_APPLIANCE
            .scenario(1)
            .expect("power")
            .delivery(),
        Delivery::OnChange,
    );
}

/// A function the actor does not have is a different answer from one it has and reads
/// on change — and a consumer that conflates them ages a value it never receives.
#[test]
fn a_function_outside_the_actors_tables_has_no_delivery() {
    assert_eq!(
        mpc::MONITORING_APPLIANCE.delivery_of(
            &FeatureType::DeviceDiagnosis,
            &Function::DeviceDiagnosisHeartbeatData,
        ),
        None,
        "MPC defines no heartbeat scenario"
    );
    assert_eq!(mpc::MONITORING_APPLIANCE.periodic_functions().count(), 0);
    assert_eq!(
        lpc::CONTROLLABLE_SYSTEM.delivery_of(
            &FeatureType::DeviceDiagnosis,
            &Function::DeviceDiagnosisHeartbeatData,
        ),
        Some(Delivery::Periodic(Duration::from_secs(60))),
    );
}

/// The features an actor subscribes to are the features it reads, and one subscription
/// serves every scenario that shares a feature (§3.3.1).
#[test]
fn the_subscription_list_is_the_scenario_tables_read_back() {
    assert_eq!(
        mpc::MONITORING_APPLIANCE
            .features_needing_subscription()
            .collect::<Vec<_>>(),
        [
            &FeatureType::ElectricalConnection,
            &FeatureType::Measurement
        ],
        "five scenarios, two features, and each named once"
    );
    assert_eq!(
        mgcp::MONITORING_APPLIANCE
            .features_needing_subscription()
            .collect::<Vec<_>>(),
        [
            &FeatureType::DeviceConfiguration,
            &FeatureType::ElectricalConnection,
            &FeatureType::Measurement,
        ],
        "MGCP scenario 1 is the curtailment factor, which is not a measurement"
    );

    // A server actor subscribes to nothing of its own accord — except where the use case
    // hands it a client role, as LPC's heartbeat does in both directions.
    assert_eq!(
        mpc::MONITORED_UNIT.features_needing_subscription().count(),
        0,
    );
    assert_eq!(
        lpc::CONTROLLABLE_SYSTEM
            .features_needing_subscription()
            .collect::<Vec<_>>(),
        [&FeatureType::DeviceDiagnosis],
        "scenario 3: the system watches the guard's heartbeat"
    );

    // The three HVAC use cases that write a temperature reach a `Setpoint` feature; the
    // six system-function ones have none in scope at all.
    for descriptor in [
        &hvac::cdt::CONFIGURATION_APPLIANCE,
        &hvac::crht::CONFIGURATION_APPLIANCE,
        &hvac::crct::CONFIGURATION_APPLIANCE,
    ] {
        let features: Vec<_> = descriptor.features_needing_subscription().collect();
        assert!(
            features.contains(&&FeatureType::Setpoint) && features.contains(&&FeatureType::HVAC),
            "{features:?}"
        );
    }
    for descriptor in [
        &hvac::cdsf::CONFIGURATION_APPLIANCE,
        &hvac::mdsf::MONITORING_APPLIANCE,
        &hvac::crhsf::CONFIGURATION_APPLIANCE,
    ] {
        assert_eq!(
            descriptor
                .features_needing_subscription()
                .collect::<Vec<_>>(),
            [&FeatureType::HVAC],
        );
    }

    // OHPCF serves both its scenarios from one feature, which is why
    // `CompressorPeer::follow` has a single `subscription` field rather than a list.
    assert_eq!(
        ohpcf::CEM
            .features_needing_subscription()
            .collect::<Vec<_>>(),
        [&FeatureType::SmartEnergyManagementPs],
    );
    assert_eq!(
        ohpcf::COMPRESSOR.features_needing_subscription().count(),
        0,
        "the compressor serves; it subscribes to nothing of the CEM's"
    );
}

/// A device with one entity carrying a server feature of each named type.
fn peer_device(
    name: &str,
    kind: DeviceType,
    entity: EntityType,
    features: &[FeatureType],
) -> LocalDevice {
    let mut device = LocalDevice::new("i:67890", name, kind).expect("a device");
    let mut holder = LocalEntity::new([1], entity);
    for (index, feature) in features.iter().enumerate() {
        holder = holder.with_feature(LocalFeature::new(
            index as u32 + 1,
            feature.clone(),
            Role::Server,
        ));
    }
    device.add_entity(holder).expect("an entity");
    device
}

/// An engine for a client actor, and the `Generic` client feature it reads from.
fn client_engine() -> (Engine, eebus::model::FeatureAddress) {
    let mut manager =
        LocalDevice::new("i:12345", "CEM-1", DeviceType::EnergyManagementSystem).expect("a device");
    manager
        .add_entity(
            LocalEntity::new([1], EntityType::CEM).with_feature(LocalFeature::new(
                1,
                FeatureType::Generic,
                Role::Client,
            )),
        )
        .expect("an entity");
    let client = manager.address_of(&[1], 1);
    (Engine::new(manager), client)
}

/// Every feature the engine has asked to subscribe to, by the type the peer serves it as.
fn subscriptions_sent(
    engine: &mut Engine,
    peer: &LocalDevice,
    served: &[FeatureType],
) -> Vec<FeatureType> {
    let addresses: Vec<_> = (0..served.len())
        .map(|index| peer.address_of(&[1], index as u32 + 1))
        .collect();
    let mut asked = Vec::new();
    while let Some(datagram) = engine.poll_transmit() {
        for cmd in datagram.payload.iter().flat_map(|p| p.cmd.iter()).flatten() {
            let Some(eebus::model::CmdData::NodeManagementSubscriptionRequestCall(call)) =
                cmd.data.as_ref()
            else {
                continue;
            };
            let server = call
                .subscription_request
                .as_ref()
                .and_then(|r| r.server_address.as_ref())
                .expect("a subscription request names a server");
            let index = addresses
                .iter()
                .position(|known| known == server)
                .unwrap_or_else(|| {
                    panic!("subscribed to a feature the peer does not serve: {server:?}")
                });
            asked.push(served[index].clone());
        }
    }
    sorted(asked)
}

fn sorted(mut features: Vec<FeatureType>) -> Vec<FeatureType> {
    features.sort_by_key(|f| format!("{f:?}"));
    features.dedup();
    features
}

fn expected(descriptor: &UseCaseDescriptor) -> Vec<FeatureType> {
    sorted(
        descriptor
            .features_needing_subscription()
            .cloned()
            .collect(),
    )
}

/// The Monitoring Appliance subscribes to exactly what its descriptor names.
///
/// The descriptor is the specification's table as data; this is the code that acts on it.
/// Nothing forces the two to agree, so a test does — otherwise a consumer that trusted
/// `features_needing_subscription` would be reasoning about a conversation this crate
/// does not have.
#[test]
fn the_monitoring_appliance_subscribes_to_what_its_descriptor_names() {
    let served = [
        FeatureType::ElectricalConnection,
        FeatureType::Measurement,
        FeatureType::DeviceConfiguration,
    ];
    let unit = peer_device(
        "HeatPump-1",
        DeviceType::HeatGenerationSystem,
        EntityType::HeatPumpAppliance,
        &served,
    );
    let (mut engine, client) = client_engine();

    let mut appliance = MonitoringApplianceActor::new(client);
    appliance.attach(
        &mut engine,
        MonitoredUnitPeer {
            device: unit.address().clone(),
            electrical_connection: Some(unit.address_of(&[1], 1)),
            measurement: Some(unit.address_of(&[1], 2)),
            curtailment: Some(unit.address_of(&[1], 3)),
        },
        Duration::ZERO,
    );

    assert_eq!(
        subscriptions_sent(&mut engine, &unit, &served),
        expected(&mgcp::MONITORING_APPLIANCE),
        "the actor and its descriptor disagree about what it subscribes to"
    );
}

/// The Energy Guard subscribes to all four, and each one earns its place.
///
/// The two that are easy to leave out are the ones this test exists for: the failsafe
/// values are writable at the appliance ([LPC-024]), and the *contractual* maximum in the
/// `ElectricalConnection` characteristics is what a §14a agreement sets — neither is a
/// value this guard is the only author of, so neither can be read once and believed.
#[test]
fn the_energy_guard_subscribes_to_what_its_descriptor_names() {
    let served = [
        FeatureType::LoadControl,
        FeatureType::DeviceConfiguration,
        FeatureType::DeviceDiagnosis,
        FeatureType::ElectricalConnection,
    ];
    let system = peer_device(
        "HeatPump-1",
        DeviceType::HeatGenerationSystem,
        EntityType::HeatPumpAppliance,
        &served,
    );
    let (mut engine, client) = client_engine();

    let mut guard = EnergyGuardActor::new(
        lpc::DIRECTION,
        client.clone(),
        client.clone(),
        Duration::ZERO,
    );
    guard.attach(
        &mut engine,
        ControllableSystemPeer {
            device: system.address().clone(),
            load_control: system.address_of(&[1], 1),
            device_configuration: system.address_of(&[1], 2),
            device_diagnosis: Some(system.address_of(&[1], 3)),
            electrical_connection: Some(system.address_of(&[1], 4)),
        },
        Duration::ZERO,
    );

    assert_eq!(
        subscriptions_sent(&mut engine, &system, &served),
        expected(&lpc::ENERGY_GUARD),
    );
}

/// And the EV guard subscribes to the permitted value sets, which change mid-session.
///
/// A car that raises its minimum current has just made the guard's last write
/// unacceptable. Reading `permittedValueSet` once at commissioning would leave the guard
/// curtailing to a value the car has stopped accepting, and the refusal is the first it
/// would hear of it.
#[test]
fn the_ev_guard_subscribes_to_what_its_descriptor_names() {
    let served = [FeatureType::LoadControl, FeatureType::ElectricalConnection];
    let car = peer_device("EV-1", DeviceType::ChargingStation, EntityType::EV, &served);
    let (mut engine, client) = client_engine();

    let mut guard = OverloadGuardActor::new(client.clone(), client.clone(), Duration::ZERO);
    guard.attach(
        &mut engine,
        EvPeer {
            device: car.address().clone(),
            load_control: car.address_of(&[1], 1),
            electrical_connection: car.address_of(&[1], 2),
            purpose: Purpose::OverloadProtection,
        },
        Duration::ZERO,
    );

    assert_eq!(
        subscriptions_sent(&mut engine, &car, &served),
        expected(&emobility::opev::ENERGY_GUARD),
    );
}
