//! E-mobility: the use cases between a wallbox, the car plugged into it, and the energy
//! manager that has to keep the house fuse intact.
//!
//! The grid use cases in [`limitation`](crate::usecases::limitation) work in watts over
//! minutes, because a heat pump's compressor cannot be asked for more. Charging works in
//! amperes per phase over seconds: the current a car draws is set by a pilot signal it
//! can follow immediately, and the fuse it would otherwise blow does not wait. That is
//! why [`opev`]'s heartbeat timeout is four seconds where LPC's is a hundred and twenty.
//!
//! A car is also the one appliance in a building that arrives, says what it is, and leaves
//! again — which is why half of this family is about finding out what is on the end of the
//! cable before anything can be asked of it.
//!
//! Six use cases, and they layer:
//!
//! **Who is there**
//!
//! * [`evsecc`] — **EVSE Commissioning and Configuration**: how a wallbox introduces
//!   itself to an energy manager, and how it reports being broken.
//! * [`evcc`] — **EV Commissioning and Configuration**: what the *car* is. Eight
//!   scenarios, and scenario 2 is the gate on the rest of the family: a car on IEC 61851
//!   has a pilot wire and nothing else, and cannot be asked its state of charge or told a
//!   plan, however willing the manager is.
//!
//! **What it may take**
//!
//! * [`opev`] — **Overload Protection by EV Charging Current Curtailment**: the manager
//!   holds the charging current below what the supply can carry, per phase, and the car
//!   falls back to a safe current the moment it stops hearing from it. An *obligation*.
//! * [`oscev`] — **Optimization of Self-Consumption During EV Charging**: the same
//!   exchange, offering the surplus the roof is producing. A *recommendation*.
//!
//! Those two are one implementation in [`charging`], because their specifications are one
//! specification with two words changed — and the two words are exactly what a car must not
//! confuse. Both usually run over the same connection at once: the fuse says what the car
//! may never exceed, the roof says what is cheap right now, and the obligation wins.
//!
//! **What it actually did**
//!
//! * [`evcem`] — **EV Charging Electricity Measurement**: the current, power and energy
//!   that really flowed, per phase. What makes [`opev`] checkable rather than asserted.
//! * [`evsoc`] — **EV State of Charge**: how full the battery is, how big it is, how
//!   healthy, and how far the car can still go.
//!
//! Both are built on [`monitoring`](crate::usecases::monitoring), the same machinery MPC
//! and MGCP use: one implementation of "describe a measurement twice and read it back",
//! serving four use cases.
//!
//! What is **not** here: Coordinated EV Charging (CEVC), which is a different shape from
//! everything above — three actors, power sequences, incentive tables and a negotiated
//! charging plan rather than a ceiling.

pub mod charging;
pub mod evcc;
pub mod evcem;
pub mod evsecc;
pub mod evsoc;
pub mod opev;
pub mod oscev;

/// Use-case names as they appear on the wire.
pub mod names {
    /// EV Commissioning and Configuration.
    pub const EVCC: &str = "evCommissioningAndConfiguration";
    /// EV Charging Electricity Measurement.
    pub const EVCEM: &str = "evChargingElectricityMeasurement";
    /// EVSE Commissioning and Configuration.
    pub const EVSECC: &str = "evseCommissioningAndConfiguration";
    /// EV State of Charge.
    pub const EVSOC: &str = "evStateOfCharge";
    /// Overload Protection by EV Charging Current Curtailment.
    pub const OPEV: &str = "overloadProtectionByEvChargingCurrentCurtailment";
    /// Optimization of Self-Consumption During EV Charging.
    pub const OSCEV: &str = "optimizationOfSelfConsumptionDuringEvCharging";
}

/// Actor names as they appear in `nodeManagementUseCaseData.useCaseInformation.actor`.
pub mod actors {
    /// The wallbox.
    pub const EVSE: &str = "EVSE";
    /// The car.
    pub const EV: &str = "EV";
    /// The energy manager, in the role of managing others.
    ///
    /// OPEV §3.2.2.1 lets an actor call itself either this or `EnergyGuard`, and says
    /// which to prefer: `CEM` for something that manages energy, `EnergyGuard` for
    /// something that only guards. A wallbox looking for a counterpart has to accept
    /// both, which is what [`super::charging::ENERGY_GUARD_ACTORS`] is for.
    pub const CEM: &str = "CEM";
    /// The actor that reads a car's battery (EVSOC).
    pub const MONITORING_APPLIANCE: &str =
        crate::usecases::descriptor::actors::MONITORING_APPLIANCE;
    /// The energy manager, in the role of protecting a limit.
    pub const ENERGY_GUARD: &str = "EnergyGuard";
}
