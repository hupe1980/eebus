//! The SPINE 1.3.0 information model.
//!
//! Every type here is generated from the XML Schemas shipped with
//! `EEBus_SPINE_V1.3.0_Final.zip` by `cargo xtask codegen`, except for the two command
//! frame types in [`payload`], which carry `xs:choice` groups and are written by hand.
//!
//! # Shape of the model
//!
//! * `xs:complexType` → a struct whose fields are all `Option`, because the schemas
//!   declare almost every element `minOccurs="0"` to allow Restricted Function
//!   Exchange to send arbitrary subsets.
//! * `xs:simpleType` restricting an integer → a distinct newtype, so a `LimitId` can
//!   never be passed where a `MeasurementId` belongs.
//! * `xs:simpleType` restricting `xs:string` with enumerations → a Rust enum; where the
//!   schema unions it with `EnumExtendType`, the enum gains an `Other(String)` variant
//!   that preserves vendor extensions across a round trip.
//! * `xs:choice` groups → Rust enums, so a command cannot carry two payloads.

#[rustfmt::skip]
mod generated;

pub use generated::*;

mod payload;
pub use payload::*;

mod values;
pub use values::*;

pub mod rfe;

pub use crate::codec::ElementTag;
