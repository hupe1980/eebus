//! E-mobility: the use cases between a wallbox, the car plugged into it, and the energy
//! manager that has to keep the house fuse intact.
//!
//! The grid use cases in [`limitation`](crate::usecases::limitation) work in watts over
//! minutes, because a heat pump's compressor cannot be asked for more. Charging works in
//! amperes per phase over seconds: the current a car draws is set by a pilot signal it
//! can follow immediately, and the fuse it would otherwise blow does not wait. That is
//! why [`opev`]'s heartbeat timeout is four seconds where LPC's is a hundred and twenty.
//!
//! What is here:
//!
//! * [`evsecc`] — **EVSE Commissioning and Configuration**: how a wallbox introduces
//!   itself to an energy manager, and how it reports being broken. Everything else in the
//!   family assumes this has happened.
//! * [`opev`] — **Overload Protection by EV Charging Current Curtailment**: the energy
//!   manager holds the charging current below what the supply can carry, per phase, and
//!   the car falls back to a safe current the moment it stops hearing from it.

pub mod evsecc;
pub mod opev;

/// Use-case names as they appear on the wire.
pub mod names {
    /// EVSE Commissioning and Configuration.
    pub const EVSECC: &str = "evseCommissioningAndConfiguration";
    /// Overload Protection by EV Charging Current Curtailment.
    pub const OPEV: &str = "overloadProtectionByEvChargingCurrentCurtailment";
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
    /// both, which is what [`super::opev::ENERGY_GUARD_ACTORS`] is for.
    pub const CEM: &str = "CEM";
    /// The energy manager, in the role of protecting a limit.
    pub const ENERGY_GUARD: &str = "EnergyGuard";
}
