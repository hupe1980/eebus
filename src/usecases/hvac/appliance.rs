//! The client side of the HVAC family, wired to a SPINE engine.
//!
//! [`HvacApplianceActor`] serves **both** client actors of the family, because they read
//! the same data and differ only in whether they also write:
//!
//! | | reads | writes |
//! |---|---|---|
//! | Monitoring Appliance | [`mdsf`](super::mdsf), [`mrhsf`](super::mrhsf), [`mrcsf`](super::mrcsf) | — |
//! | Configuration Appliance | [`cdsf`](super::cdsf), [`crhsf`](super::crhsf), [`crcsf`](super::crcsf), [`cdt`](super::cdt), [`crht`](super::crht), [`crct`](super::crct) | the mode, the overrun, the temperature |
//!
//! What it holds is the bookkeeping an appliance would otherwise keep itself: a reader per
//! system function, a reader per setpoint scope, routing by feature address, change
//! detection so a re-notified mode is not reported as a change, and the join between the
//! two halves that makes a temperature write mean anything.
//!
//! # One unit, several use cases
//!
//! §3.2.2.2.1 puts at most one `HVAC` feature and one `Setpoint` feature on an entity, so
//! everything served from one entity arrives on the same two addresses: a room that heats
//! and cools publishes both system functions in the *same* lists and both setpoints under
//! the *same* `roomAirTemperature`. So the actor tracks a **unit** — a [`UnitId`], device
//! and entity — with a reader per system function attached to it and one per setpoint
//! scope, and feeds every payload to all of them. Each keeps what belongs to it.
//!
//! ```no_run
//! # use core::time::Duration;
//! use eebus::model::HvacOperationModeType;
//! use eebus::usecases::hvac::{self, HvacApplianceActor, cdsf, cdt};
//! # fn example(
//! #     engine: &mut eebus::spine::Engine,
//! #     remote: &eebus::spine::RemoteDevice,
//! #     client: &eebus::model::FeatureAddress,
//! #     now: Duration,
//! # ) -> Option<()> {
//! let mut appliance = HvacApplianceActor::new(client.clone());
//!
//! // Both use cases of one circuit: the mode and the temperature, on one entity.
//! appliance.attach(engine, cdsf::locate(remote)?, now);
//! appliance.attach(engine, cdt::locate(remote)?, now);
//!
//! // …then, once the replies have been fed to `handle_event`:
//! let unit = appliance.units().next()?.clone();
//! appliance.set_mode(engine, &unit, &hvac::DHW, &HvacOperationModeType::On, now).ok()?;
//! appliance.set_temperature(engine, &unit, &hvac::DHW, 60.0, now).ok()?;
//! # Some(())
//! # }
//! ```

use alloc::vec::Vec;
use core::time::Duration;

use crate::model::{
    CmdData, FeatureAddress, HvacOperationModeId, HvacOperationModeType, HvacSystemFunctionType,
    MsgCounter, ScopeType, SetpointId,
};
use crate::spine::{Engine, SpineEvent};
use crate::usecases::UnitId;

use super::peer::{Following, HvacPeer};
use super::setpoint::{SetpointEffect, Setpoints, WriteRefused};
use super::system_function::{ModeRefused, OverrunReport, SystemFunction};

/// What an HVAC appliance learned.
///
/// Only *changes* are reported. A server notifies its whole `HVAC` feature, so a payload
/// carrying an unchanged mode arrives every time anything else on that feature moves, and
/// an application told about it again would re-derive a decision that did not change.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum HvacEvent {
    /// A system function can now be reported: it has an identifier, its modes, and a
    /// current one.
    ///
    /// Scenario 1 of whichever use case is following it, and the point from which
    /// [`set_mode`](HvacApplianceActor::set_mode) will build a write rather than refuse.
    FunctionDescribed {
        /// The unit.
        unit: UnitId,
        /// Which system function.
        function: HvacSystemFunctionType,
    },
    /// A system function changed operation mode.
    ///
    /// Including when the *appliance* changed it — somebody at the wall panel, or its own
    /// scheduler. That is what the subscription is for, and it is the reason a manager
    /// cannot treat the mode it last wrote as the mode in force.
    ModeChanged {
        /// The unit.
        unit: UnitId,
        /// Which system function.
        function: HvacSystemFunctionType,
        /// The mode it is in now, as the appliance named it.
        mode: HvacOperationModeType,
        /// And its identifier, which is what the setpoint relations are keyed by.
        id: HvacOperationModeId,
    },
    /// An overrun started, or stopped ([CDSF-002], [CDSF-003]).
    ///
    /// Only the hot water has one: somebody pressed the one-time loading button in the
    /// bathroom. A manager that sees the tank drawing power while the mode says `off` is
    /// not looking at a fault.
    OverrunChanged {
        /// The unit.
        unit: UnitId,
        /// Which system function it affects.
        function: HvacSystemFunctionType,
        /// What it is doing now.
        report: OverrunReport,
    },
    /// An overrun announced that it had **just** finished ([MDSF-002]).
    ///
    /// A one-shot notification rather than a state, and separate from
    /// [`OverrunChanged`](Self::OverrunChanged) for that reason: an application that stored
    /// `finished` would go on reporting a completed heating for as long as nothing else
    /// arrived.
    OverrunFinished {
        /// The unit.
        unit: UnitId,
        /// Which system function it affected.
        function: HvacSystemFunctionType,
    },
    /// A setpoint's value changed.
    ///
    /// Again including when the appliance changed it. Whether it is the setpoint the
    /// current mode actually reads is [`SetpointEffect`]'s question, and
    /// [`effect`](HvacApplianceActor::effect) answers it.
    SetpointChanged {
        /// The unit.
        unit: UnitId,
        /// Which setpoint, in the appliance's own numbering.
        setpoint: SetpointId,
        /// What it is now, in the unit the appliance published for it.
        degrees: f64,
    },
}

/// One system function of one unit, and what has already been reported about it.
#[derive(Debug)]
struct TrackedFunction {
    kind: HvacSystemFunctionType,
    reader: SystemFunction,
    described: bool,
    mode: Option<HvacOperationModeId>,
    overrun: Option<OverrunReport>,
}

/// One scope of setpoints on a unit, and the system functions whose temperature use cases
/// write it.
#[derive(Debug)]
struct TrackedSetpoints {
    scope: ScopeType,
    reader: Setpoints,
    /// Which system functions were attached against this scope: `dhw` for
    /// [`cdt`](super::cdt), `heating` for [`crht`](super::crht), `cooling` for
    /// [`crct`](super::crct) — the last two sharing one reader, because they share
    /// `roomAirTemperature` and it is the *relation's* `systemFunctionId` that tells their
    /// setpoints apart.
    functions: Vec<HvacSystemFunctionType>,
    /// The value last reported for each setpoint, so an unchanged one is not reported
    /// again.
    seen: Vec<(SetpointId, f64)>,
}

/// One entity of a peer, with every HVAC use case attached to it.
#[derive(Debug)]
struct TrackedUnit {
    id: UnitId,
    /// The `HVAC` feature every use case on this entity shares.
    hvac: FeatureAddress,
    /// Its `Setpoint` feature, once a temperature use case has been attached.
    setpoint: Option<FeatureAddress>,
    functions: Vec<TrackedFunction>,
    setpoints: Vec<TrackedSetpoints>,
}

impl TrackedUnit {
    fn function(&self, kind: &HvacSystemFunctionType) -> Option<&TrackedFunction> {
        self.functions.iter().find(|f| &f.kind == kind)
    }

    /// The setpoint reader the temperature use case of `kind` writes.
    ///
    /// Keyed by the system function rather than taken as "the one this unit has", because
    /// a conformant appliance can have two: §3.2.1.1 puts a DHW circuit on its own entity
    /// type, and a device that ignored that would have `dhwTemperature` and
    /// `roomAirTemperature` on one `Setpoint` feature — where writing 60 to the wrong one
    /// heats a living room.
    fn setpoints_of(&self, kind: &HvacSystemFunctionType) -> Option<&Setpoints> {
        self.setpoints
            .iter()
            .find(|s| s.functions.contains(kind))
            .map(|s| &s.reader)
    }

    fn serves(&self, feature: &FeatureAddress) -> bool {
        if feature.device.as_ref() != Some(&self.id.device) {
            return false;
        }
        core::iter::once(&self.hvac)
            .chain(self.setpoint.as_ref())
            .any(|known| crate::spine::same_feature(known, feature))
    }
}

/// A Monitoring or Configuration Appliance, wired to a SPINE engine.
///
/// **There is no binding**, and that is the specification's own instruction: all nine use
/// cases say "Binding SHOULD NOT be used for this Scenario", including the six that write.
/// See [`WriteBinding`](crate::spine::WriteBinding).
///
/// **There are no timers.** Like
/// [`MonitoringApplianceActor`](crate::usecases::monitoring::MonitoringApplianceActor),
/// this actor reads, subscribes and resolves; the engine owns the retry ladder and the
/// response deadlines, so there is no `handle_timeout` and no `poll_timeout` to fold into a
/// driver's sleep.
#[derive(Debug)]
pub struct HvacApplianceActor {
    client: FeatureAddress,
    units: Vec<TrackedUnit>,
}

impl HvacApplianceActor {
    /// An appliance reading and writing from `client`, its single `Generic` client feature.
    ///
    /// One feature for every server it talks to, whatever their types: the LPC
    /// implementation guide §3.3 asks an actor to "use a client Feature with featureType
    /// `Generic` for all its client functionality".
    /// [`limitation::client_feature`](crate::usecases::limitation::client_feature) builds
    /// it, and it is the same one an Energy Guard and a Monitoring Appliance hold — an
    /// application running all three has one client feature, not three.
    pub fn new(client: FeatureAddress) -> Self {
        Self {
            client,
            units: Vec::new(),
        }
    }

    /// Follows one use case of one peer: subscribes, reads, and keeps the reader it needs.
    ///
    /// [`HvacPeer::follow`] plus the bookkeeping. Attach every use case the application
    /// runs against a unit — a DHW circuit is usually [`cdsf`](super::cdsf) and
    /// [`cdt`](super::cdt) together, a room [`crhsf`](super::crhsf),
    /// [`crcsf`](super::crcsf), [`crht`](super::crht) and [`crct`](super::crct) — and they
    /// gather on one unit, because they are served from one entity.
    ///
    /// Which readers exist follows from what is attached, which is why attaching only the
    /// temperature half is a thing this reports rather than papers over: without a system
    /// function to say which mode the appliance is in,
    /// [`set_temperature`](Self::set_temperature) answers
    /// [`WriteRefused::ModeUnknown`] rather than writing into a mode that may read some
    /// other setpoint. [xDT-005] makes the mode use case mandatory for a server that serves
    /// the temperature one for exactly this reason.
    ///
    /// Attaching the same use case again restarts the exchange, which is what a
    /// reconnection needs: the subscription did not survive it, and the appliance may have
    /// changed its mind while the connection was down. What it has already learned is
    /// **kept** — the replies will replace it, and a manager reading the actor in between
    /// gets the last known state rather than nothing.
    pub fn attach(&mut self, engine: &mut Engine, peer: HvacPeer, now: Duration) -> Following {
        let following = peer.follow(engine, &self.client, now);
        let id = peer.id();

        let index = match self.units.iter().position(|u| u.id == id) {
            Some(index) => index,
            None => {
                self.units.push(TrackedUnit {
                    id,
                    hvac: peer.hvac.clone(),
                    setpoint: None,
                    functions: Vec::new(),
                    setpoints: Vec::new(),
                });
                self.units.len() - 1
            }
        };
        let unit = &mut self.units[index];
        unit.hvac = peer.hvac.clone();
        if let Some(feature) = peer.setpoint.clone() {
            unit.setpoint = Some(feature);
        }

        match peer.subject.scope.clone() {
            // A temperature use case: a setpoint reader for its scope, and no system
            // function of its own — the relations it reads are keyed by one the *mode* use
            // case identifies.
            Some(scope) => {
                let function = peer.subject.function.clone();
                match unit.setpoints.iter_mut().find(|s| s.scope == scope) {
                    Some(tracked) => {
                        if !tracked.functions.contains(&function) {
                            tracked.functions.push(function);
                        }
                    }
                    None => unit.setpoints.push(TrackedSetpoints {
                        reader: Setpoints::of(scope.clone()),
                        scope,
                        functions: alloc::vec![function],
                        seen: Vec::new(),
                    }),
                }
            }
            // A system-function use case: a reader for its kind. An entity may have
            // several — a room heats and cools from one feature — and each is its own.
            None => {
                if unit.function(&peer.subject.function).is_none() {
                    unit.functions.push(TrackedFunction {
                        kind: peer.subject.function.clone(),
                        reader: peer.subject.system_function(),
                        described: false,
                        mode: None,
                        overrun: None,
                    });
                }
            }
        }
        following
    }

    /// Stops following a unit.
    pub fn detach(&mut self, unit: &UnitId) {
        self.units.retain(|u| &u.id != unit);
    }

    /// Stops following every unit of a device — what a lost connection removes.
    ///
    /// A device that goes away takes all of its entities with it, and an appliance holding
    /// one room of a gateway and not the other three is worse than one holding none.
    pub fn detach_device(&mut self, device: &crate::model::AddressDevice) {
        self.units.retain(|u| &u.id.device != device);
    }

    /// The units being followed.
    pub fn units(&self) -> impl Iterator<Item = &UnitId> {
        self.units.iter().map(|u| &u.id)
    }

    /// What is known about one system function of a unit.
    ///
    /// [`None`] until a system-function use case has been attached for that kind.
    pub fn system_function(
        &self,
        unit: &UnitId,
        function: &HvacSystemFunctionType,
    ) -> Option<&SystemFunction> {
        Some(&self.unit(unit)?.function(function)?.reader)
    }

    /// What is known about the setpoints a system function's temperature use case writes.
    ///
    /// [`None`] until such a use case has been attached — [`cdt`](super::cdt) for `dhw`,
    /// [`crht`](super::crht) for `heating`, [`crct`](super::crct) for `cooling`. The last
    /// two share one reader, because they share `roomAirTemperature`.
    pub fn setpoints(
        &self,
        unit: &UnitId,
        function: &HvacSystemFunctionType,
    ) -> Option<&Setpoints> {
        self.unit(unit)?.setpoints_of(function)
    }

    /// The mode a system function is in, by name.
    pub fn mode(
        &self,
        unit: &UnitId,
        function: &HvacSystemFunctionType,
    ) -> Option<&HvacOperationModeType> {
        self.system_function(unit, function)?.mode()
    }

    /// The temperature the appliance is **currently** working to, and its setpoint.
    ///
    /// The join both halves exist for: the system function's identifier, the mode it is in,
    /// the relation keyed by both, and the value of the setpoint that names. [`None`] where
    /// any of those is missing, or where the current mode reads no single setpoint — see
    /// [`Setpoints::current_setpoint`].
    pub fn temperature(
        &self,
        unit: &UnitId,
        function: &HvacSystemFunctionType,
    ) -> Option<(SetpointId, f64)> {
        let unit = self.unit(unit)?;
        let state = &unit.function(function)?.reader;
        let setpoints = unit.setpoints_of(function)?;
        let id = setpoints.current_setpoint(state).ok()?;
        Some((id, setpoints.temperature(id)?))
    }

    /// What writing one setpoint of a unit would actually do.
    ///
    /// [`Setpoints::effect_of`] with both halves supplied.
    pub fn effect(
        &self,
        unit: &UnitId,
        function: &HvacSystemFunctionType,
        setpoint: SetpointId,
    ) -> SetpointEffect {
        let Some(unit) = self.unit(unit) else {
            return SetpointEffect::Unknown;
        };
        let (Some(tracked), Some(setpoints)) =
            (unit.function(function), unit.setpoints_of(function))
        else {
            return SetpointEffect::Unknown;
        };
        setpoints.effect_of(setpoint, &tracked.reader)
    }

    fn unit(&self, id: &UnitId) -> Option<&TrackedUnit> {
        self.units.iter().find(|u| &u.id == id)
    }

    // ---- what a Configuration Appliance writes ------------------------------------------

    /// Puts a system function into an operation mode, by name (scenario 1).
    ///
    /// The name rather than the identifier, because the identifier is the appliance's own
    /// and resolving it through what the appliance described is the only correct way to get
    /// one. Refused where the appliance has not described the function, where it does not
    /// relate that mode to it — a device describes every mode it has once and relates a
    /// subset to each function, so `eco` may be a hot water mode and not a room heating
    /// one — or where it published `isOperationModeIdChangeable: false`.
    ///
    /// No binding, and none is requested: §3.4.1.1 of all six says not to use one.
    pub fn set_mode(
        &self,
        engine: &mut Engine,
        unit: &UnitId,
        function: &HvacSystemFunctionType,
        mode: &HvacOperationModeType,
        now: Duration,
    ) -> Result<MsgCounter, ModeRefused> {
        let (feature, write) = {
            let unit = self.unit(unit).ok_or(ModeRefused::FunctionUnknown)?;
            let tracked = unit
                .function(function)
                .ok_or(ModeRefused::FunctionUnknown)?;
            (unit.hvac.clone(), tracked.reader.set_mode_named(mode)?)
        };
        Ok(self.send(engine, &feature, write, now))
    }

    /// Starts a system function's overrun — the one-time hot water loading ([CDSF-002]).
    ///
    /// Refused where the appliance described no overrun of the kind this use case carries.
    /// Starting one already running is *not* refused: the specification makes it
    /// idempotent, and a manager re-asserting a state it wants after a reconnection is
    /// doing the right thing.
    pub fn start_overrun(
        &self,
        engine: &mut Engine,
        unit: &UnitId,
        function: &HvacSystemFunctionType,
        now: Duration,
    ) -> Result<MsgCounter, ModeRefused> {
        self.overrun(engine, unit, function, now, true)
    }

    /// Stops it ([CDSF-003]).
    pub fn stop_overrun(
        &self,
        engine: &mut Engine,
        unit: &UnitId,
        function: &HvacSystemFunctionType,
        now: Duration,
    ) -> Result<MsgCounter, ModeRefused> {
        self.overrun(engine, unit, function, now, false)
    }

    fn overrun(
        &self,
        engine: &mut Engine,
        unit: &UnitId,
        function: &HvacSystemFunctionType,
        now: Duration,
        start: bool,
    ) -> Result<MsgCounter, ModeRefused> {
        let (feature, write) = {
            let unit = self.unit(unit).ok_or(ModeRefused::NoOverrun)?;
            let tracked = unit.function(function).ok_or(ModeRefused::NoOverrun)?;
            let write = if start {
                tracked.reader.start_overrun()?
            } else {
                tracked.reader.stop_overrun()?
            };
            (unit.hvac.clone(), write)
        };
        Ok(self.send(engine, &feature, write, now))
    }

    /// Sets the temperature the appliance is **currently reading** for a system function.
    ///
    /// What an application means by "heat the water to 60", and the one call in this crate
    /// that resolves the whole chain: the system function's identifier, the mode it is in,
    /// the setpoint that mode relates to, and the constraints the appliance published for
    /// it. Every number in that chain is the appliance's own.
    ///
    /// Refused rather than sent where it would change nothing — a setpoint written into a
    /// mode the appliance is not in is applied, acknowledged, and heats nothing. An overrun
    /// running over the top of it is *not* a refusal: the write lands where it was meant to
    /// and takes effect when the overrun ends.
    ///
    /// [`WriteRefused::SeveralSetpoints`] where the current mode relates to more than one,
    /// which [xDT-003/1] permits `auto` to do: which of them applies is the appliance's own
    /// business, so name one with [`set_setpoint`](Self::set_setpoint).
    pub fn set_temperature(
        &self,
        engine: &mut Engine,
        unit: &UnitId,
        function: &HvacSystemFunctionType,
        degrees: f64,
        now: Duration,
    ) -> Result<MsgCounter, WriteRefused> {
        let (feature, write) = {
            let unit = self.unit(unit).ok_or(WriteRefused::ModeUnknown)?;
            let tracked = unit.function(function).ok_or(WriteRefused::ModeUnknown)?;
            let setpoints = unit
                .setpoints_of(function)
                .ok_or(WriteRefused::UnknownSetpoint)?;
            let feature = unit.setpoint.clone().ok_or(WriteRefused::UnknownSetpoint)?;
            (feature, setpoints.write_current(degrees, &tracked.reader)?)
        };
        Ok(self.send(engine, &feature, write, now))
    }

    /// Sets one named setpoint, checked against the mode the appliance is in.
    ///
    /// For a mode that reads several, and for a manager that has asked
    /// [`Setpoints::for_mode`] which they are. Otherwise
    /// [`set_temperature`](Self::set_temperature) is the call that does not require the
    /// application to know the appliance's numbering.
    pub fn set_setpoint(
        &self,
        engine: &mut Engine,
        unit: &UnitId,
        function: &HvacSystemFunctionType,
        setpoint: SetpointId,
        degrees: f64,
        now: Duration,
    ) -> Result<MsgCounter, WriteRefused> {
        let (feature, write) = {
            let unit = self.unit(unit).ok_or(WriteRefused::ModeUnknown)?;
            let tracked = unit.function(function).ok_or(WriteRefused::ModeUnknown)?;
            let setpoints = unit
                .setpoints_of(function)
                .ok_or(WriteRefused::UnknownSetpoint)?;
            let feature = unit.setpoint.clone().ok_or(WriteRefused::UnknownSetpoint)?;
            (
                feature,
                setpoints.write_effective(setpoint, degrees, &tracked.reader)?,
            )
        };
        Ok(self.send(engine, &feature, write, now))
    }

    /// Sets one setpoint **without** asking whether the current mode reads it.
    ///
    /// For a manager pre-loading the setpoint of a mode it is about to ask for, which is a
    /// sensible thing to do and the one case the gate above would get wrong. The appliance's
    /// own constraints are still checked.
    pub fn preload_setpoint(
        &self,
        engine: &mut Engine,
        unit: &UnitId,
        function: &HvacSystemFunctionType,
        setpoint: SetpointId,
        degrees: f64,
        now: Duration,
    ) -> Result<MsgCounter, WriteRefused> {
        let (feature, write) = {
            let unit = self.unit(unit).ok_or(WriteRefused::UnknownSetpoint)?;
            let setpoints = unit
                .setpoints_of(function)
                .ok_or(WriteRefused::UnknownSetpoint)?;
            let feature = unit.setpoint.clone().ok_or(WriteRefused::UnknownSetpoint)?;
            (feature, setpoints.write(setpoint, degrees)?)
        };
        Ok(self.send(engine, &feature, write, now))
    }

    /// Every write in this family is partial and unbound.
    ///
    /// Partial because these are *list* functions carrying every system function and every
    /// setpoint the appliance has: a full write would replace the list, and a manager
    /// setting the hot water mode would silently clear the living room's.
    fn send(
        &self,
        engine: &mut Engine,
        feature: &FeatureAddress,
        write: CmdData,
        now: Duration,
    ) -> MsgCounter {
        engine.write(feature, &self.client, write, true, now)
    }

    // ---- what arrives -------------------------------------------------------------------

    /// Feeds one engine event to the actor, and reports what changed.
    ///
    /// **A list, not an option**, which is where this actor differs from the others here
    /// and for a reason the payloads force: one `hvacSystemFunctionListData` carries every
    /// system function the appliance has, so a single notification legitimately moves a
    /// room's heating *and* its cooling. Reporting one and dropping the other would be
    /// worse than the extra allocation.
    ///
    /// Empty for an event this actor has nothing to do with — every event from a feature it
    /// does not follow, and every payload that changed nothing it is watching.
    pub fn handle_event(&mut self, event: &SpineEvent) -> Vec<HvacEvent> {
        // `resolved`, not `data`. A notification may be partial, and an omitted element
        // then means *unchanged* (SPINE IG §3.3): a `hvacSystemFunctionListData` that
        // carries only the function whose mode moved leaves every other function's mode
        // exactly where it was, and reading the fragment alone would report the rest as
        // having become unknown.
        let (feature, data) = match event {
            SpineEvent::ReplyReceived {
                feature, resolved, ..
            }
            | SpineEvent::DataNotified {
                feature, resolved, ..
            } => (feature, resolved),
            _ => return Vec::new(),
        };
        // By feature, not by device. Two `HVACRoom` entities of one gateway are two units
        // with two `HVAC` features, and a device-wide lookup would resolve both rooms'
        // notifications against whichever was attached first.
        let Some(index) = self.units.iter().position(|u| u.serves(feature)) else {
            return Vec::new();
        };
        self.units[index].apply(data)
    }
}

impl TrackedUnit {
    /// Takes one payload into every reader on this unit, and says what moved.
    ///
    /// Every reader sees every payload, which is safe because each recognises only what
    /// belongs to it: a `Setpoint` payload means nothing to a [`SystemFunction`], and of
    /// the four functions on the `HVAC` feature only the setpoint relations mean anything
    /// to [`Setpoints`].
    fn apply(&mut self, data: &CmdData) -> Vec<HvacEvent> {
        let mut events = Vec::new();
        let id = &self.id;

        for tracked in &mut self.functions {
            let before = (tracked.mode, tracked.overrun);
            if !tracked.reader.learn(data) {
                continue;
            }
            if !tracked.described && tracked.reader.is_complete() {
                tracked.described = true;
                events.push(HvacEvent::FunctionDescribed {
                    unit: id.clone(),
                    function: tracked.kind.clone(),
                });
            }
            tracked.mode = tracked.reader.mode_id();
            tracked.overrun = tracked.reader.overrun();

            if let Some(mode) = tracked.mode
                && before.0 != tracked.mode
                && let Some(kind) = tracked.reader.modes().kind_of(mode)
            {
                events.push(HvacEvent::ModeChanged {
                    unit: id.clone(),
                    function: tracked.kind.clone(),
                    mode: kind.clone(),
                    id: mode,
                });
            }
            if let Some(report) = tracked.overrun
                && before.1 != tracked.overrun
            {
                events.push(HvacEvent::OverrunChanged {
                    unit: id.clone(),
                    function: tracked.kind.clone(),
                    report,
                });
            }
            // A one-shot (Table 14). The reader's flag stays set until another overrun
            // payload replaces it, so it is asked only when the payload in hand is one:
            // otherwise the next mode change on this feature would re-announce a heating
            // that completed an hour ago.
            if matches!(data, CmdData::HvacOverrunListData(_))
                && tracked.reader.overrun_just_finished()
            {
                events.push(HvacEvent::OverrunFinished {
                    unit: id.clone(),
                    function: tracked.kind.clone(),
                });
            }
        }

        for tracked in &mut self.setpoints {
            if !tracked.reader.learn(data) {
                continue;
            }
            let known: Vec<SetpointId> = tracked.reader.temperature_setpoints().collect();
            for setpoint in known {
                let Some(degrees) = tracked.reader.temperature(setpoint) else {
                    continue;
                };
                match tracked.seen.iter_mut().find(|(id, _)| *id == setpoint) {
                    Some((_, stored)) if *stored == degrees => continue,
                    Some((_, stored)) => *stored = degrees,
                    None => tracked.seen.push((setpoint, degrees)),
                }
                events.push(HvacEvent::SetpointChanged {
                    unit: id.clone(),
                    setpoint,
                    degrees,
                });
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    use crate::model::{DeviceType, EntityType};
    use crate::spine::{LocalDevice, LocalEntity, RemoteDevice, detailed_discovery, use_case_data};
    use crate::usecases::hvac::{self, crhsf, crht, system_function};

    /// A gateway with two rooms, each an `HVACRoom` entity with its own `HVAC` feature.
    fn gateway() -> LocalDevice {
        let mut device =
            LocalDevice::new("i:46925", "HeatPump-1", DeviceType::HeatGenerationSystem).unwrap();
        for room in [1u32, 2] {
            device
                .add_entity(
                    LocalEntity::new([room], EntityType::HVACRoom)
                        .with_feature(crht::setpoint_feature(1))
                        .with_feature(crhsf::with_setpoints(2)),
                )
                .unwrap();
        }
        device
    }

    fn discovered(device: &LocalDevice) -> RemoteDevice {
        let mut remote = RemoteDevice::default();
        remote.apply_detailed_discovery(&detailed_discovery(device));
        remote.apply_use_case_data(&use_case_data(
            device,
            &[
                (vec![1], 2, &crhsf::HVAC_ROOM, vec![1]),
                (vec![2], 2, &crhsf::HVAC_ROOM, vec![1]),
                (vec![1], 2, &crht::HVAC_ROOM, vec![1]),
                (vec![2], 2, &crht::HVAC_ROOM, vec![1]),
            ],
        ));
        remote
    }

    fn manager() -> (crate::spine::Engine, crate::model::FeatureAddress) {
        let mut device =
            LocalDevice::new("i:12345", "CEM-1", DeviceType::EnergyManagementSystem).unwrap();
        device
            .add_entity(
                LocalEntity::new([1], EntityType::CEM)
                    .with_feature(crate::usecases::limitation::client_feature(1)),
            )
            .unwrap();
        let client = device.address_of(&[1], 1);
        (crate::spine::Engine::new(device), client)
    }

    fn notified(feature: &FeatureAddress, data: CmdData) -> SpineEvent {
        SpineEvent::DataNotified {
            feature: feature.clone(),
            data: data.clone(),
            resolved: data,
        }
    }

    fn modes() -> [HvacOperationModeType; 2] {
        [HvacOperationModeType::Auto, HvacOperationModeType::Off]
    }

    fn auto() -> HvacOperationModeId {
        crhsf::operation_mode_id(&HvacOperationModeType::Auto).expect("a numbered mode")
    }

    fn off() -> HvacOperationModeId {
        crhsf::operation_mode_id(&HvacOperationModeType::Off).expect("a numbered mode")
    }

    /// Commissions both rooms and returns the actor, the two units and the two features.
    fn two_rooms() -> (
        HvacApplianceActor,
        crate::spine::Engine,
        Vec<UnitId>,
        Vec<FeatureAddress>,
    ) {
        let device = gateway();
        let remote = discovered(&device);
        let (mut engine, client) = manager();
        let mut actor = HvacApplianceActor::new(client);

        let rooms = crhsf::locate_all(&remote);
        assert_eq!(rooms.len(), 2);
        let units: Vec<UnitId> = rooms.iter().map(HvacPeer::id).collect();
        let features: Vec<FeatureAddress> = rooms.iter().map(|r| r.hvac.clone()).collect();
        // Both halves of each room, which is the shape [CRHT-005] requires of a server
        // that offers the temperature one at all.
        for room in rooms.into_iter().chain(crht::locate_all(&remote)) {
            actor.attach(&mut engine, room, Duration::ZERO);
        }

        // The descriptions, to both rooms: they are separate features and each answers for
        // itself.
        for feature in &features {
            for payload in [
                crhsf::system_function_description(),
                crhsf::operation_mode_descriptions(&modes()).expect("two modes"),
                crhsf::operation_mode_relations(&modes()).expect("two modes"),
                crhsf::system_function_state(auto(), false, Some(true)),
            ] {
                actor.handle_event(&notified(feature, payload));
            }
        }
        (actor, engine, units, features)
    }

    /// Two rooms of one gateway are two units, and neither hears the other's notifications.
    ///
    /// The failure this guards against is silent: an actor keyed by device reports the
    /// bedroom's mode as the living room's, in the right vocabulary and with nothing
    /// out of place.
    #[test]
    fn two_rooms_of_one_gateway_are_two_units() {
        let (mut actor, _engine, units, features) = two_rooms();

        assert_eq!(actor.units().count(), 2);
        assert_ne!(units[0], units[1]);
        for unit in &units {
            assert_eq!(
                actor.mode(unit, &hvac::HEATING),
                Some(&HvacOperationModeType::Auto)
            );
        }

        // Only the second room's heating is turned off.
        let events = actor.handle_event(&notified(
            &features[1],
            crhsf::system_function_state(off(), false, Some(true)),
        ));
        assert_eq!(
            events,
            [HvacEvent::ModeChanged {
                unit: units[1].clone(),
                function: hvac::HEATING,
                mode: HvacOperationModeType::Off,
                id: off(),
            }]
        );
        assert_eq!(
            actor.mode(&units[0], &hvac::HEATING),
            Some(&HvacOperationModeType::Auto),
            "the other room did not move"
        );
    }

    /// A mode notified again is not a change, and is not reported as one.
    ///
    /// A server notifies its whole `HVAC` feature, so a payload carrying an unchanged mode
    /// arrives every time anything else on that feature moves.
    #[test]
    fn an_unchanged_mode_is_not_reported_again() {
        let (mut actor, _engine, units, features) = two_rooms();

        let again = actor.handle_event(&notified(
            &features[0],
            crhsf::system_function_state(auto(), false, Some(true)),
        ));
        assert_eq!(again, [], "nothing moved");

        let moved = actor.handle_event(&notified(
            &features[0],
            crhsf::system_function_state(off(), false, Some(true)),
        ));
        assert_eq!(moved.len(), 1);
        assert!(matches!(
            moved[0],
            HvacEvent::ModeChanged { ref unit, .. } if unit == &units[0]
        ));
    }

    /// An event from a feature this actor does not follow is not its business.
    #[test]
    fn a_feature_that_is_not_followed_is_ignored() {
        let (mut actor, _engine, _units, _features) = two_rooms();

        let stranger = LocalDevice::new("i:99999", "Other-1", DeviceType::HeatGenerationSystem)
            .unwrap()
            .address_of(&[1], 2);
        assert_eq!(
            actor.handle_event(&notified(
                &stranger,
                crhsf::system_function_state(off(), false, Some(true))
            )),
            [],
            "another device may well number an entity and a feature the same way"
        );
    }

    /// [xDT-003/1]: `auto` may read one to four setpoints, and which applies is the
    /// server's business — so "set the temperature" is refused rather than guessed.
    #[test]
    fn a_mode_reading_several_setpoints_is_not_guessed_at() {
        use crate::model::SetpointId;
        use crate::usecases::hvac::setpoint;

        let (mut actor, mut engine, units, features) = two_rooms();
        let unit = units[0].clone();
        let heating = hvac::system_function_id(&hvac::HEATING);

        // A room relating `auto` to two setpoints, which Table 10 permits.
        let day = SetpointId(1);
        let night = SetpointId(2);
        for payload in [
            setpoint::descriptions(&[
                (
                    day,
                    crht::TEMPERATURE_SCOPE,
                    crate::model::UnitOfMeasurement::DegC,
                    None,
                ),
                (
                    night,
                    crht::TEMPERATURE_SCOPE,
                    crate::model::UnitOfMeasurement::DegC,
                    None,
                ),
            ]),
            setpoint::constraints_of(&[(day, 16.0, 26.0, None), (night, 16.0, 26.0, None)]),
            setpoint::values(&[(day, 21.0), (night, 18.0)]),
            setpoint::relations(
                heating,
                &[(auto(), HvacOperationModeType::Auto, vec![day, night])],
            )
            .expect("well formed"),
        ] {
            actor.handle_event(&notified(&features[0], payload));
        }

        assert_eq!(
            actor.set_temperature(&mut engine, &unit, &hvac::HEATING, 21.0, Duration::ZERO),
            Err(WriteRefused::SeveralSetpoints { count: 2 }),
            "picking the first would be a guess about which one the room reads"
        );
        assert_eq!(actor.temperature(&unit, &hvac::HEATING), None);
        assert!(
            actor
                .set_setpoint(
                    &mut engine,
                    &unit,
                    &hvac::HEATING,
                    night,
                    19.0,
                    Duration::ZERO
                )
                .is_ok(),
            "naming one is what the caller has to do, and it is still checked"
        );
        assert_eq!(
            actor
                .set_setpoint(
                    &mut engine,
                    &unit,
                    &hvac::HEATING,
                    SetpointId(9),
                    19.0,
                    Duration::ZERO
                )
                .unwrap_err(),
            WriteRefused::UnknownSetpoint
        );
    }

    /// A unit that was never attached is a refusal, not a panic.
    #[test]
    fn an_unknown_unit_refuses_every_write() {
        let (actor, mut engine, _units, _features) = two_rooms();
        let nobody = UnitId {
            device: crate::model::AddressDevice::from("d:_i:99999_Other-1"),
            entity: vec![7],
        };

        assert_eq!(
            actor.set_mode(
                &mut engine,
                &nobody,
                &hvac::HEATING,
                &HvacOperationModeType::Off,
                Duration::ZERO
            ),
            Err(system_function::ModeRefused::FunctionUnknown)
        );
        assert_eq!(
            actor.set_temperature(&mut engine, &nobody, &hvac::HEATING, 21.0, Duration::ZERO),
            Err(WriteRefused::ModeUnknown)
        );
        assert_eq!(actor.mode(&nobody, &hvac::HEATING), None);
        assert_eq!(
            actor.effect(&nobody, &hvac::HEATING, crate::model::SetpointId(1)),
            SetpointEffect::Unknown
        );
    }

    /// Detaching a device takes all of its rooms, which is what a lost connection does.
    #[test]
    fn a_lost_device_takes_all_of_its_units() {
        let (mut actor, _engine, units, _features) = two_rooms();

        actor.detach(&units[0]);
        assert_eq!(actor.units().count(), 1);
        actor.detach_device(&units[1].device);
        assert_eq!(actor.units().count(), 0);
    }
}
