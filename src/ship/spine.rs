//! Carrying SPINE over SHIP.
//!
//! A SHIP data message is a `protocolId` and a payload of `xs:anyType` — SHIP does not
//! know or care what is inside. For EEBUS the payload is a SPINE datagram and the
//! protocol identifier is `ee1.0`, and this module is the two lines of glue that says so
//! in one place rather than at every call site.

use crate::model::{self, Datagram};
use crate::ship::{Data, DataMessage, Header, ProtocolId, ShipMessage};

/// The `protocolId` that marks a SHIP data message as carrying SPINE.
pub const SPINE_PROTOCOL_ID: &str = "ee1.0";

/// Wraps a SPINE datagram in a SHIP data message.
///
/// ```
/// use eebus::model::Datagram;
/// use eebus::ship::{spine_datagram, spine_message, ShipMessage};
///
/// let datagram = Datagram::default();
/// let ShipMessage::Data(data) = spine_message(&datagram).unwrap() else {
///     unreachable!("a data message");
/// };
/// assert_eq!(spine_datagram(&data).unwrap(), Some(datagram));
/// ```
pub fn spine_message(datagram: &Datagram) -> Result<ShipMessage, serde_json::Error> {
    Ok(ShipMessage::Data(DataMessage::Data(Data {
        header: Some(Header {
            protocol_id: Some(ProtocolId(SPINE_PROTOCOL_ID.into())),
        }),
        payload: Some(model::to_json_value(datagram)?),
        extension: None,
    })))
}

/// Reads the SPINE datagram out of a SHIP data message.
///
/// Returns [`None`] for a message carrying some other protocol — SHIP is not exclusive to
/// SPINE, and a payload this node does not understand is not an error, only none of its
/// business.
pub fn spine_datagram(message: &DataMessage) -> Result<Option<Datagram>, serde_json::Error> {
    // `DataMessage` has one variant today; the match keeps this honest if SHIP adds
    // another kind of data message later.
    #[allow(irrefutable_let_patterns)]
    let DataMessage::Data(data) = message else {
        return Ok(None);
    };
    let carries_spine = data
        .header
        .as_ref()
        .and_then(|header| header.protocol_id.as_ref())
        .is_some_and(|id| id.0 == SPINE_PROTOCOL_ID);
    if !carries_spine {
        return Ok(None);
    }
    let Some(payload) = data.payload.clone() else {
        return Ok(None);
    };
    model::from_json_value(payload).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_datagram_survives_the_ship_wrapper() {
        let datagram = Datagram::default();
        let message = spine_message(&datagram).unwrap();

        // Through the SHIP framing as well, which is what actually goes on the socket.
        let framed = message.encode().unwrap();
        let ShipMessage::Data(data) = ShipMessage::decode(&framed).unwrap() else {
            panic!("a data message");
        };
        assert_eq!(spine_datagram(&data).unwrap(), Some(datagram));
    }

    #[test]
    fn a_payload_for_another_protocol_is_not_ours() {
        let message = DataMessage::Data(Data {
            header: Some(Header {
                protocol_id: Some(ProtocolId("something-else".into())),
            }),
            payload: Some(serde_json::json!({"whatever": 1})),
            extension: None,
        });
        assert_eq!(spine_datagram(&message).unwrap(), None);
    }
}
