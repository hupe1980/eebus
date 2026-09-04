//! The e-mobility family, over the SPINE engine.
//!
//! `opev_over_the_wire.rs` drives one use case in detail. This file checks the parts that
//! only appear when the family is taken together: that a car's commissioning data reaches
//! an energy manager through discovery and reads, that the two curtailment use cases stay
//! distinguishable on one connection, and that what a car measures can be checked against
//! what it was told.

use core::time::Duration;

use eebus::model::{
    CmdData, DeviceType, ElectricalConnectionPhaseName as Phase, EntityType, Function,
};
use eebus::spine::{Engine, LocalDevice, LocalEntity, SpineEvent, node_management};
use eebus::usecases::emobility::{charging, evcc, evcem, evsoc, opev, oscev};
use eebus::usecases::monitoring::{Measurand, Quantity, Readings};

/// A car and an energy manager, exchanging real datagrams over a virtual clock.
struct Link {
    car: Engine,
    manager: Engine,
    now: Duration,
}

impl Link {
    fn new() -> Self {
        // The car sits under the wallbox, as EVCC scenario 1 describes: entity [1] is the
        // EVSE, entity [1, 1] is the EV that appeared when it was plugged in.
        let mut car_device =
            LocalDevice::new("i:46925", "Wallbox-1", DeviceType::ChargingStation).unwrap();
        car_device
            .add_entity(LocalEntity::new([1], EntityType::EVSE))
            .unwrap();
        car_device
            .add_entity(
                LocalEntity::new([1, 1], EntityType::EV)
                    .with_feature(evcc::device_configuration_feature(1))
                    .with_feature(evcc::identification_feature(2))
                    .with_feature(evcc::device_classification_feature(3))
                    .with_feature(charging::load_control_feature(4))
                    .with_feature(evcc::electrical_connection_feature(5))
                    .with_feature(evcc::device_diagnosis_feature(6)),
            )
            .unwrap();

        let mut car = Engine::new(car_device);
        car.add_use_case([1, 1], 1, &evcc::EV);
        car.add_use_case([1, 1], 4, &opev::EV);
        car.add_use_case([1, 1], 4, &oscev::EV);

        let mut manager_device =
            LocalDevice::new("i:46925", "CEM-1", DeviceType::EnergyManagementSystem).unwrap();
        manager_device
            .add_entity(LocalEntity::new([1], EntityType::CEM))
            .unwrap();
        let mut manager = Engine::new(manager_device);
        manager.add_use_case([1], 0, &evcc::CEM);

        Self {
            car,
            manager,
            now: Duration::ZERO,
        }
    }

    /// Discovery both ways, so each side knows what the other is.
    fn discover(&mut self) {
        let car_nm = node_management(self.car.device().address());
        let manager_nm = node_management(self.manager.device().address());
        for function in [
            Function::NodeManagementDetailedDiscoveryData,
            Function::NodeManagementUseCaseData,
        ] {
            self.manager
                .read(&car_nm, &manager_nm, function.clone(), self.now);
            self.car.read(&manager_nm, &car_nm, function, self.now);
        }
        self.settle();
    }

    fn settle(&mut self) {
        for _ in 0..64 {
            let mut moved = false;
            while let Some(datagram) = self.manager.poll_transmit() {
                self.car.handle_datagram(&datagram, self.now);
                moved = true;
            }
            while let Some(datagram) = self.car.poll_transmit() {
                self.manager.handle_datagram(&datagram, self.now);
                moved = true;
            }
            while self.car.poll_event().is_some() {
                moved = true;
            }
            if !moved {
                return;
            }
        }
        panic!("the exchange did not settle");
    }

    /// Reads one function off the car and returns every reply that has arrived.
    ///
    /// Every reply, not the first: the manager's event queue also holds the discovery
    /// replies, and taking the first match would hand back whichever of them happened to
    /// be at the front.
    fn read(&mut self, feature: u32, function: Function) -> Vec<CmdData> {
        let source = self.manager.device().address_of(&[1], 0);
        let target = self.car.device().address_of(&[1, 1], feature);
        self.manager.read(&target, &source, function, self.now);
        self.settle();

        core::iter::from_fn(|| self.manager.poll_event())
            .filter_map(|event| match event {
                SpineEvent::ReplyReceived { data, .. } => Some(data),
                _ => None,
            })
            .collect()
    }
}

/// The car publishes what it is, and the manager reads it back — the whole of EVCC's
/// scenarios 2 to 7 over the wire.
#[test]
fn a_car_describes_itself_and_the_manager_learns_it() {
    let mut link = Link::new();

    let profile = evcc::EvProfile::new()
        .communication_standard(evcc::CommunicationStandard::Iso15118Ed2)
        .asymmetric_charging(true)
        .identification(evcc::EvIdentification::eui48("AA-BB-CC-DD-EE-FF"))
        .charging_power(1_400.0, 11_000.0)
        .asleep(false);

    // What the car serves.
    for (feature, payload) in [
        (1, profile.key_descriptions()),
        (1, profile.key_values()),
        (2, profile.identification_data()),
        (5, profile.power_parameter_description()),
        (5, profile.power_limits()),
        (6, profile.state_data()),
    ] {
        let address = link.car.device().address_of(&[1, 1], feature);
        link.car
            .device_mut()
            .resolve_mut(&address)
            .expect("the feature exists")
            .set_data(payload)
            .expect("a payload the feature serves");
    }
    link.discover();

    // What the manager reads.
    let mut reader = evcc::EvReader::new();
    for (feature, function) in [
        // The descriptions first, because they are what say which of *this car's*
        // identifiers is which. Reading the values alone leaves them unaddressable.
        (1, Function::DeviceConfigurationKeyValueDescriptionListData),
        (
            5,
            Function::ElectricalConnectionParameterDescriptionListData,
        ),
        (1, Function::DeviceConfigurationKeyValueListData),
        (2, Function::IdentificationListData),
        (5, Function::ElectricalConnectionPermittedValueSetListData),
        (6, Function::DeviceDiagnosisStateData),
    ] {
        let replies = link.read(feature, function.clone());
        assert!(!replies.is_empty(), "no reply to {function:?}");
        for reply in &replies {
            reader.apply(reply);
        }
    }
    let learned = reader.profile();

    assert_eq!(
        learned.communication_standard,
        Some(evcc::CommunicationStandard::Iso15118Ed2)
    );
    assert!(learned.supports_data_exchange(), "so EVSOC may be asked");
    assert_eq!(learned.asymmetric_charging, Some(true));
    assert_eq!(
        learned.identification,
        Some(evcc::EvIdentification::eui48("AA-BB-CC-DD-EE-FF"))
    );
    assert!(
        learned
            .identification
            .as_ref()
            .is_some_and(|id| id.is_well_formed())
    );
    assert_eq!(learned.charging_power, Some((1_400.0, Some(11_000.0))));
    assert_eq!(learned.asleep, Some(false));
}

/// A car announcing both curtailment use cases stays legible: discovery finds each of them
/// under its own name, and the descriptions differ in exactly the two elements that decide
/// whether the ceiling is a fuse or an offer.
#[test]
fn the_two_curtailment_use_cases_are_told_apart_on_one_connection() {
    let mut link = Link::new();
    link.discover();

    let device = link.car.device().address().clone();
    let remote = link.manager.peer(&device).expect("the car");

    let protection = charging::locate(remote, opev::PURPOSE).expect("OPEV is announced");
    let surplus = charging::locate(remote, oscev::PURPOSE).expect("OSCEV is announced");
    assert_eq!(
        protection.load_control, surplus.load_control,
        "one LoadControl feature carries both, which is why the payload has to say which"
    );

    let obligation = charging::limit_descriptions(opev::PURPOSE, &charging::PHASES);
    let recommendation = charging::limit_descriptions(oscev::PURPOSE, &charging::PHASES);
    assert_ne!(obligation, recommendation);

    let CmdData::LoadControlLimitDescriptionListData(list) = &obligation else {
        unreachable!("built above");
    };
    let entry = &list.load_control_limit_description_data.as_ref().unwrap()[0];
    assert_eq!(
        entry.limit_category.as_ref().map(|c| c.as_str()),
        Some("obligation")
    );
    assert_eq!(
        entry.scope_type.as_ref().map(|s| s.as_str()),
        Some("overloadProtection")
    );

    let CmdData::LoadControlLimitDescriptionListData(list) = &recommendation else {
        unreachable!("built above");
    };
    let entry = &list.load_control_limit_description_data.as_ref().unwrap()[0];
    assert_eq!(
        entry.limit_category.as_ref().map(|c| c.as_str()),
        Some("recommendation")
    );
    assert_eq!(
        entry.scope_type.as_ref().map(|s| s.as_str()),
        Some("selfConsumption")
    );
}

/// What EVCEM is for: a manager that limited a car to 6 A on phase B can see that it took
/// 6 A on phase B, rather than believing its own write.
#[test]
fn evcem_makes_a_curtailment_checkable_rather_than_asserted() {
    let mut car = evcem::monitored_unit(1)
        .with(Measurand::on(Quantity::Current, Phase::A))
        .with(Measurand::on(Quantity::Current, Phase::B))
        .with(Measurand::unphased(Quantity::EnergyCharged));

    // The manager wrote 16 A on A and 6 A on B; this is what the car actually drew.
    car.set(
        &Measurand::on(Quantity::Current, Phase::A),
        15.9,
        Duration::ZERO,
    );
    car.set(
        &Measurand::on(Quantity::Current, Phase::B),
        6.0,
        Duration::ZERO,
    );
    car.set(
        &Measurand::unphased(Quantity::EnergyCharged),
        8_400.0,
        Duration::ZERO,
    );

    let mut readings = Readings::new();
    readings.describe(&car.measurement_descriptions());
    readings.describe(&car.parameter_descriptions());
    readings.apply(&car.measurements());

    let written = charging::ChargingCurrents::new(16.0, 6.0, 0.0);
    for (phase, wrote) in [(Phase::A, written.a), (Phase::B, written.b)] {
        let drew = readings
            .value(&Measurand::on(Quantity::Current, phase.clone()))
            .expect("the car measured this phase");
        let wrote = wrote.expect("the manager wrote this phase");
        assert!(
            drew <= wrote + 0.1,
            "the car drew {drew} A where it was allowed {wrote} A"
        );
    }
    assert_eq!(
        readings.value(&Measurand::unphased(Quantity::EnergyCharged)),
        Some(8_400.0)
    );
}

/// EVSOC over EVCC: the battery may only be asked about once the car has said it has a
/// data link to ask over.
#[test]
fn a_car_without_a_data_link_is_not_asked_about_its_battery() {
    let pilot_wire =
        evcc::EvProfile::new().communication_standard(evcc::CommunicationStandard::Iec61851);
    assert!(
        !pilot_wire.supports_data_exchange(),
        "a pilot wire carries a current and no questions"
    );

    let iso =
        evcc::EvProfile::new().communication_standard(evcc::CommunicationStandard::Iso15118Ed2);
    assert!(iso.supports_data_exchange());

    // And what it would be asked, once it can be.
    let mut battery = evsoc::Battery::new();
    let mut car = evsoc::monitored_unit(1).with(Measurand::unphased(Quantity::StateOfCharge));
    car.set(
        &Measurand::unphased(Quantity::StateOfCharge),
        40.0,
        Duration::ZERO,
    );
    let mut readings = Readings::new();
    readings.describe(&car.measurement_descriptions());
    readings.describe(&car.parameter_descriptions());

    battery.apply(&car.measurements(), &readings);
    battery.apply(&evsoc::nominal_capacity(77_000.0), &readings);
    assert_eq!(battery.energy_to_full(), Some(46_200.0));
}
