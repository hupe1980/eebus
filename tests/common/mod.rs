//! A control box and a heat pump, wired to each other in memory.
//!
//! Shared by `limitation_both_actors.rs` and `conformance.rs`: both need two real actors
//! exchanging real datagrams over a virtual clock, and a second copy of the harness would
//! be a second thing to keep in step with the crate.
//!
//! Every datagram makes the round trip through the wire format on the way across, so what
//! is under test is the encoding as well as the logic.
#![allow(dead_code)]

use core::time::Duration;

use eebus::model::{Datagram, DeviceType, EntityType, Function};
use eebus::spine::{Engine, LocalDevice, LocalEntity, SpineEvent, node_management};
use eebus::usecases::limitation::{
    self, ControllableSystem, ControllableSystemActor, CsConfig, CsEvent, EnergyGuardActor,
    GuardEvent, LimitWrite,
};
use eebus::usecases::lpc;

pub const FAILSAFE_WATTS: f64 = 4_200.0;
pub const NOMINAL_MAX_WATTS: f64 = 11_000.0;

/// A control box and a heat pump, each driven by its own actor.
pub struct Pair {
    pub guard_engine: Engine,
    pub guard: EnergyGuardActor,
    pub pump_engine: Engine,
    pub pump: ControllableSystemActor,
    pub now: Duration,
    /// Everything the Energy Guard has reported.
    pub reports: Vec<GuardEvent>,
    /// Everything the Controllable System has reported.
    pub decisions: Vec<CsEvent>,
    /// Every datagram that crossed, in order, with the direction it went.
    ///
    /// `"box"` for the Energy Guard's, `"pump"` for the Controllable System's. Filled by
    /// [`Pair::settle`], and what `golden_vectors.rs` compares against a checked-in file.
    pub trace: Vec<(&'static str, Datagram)>,
}

impl Pair {
    pub fn new() -> Self {
        let now = Duration::ZERO;

        let mut guard_device = LocalDevice::new(
            "i:12345",
            "ControlBox-1",
            DeviceType::ElectricitySupplySystem,
        )
        .unwrap();
        guard_device
            .add_entity(
                LocalEntity::new([1], EntityType::GridGuard)
                    .with_feature(limitation::client_feature(1))
                    .with_feature(limitation::device_diagnosis_feature(2)),
            )
            .unwrap();
        let client = guard_device.address_of(&[1], 1);
        let guard_diagnosis = guard_device.address_of(&[1], 2);
        let mut guard_engine = Engine::new(guard_device);
        guard_engine.add_use_case([1], 1, &lpc::ENERGY_GUARD);
        let guard = EnergyGuardActor::new(lpc::DIRECTION, client, guard_diagnosis, now);

        let mut pump_device =
            LocalDevice::new("i:67890", "HeatPump-1", DeviceType::HeatGenerationSystem).unwrap();
        pump_device
            .add_entity(
                LocalEntity::new([1], EntityType::HeatPumpAppliance)
                    .with_feature(limitation::load_control_feature(1))
                    .with_feature(limitation::device_configuration_feature(2))
                    .with_feature(limitation::device_diagnosis_feature(3))
                    .with_feature(limitation::electrical_connection_feature(4)),
            )
            .unwrap();
        let load_control = pump_device.address_of(&[1], 1);
        let configuration = pump_device.address_of(&[1], 2);
        let diagnosis = pump_device.address_of(&[1], 3);
        let electrical = pump_device.address_of(&[1], 4);
        let mut pump_engine = Engine::new(pump_device);
        pump_engine.add_use_case([1], 1, &lpc::CONTROLLABLE_SYSTEM);
        let pump = ControllableSystemActor::new(
            ControllableSystem::new(
                CsConfig::new(FAILSAFE_WATTS, Duration::from_secs(2 * 3_600))
                    .with_nominal_max(NOMINAL_MAX_WATTS),
                now,
            ),
            lpc::DIRECTION,
            load_control,
            configuration,
            diagnosis,
        )
        .with_electrical_connection(electrical);
        pump.install(&mut pump_engine, now);

        Self {
            guard_engine,
            guard,
            pump_engine,
            pump,
            now,
            reports: Vec::new(),
            decisions: Vec::new(),
            trace: Vec::new(),
        }
    }

    /// Discovery both ways, and the Energy Guard taking control.
    pub fn commission(&mut self) {
        let guard_nm = node_management(self.guard_engine.device().address());
        let pump_nm = node_management(self.pump_engine.device().address());
        for function in [
            Function::NodeManagementDetailedDiscoveryData,
            Function::NodeManagementUseCaseData,
        ] {
            self.guard_engine
                .read(&pump_nm, &guard_nm, function.clone(), self.now);
            self.pump_engine
                .read(&guard_nm, &pump_nm, function, self.now);
        }
        self.settle();

        let device = self.pump_engine.device().address().clone();
        let remote = self.guard_engine.peer(&device).expect("the heat pump");
        let peer = limitation::locate(remote, lpc::DIRECTION).expect("a Controllable System");
        self.guard.attach(&mut self.guard_engine, peer, self.now);
        self.settle();
    }

    /// Carries datagrams both ways until neither side has anything left to say.
    pub fn settle(&mut self) {
        for _ in 0..128 {
            let mut moved = false;
            while let Some(datagram) = self.guard_engine.poll_transmit() {
                self.trace.push(("box", datagram.clone()));
                self.pump_engine
                    .handle_datagram(&round_trip(&datagram), self.now);
                moved = true;
            }
            while let Some(datagram) = self.pump_engine.poll_transmit() {
                self.trace.push(("pump", datagram.clone()));
                self.guard_engine
                    .handle_datagram(&round_trip(&datagram), self.now);
                moved = true;
            }
            let events: Vec<SpineEvent> =
                core::iter::from_fn(|| self.pump_engine.poll_event()).collect();
            for event in &events {
                if let Some(decision) =
                    self.pump
                        .handle_event(&mut self.pump_engine, event, self.now)
                {
                    self.decisions.push(decision);
                    moved = true;
                }
            }
            let events: Vec<SpineEvent> =
                core::iter::from_fn(|| self.guard_engine.poll_event()).collect();
            for event in &events {
                if let Some(report) =
                    self.guard
                        .handle_event(&mut self.guard_engine, event, self.now)
                {
                    self.reports.push(report);
                    moved = true;
                }
            }
            if !moved {
                return;
            }
        }
        panic!("the exchange did not settle");
    }

    /// Fires both actors' timers, then settles.
    pub fn advance(&mut self, by: Duration) {
        self.now += by;
        let reports = self.guard.handle_timeout(&mut self.guard_engine, self.now);
        self.reports.extend(reports);
        if let Some(decision) = self.pump.handle_timeout(&mut self.pump_engine, self.now) {
            self.decisions.push(decision);
        }
        self.guard_engine.handle_timeout(self.now);
        self.pump_engine.handle_timeout(self.now);
        self.settle();
    }

    pub fn device(&self) -> eebus::model::AddressDevice {
        self.pump_engine.device().address().clone()
    }

    pub fn guard_client(&self) -> eebus::model::FeatureAddress {
        self.guard_engine.device().address_of(&[1], 1)
    }

    pub fn pump_load_control(&self) -> eebus::model::FeatureAddress {
        self.pump_engine.device().address_of(&[1], 1)
    }

    /// Drops what has been reported so far, so that a test's counts are about what the
    /// test did.
    ///
    /// Commissioning is not silent: implementation guide §2.11 makes the Energy Guard owe
    /// an opening write on the limit as soon as it is bound, whether or not the
    /// application has decided anything, so a freshly commissioned pair already has one
    /// accepted deactivation behind it.
    pub fn forget_history(&mut self) {
        self.reports.clear();
        self.decisions.clear();
    }

    /// The datagrams that have crossed, as they appeared on the wire.
    pub fn wire(&self) -> Vec<String> {
        self.trace
            .iter()
            .map(|(from, datagram)| {
                let json = eebus::model::to_json(datagram).expect("encodable");
                alloc_format(from, &json)
            })
            .collect()
    }

    pub fn accepted(&self) -> Vec<LimitWrite> {
        self.reports
            .iter()
            .filter_map(|r| match r {
                GuardEvent::LimitAccepted { limit, .. } => Some(*limit),
                _ => None,
            })
            .collect()
    }

    pub fn refused(&self) -> Vec<LimitWrite> {
        self.reports
            .iter()
            .filter_map(|r| match r {
                GuardEvent::LimitRefused { limit, .. } => Some(*limit),
                _ => None,
            })
            .collect()
    }
}

pub fn round_trip(datagram: &Datagram) -> Datagram {
    let wire = eebus::model::to_json(datagram).expect("encode");
    let decoded = eebus::model::from_json_str(&wire).expect("decode");
    assert_eq!(&decoded, datagram, "the datagram survives the wire");
    decoded
}
/// One traced datagram as a line: who sent it, and exactly what went out.
fn alloc_format(from: &str, json: &str) -> String {
    format!("{from:>4} | {json}")
}
