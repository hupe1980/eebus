//! The EEBUS JSON wire format.
//!
//! SPINE payloads are transported over SHIP as "JSON-UTF8", a JSON projection of the
//! SPINE XML Schema defined by **SHIP 1.1.0 §11.4** ("XML to JSON Transformation").
//! The rules that matter for us:
//!
//! | XSD construct                       | JSON                                        |
//! |-------------------------------------|---------------------------------------------|
//! | `xs:sequence` / `xs:choice` in a `xs:complexType` | **array of single-key objects** |
//! | `xs:all`                            | object                                      |
//! | `maxOccurs > 1`                     | array of the item's representation          |
//! | empty element (`ElementTagType`)    | `[]`                                        |
//! | `xs:nil`                            | `null`                                      |
//! | numbers / booleans                  | JSON number / boolean                       |
//! | `xs:string`, `xs:hexBinary`, `xs:dateTime`, `xs:duration` | JSON string        |
//! | attributes                          | absorbed                                    |
//! | namespace prefixes                  | omitted                                     |
//!
//! So a SPINE header serialises as
//!
//! ```json
//! [{"specificationVersion":"1.3.0"},{"msgCounter":1},{"cmdClassifier":"read"}]
//! ```
//!
//! and *not* as a JSON object. Unlike other implementations, which marshal to ordinary
//! JSON and then rewrite the tree, this crate encodes the format directly through
//! [`serde`]: the [`eebus_struct!`] macro emits a `Serialize`/`Deserialize` pair that
//! reads and writes the array-of-single-key-objects form in a single streaming pass,
//! with no intermediate `Value` and no allocation for field names.
//!
//! Element order follows XSD declaration order, which is also the order the macro
//! lists the fields in. Unknown keys are ignored on the way in, as required by
//! `TC_SPINE_COMP_003` and `TC_SPINE_RTS_004`.
//!
//! [`eebus_struct!`]: crate::eebus_struct

use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};

mod macros;

/// Serialises `&T` as a single-key JSON object `{"name": value}`.
///
/// This is the building block of the EEBUS array-of-single-key-objects encoding; one
/// `Entry` is emitted per present field of a struct.
#[derive(Debug)]
pub struct Entry<'a, T: ?Sized>(pub &'static str, pub &'a T);

impl<T: Serialize + ?Sized> Serialize for Entry<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry(self.0, self.1)?;
        map.end()
    }
}

/// An XSD `ElementTagType`: an element whose presence alone carries the meaning.
///
/// Encoded as the empty array `[]` (SHIP §11.4.6 rule 4). These appear as the payload
/// of Restricted Function Exchange *elements* filters and as `cmdControl.partial` /
/// `cmdControl.delete`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ElementTag;

impl Serialize for ElementTag {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_seq(Some(0))?.end()
    }
}

impl<'de> serde::Deserialize<'de> for ElementTag {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = ElementTag;
            fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str("an empty EEBUS element tag `[]`")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(ElementTag)
            }
            // Tolerate peers that encode a tag as `{}` or `null` instead of `[]`.
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                while map
                    .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(ElementTag)
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(ElementTag)
            }
            fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(ElementTag)
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// Items re-exported for the generated code; not part of the public API.
#[doc(hidden)]
pub mod __private {
    pub use core::fmt;
    pub use core::option::Option::{self, None, Some};
    pub use core::result::Result::{self, Err, Ok};

    pub use serde::Deserialize;
    pub use serde::de::{
        DeserializeSeed, Deserializer, Error as DeError, IgnoredAny, MapAccess, SeqAccess, Visitor,
    };
    pub use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};

    extern crate alloc;
    pub use alloc::borrow::ToOwned;
    pub use alloc::boxed::Box;
    pub use alloc::string::{String, ToString};
    pub use alloc::vec::Vec;
}

/// Merging a partial update into stored data.
///
/// SPINE's Restricted Function Exchange lets a peer send only what changed. The
/// receiver's job is then a *merge*, and the SPINE implementation guide §3.3 is precise
/// about what that means: an omitted element is "unchanged", and that reading takes
/// precedence over any default the Resource Specification would otherwise supply.
///
/// The consequence is that merging has to recurse. A `scaledNumber` carrying only
/// `number` updates the number and keeps the stored `scale`; replacing the whole value
/// would silently rescale it — by a factor of ten thousand in the worst case the guide
/// describes, which for a power limit is the difference between 4.2 kW and 42 MW.
pub trait Merge {
    /// Applies `update` to `self`, keeping whatever `update` leaves out.
    fn merge(&mut self, update: Self);
}

/// Leaf values are replaced wholesale: they have no parts to keep.
macro_rules! merge_by_replacement {
    ($($ty:ty),* $(,)?) => {
        $(
            impl Merge for $ty {
                fn merge(&mut self, update: Self) { *self = update; }
            }
        )*
    };
}

merge_by_replacement!(
    bool,
    i8,
    i16,
    i32,
    i64,
    u8,
    u16,
    u32,
    u64,
    f32,
    f64,
    alloc::string::String,
    ElementTag,
    serde_json::Value,
);

impl<T> Merge for alloc::vec::Vec<T> {
    /// A repeated element inside a single entry is replaced as a unit.
    ///
    /// Merging *lists of entries* by identifier is a different operation, and lives in
    /// [`crate::model::rfe`], because it needs to know which elements identify an entry.
    fn merge(&mut self, update: Self) {
        *self = update;
    }
}

impl<T: Merge> Merge for Option<T> {
    fn merge(&mut self, update: Self) {
        match (self.as_mut(), update) {
            (Some(existing), Some(update)) => existing.merge(update),
            (None, Some(update)) => *self = Some(update),
            (_, None) => {}
        }
    }
}
