//! The SPINE command frame: [`Cmd`] and [`Filter`].
//!
//! These two types are written by hand rather than generated because they embed
//! `xs:choice` groups, which appear *inline* in the enclosing sequence rather than
//! under a key of their own. Modelling the choice as a Rust enum rules out the
//! "two payloads in one command" state that a struct of two hundred `Option` fields —
//! the shape other implementations use — happily represents.

use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeSeq, Serializer};
use serde::{Deserialize, Deserializer, Serialize};

use alloc::string::String;
use alloc::vec::Vec;

use crate::codec::Entry;
use crate::model::{
    AbsoluteOrRelativeTime, CmdControl, CmdData, Datagram, FilterElements, FilterId,
    FilterSelectors, Function,
};

/// One command of a SPINE payload.
///
/// A command names the `function` it addresses, optionally restricts it with `filter`s
/// (Restricted Function Exchange), and carries at most one payload alternative in
/// `data`.
///
/// ```
/// use eebus::model::{Cmd, CmdData, Function, LoadControlLimitListData};
///
/// let cmd = Cmd::read(Function::LoadControlLimitListData);
/// assert_eq!(cmd.function, Some(Function::LoadControlLimitListData));
/// assert!(cmd.data.is_none());
///
/// let notify = Cmd::with_data(CmdData::LoadControlLimitListData(
///     LoadControlLimitListData::default(),
/// ));
/// assert_eq!(notify.function, Some(Function::LoadControlLimitListData));
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cmd {
    /// The function this command addresses.
    ///
    /// The specification allows it to be omitted when `data` is present, but the SPINE
    /// implementation guide asks for complete protocol data, so the constructors here
    /// always set it.
    pub function: Option<Function>,
    /// Restricted Function Exchange filters: selectors choose list entries, elements
    /// choose the parts of an entry, and `cmdControl` says whether this is a partial
    /// update or a delete.
    pub filter: Option<Vec<Filter>>,
    /// The payload, i.e. the chosen alternative of the schema's `DataChoiceGroup`.
    pub data: Option<CmdData>,
    /// Vendor-specific extension, hex encoded.
    pub manufacturer_specific_extension: Option<String>,
    /// When the data was last updated at the owner.
    pub last_update_at: Option<AbsoluteOrRelativeTime>,
}

impl Cmd {
    /// A command that reads `function` in full.
    pub fn read(function: Function) -> Self {
        Self {
            function: Some(function),
            ..Self::default()
        }
    }

    /// A command carrying `data`, with `function` derived from the payload.
    pub fn with_data(data: CmdData) -> Self {
        Self {
            function: Some(Function::from(data.key())),
            data: Some(data),
            ..Self::default()
        }
    }

    /// Adds a filter, marking this command as a partial (Restricted Function Exchange)
    /// operation.
    #[must_use]
    pub fn with_filter(mut self, filter: Filter) -> Self {
        self.filter.get_or_insert_with(Vec::new).push(filter);
        self
    }

    /// True when any filter requests a partial exchange.
    pub fn is_partial(&self) -> bool {
        self.filter.iter().flatten().any(Filter::is_partial)
    }

    /// True when any filter requests a delete.
    pub fn is_delete(&self) -> bool {
        self.filter.iter().flatten().any(Filter::is_delete)
    }
}

impl Serialize for Cmd {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut len = 0;
        if self.function.is_some() {
            len += 1;
        }
        if self.filter.is_some() {
            len += 1;
        }
        if self.data.is_some() {
            len += 1;
        }
        if self.manufacturer_specific_extension.is_some() {
            len += 1;
        }
        if self.last_update_at.is_some() {
            len += 1;
        }

        let mut seq = serializer.serialize_seq(Some(len))?;
        if let Some(v) = &self.function {
            seq.serialize_element(&Entry("function", v))?;
        }
        if let Some(v) = &self.filter {
            seq.serialize_element(&Entry("filter", v))?;
        }
        // The choice group is inline: the alternative already encodes as `{"key": …}`.
        if let Some(v) = &self.data {
            seq.serialize_element(v)?;
        }
        if let Some(v) = &self.manufacturer_specific_extension {
            seq.serialize_element(&Entry("manufacturerSpecificExtension", v))?;
        }
        if let Some(v) = &self.last_update_at {
            seq.serialize_element(&Entry("lastUpdateAt", v))?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for Cmd {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct CmdVisitor;
        impl<'de> Visitor<'de> for CmdVisitor {
            type Value = Cmd;
            fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str("an EEBUS sequence for `Cmd`")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Cmd, A::Error> {
                let mut out = Cmd::default();
                while seq.next_element_seed(CmdEntry(&mut out))?.is_some() {}
                Ok(out)
            }
        }

        struct CmdEntry<'a>(&'a mut Cmd);
        impl<'de> serde::de::DeserializeSeed<'de> for CmdEntry<'_> {
            type Value = ();
            fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
                d.deserialize_map(self)
            }
        }
        impl<'de> Visitor<'de> for CmdEntry<'_> {
            type Value = ();
            fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str("an element of `Cmd`")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "function" => self.0.function = Some(map.next_value()?),
                        "filter" => self.0.filter = Some(map.next_value()?),
                        "manufacturerSpecificExtension" => {
                            self.0.manufacturer_specific_extension = Some(map.next_value()?);
                        }
                        "lastUpdateAt" => self.0.last_update_at = Some(map.next_value()?),
                        other => {
                            if let Some(data) = CmdData::read_value(other, &mut map)? {
                                self.0.data = Some(data);
                            } else {
                                map.next_value::<IgnoredAny>()?;
                            }
                        }
                    }
                }
                Ok(())
            }
        }

        deserializer.deserialize_seq(CmdVisitor)
    }
}

/// A Restricted Function Exchange filter (SPINE Protocol Specification §5.3.4).
///
/// A filter answers three questions about a partial exchange: *which* list entries are
/// addressed (`selectors`), *which* elements of them (`elements`), and *what* to do
/// with them (`cmd_control`: partial update or delete).
///
/// ```
/// use eebus::model::{Filter, LoadControlLimitListDataSelectors, FilterSelectors};
///
/// let f = Filter::partial().select(FilterSelectors::LoadControlLimitListDataSelectors(
///     LoadControlLimitListDataSelectors::default(),
/// ));
/// assert!(f.is_partial());
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    /// Correlates a filter with a reply that answers it.
    pub filter_id: Option<FilterId>,
    /// Whether this filter describes a partial update or a delete.
    pub cmd_control: Option<CmdControl>,
    /// Which list entries the filter addresses.
    pub selectors: Option<Vec<FilterSelectors>>,
    /// Which elements of the addressed entries the filter covers.
    pub elements: Option<FilterElements>,
}

impl Filter {
    /// A filter marking the command as a partial update.
    pub fn partial() -> Self {
        Self {
            cmd_control: Some(CmdControl {
                partial: Some(crate::codec::ElementTag),
                ..CmdControl::default()
            }),
            ..Self::default()
        }
    }

    /// A filter marking the command as a delete.
    pub fn delete() -> Self {
        Self {
            cmd_control: Some(CmdControl {
                delete: Some(crate::codec::ElementTag),
                ..CmdControl::default()
            }),
            ..Self::default()
        }
    }

    /// Adds a selector.
    #[must_use]
    pub fn select(mut self, selector: FilterSelectors) -> Self {
        self.selectors.get_or_insert_with(Vec::new).push(selector);
        self
    }

    /// Sets the elements this filter covers.
    #[must_use]
    pub fn covering(mut self, elements: FilterElements) -> Self {
        self.elements = Some(elements);
        self
    }

    /// True when `cmdControl.partial` is set.
    pub fn is_partial(&self) -> bool {
        self.cmd_control
            .as_ref()
            .is_some_and(|c| c.partial.is_some())
    }

    /// True when `cmdControl.delete` is set.
    pub fn is_delete(&self) -> bool {
        self.cmd_control
            .as_ref()
            .is_some_and(|c| c.delete.is_some())
    }
}

impl Serialize for Filter {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut len = 0;
        if self.filter_id.is_some() {
            len += 1;
        }
        if self.cmd_control.is_some() {
            len += 1;
        }
        len += self.selectors.as_ref().map_or(0, Vec::len);
        if self.elements.is_some() {
            len += 1;
        }

        let mut seq = serializer.serialize_seq(Some(len))?;
        if let Some(v) = &self.filter_id {
            seq.serialize_element(&Entry("filterId", v))?;
        }
        if let Some(v) = &self.cmd_control {
            seq.serialize_element(&Entry("cmdControl", v))?;
        }
        // Both choice groups appear inline, each already a single-key object.
        for s in self.selectors.iter().flatten() {
            seq.serialize_element(s)?;
        }
        if let Some(v) = &self.elements {
            seq.serialize_element(v)?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for Filter {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FilterVisitor;
        impl<'de> Visitor<'de> for FilterVisitor {
            type Value = Filter;
            fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str("an EEBUS sequence for `Filter`")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Filter, A::Error> {
                let mut out = Filter::default();
                while seq.next_element_seed(FilterEntry(&mut out))?.is_some() {}
                Ok(out)
            }
        }

        struct FilterEntry<'a>(&'a mut Filter);
        impl<'de> serde::de::DeserializeSeed<'de> for FilterEntry<'_> {
            type Value = ();
            fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
                d.deserialize_map(self)
            }
        }
        impl<'de> Visitor<'de> for FilterEntry<'_> {
            type Value = ();
            fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str("an element of `Filter`")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "filterId" => self.0.filter_id = Some(map.next_value()?),
                        "cmdControl" => self.0.cmd_control = Some(map.next_value()?),
                        other => {
                            if let Some(sel) = FilterSelectors::read_value(other, &mut map)? {
                                self.0.selectors.get_or_insert_with(Vec::new).push(sel);
                            } else if let Some(el) = FilterElements::read_value(other, &mut map)? {
                                self.0.elements = Some(el);
                            } else {
                                map.next_value::<IgnoredAny>()?;
                            }
                        }
                    }
                }
                Ok(())
            }
        }

        deserializer.deserialize_seq(FilterVisitor)
    }
}

/// The root object of a SPINE message: `{"datagram": [...]}`.
///
/// SHIP carries SPINE inside `data.payload`, which the SHIP schema declares as
/// `xs:anyType`; the JSON transformation of SPINE's root `datagram` element turns it
/// into a single-key object. [`to_json`] and [`from_json_slice`] wrap and unwrap it.
struct DatagramRoot<'a>(&'a Datagram);

impl Serialize for DatagramRoot<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        Entry("datagram", self.0).serialize(serializer)
    }
}

/// Encodes a datagram as a complete SPINE message payload.
///
/// ```
/// use eebus::model::{CmdClassifier, Datagram, Header, Payload, to_json};
///
/// let dg = Datagram {
///     header: Some(Header {
///         cmd_classifier: Some(CmdClassifier::Read),
///         ..Header::default()
///     }),
///     payload: Some(Payload::default()),
/// };
/// assert_eq!(
///     to_json(&dg).unwrap(),
///     r#"{"datagram":[{"header":[{"cmdClassifier":"read"}]},{"payload":[]}]}"#
/// );
/// ```
pub fn to_json(datagram: &Datagram) -> Result<String, serde_json::Error> {
    serde_json::to_string(&DatagramRoot(datagram))
}

/// Encodes a datagram into a byte vector.
pub fn to_json_vec(datagram: &Datagram) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&DatagramRoot(datagram))
}

/// Decodes a SPINE message payload.
///
/// Whitespace is insignificant, as required by the SHIP implementation guide §2.2:
/// peers may send pretty-printed JSON and a stack that assumes minified input is
/// non-compliant.
pub fn from_json_slice(bytes: &[u8]) -> Result<Datagram, serde_json::Error> {
    #[derive(Deserialize)]
    struct Root {
        datagram: Datagram,
    }
    let root: Root = serde_json::from_slice(bytes)?;
    Ok(root.datagram)
}

/// Decodes a SPINE message payload from a string.
pub fn from_json_str(s: &str) -> Result<Datagram, serde_json::Error> {
    from_json_slice(s.as_bytes())
}

/// Encodes a datagram as the `serde_json::Value` a SHIP `data.payload` carries.
///
/// SHIP declares `data.payload` as `xs:anyType` — an opaque document belonging to
/// whatever protocol `data.header.protocolId` names — so the boundary between the two
/// layers is exactly this conversion.
pub fn to_json_value(datagram: &Datagram) -> Result<serde_json::Value, serde_json::Error> {
    serde_json::to_value(DatagramRoot(datagram))
}

/// Decodes a datagram from the `serde_json::Value` of a SHIP `data.payload`.
///
/// ```
/// use eebus::model::{to_json_value, from_json_value, Datagram, Header, MsgCounter};
///
/// let datagram = Datagram {
///     header: Some(Header { msg_counter: Some(MsgCounter(1)), ..Default::default() }),
///     payload: None,
/// };
/// let value = to_json_value(&datagram).unwrap();
/// assert_eq!(from_json_value(value).unwrap(), datagram);
/// ```
pub fn from_json_value(value: serde_json::Value) -> Result<Datagram, serde_json::Error> {
    #[derive(Deserialize)]
    struct Root {
        datagram: Datagram,
    }
    let root: Root = serde_json::from_value(value)?;
    Ok(root.datagram)
}
