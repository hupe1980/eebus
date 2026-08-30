//! SHIP message framing.
//!
//! Every WebSocket message on a SHIP connection is a **binary** frame whose first byte
//! names the message type and whose remainder is the message itself (SHIP §13.4.3):
//!
//! | Byte   | Type      | Remainder                                    |
//! |--------|-----------|----------------------------------------------|
//! | `0x00` | init      | one byte, the CMI head, also `0x00`          |
//! | `0x01` | control   | JSON: the handshake and key-material messages |
//! | `0x02` | data      | JSON: `{"data": …}`, carrying SPINE          |
//! | `0x03` | end       | JSON: `{"connectionClose": …}`               |
//!
//! Text frames are a protocol violation: SHIP §10.3 requires closing with status 1003
//! when one arrives, and 1002 for a reserved opcode.

use alloc::vec::Vec;

use super::{ControlMessage, DataMessage, EndMessage};

/// `MSG_TYPE = init`.
pub const MSG_TYPE_INIT: u8 = 0x00;
/// `MSG_TYPE = control`.
pub const MSG_TYPE_CONTROL: u8 = 0x01;
/// `MSG_TYPE = data`.
pub const MSG_TYPE_DATA: u8 = 0x02;
/// `MSG_TYPE = end`.
pub const MSG_TYPE_END: u8 = 0x03;

/// The single payload byte of the Connection Mode Initialisation message.
pub const CMI_HEAD: u8 = 0x00;

/// The complete CMI message, `[0x00, 0x00]`, which both peers send first.
pub const CMI_MESSAGE: [u8; 2] = [MSG_TYPE_INIT, CMI_HEAD];

/// The WebSocket sub-protocol both peers must offer (SHIP §10.2).
pub const SUBPROTOCOL: &str = "ship";

/// The WebSocket path advertised in the mDNS TXT record.
pub const DEFAULT_PATH: &str = "/ship/";

/// The only message format this version of SHIP requires (§13.4.4.2.1).
pub const FORMAT_JSON_UTF8: &str = "JSON-UTF8";

/// The optional wide format, which this implementation announces support for but never
/// selects.
pub const FORMAT_JSON_UTF16: &str = "JSON-UTF16";

/// One framed SHIP message.
#[derive(Clone, Debug, PartialEq)]
pub enum ShipMessage {
    /// Connection Mode Initialisation: the two-byte opener, not a JSON message.
    Cmi,
    /// A handshake or key-material message.
    Control(ControlMessage),
    /// A payload message; for EEBUS the payload is a SPINE datagram.
    Data(DataMessage),
    /// A connection termination message.
    End(EndMessage),
}

/// Why a received frame could not be read.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The frame carried no bytes at all.
    #[error("empty SHIP frame")]
    Empty,
    /// The first byte named a message type this specification version does not define.
    #[error("unknown SHIP message type 0x{0:02x}")]
    UnknownType(u8),
    /// An init frame was not exactly `[0x00, 0x00]`.
    #[error("malformed CMI message: expected [0x00, 0x00], found {0:02x?}")]
    MalformedCmi(Vec<u8>),
    /// The JSON body did not parse, or named no known message.
    #[error("malformed {kind} message: {source}")]
    Json {
        /// Which message type failed to parse.
        kind: &'static str,
        /// The underlying JSON error.
        source: serde_json::Error,
    },
}

impl ShipMessage {
    /// The message-type byte this message is framed with.
    pub const fn message_type(&self) -> u8 {
        match self {
            ShipMessage::Cmi => MSG_TYPE_INIT,
            ShipMessage::Control(_) => MSG_TYPE_CONTROL,
            ShipMessage::Data(_) => MSG_TYPE_DATA,
            ShipMessage::End(_) => MSG_TYPE_END,
        }
    }

    /// Encodes the message into the body of a WebSocket binary frame.
    ///
    /// ```
    /// use eebus::ship::{ShipMessage, CMI_MESSAGE};
    ///
    /// assert_eq!(ShipMessage::Cmi.encode().unwrap(), CMI_MESSAGE.to_vec());
    /// ```
    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        // `serde_json::to_writer` needs `std::io::Write`, which a bare-metal target does
        // not have; going through a vector keeps one code path for every target.
        let body = match self {
            ShipMessage::Cmi => alloc::vec![CMI_HEAD],
            ShipMessage::Control(m) => serde_json::to_vec(m)?,
            ShipMessage::Data(m) => serde_json::to_vec(m)?,
            ShipMessage::End(m) => serde_json::to_vec(m)?,
        };
        let mut out = Vec::with_capacity(body.len() + 1);
        out.push(self.message_type());
        out.extend_from_slice(&body);
        Ok(out)
    }

    /// Decodes the body of a WebSocket binary frame.
    ///
    /// ```
    /// use eebus::ship::{ShipMessage, ControlMessage};
    ///
    /// let bytes = br#"{"connectionHello":[{"phase":"ready"}]}"#;
    /// let mut frame = vec![0x01];
    /// frame.extend_from_slice(bytes);
    ///
    /// let ShipMessage::Control(ControlMessage::ConnectionHello(hello)) =
    ///     ShipMessage::decode(&frame).unwrap()
    /// else {
    ///     panic!("expected a hello");
    /// };
    /// assert!(hello.phase.is_some());
    /// ```
    pub fn decode(frame: &[u8]) -> Result<Self, FrameError> {
        let (&kind, body) = frame.split_first().ok_or(FrameError::Empty)?;
        match kind {
            MSG_TYPE_INIT => {
                if body == [CMI_HEAD] {
                    Ok(ShipMessage::Cmi)
                } else {
                    Err(FrameError::MalformedCmi(frame.to_vec()))
                }
            }
            MSG_TYPE_CONTROL => serde_json::from_slice(body)
                .map(ShipMessage::Control)
                .map_err(|source| FrameError::Json {
                    kind: "control",
                    source,
                }),
            MSG_TYPE_DATA => {
                serde_json::from_slice(body)
                    .map(ShipMessage::Data)
                    .map_err(|source| FrameError::Json {
                        kind: "data",
                        source,
                    })
            }
            MSG_TYPE_END => serde_json::from_slice(body)
                .map(ShipMessage::End)
                .map_err(|source| FrameError::Json {
                    kind: "end",
                    source,
                }),
            other => Err(FrameError::UnknownType(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ship::{
        ConnectionClose, ConnectionClosePhase, ConnectionHello, ConnectionHelloPhase,
    };

    #[test]
    fn cmi_is_two_zero_bytes() {
        assert_eq!(ShipMessage::Cmi.encode().unwrap(), vec![0x00, 0x00]);
        assert_eq!(
            ShipMessage::decode(&[0x00, 0x00]).unwrap(),
            ShipMessage::Cmi
        );
    }

    /// `TC_SHIP_CMI_005` / `TC_SHIP_CMI_006`: an init frame with any other content is
    /// rejected rather than tolerated.
    #[test]
    fn a_wrong_cmi_head_is_rejected() {
        assert!(matches!(
            ShipMessage::decode(&[0x00, 0x01]),
            Err(FrameError::MalformedCmi(_))
        ));
        assert!(matches!(
            ShipMessage::decode(&[0x00]),
            Err(FrameError::MalformedCmi(_))
        ));
    }

    /// `TC_SHIP_MSG_002`: an unknown message type is rejected.
    #[test]
    fn unknown_message_types_are_rejected() {
        assert!(matches!(
            ShipMessage::decode(&[0x07, b'{', b'}']),
            Err(FrameError::UnknownType(0x07))
        ));
        assert!(matches!(ShipMessage::decode(&[]), Err(FrameError::Empty)));
    }

    #[test]
    fn control_messages_round_trip() {
        let hello = ShipMessage::Control(ControlMessage::ConnectionHello(ConnectionHello {
            phase: Some(ConnectionHelloPhase::Ready),
            waiting: Some(60_000),
            ..Default::default()
        }));
        let bytes = hello.encode().unwrap();
        assert_eq!(bytes[0], MSG_TYPE_CONTROL);
        assert_eq!(
            core::str::from_utf8(&bytes[1..]).unwrap(),
            r#"{"connectionHello":[{"phase":"ready"},{"waiting":60000}]}"#
        );
        assert_eq!(ShipMessage::decode(&bytes).unwrap(), hello);
    }

    #[test]
    fn end_messages_round_trip() {
        let close = ShipMessage::End(EndMessage::ConnectionClose(ConnectionClose {
            phase: Some(ConnectionClosePhase::Announce),
            max_time: Some(500),
            ..Default::default()
        }));
        let bytes = close.encode().unwrap();
        assert_eq!(bytes[0], MSG_TYPE_END);
        assert_eq!(ShipMessage::decode(&bytes).unwrap(), close);
    }

    /// `TC_SHIP_MSG_003`: pretty-printed JSON must parse.
    #[test]
    fn whitespace_in_the_body_is_tolerated() {
        let frame = b"\x01{\n  \"connectionHello\" : [ { \"phase\" : \"ready\" } ]\n}";
        assert!(matches!(
            ShipMessage::decode(frame).unwrap(),
            ShipMessage::Control(ControlMessage::ConnectionHello(_))
        ));
    }

    /// `TC_SHIP_MSG_001`: a control frame that names no known message is rejected.
    #[test]
    fn an_unknown_control_message_is_rejected() {
        let frame = br#"{"somethingElse":[]}"#;
        let mut buf = vec![MSG_TYPE_CONTROL];
        buf.extend_from_slice(frame);
        assert!(matches!(
            ShipMessage::decode(&buf),
            Err(FrameError::Json {
                kind: "control",
                ..
            })
        ));
    }
}
