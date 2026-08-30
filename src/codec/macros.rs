//! Macros that generate the EEBUS JSON codec for SPINE resource types.
//!
//! The SPINE data model is machine-generated from the specification's XML Schemas by
//! `cargo xtask codegen`, which emits invocations of the macros defined here. Keeping
//! the encoding rules in four hand-written, hand-tested macros (rather than in ~30 000
//! lines of emitted `impl` blocks) means the wire format is defined in exactly one
//! place and the generated model stays readable and reviewable.

/// Defines a SPINE resource type: a struct encoded as an *array of single-key objects*.
///
/// Every field is optional — the SPINE schemas declare almost all elements
/// `minOccurs="0"` so that Restricted Function Exchange can transmit arbitrary subsets —
/// so the macro wraps each field in [`Option`]. Repeated elements
/// (`maxOccurs="unbounded"`) are written as `Vec<T>`.
///
/// ```ignore
/// eebus_struct! {
///     /// SPINE datagram header.
///     pub struct Header {
///         "specificationVersion" => specification_version: SpecificationVersion,
///         "msgCounter" => msg_counter: MsgCounter,
///     }
/// }
/// ```
///
/// serialises `Header { specification_version: Some(..), msg_counter: Some(1) }` as
/// `[{"specificationVersion":"1.3.0"},{"msgCounter":1}]`, and skips absent fields.
/// On the way in, keys are matched without allocating and unknown keys are ignored.
#[macro_export]
macro_rules! eebus_struct {
    (
        $(#[$attr:meta])*
        $vis:vis struct $name:ident {
            $(
                $(#[$fattr:meta])*
                $wire:literal => $field:ident : $ty:ty
            ),* $(,)?
        }
    ) => {
        $(#[$attr])*
        #[derive(Clone, Debug, Default, PartialEq)]
        $vis struct $name {
            $(
                $(#[$fattr])*
                pub $field: $crate::codec::__private::Option<$ty>,
            )*
        }

        impl $name {
            /// The wire (XSD element) names of this type's fields, in schema order.
            pub const FIELD_NAMES: &'static [&'static str] = &[$($wire),*];

            /// True when no field is set, i.e. the type encodes as `[]`.
            pub fn is_empty(&self) -> bool {
                true $( && self.$field.is_none() )*
            }
        }

        impl $crate::codec::Merge for $name {
            #[allow(unused_variables)]
            fn merge(&mut self, update: Self) {
                $( $crate::codec::Merge::merge(&mut self.$field, update.$field); )*
            }
        }

        impl $crate::codec::__private::Serialize for $name {
            fn serialize<S>(&self, serializer: S)
                -> $crate::codec::__private::Result<S::Ok, S::Error>
            where
                S: $crate::codec::__private::Serializer,
            {
                use $crate::codec::__private::SerializeSeq as _;
                let mut __len = 0usize;
                $( if self.$field.is_some() { __len += 1; } )*
                let mut __seq = serializer.serialize_seq($crate::codec::__private::Some(__len))?;
                $(
                    if let $crate::codec::__private::Some(ref __v) = self.$field {
                        __seq.serialize_element(&$crate::codec::Entry($wire, __v))?;
                    }
                )*
                __seq.end()
            }
        }

        impl<'de> $crate::codec::__private::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D)
                -> $crate::codec::__private::Result<Self, D::Error>
            where
                D: $crate::codec::__private::Deserializer<'de>,
            {
                #[allow(non_camel_case_types)]
                enum __Fld { $($field,)* __Unknown }

                struct __FldVisitor;
                impl<'de> $crate::codec::__private::Visitor<'de> for __FldVisitor {
                    type Value = __Fld;
                    fn expecting(&self, __f: &mut $crate::codec::__private::fmt::Formatter<'_>)
                        -> $crate::codec::__private::fmt::Result
                    {
                        __f.write_str(concat!("a field of `", stringify!($name), "`"))
                    }
                    fn visit_str<E>(self, __v: &str)
                        -> $crate::codec::__private::Result<__Fld, E>
                    where
                        E: $crate::codec::__private::DeError,
                    {
                        $crate::codec::__private::Ok(match __v {
                            $($wire => __Fld::$field,)*
                            _ => __Fld::__Unknown,
                        })
                    }
                }

                struct __FldSeed;
                impl<'de> $crate::codec::__private::DeserializeSeed<'de> for __FldSeed {
                    type Value = __Fld;
                    fn deserialize<D>(self, __d: D)
                        -> $crate::codec::__private::Result<__Fld, D::Error>
                    where
                        D: $crate::codec::__private::Deserializer<'de>,
                    {
                        __d.deserialize_identifier(__FldVisitor)
                    }
                }

                // Reads one `{"key": value}` object of the outer array straight into
                // the target struct, so no intermediate buffering is needed.
                #[allow(dead_code)]
                struct __EntrySeed<'a>(&'a mut $name);
                impl<'de, 'a> $crate::codec::__private::DeserializeSeed<'de> for __EntrySeed<'a> {
                    type Value = ();
                    fn deserialize<D>(self, __d: D)
                        -> $crate::codec::__private::Result<(), D::Error>
                    where
                        D: $crate::codec::__private::Deserializer<'de>,
                    {
                        __d.deserialize_map(self)
                    }
                }
                impl<'de, 'a> $crate::codec::__private::Visitor<'de> for __EntrySeed<'a> {
                    type Value = ();
                    fn expecting(&self, __f: &mut $crate::codec::__private::fmt::Formatter<'_>)
                        -> $crate::codec::__private::fmt::Result
                    {
                        __f.write_str(concat!("an element of `", stringify!($name), "`"))
                    }
                    fn visit_map<A>(self, mut __map: A)
                        -> $crate::codec::__private::Result<(), A::Error>
                    where
                        A: $crate::codec::__private::MapAccess<'de>,
                    {
                        while let $crate::codec::__private::Some(__k) =
                            __map.next_key_seed(__FldSeed)?
                        {
                            match __k {
                                $(
                                    __Fld::$field => {
                                        self.0.$field =
                                            $crate::codec::__private::Some(__map.next_value()?);
                                    }
                                )*
                                __Fld::__Unknown => {
                                    __map.next_value::<$crate::codec::__private::IgnoredAny>()?;
                                }
                            }
                        }
                        $crate::codec::__private::Ok(())
                    }
                }

                struct __Visitor;
                impl<'de> $crate::codec::__private::Visitor<'de> for __Visitor {
                    type Value = $name;
                    fn expecting(&self, __f: &mut $crate::codec::__private::fmt::Formatter<'_>)
                        -> $crate::codec::__private::fmt::Result
                    {
                        __f.write_str(concat!("an EEBUS sequence for `", stringify!($name), "`"))
                    }
                    fn visit_seq<A>(self, mut __seq: A)
                        -> $crate::codec::__private::Result<$name, A::Error>
                    where
                        A: $crate::codec::__private::SeqAccess<'de>,
                    {
                        let mut __out = <$name as ::core::default::Default>::default();
                        while __seq.next_element_seed(__EntrySeed(&mut __out))?.is_some() {}
                        $crate::codec::__private::Ok(__out)
                    }
                    // Some peers emit a plain object instead of the array form.
                    fn visit_map<A>(self, mut __map: A)
                        -> $crate::codec::__private::Result<$name, A::Error>
                    where
                        A: $crate::codec::__private::MapAccess<'de>,
                    {
                        let mut __out = <$name as ::core::default::Default>::default();
                        while let $crate::codec::__private::Some(__k) =
                            __map.next_key_seed(__FldSeed)?
                        {
                            match __k {
                                $(
                                    __Fld::$field => {
                                        __out.$field =
                                            $crate::codec::__private::Some(__map.next_value()?);
                                    }
                                )*
                                __Fld::__Unknown => {
                                    __map.next_value::<$crate::codec::__private::IgnoredAny>()?;
                                }
                            }
                        }
                        $crate::codec::__private::Ok(__out)
                    }
                }

                deserializer.deserialize_seq(__Visitor)
            }
        }
    };
}

/// Defines a closed SPINE enumeration (`xs:restriction` of `xs:string`).
///
/// Unknown values are rejected: the schema permits no extension, and a message with an
/// unparsable enumeration is malformed rather than merely unknown.
#[macro_export]
macro_rules! eebus_enum {
    (
        $(#[$attr:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$vattr:meta])* $wire:literal => $variant:ident ),* $(,)?
        }
    ) => {
        $(#[$attr])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        $vis enum $name {
            $( $(#[$vattr])* $variant, )*
        }

        impl $name {
            /// Every value defined by the schema.
            pub const ALL: &'static [$name] = &[$($name::$variant),*];

            /// The value's representation on the wire.
            pub const fn as_str(&self) -> &'static str {
                match self { $( $name::$variant => $wire, )* }
            }

            /// Parses a wire value, returning [`None`] for anything the schema does not define.
            pub fn from_wire(s: &str) -> $crate::codec::__private::Option<$name> {
                match s {
                    $( $wire => $crate::codec::__private::Some($name::$variant), )*
                    _ => $crate::codec::__private::None,
                }
            }
        }

        impl ::core::fmt::Display for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl $crate::codec::Merge for $name {
            fn merge(&mut self, update: Self) { *self = update; }
        }

        impl $crate::codec::__private::Serialize for $name {
            fn serialize<S>(&self, serializer: S)
                -> $crate::codec::__private::Result<S::Ok, S::Error>
            where
                S: $crate::codec::__private::Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> $crate::codec::__private::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D)
                -> $crate::codec::__private::Result<Self, D::Error>
            where
                D: $crate::codec::__private::Deserializer<'de>,
            {
                struct __V;
                impl<'de> $crate::codec::__private::Visitor<'de> for __V {
                    type Value = $name;
                    fn expecting(&self, f: &mut $crate::codec::__private::fmt::Formatter<'_>)
                        -> $crate::codec::__private::fmt::Result
                    {
                        f.write_str(concat!("a `", stringify!($name), "` value"))
                    }
                    fn visit_str<E>(self, v: &str)
                        -> $crate::codec::__private::Result<$name, E>
                    where
                        E: $crate::codec::__private::DeError,
                    {
                        match $name::from_wire(v) {
                            $crate::codec::__private::Some(x) => $crate::codec::__private::Ok(x),
                            $crate::codec::__private::None => {
                                $crate::codec::__private::Err(E::unknown_variant(v, &[$($wire),*]))
                            }
                        }
                    }
                }
                deserializer.deserialize_str(__V)
            }
        }
    };
}

/// Defines an extensible SPINE enumeration.
///
/// The schema declares these as `xs:union` of an enumeration and `EnumExtendType`, the
/// latter matching vendor-defined values of the form `_i:<PEN>_<name>`. Unknown values
/// are preserved verbatim in the generated `Other` variant so that they survive a
/// decode/encode round trip instead of being dropped.
#[macro_export]
macro_rules! eebus_enum_ext {
    (
        $(#[$attr:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$vattr:meta])* $wire:literal => $variant:ident ),* $(,)?
        }
    ) => {
        $(#[$attr])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        $vis enum $name {
            $( $(#[$vattr])* $variant, )*
            /// A value not defined by this version of the schema, kept verbatim.
            Other($crate::codec::__private::String),
        }

        impl $name {
            /// Every value defined by the schema (excluding [`Other`](Self::Other)).
            pub const ALL: &'static [$name] = &[$($name::$variant),*];

            /// The value's representation on the wire.
            pub fn as_str(&self) -> &str {
                match self {
                    $( $name::$variant => $wire, )*
                    $name::Other(s) => s.as_str(),
                }
            }

            /// True for a value this version of the schema does not define.
            pub fn is_extension(&self) -> bool {
                matches!(self, $name::Other(_))
            }
        }

        impl ::core::fmt::Display for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl $crate::codec::Merge for $name {
            fn merge(&mut self, update: Self) { *self = update; }
        }

        impl ::core::convert::From<&str> for $name {
            fn from(s: &str) -> Self {
                match s {
                    $( $wire => $name::$variant, )*
                    _ => {
                        use $crate::codec::__private::ToOwned as _;
                        $name::Other(s.to_owned())
                    }
                }
            }
        }

        impl $crate::codec::__private::Serialize for $name {
            fn serialize<S>(&self, serializer: S)
                -> $crate::codec::__private::Result<S::Ok, S::Error>
            where
                S: $crate::codec::__private::Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> $crate::codec::__private::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D)
                -> $crate::codec::__private::Result<Self, D::Error>
            where
                D: $crate::codec::__private::Deserializer<'de>,
            {
                struct __V;
                impl<'de> $crate::codec::__private::Visitor<'de> for __V {
                    type Value = $name;
                    fn expecting(&self, f: &mut $crate::codec::__private::fmt::Formatter<'_>)
                        -> $crate::codec::__private::fmt::Result
                    {
                        f.write_str(concat!("a `", stringify!($name), "` value"))
                    }
                    fn visit_str<E>(self, v: &str)
                        -> $crate::codec::__private::Result<$name, E>
                    where
                        E: $crate::codec::__private::DeError,
                    {
                        $crate::codec::__private::Ok(<$name as ::core::convert::From<&str>>::from(v))
                    }
                }
                deserializer.deserialize_str(__V)
            }
        }
    };
}

/// Defines a numeric identifier newtype (`xs:restriction` of an integer type).
///
/// SPINE uses bare integers for a great many identifiers — `limitId`, `measurementId`,
/// `electricalConnectionId`, `parameterId`, … — that are routinely correlated across
/// features. Giving each its own type makes mixing them up a compile error, while the
/// wire representation stays a plain JSON number.
#[macro_export]
macro_rules! eebus_id {
    (
        $(#[$attr:meta])*
        $vis:vis struct $name:ident($inner:ty);
    ) => {
        $(#[$attr])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
        $vis struct $name(pub $inner);

        impl $name {
            /// The underlying value.
            pub const fn get(self) -> $inner { self.0 }
        }

        impl ::core::convert::From<$inner> for $name {
            fn from(v: $inner) -> Self { $name(v) }
        }

        impl ::core::convert::From<$name> for $inner {
            fn from(v: $name) -> Self { v.0 }
        }

        impl ::core::fmt::Display for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                ::core::fmt::Display::fmt(&self.0, f)
            }
        }

        impl $crate::codec::Merge for $name {
            fn merge(&mut self, update: Self) { *self = update; }
        }

        impl $crate::codec::__private::Serialize for $name {
            fn serialize<S>(&self, serializer: S)
                -> $crate::codec::__private::Result<S::Ok, S::Error>
            where
                S: $crate::codec::__private::Serializer,
            {
                $crate::codec::__private::Serialize::serialize(&self.0, serializer)
            }
        }

        impl<'de> $crate::codec::__private::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D)
                -> $crate::codec::__private::Result<Self, D::Error>
            where
                D: $crate::codec::__private::Deserializer<'de>,
            {
                $crate::codec::__private::Ok($name(
                    <$inner as $crate::codec::__private::Deserialize>::deserialize(deserializer)?,
                ))
            }
        }
    };
}

/// Defines a string-valued newtype (`xs:restriction` of `xs:string`, `xs:hexBinary`,
/// `xs:dateTime`, `xs:duration`, or a union of those).
#[macro_export]
macro_rules! eebus_str {
    (
        $(#[$attr:meta])*
        $vis:vis struct $name:ident;
    ) => {
        $(#[$attr])*
        #[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
        $vis struct $name(pub $crate::codec::__private::String);

        impl $name {
            /// The value as a string slice.
            pub fn as_str(&self) -> &str { self.0.as_str() }
        }

        impl ::core::convert::From<&str> for $name {
            fn from(v: &str) -> Self {
                use $crate::codec::__private::ToOwned as _;
                $name(v.to_owned())
            }
        }

        impl ::core::convert::From<$crate::codec::__private::String> for $name {
            fn from(v: $crate::codec::__private::String) -> Self { $name(v) }
        }

        impl ::core::fmt::Display for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl $crate::codec::Merge for $name {
            fn merge(&mut self, update: Self) { *self = update; }
        }

        impl $crate::codec::__private::Serialize for $name {
            fn serialize<S>(&self, serializer: S)
                -> $crate::codec::__private::Result<S::Ok, S::Error>
            where
                S: $crate::codec::__private::Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> $crate::codec::__private::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D)
                -> $crate::codec::__private::Result<Self, D::Error>
            where
                D: $crate::codec::__private::Deserializer<'de>,
            {
                $crate::codec::__private::Ok($name(
                    <$crate::codec::__private::String as
                        $crate::codec::__private::Deserialize>::deserialize(deserializer)?,
                ))
            }
        }
    };
}

/// Defines an `xs:choice` group as a Rust enum encoded as a single-key object.
///
/// The SPINE command frame carries exactly one payload function out of roughly two
/// hundred, chosen by `DataChoiceGroup`. Modelling that as an enum makes "two payloads
/// in one command" unrepresentable, where a struct of ~200 `Option` fields — the shape
/// other implementations use — cannot.
#[macro_export]
macro_rules! eebus_choice {
    (
        $(#[$attr:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$vattr:meta])* $kind:ident $wire:literal => $variant:ident($ty:ty) ),* $(,)?
        }
    ) => {
        $(#[$attr])*
        #[derive(Clone, Debug, PartialEq)]
        $vis enum $name {
            $( $(#[$vattr])* $variant($ty), )*
        }

        impl $name {
            /// Every wire name this choice can take.
            pub const KEYS: &'static [&'static str] = &[$($wire),*];

            /// The wire name of the selected alternative.
            pub const fn key(&self) -> &'static str {
                match self { $( $name::$variant(_) => $wire, )* }
            }

            /// True when this alternative holds a list whose entries carry identifiers,
            /// and so can take part in a Restricted Function Exchange merge.
            pub const fn is_mergeable_list(&self) -> bool {
                match self {
                    $( $name::$variant(_) => $crate::eebus_choice!(@is_list $kind), )*
                }
            }

            /// Applies `update` to this alternative with the semantics `partial` asks for.
            ///
            /// A partial update merges into what is stored: for a list, entry by entry
            /// by identifier; for anything else, element by element. A full update
            /// replaces. Returns an error when the two alternatives are not the same
            /// function, which is a protocol error rather than a merge.
            #[allow(unreachable_patterns)]
            pub fn apply(
                &mut self,
                update: Self,
                partial: bool,
            ) -> $crate::codec::__private::Result<(), $crate::model::rfe::FunctionMismatch> {
                match (self, update) {
                    $(
                        ($name::$variant(stored), $name::$variant(update)) => {
                            $crate::eebus_choice!(@apply $kind stored update partial);
                            $crate::codec::__private::Ok(())
                        }
                    )*
                    (stored, update) => {
                        $crate::codec::__private::Err($crate::model::rfe::FunctionMismatch {
                            stored: stored.key(),
                            received: update.key(),
                        })
                    }
                }
            }

            /// Deletes what `update` identifies: whole entries for a list, and the
            /// whole value otherwise.
            #[allow(unreachable_patterns)]
            pub fn delete(
                &mut self,
                update: &Self,
            ) -> $crate::codec::__private::Result<(), $crate::model::rfe::FunctionMismatch> {
                match (self, update) {
                    $(
                        ($name::$variant(stored), $name::$variant(update)) => {
                            $crate::eebus_choice!(@delete $kind stored update);
                            $crate::codec::__private::Ok(())
                        }
                    )*
                    (stored, update) => {
                        $crate::codec::__private::Err($crate::model::rfe::FunctionMismatch {
                            stored: stored.key(),
                            received: update.key(),
                        })
                    }
                }
            }

            /// Reads the alternative named `key` from a map access positioned at its value.
            ///
            /// Returns `Ok(None)` without consuming the value if `key` names no
            /// alternative, leaving the caller to skip it.
            pub fn read_value<'de, A>(key: &str, map: &mut A)
                -> $crate::codec::__private::Result<$crate::codec::__private::Option<Self>, A::Error>
            where
                A: $crate::codec::__private::MapAccess<'de>,
            {
                $crate::codec::__private::Ok(match key {
                    $( $wire => $crate::codec::__private::Some(
                        $name::$variant(map.next_value()?)
                    ), )*
                    _ => $crate::codec::__private::None,
                })
            }
        }

        impl $crate::codec::Merge for $name {
            // A choice with a single alternative makes the fallback arm unreachable.
            #[allow(unreachable_patterns)]
            fn merge(&mut self, update: Self) {
                match (self, update) {
                    $(
                        ($name::$variant(existing), $name::$variant(update)) => {
                            $crate::codec::Merge::merge(existing, update);
                        }
                    )*
                    (this, update) => *this = update,
                }
            }
        }

        impl $crate::codec::__private::Serialize for $name {
            fn serialize<S>(&self, serializer: S)
                -> $crate::codec::__private::Result<S::Ok, S::Error>
            where
                S: $crate::codec::__private::Serializer,
            {
                use $crate::codec::__private::SerializeMap as _;
                let mut __m = serializer.serialize_map($crate::codec::__private::Some(1))?;
                match self {
                    $( $name::$variant(__v) => __m.serialize_entry($wire, __v)?, )*
                }
                __m.end()
            }
        }

        impl<'de> $crate::codec::__private::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D)
                -> $crate::codec::__private::Result<Self, D::Error>
            where
                D: $crate::codec::__private::Deserializer<'de>,
            {
                struct __V;
                impl<'de> $crate::codec::__private::Visitor<'de> for __V {
                    type Value = $name;
                    fn expecting(&self, f: &mut $crate::codec::__private::fmt::Formatter<'_>)
                        -> $crate::codec::__private::fmt::Result
                    {
                        f.write_str(concat!("a `", stringify!($name), "` alternative"))
                    }
                    fn visit_map<A>(self, mut __map: A)
                        -> $crate::codec::__private::Result<$name, A::Error>
                    where
                        A: $crate::codec::__private::MapAccess<'de>,
                    {
                        use $crate::codec::__private::{String, DeError};
                        while let $crate::codec::__private::Some(__k) =
                            __map.next_key::<String>()?
                        {
                            if let $crate::codec::__private::Some(__v) =
                                $name::read_value(__k.as_str(), &mut __map)?
                            {
                                while __map
                                    .next_entry::<$crate::codec::__private::IgnoredAny,
                                                  $crate::codec::__private::IgnoredAny>()?
                                    .is_some() {}
                                return $crate::codec::__private::Ok(__v);
                            }
                            __map.next_value::<$crate::codec::__private::IgnoredAny>()?;
                        }
                        $crate::codec::__private::Err(A::Error::custom(concat!(
                            "no known alternative for `", stringify!($name), "`"
                        )))
                    }
                }
                deserializer.deserialize_map(__V)
            }
        }
    };

    (@is_list list) => { true };
    (@is_list plain) => { false };

    (@apply list $stored:ident $update:ident $partial:ident) => {
        if $partial {
            $crate::model::rfe::apply_partial($stored, $update);
        } else {
            $crate::model::rfe::apply_full($stored, $update);
        }
    };
    (@apply plain $stored:ident $update:ident $partial:ident) => {
        if $partial {
            $crate::codec::Merge::merge($stored, $update);
        } else {
            *$stored = $update;
        }
    };

    (@delete list $stored:ident $update:ident) => {
        $crate::model::rfe::delete_entries($stored, $update);
    };
    (@delete plain $stored:ident $update:ident) => {
        let _ = $update;
        *$stored = ::core::default::Default::default();
    };
}

/// Declares which elements identify one entry of a list.
///
/// SPINE lists are keyed by "primary" and "sub" identifiers, which the schemas do not
/// mark but the specification's convention places first among an entry's elements. The
/// generator derives them from that convention; this macro turns them into the
/// [`Identified`](crate::model::rfe::Identified) implementation that Restricted Function
/// Exchange needs in order to know which stored entry an update refers to.
#[macro_export]
macro_rules! eebus_identity {
    ($name:ident { $($field:ident),+ $(,)? }) => {
        impl $crate::model::rfe::Identified for $name {
            const IDENTIFIER_FIELDS: &'static [&'static str] = &[$(stringify!($field)),+];

            fn has_identity(&self) -> bool {
                true $( && self.$field.is_some() )+
            }

            fn same_entry(&self, other: &Self) -> bool {
                self.has_identity()
                    && other.has_identity()
                    $( && self.$field == other.$field )+
            }
        }
    };
}

/// Declares a list function and the entry type it holds.
#[macro_export]
macro_rules! eebus_list {
    ($list:ident => $item:ty { $field:ident }) => {
        impl $crate::model::rfe::ListData for $list {
            type Item = $item;

            fn entries(&self) -> ::core::option::Option<&[Self::Item]> {
                self.$field.as_deref()
            }

            fn entries_mut(
                &mut self,
            ) -> &mut ::core::option::Option<$crate::codec::__private::Vec<Self::Item>> {
                &mut self.$field
            }
        }
    };
}

/// Declares how an *elements* filter maps onto the data type it addresses.
///
/// A filter marked `tag` names a leaf element, which a delete removes outright. One
/// marked `nested` names a structured element: an empty filter removes the whole
/// structure, a populated one descends into it. `nested_each` is the same, for an
/// element that repeats: the filter applies to every occurrence. That distinction is what lets the
/// combined delete-and-write of LPC §3.4.1.4 remove `timePeriod.endTime` while leaving
/// the rest of `timePeriod` alone.
#[macro_export]
macro_rules! eebus_elements {
    ($target:ident => $elements:ident { $($kind:ident $field:ident),* $(,)? }) => {
        impl $crate::model::rfe::Elements for $elements {
            type Target = $target;

            #[allow(unused_variables)]
            fn clear_from(&self, target: &mut Self::Target) {
                $( $crate::eebus_elements!(@clear $kind self target $field); )*
            }

            #[allow(unused_variables)]
            fn retain_in(&self, target: &mut Self::Target) {
                $( $crate::eebus_elements!(@retain $kind self target $field); )*
            }
        }
    };

    (@clear tag $self:ident $target:ident $field:ident) => {
        if $self.$field.is_some() {
            $target.$field = ::core::option::Option::None;
        }
    };
    (@clear nested $self:ident $target:ident $field:ident) => {
        if let ::core::option::Option::Some(sub) = &$self.$field {
            if sub.is_empty() {
                $target.$field = ::core::option::Option::None;
            } else if let ::core::option::Option::Some(inner) = &mut $target.$field {
                $crate::model::rfe::Elements::clear_from(sub, inner);
            }
        }
    };
    (@clear nested_each $self:ident $target:ident $field:ident) => {
        if let ::core::option::Option::Some(sub) = &$self.$field {
            if sub.is_empty() {
                $target.$field = ::core::option::Option::None;
            } else if let ::core::option::Option::Some(items) = &mut $target.$field {
                for item in items.iter_mut() {
                    $crate::model::rfe::Elements::clear_from(sub, item);
                }
            }
        }
    };
    (@retain tag $self:ident $target:ident $field:ident) => {
        if $self.$field.is_none() {
            $target.$field = ::core::option::Option::None;
        }
    };
    (@retain nested $self:ident $target:ident $field:ident) => {
        match &$self.$field {
            ::core::option::Option::None => $target.$field = ::core::option::Option::None,
            ::core::option::Option::Some(sub) => {
                if !sub.is_empty()
                    && let ::core::option::Option::Some(inner) = &mut $target.$field
                {
                    $crate::model::rfe::Elements::retain_in(sub, inner);
                }
            }
        }
    };
    (@retain nested_each $self:ident $target:ident $field:ident) => {
        match &$self.$field {
            ::core::option::Option::None => $target.$field = ::core::option::Option::None,
            ::core::option::Option::Some(sub) => {
                if !sub.is_empty()
                    && let ::core::option::Option::Some(items) = &mut $target.$field
                {
                    for item in items.iter_mut() {
                        $crate::model::rfe::Elements::retain_in(sub, item);
                    }
                }
            }
        }
    };
}

/// Declares how a *selectors* filter matches entries of a list.
///
/// Only elements the selector and the entry share by name can be compared directly.
/// Anything else — `timestampInterval` and its kin, which select a range rather than a
/// value — is listed in [`UNSUPPORTED_FIELDS`](crate::model::rfe::Selectors::UNSUPPORTED_FIELDS)
/// so that the engine can answer a request using one with SPINE `errorNumber` 8,
/// "restricted function exchange combination not supported", instead of quietly
/// returning the wrong entries.
#[macro_export]
macro_rules! eebus_selectors {
    (
        $target:ident => $selectors:ident {
            $($field:ident),* $(,)?
        } unsupported { $($unsupported:literal),* $(,)? }
    ) => {
        impl $crate::model::rfe::Selectors for $selectors {
            type Target = $target;

            const UNSUPPORTED_FIELDS: &'static [&'static str] = &[$($unsupported),*];

            #[allow(unused_variables)]
            fn matches(&self, target: &Self::Target) -> bool {
                $(
                    if let ::core::option::Option::Some(wanted) = &self.$field
                        && target.$field.as_ref() != ::core::option::Option::Some(wanted)
                    {
                        return false;
                    }
                )*
                true
            }

            #[allow(unused_variables)]
            fn is_empty(&self) -> bool {
                true $( && self.$field.is_none() )*
            }
        }
    };
}
