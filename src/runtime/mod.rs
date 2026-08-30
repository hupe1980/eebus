//! Sockets: TCP, TLS and WebSocket on Tokio, driving the sans-IO cores.
//!
//! Everything below this module is a state machine with no I/O in it: the SHIP handshake
//! and the SPINE engine are fed messages and a clock and asked what to send. This module
//! is the part that owns a socket and a timer, and it is deliberately the only part that
//! does — which is what lets every protocol rule in the crate be tested against a virtual
//! clock, and what would let the same code run under a different runtime, in a simulator,
//! or on a microcontroller.
//!
//! What it does is the SHIP transport stack, in order (SHIP §10):
//!
//! 1. TCP, then TLS 1.2 with mutual authentication ([`crate::tls`]), which is where each
//!    end learns the other's SKI.
//! 2. A WebSocket upgrade offering the `ship` subprotocol, which a peer that speaks
//!    something else will decline.
//! 3. The SHIP handshake — CMI, hello, protocol, access methods — driven to the point
//!    where SPINE data may be exchanged.
//! 4. Binary frames carrying SPINE datagrams, in both directions.
//!
//! Requires the `runtime` feature.

mod connection;
pub use connection::*;

mod hub;
pub use hub::*;

mod node;
pub use node::*;

mod reconnect;
pub use reconnect::*;
