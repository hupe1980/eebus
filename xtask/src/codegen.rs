//! Lowering the parsed EEBUS schemas to Rust.
//!
//! Two schemas go through here: SPINE 1.3.0, whose forty files become one module each
//! under `src/model/generated/`, and the SHIP 1.1.0 transfer protocol, which is small
//! enough to be a single module under `src/ship/generated.rs`. Both use the same
//! XML→JSON mapping, so both use the same emitters.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::naming::{field_name, module_name, pascal, snake, type_name, variant_name};
use crate::xsd::{self, ComplexDef, GroupDef, Particle, Schema, SimpleDef};

/// How one schema is turned into Rust.
struct Profile {
    /// Human-readable name, used in the report and in module docs.
    label: &'static str,
    /// Specification version, for the `@generated` banner.
    version: &'static str,
    xsd_dir: PathBuf,
    out_dir: PathBuf,
    /// `Some(name)` collapses the whole schema into one module file.
    single_module: Option<&'static str>,
    /// Types written by hand instead of generated.
    handwritten: &'static [&'static str],
    /// Groups that are not lowered at all.
    skip_groups: &'static [&'static str],
    /// `(group, enum name, field name)` for choice groups.
    choices: &'static [(&'static str, &'static str, &'static str)],
    /// Groups whose alternatives are folded into the enclosing choice.
    inline_choices: &'static [&'static str],
    /// The module path prefix generated modules import from.
    parent_use: &'static str,
}

const SPINE_CHOICES: &[(&str, &str, &str)] = &[
    ("DataChoiceGroup", "CmdData", "data"),
    ("DataSelectorsChoiceGroup", "FilterSelectors", "selectors"),
    ("DataElementsChoiceGroup", "FilterElements", "elements"),
];

const SHIP_CHOICES: &[(&str, &str, &str)] = &[
    ("MsgTypeControlGroup", "ControlMessage", "control"),
    ("MsgTypeDataGroup", "DataMessage", "data"),
    ("MsgTypeEndGroup", "EndMessage", "end"),
];

pub fn run(root: &Path) -> Result<String, String> {
    let spine = Profile {
        label: "SPINE",
        version: "1.3.0",
        xsd_dir: root.join("specs/EEBus_SPINE_V1.3.0/EEBus_SPINE_V1.3.0_Final_hp/XSDs"),
        out_dir: root.join("src/model/generated"),
        single_module: None,
        handwritten: &["ElementTagType", "CmdType", "FilterType", "ErrorNumberType"],
        skip_groups: &[],
        choices: SPINE_CHOICES,
        inline_choices: &[],
        parent_use: "super::super",
    };
    let ship = Profile {
        label: "SHIP",
        version: "1.1.0",
        xsd_dir: root.join(
            "specs/EEBus_SHIP_TS_Specification_v1.1.0_public/\
             EEBus_SHIP_TS_Specification_v1.1.0_public",
        ),
        out_dir: root.join("src/ship/generated"),
        single_module: Some("messages"),
        handwritten: &[],
        // MSG_TYPE=init is the two-byte CMI handshake, not a JSON message, and
        // MessageValueGroup only exists to union the four message-type groups.
        skip_groups: &["MsgTypeInitGroup", "MessageValueGroup"],
        choices: SHIP_CHOICES,
        inline_choices: &["MsgTypeControlKeyMaterialExchangeGroup"],
        parent_use: "super::super",
    };

    let mut report = String::new();
    for profile in [spine, ship] {
        if !profile.xsd_dir.is_dir() {
            return Err(format!(
                "{} schemas not found at {}.\n\
                 Download them from https://www.eebus.org/specifications-media/ \
                 (free registration) and unpack into specs/.",
                profile.label,
                profile.xsd_dir.display()
            ));
        }
        let schema = xsd::load_dir(&profile.xsd_dir, |stem| {
            profile
                .single_module
                .map_or_else(|| module_name(stem), str::to_string)
        })?;
        let lowered = lower(&profile, &schema)?;
        write_modules(&profile, &lowered)?;
        writeln!(
            report,
            "{}: {} types ({} structs, {} enums, {} newtypes, {} choices, {} aliases) \
             in {} module(s) → {}",
            profile.label,
            lowered.total(),
            lowered.structs,
            lowered.enums,
            lowered.newtypes,
            lowered.choices,
            lowered.aliases,
            lowered.modules.len(),
            profile.out_dir.display()
        )
        .unwrap();
    }
    Ok(report.trim_end().to_string())
}

#[derive(Default)]
struct Lowered {
    modules: BTreeMap<String, String>,
    structs: usize,
    enums: usize,
    newtypes: usize,
    choices: usize,
    aliases: usize,
}

impl Lowered {
    fn total(&self) -> usize {
        self.structs + self.enums + self.newtypes + self.choices + self.aliases
    }
}

struct Ctx<'a> {
    profile: &'a Profile,
    schema: &'a Schema,
    /// `*EnumType` names absorbed into an extensible enum.
    consumed: BTreeSet<String>,
}

impl Ctx<'_> {
    fn choice_type(&self, group: &str) -> String {
        self.profile
            .choices
            .iter()
            .find(|(g, _, _)| *g == group)
            .map(|(_, t, _)| (*t).to_string())
            .unwrap_or_else(|| pascal(group.strip_suffix("Group").unwrap_or(group)))
    }

    fn choice_field(&self, group: &str) -> String {
        self.profile
            .choices
            .iter()
            .find(|(g, _, _)| *g == group)
            .map(|(_, _, f)| (*f).to_string())
            .unwrap_or_else(|| snake(group.strip_suffix("Group").unwrap_or(group)))
    }

    fn is_generated_choice(&self, group: &str) -> bool {
        !self.profile.skip_groups.contains(&group) && !self.profile.inline_choices.contains(&group)
    }
}

fn lower(profile: &Profile, schema: &Schema) -> Result<Lowered, String> {
    let mut consumed = BTreeSet::new();
    for def in schema.simple.values() {
        if let SimpleDef::Union { members } = def
            && members.iter().any(|m| m == "EnumExtendType")
        {
            for m in members {
                if m.ends_with("EnumType") {
                    consumed.insert(m.clone());
                }
            }
        }
    }
    let ctx = Ctx {
        profile,
        schema,
        consumed,
    };

    let mut out = Lowered::default();
    let mut bodies: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let default_module = profile.single_module.unwrap_or("common_data_types");

    let module_of = |name: &str| {
        schema
            .module_of
            .get(name)
            .cloned()
            .unwrap_or_else(|| default_module.to_string())
    };

    for (name, def) in &schema.simple {
        if ctx.consumed.contains(name) || profile.handwritten.contains(&name.as_str()) {
            continue;
        }
        let (text, kind) = emit_simple(&ctx, name, def)?;
        out.count(&kind);
        bodies
            .entry(module_of(name))
            .or_default()
            .push((name.clone(), text));
    }

    for (name, def) in &schema.complex {
        if profile.handwritten.contains(&name.as_str()) {
            continue;
        }
        let (text, kind) = emit_complex(&ctx, name, def)?;
        out.count(&kind);
        bodies
            .entry(module_of(name))
            .or_default()
            .push((name.clone(), text));
    }

    let mergeable = mergeable_lists(&ctx)?;

    for (name, group) in &schema.groups {
        if !group.is_choice || !ctx.is_generated_choice(name) {
            continue;
        }
        let text = emit_choice(&ctx, name, group, &mergeable)?;
        out.choices += 1;
        let module = profile.single_module.unwrap_or("command_frame").to_string();
        bodies.entry(module).or_default().push((name.clone(), text));
    }

    let rfe = emit_rfe_metadata(&ctx, &mut bodies, &module_of)?;
    if let Some(text) = emit_restrict_table(&ctx, &rfe)? {
        let module = profile.single_module.unwrap_or("command_frame").to_string();
        bodies
            .entry(module)
            .or_default()
            .push(("~restrict".to_string(), text));
    }

    for (module, mut items) in bodies {
        items.sort_by(|a, b| a.0.cmp(&b.0));
        let mut src = module_header(profile, &module);
        for (_, text) in items {
            src.push_str(&text);
            src.push('\n');
        }
        out.modules.insert(module, src);
    }

    Ok(out)
}

impl Lowered {
    fn count(&mut self, kind: &Kind) {
        match kind {
            Kind::Struct => self.structs += 1,
            Kind::Enum => self.enums += 1,
            Kind::Newtype => self.newtypes += 1,
            Kind::Alias => self.aliases += 1,
        }
    }
}

enum Kind {
    Struct,
    Enum,
    Newtype,
    Alias,
}

fn module_header(profile: &Profile, module: &str) -> String {
    format!(
        "//! {label} `{module}` types.\n\
         //!\n\
         //! @generated by `cargo xtask codegen` from the {label} {version} XML Schema.\n\
         //! Do not edit by hand; edit the generator instead.\n\
         \n\
         #[allow(unused_imports)]\n\
         use {parent}::*;\n\
         #[allow(unused_imports)]\n\
         use alloc::{{string::String, vec::Vec}};\n\n",
        label = profile.label,
        version = profile.version,
        parent = profile.parent_use,
    )
}

fn emit_simple(ctx: &Ctx<'_>, name: &str, def: &SimpleDef) -> Result<(String, Kind), String> {
    let rust = type_name(name);
    let mut s = String::new();
    match def {
        SimpleDef::Enum { values } => {
            writeln!(s, "crate::eebus_enum! {{").unwrap();
            writeln!(s, "    /// `{name}`.").unwrap();
            writeln!(s, "    pub enum {rust} {{").unwrap();
            for (wire, variant) in dedupe_variants(values) {
                writeln!(s, "        /// `{wire}`.").unwrap();
                writeln!(s, "        {wire:?} => {variant},").unwrap();
            }
            writeln!(s, "    }}\n}}").unwrap();
            Ok((s, Kind::Enum))
        }
        SimpleDef::Union { members } => {
            if members.iter().any(|m| m == "EnumExtendType")
                && let Some(em) = members.iter().find(|m| m.ends_with("EnumType"))
                && let Some(SimpleDef::Enum { values }) = ctx.schema.simple.get(em)
            {
                writeln!(s, "crate::eebus_enum_ext! {{").unwrap();
                writeln!(s, "    /// `{name}`.").unwrap();
                writeln!(s, "    ///").unwrap();
                writeln!(
                    s,
                    "    /// Extensible: vendors may add values of the form `_i:<PEN>_<name>`,"
                )
                .unwrap();
                writeln!(s, "    /// which are preserved in [`Other`](Self::Other).").unwrap();
                writeln!(s, "    pub enum {rust} {{").unwrap();
                for (wire, variant) in dedupe_variants(values) {
                    writeln!(s, "        /// `{wire}`.").unwrap();
                    writeln!(s, "        {wire:?} => {variant},").unwrap();
                }
                writeln!(s, "    }}\n}}").unwrap();
                return Ok((s, Kind::Enum));
            }
            writeln!(s, "crate::eebus_str! {{").unwrap();
            writeln!(s, "    /// `{name}` (union of `{}`).", members.join(" | ")).unwrap();
            writeln!(s, "    pub struct {rust};\n}}").unwrap();
            Ok((s, Kind::Newtype))
        }
        SimpleDef::List { item } => {
            let inner = rust_type(ctx, item, false)?;
            writeln!(s, "/// `{name}` (whitespace-separated list).").unwrap();
            writeln!(s, "pub type {rust} = Vec<{inner}>;").unwrap();
            Ok((s, Kind::Alias))
        }
        SimpleDef::Restrict { base } => match builtin(base) {
            Some("String") => {
                writeln!(s, "crate::eebus_str! {{").unwrap();
                writeln!(s, "    /// `{name}`.").unwrap();
                writeln!(s, "    pub struct {rust};\n}}").unwrap();
                Ok((s, Kind::Newtype))
            }
            Some(prim) if prim == "bool" || prim.starts_with('f') => {
                writeln!(s, "/// `{name}`.").unwrap();
                writeln!(s, "pub type {rust} = {prim};").unwrap();
                Ok((s, Kind::Alias))
            }
            Some(prim) => {
                writeln!(s, "crate::eebus_id! {{").unwrap();
                writeln!(s, "    /// `{name}`.").unwrap();
                writeln!(s, "    pub struct {rust}({prim});\n}}").unwrap();
                Ok((s, Kind::Newtype))
            }
            None => {
                let target = rust_type(ctx, base, false)?;
                writeln!(s, "/// `{name}`.").unwrap();
                writeln!(s, "pub type {rust} = {target};").unwrap();
                Ok((s, Kind::Alias))
            }
        },
    }
}

fn emit_complex(ctx: &Ctx<'_>, name: &str, def: &ComplexDef) -> Result<(String, Kind), String> {
    let rust = type_name(name);

    // `xs:simpleContent` means the type carries a scalar, with attributes the JSON
    // mapping absorbs; it is that scalar as far as the wire is concerned.
    if let Some(base) = &def.simple_content {
        let target = rust_type(ctx, base, false)?;
        let mut s = String::new();
        writeln!(s, "/// `{name}` (a `{base}` with attributes).").unwrap();
        writeln!(s, "pub type {rust} = {target};").unwrap();
        return Ok((s, Kind::Alias));
    }

    let fields = flatten_fields(ctx, name, def, 0)?;

    if fields.is_empty()
        && let Some(base) = &def.restricts
    {
        let target = type_name(base);
        let mut s = String::new();
        writeln!(s, "/// `{name}` (restriction of `{base}`).").unwrap();
        writeln!(s, "pub type {rust} = {target};").unwrap();
        return Ok((s, Kind::Alias));
    }

    let mut s = String::new();
    writeln!(s, "crate::eebus_struct! {{").unwrap();
    writeln!(s, "    /// `{name}`.").unwrap();
    writeln!(s, "    pub struct {rust} {{").unwrap();
    for f in &fields {
        writeln!(s, "        /// `{}`.", f.doc_name()).unwrap();
        writeln!(
            s,
            "        {:?} => {}: {},",
            f.wire, f.rust_name, f.rust_type
        )
        .unwrap();
    }
    writeln!(s, "    }}\n}}").unwrap();
    Ok((s, Kind::Struct))
}

/// The list types whose entries carry identifiers, and so can be merged entry by entry.
///
/// A list without identifiers — `nodeManagementUseCaseData`, for one — has no way to
/// say which stored entry an update refers to, so a partial update of it can only
/// replace, and the choice marks it `plain`.
fn mergeable_lists(ctx: &Ctx<'_>) -> Result<BTreeSet<String>, String> {
    let mut out = BTreeSet::new();
    for (list_name, list_def) in &ctx.schema.complex {
        let Some(item) = single_repeated_item(ctx, list_def)? else {
            continue;
        };
        let Some(item_def) = ctx.schema.complex.get(&item) else {
            continue;
        };
        if is_alias(ctx, list_name)? || is_alias(ctx, &item)? {
            continue;
        }
        if !leading_identifiers(ctx, &item, item_def)?.is_empty() {
            out.insert(list_name.clone());
        }
    }
    Ok(out)
}

fn emit_choice(
    ctx: &Ctx<'_>,
    group: &str,
    def: &GroupDef,
    mergeable: &BTreeSet<String>,
) -> Result<String, String> {
    let rust = ctx.choice_type(group);
    let mut alternatives = Vec::new();
    collect_alternatives(ctx, group, def, &mut alternatives, 0)?;

    let mut s = String::new();
    writeln!(s, "crate::eebus_choice! {{").unwrap();
    writeln!(s, "    /// `{group}`: exactly one alternative.").unwrap();
    writeln!(s, "    pub enum {rust} {{").unwrap();
    let mut seen = BTreeSet::new();
    for (wire, ty, xsd) in alternatives {
        let variant = variant_name(&wire);
        if !seen.insert(variant.clone()) {
            continue;
        }
        let kind = if mergeable.contains(&xsd) {
            "list"
        } else {
            "plain"
        };
        writeln!(s, "        /// `{wire}`.").unwrap();
        writeln!(s, "        {kind} {wire:?} => {variant}({ty}),").unwrap();
    }
    writeln!(s, "    }}\n}}").unwrap();
    Ok(s)
}

/// Gathers a choice group's alternatives, folding in nested groups listed as inline.
fn collect_alternatives(
    ctx: &Ctx<'_>,
    owner: &str,
    def: &GroupDef,
    out: &mut Vec<(String, String, String)>,
    depth: usize,
) -> Result<(), String> {
    if depth > 4 {
        return Err(format!("choice group {owner} nests too deeply"));
    }
    for p in &def.particles {
        match p {
            Particle::Element { wire, ty, .. } => {
                let resolved = resolve(ctx, ty)?;
                let rust = rust_type(ctx, &resolved, false)?;
                out.push((wire.clone(), rust, resolved));
            }
            Particle::Group { name, .. } => {
                let nested = ctx
                    .schema
                    .groups
                    .get(name)
                    .ok_or_else(|| format!("{owner} references unknown group {name}"))?;
                collect_alternatives(ctx, name, nested, out, depth + 1)?;
            }
        }
    }
    Ok(())
}

struct Field {
    wire: String,
    rust_name: String,
    rust_type: String,
    /// The resolved XSD type, needed to pair a data type with its elements filter.
    xsd_type: String,
}

impl Field {
    /// The name to show in documentation; choice groups have no wire name of their own.
    fn doc_name(&self) -> &str {
        self.wire
            .strip_prefix("@choice:")
            .unwrap_or(self.wire.as_str())
    }
}

fn flatten_fields(
    ctx: &Ctx<'_>,
    owner: &str,
    def: &ComplexDef,
    depth: usize,
) -> Result<Vec<Field>, String> {
    if depth > 8 {
        return Err(format!("type hierarchy of {owner} is too deep"));
    }
    let mut out = Vec::new();

    if let Some(base) = &def.extends
        && let Some(base_def) = ctx.schema.complex.get(base)
    {
        out.extend(flatten_fields(ctx, base, base_def, depth + 1)?);
    }

    for p in &def.particles {
        match p {
            Particle::Element { wire, ty, repeated } => {
                let resolved = resolve(ctx, ty)?;
                let rust_type = rust_type(ctx, &resolved, *repeated)?;
                out.push(Field {
                    wire: wire.clone(),
                    rust_name: field_name(wire),
                    rust_type,
                    xsd_type: resolved,
                });
            }
            Particle::Group { name, repeated } => {
                let group = ctx
                    .schema
                    .groups
                    .get(name)
                    .ok_or_else(|| format!("{owner} references unknown group {name}"))?;
                if group.is_choice {
                    let ty = ctx.choice_type(name);
                    let ty = if *repeated { format!("Vec<{ty}>") } else { ty };
                    out.push(Field {
                        wire: format!("@choice:{name}"),
                        rust_name: ctx.choice_field(name),
                        rust_type: ty,
                        xsd_type: name.clone(),
                    });
                } else {
                    let inline = ComplexDef {
                        extends: None,
                        restricts: None,
                        simple_content: None,
                        particles: group.particles.clone(),
                    };
                    out.extend(flatten_fields(ctx, name, &inline, depth + 1)?);
                }
            }
        }
    }

    // The schemas occasionally repeat an inherited element in a derived type; the last
    // declaration wins, matching XSD restriction semantics.
    let mut seen = BTreeSet::new();
    let mut deduped: Vec<Field> = Vec::new();
    for f in out.into_iter().rev() {
        if seen.insert(f.rust_name.clone()) {
            deduped.push(f);
        }
    }
    deduped.reverse();
    Ok(deduped)
}

/// Resolves `@element:name` indirections to the element's type name.
fn resolve(ctx: &Ctx<'_>, ty: &str) -> Result<String, String> {
    match ty.strip_prefix("@element:") {
        Some(element) => ctx
            .schema
            .elements
            .get(element)
            .cloned()
            .ok_or_else(|| format!("reference to undeclared element `{element}`")),
        None => Ok(ty.to_string()),
    }
}

fn rust_type(ctx: &Ctx<'_>, ty: &str, repeated: bool) -> Result<String, String> {
    let base = if let Some(prim) = builtin(ty) {
        prim.to_string()
    } else if ty == "ElementTagType" {
        "ElementTag".to_string()
    } else if ctx.consumed.contains(ty) {
        let union = ctx
            .schema
            .simple
            .iter()
            .find(|(_, d)| {
                matches!(d, SimpleDef::Union { members } if members.iter().any(|m| m == ty))
            })
            .map(|(n, _)| n.clone())
            .unwrap_or_else(|| ty.to_string());
        type_name(&union)
    } else {
        type_name(ty)
    };
    Ok(if repeated {
        format!("Vec<{base}>")
    } else {
        base
    })
}

fn builtin(ty: &str) -> Option<&'static str> {
    Some(match ty {
        "xs:string"
        | "xs:normalizedString"
        | "xs:token"
        | "xs:language"
        | "xs:anyURI"
        | "xs:hexBinary"
        | "xs:base64Binary"
        | "xs:dateTime"
        | "xs:date"
        | "xs:time"
        | "xs:duration"
        | "xs:gYear"
        | "xs:NMTOKEN" => "String",
        "xs:boolean" => "bool",
        "xs:byte" => "i8",
        "xs:short" => "i16",
        "xs:int" => "i32",
        "xs:long" | "xs:integer" => "i64",
        "xs:unsignedByte" => "u8",
        "xs:unsignedShort" => "u16",
        "xs:unsignedInt" => "u32",
        "xs:unsignedLong" => "u64",
        "xs:float" => "f32",
        "xs:double" | "xs:decimal" => "f64",
        // SHIP's `data.payload` is `xs:anyType`: an opaque document belonging to the
        // protocol named by `data.header.protocolId`, which SHIP itself never inspects.
        "xs:anyType" => "serde_json::Value",
        _ => return None,
    })
}

fn dedupe_variants(values: &[String]) -> Vec<(String, String)> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut out = Vec::with_capacity(values.len());
    for v in values {
        let mut variant = variant_name(v);
        let counter = seen.entry(variant.clone()).or_insert(0);
        *counter += 1;
        if *counter > 1 {
            variant = format!("{variant}{counter}");
        }
        out.push((v.clone(), variant));
    }
    out
}

fn write_modules(profile: &Profile, lowered: &Lowered) -> Result<(), String> {
    let out_dir = &profile.out_dir;
    if out_dir.exists() {
        std::fs::remove_dir_all(out_dir).map_err(|e| format!("clearing {out_dir:?}: {e}"))?;
    }
    std::fs::create_dir_all(out_dir).map_err(|e| format!("creating {out_dir:?}: {e}"))?;

    for (module, src) in &lowered.modules {
        let path = out_dir.join(format!("{module}.rs"));
        std::fs::write(&path, src).map_err(|e| format!("writing {path:?}: {e}"))?;
    }

    let mut mod_rs = format!(
        "//! The generated {label} {version} data model.\n\
         //!\n\
         //! @generated by `cargo xtask codegen`. Do not edit by hand.\n\
         \n\
         #![allow(clippy::large_enum_variant)]\n\
         #![allow(clippy::enum_variant_names)]\n\
         #![allow(clippy::doc_markdown)]\n\
         \n",
        label = profile.label,
        version = profile.version,
    );
    for module in lowered.modules.keys() {
        writeln!(mod_rs, "mod {module};").unwrap();
    }
    mod_rs.push('\n');
    for module in lowered.modules.keys() {
        writeln!(mod_rs, "pub use {module}::*;").unwrap();
    }
    std::fs::write(out_dir.join("mod.rs"), mod_rs)
        .map_err(|e| format!("writing {out_dir:?}/mod.rs: {e}"))?;
    format_dir(out_dir)
}

/// Runs `rustfmt` over what was just written.
///
/// Without this the generator's output differs from the same code after `cargo fmt`,
/// and the CI job that re-runs codegen and checks for drift fails on whitespace rather
/// than on a real change to the model.
fn format_dir(dir: &Path) -> Result<(), String> {
    let files: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("reading {dir:?}: {e}"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "rs"))
        .collect();

    let status = std::process::Command::new("rustfmt")
        .arg("--edition")
        .arg("2024")
        .args(&files)
        .status()
        .map_err(|e| format!("running rustfmt: {e}"))?;
    if !status.success() {
        return Err(format!("rustfmt failed on {dir:?}"));
    }
    Ok(())
}

/// Emits the Restricted Function Exchange metadata: which elements identify an entry,
/// which function holds a list, how an elements filter maps onto its data type, and how
/// a selectors filter matches.
///
/// None of this is marked in the schemas. It is derived from the conventions SPINE
/// follows throughout — identifiers first, `XListData` holding `XData`, `XDataElements`
/// mirroring `XData` — so that the merge rules can be written once, generically, rather
/// than per use case.
/// Which Restricted Function Exchange implementations were actually emitted.
///
/// The filters are matched to their data types by name, and the names do not always
/// line up: a few filter types are aliases, a few are shared by two data types, and a
/// list without leading identifiers gets no `Identified` implementation at all. Only the
/// pairs recorded here exist as trait implementations, so only they may appear in the
/// restrict table.
#[derive(Default)]
struct RfeImpls {
    /// `XListDataType` → its entry type.
    lists: BTreeMap<String, String>,
    /// Entry types that have an `Identified` implementation.
    identified: BTreeSet<String>,
    /// `XElementsType` → the data type it addresses.
    elements: BTreeMap<String, String>,
    /// `XSelectorsType` → the entry type it selects from.
    selectors: BTreeMap<String, String>,
}

fn emit_rfe_metadata(
    ctx: &Ctx<'_>,
    bodies: &mut BTreeMap<String, Vec<(String, String)>>,
    module_of: &dyn Fn(&str) -> String,
) -> Result<RfeImpls, String> {
    let schema = ctx.schema;
    let mut impls = RfeImpls::default();

    for (list_name, list_def) in &schema.complex {
        let fields = flatten_fields(ctx, list_name, list_def, 0)?;
        let [only] = fields.as_slice() else { continue };
        let Some(item_xsd) = single_repeated_item(ctx, list_def)? else {
            continue;
        };
        if !schema.complex.contains_key(&item_xsd) || is_alias(ctx, list_name)? {
            continue;
        }

        let list_rust = type_name(list_name);
        let item_rust = type_name(&item_xsd);
        let mut text = String::new();
        writeln!(
            text,
            "crate::eebus_list!({list_rust} => {item_rust} {{ {} }});",
            only.rust_name
        )
        .unwrap();
        bodies
            .entry(module_of(list_name))
            .or_default()
            .push((format!("{list_name}~list"), text));
        impls.lists.insert(list_name.clone(), item_xsd.clone());

        // Identifiers of the entry type.
        let item_def = &schema.complex[&item_xsd];
        let identifiers = leading_identifiers(ctx, &item_xsd, item_def)?;
        if !identifiers.is_empty() && !is_alias(ctx, &item_xsd)? {
            impls.identified.insert(item_xsd.clone());
            let mut text = String::new();
            writeln!(
                text,
                "crate::eebus_identity!({item_rust} {{ {} }});",
                identifiers.join(", ")
            )
            .unwrap();
            bodies
                .entry(module_of(&item_xsd))
                .or_default()
                .push((format!("{item_xsd}~identity"), text));
        }
    }

    // Elements filters mirror their data type element for element. The top-level pairs
    // are found by name; nested ones are reached by following a `nested` field into the
    // types on both sides, because the schemas give those inline definitions synthetic
    // names that do not line up.
    let mut queue: Vec<(String, String)> = schema
        .complex
        .keys()
        .filter_map(|data_name| {
            let elements_name = format!(
                "{}ElementsType",
                data_name.strip_suffix("Type").unwrap_or(data_name)
            );
            schema
                .complex
                .contains_key(&elements_name)
                .then(|| (data_name.clone(), elements_name))
        })
        .collect();
    // One implementation per filter type. Where two data types share a filter — the
    // schemas do that for a handful — the pair found by name wins, so the queue is
    // worked front to back with the name-matched seeds first.
    let mut emitted: BTreeSet<String> = BTreeSet::new();
    let mut cursor = 0usize;

    while cursor < queue.len() {
        let (data_name, elements_name) = queue[cursor].clone();
        cursor += 1;
        // The implementation is attached to the filter type, so only that side must not
        // be an alias. A scalar data side is fine: the filter then names nothing and the
        // parent's emptiness check decides.
        if is_alias(ctx, &elements_name)? {
            continue;
        }
        if !emitted.insert(elements_name.clone()) {
            continue;
        }
        let (Some(data_def), Some(elements_def)) = (
            schema.complex.get(&data_name),
            schema.complex.get(&elements_name),
        ) else {
            continue;
        };
        let data_fields = flatten_fields(ctx, &data_name, data_def, 0)?;
        let element_fields = flatten_fields(ctx, &elements_name, elements_def, 0)?;

        let mut entries = Vec::new();
        for f in &element_fields {
            let Some(data_field) = data_fields.iter().find(|d| d.rust_name == f.rust_name) else {
                continue;
            };
            if f.rust_type == "ElementTag" {
                entries.push(format!("tag {}", f.rust_name));
            } else {
                // A filter for a repeating element addresses every occurrence of it.
                let kind = if data_field.rust_type.starts_with("Vec<") {
                    "nested_each"
                } else {
                    "nested"
                };
                entries.push(format!("{kind} {}", f.rust_name));
                let pair = (data_field.xsd_type.clone(), f.xsd_type.clone());
                if schema.complex.contains_key(&pair.0)
                    && schema.complex.contains_key(&pair.1)
                    && !emitted.contains(&pair.1)
                    && !queue.contains(&pair)
                {
                    queue.push(pair);
                }
            }
        }

        let mut text = String::new();
        writeln!(
            text,
            "crate::eebus_elements!({} => {} {{ {} }});",
            type_name(&data_name),
            type_name(&elements_name),
            entries.join(", ")
        )
        .unwrap();
        bodies
            .entry(module_of(&elements_name))
            .or_default()
            .push((format!("{elements_name}~elements"), text));
        impls.elements.insert(elements_name, data_name);
    }

    // Selectors filters match entries of the list they belong to.
    for (selectors_name, selectors_def) in &schema.complex {
        let Some(list_stem) = selectors_name.strip_suffix("SelectorsType") else {
            continue;
        };
        let list_name = format!("{list_stem}Type");
        let Some(list_def) = schema.complex.get(&list_name) else {
            continue;
        };
        let Some(item_xsd) = single_repeated_item(ctx, list_def)? else {
            continue;
        };
        let Some(item_def) = schema.complex.get(&item_xsd) else {
            continue;
        };
        if is_alias(ctx, selectors_name)? {
            continue;
        }

        let item_fields = flatten_fields(ctx, &item_xsd, item_def, 0)?;
        let selector_fields = flatten_fields(ctx, selectors_name, selectors_def, 0)?;

        let mut matched = Vec::new();
        let mut unsupported = Vec::new();
        for f in &selector_fields {
            match item_fields
                .iter()
                .find(|i| i.rust_name == f.rust_name && i.rust_type == f.rust_type)
            {
                Some(_) => matched.push(f.rust_name.clone()),
                None => unsupported.push(format!("{}: {:?}", f.rust_name, f.wire)),
            }
        }

        let mut text = String::new();
        writeln!(
            text,
            "crate::eebus_selectors!({} => {} {{ {} }} unsupported {{ {} }});",
            type_name(&item_xsd),
            type_name(selectors_name),
            matched.join(", "),
            unsupported.join(", ")
        )
        .unwrap();
        bodies
            .entry(module_of(selectors_name))
            .or_default()
            .push((format!("{selectors_name}~selectors"), text));
        impls
            .selectors
            .insert(selectors_name.clone(), item_xsd.clone());
    }

    Ok(impls)
}

/// Emits the table that ties each data alternative to the filters that address it.
///
/// A line appears only where every implementation it needs was emitted above. A data
/// type whose filters are missing — because the schemas alias them, share them, or give
/// its list no identifiers — is simply absent, and the engine answers a filtered request
/// for it with `errorNumber` 8 rather than serving it unfiltered.
fn emit_restrict_table(ctx: &Ctx<'_>, rfe: &RfeImpls) -> Result<Option<String>, String> {
    let Some((data_group, sel_group, el_group)) =
        ctx.profile.choices.iter().find_map(|(g, _, _)| {
            (*g == "DataChoiceGroup").then_some((
                "DataChoiceGroup",
                "DataSelectorsChoiceGroup",
                "DataElementsChoiceGroup",
            ))
        })
    else {
        return Ok(None);
    };

    let alternatives = |group: &str| -> Result<Vec<(String, String)>, String> {
        let Some(def) = ctx.schema.groups.get(group) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        collect_alternatives(ctx, group, def, &mut out, 0)?;
        Ok(out
            .into_iter()
            .map(|(wire, _, xsd)| (xsd, variant_name(&wire)))
            .collect())
    };

    let data_alts = alternatives(data_group)?;
    let sel_alts: BTreeMap<String, String> = alternatives(sel_group)?.into_iter().collect();
    let el_alts: BTreeMap<String, String> = alternatives(el_group)?.into_iter().collect();

    let stem = |name: &str| name.strip_suffix("Type").unwrap_or(name).to_string();
    let wire_of: BTreeMap<String, String> = {
        let Some(def) = ctx.schema.groups.get(data_group) else {
            return Ok(None);
        };
        let mut raw = Vec::new();
        collect_alternatives(ctx, data_group, def, &mut raw, 0)?;
        raw.into_iter()
            .map(|(wire, _, _)| (variant_name(&wire), wire))
            .collect()
    };
    let mut lines = Vec::new();
    let mut seen = BTreeSet::new();

    for (data_xsd, variant) in &data_alts {
        if !seen.insert(variant.clone()) {
            continue;
        }
        let wire = format!("{:?}", wire_of[variant]);
        // Which type the filters address: a list's entry, or the data itself.
        let target = rfe.lists.get(data_xsd).unwrap_or(data_xsd);
        let is_list = rfe.lists.contains_key(data_xsd);

        let selectors = {
            let name = format!("{}SelectorsType", stem(data_xsd));
            (rfe.selectors.get(&name) == Some(target))
                .then(|| sel_alts.get(&name))
                .flatten()
        };
        let elements = {
            let name = format!("{}ElementsType", stem(target));
            (rfe.elements.get(&name) == Some(target))
                .then(|| el_alts.get(&name))
                .flatten()
        };

        match (is_list, selectors, elements) {
            // A list needs identifiers before its entries can be cut down safely.
            (true, sel, el) if sel.is_some() || el.is_some() => {
                if el.is_some() && !rfe.identified.contains(target) {
                    if let Some(sel) = sel {
                        lines.push(format!("        list {wire} {variant} sel {sel};"));
                    }
                    continue;
                }
                match (sel, el) {
                    (Some(sel), Some(el)) => {
                        lines.push(format!("        list {wire} {variant} sel {sel} el {el};"));
                    }
                    (Some(sel), None) => {
                        lines.push(format!("        list {wire} {variant} sel {sel};"))
                    }
                    (None, Some(el)) => {
                        // Elements alone on a list still needs the selector type to
                        // exist for the generic path; without one there is nothing to
                        // choose, so the whole list is returned element-filtered.
                        lines.push(format!("        list {wire} {variant} el {el};"));
                    }
                    (None, None) => {}
                }
            }
            (false, None, Some(el)) => {
                lines.push(format!("        plain {wire} {variant} el {el};"))
            }
            _ => {}
        }
    }

    if lines.is_empty() {
        return Ok(None);
    }
    lines.sort();
    Ok(Some(format!(
        "crate::eebus_restrict! {{\n    CmdData by FilterSelectors / FilterElements {{\n{}\n    }}\n}}\n",
        lines.join("\n")
    )))
}

/// True for a complex type emitted as a type alias rather than a struct.
///
/// The schemas define a few types purely as a restriction of another, and one as an
/// `xs:simpleContent` wrapper. Those become `pub type` aliases, so attaching a trait
/// implementation to them would collide with the implementation on the type they name.
fn is_alias(ctx: &Ctx<'_>, name: &str) -> Result<bool, String> {
    let Some(def) = ctx.schema.complex.get(name) else {
        return Ok(false);
    };
    if def.simple_content.is_some() {
        return Ok(true);
    }
    Ok(def.restricts.is_some() && flatten_fields(ctx, name, def, 0)?.is_empty())
}

/// The item type of a complex type that holds exactly one repeated element.
fn single_repeated_item(ctx: &Ctx<'_>, def: &ComplexDef) -> Result<Option<String>, String> {
    if def.extends.is_some() || def.particles.len() != 1 {
        return Ok(None);
    }
    match &def.particles[0] {
        Particle::Element {
            ty, repeated: true, ..
        } => Ok(Some(resolve(ctx, ty)?)),
        _ => Ok(None),
    }
}

/// Every identifier type SPINE defines, and the element name it is used as.
///
/// **Transcribed from the Resource Specification's Annex B.7, Table 358**, which is the
/// authority: "The following table list all identifiers that belong to the identifiers
/// concept as described in section 3.4." A naming convention is not a substitute for it —
/// `MessagingNumberType` and `PowerTimeSlotNumberType` are identifiers whose names end in
/// neither `IdType` nor anything else predictable, and `timeTableId` is declared in one
/// place as `ns_p:TimeTableIdType` and in another as a bare `xs:unsignedInt`.
///
/// Three rows of the table are deliberately absent, and each absence is a decision:
///
/// * `AbsoluteOrRelativeTimeType`/`timestamp`. §3.4.2.3 gives it rules of its own and the
///   per-function element tables mark it PRIMARY or SUB in a handful of series-shaped
///   functions (`sensingListData`, `measurementListData`) and plain metadata everywhere
///   else. Treating it as identity everywhere would split a list by timestamp; treating it
///   as identity nowhere is what the functions this crate exchanges want, and is what the
///   `identifier_classification` test pins.
/// * `AddressDeviceType`/`AddressEntityType`/`AddressFeatureType`. A SPINE address is
///   "indivisible information" (§3.4.1), and the lists keyed by one hold it as a nested
///   `deviceAddress`/`featureAddress` structure rather than as leading scalars, so the
///   leading-run rule below never reaches it.
const IDENTIFIER_TYPES: &[(&str, &str)] = &[
    ("AlarmIdType", "alarmId"),
    ("AlternativesIdType", "alternativesId"),
    ("BillCostIdType", "costId"),
    ("BillIdType", "billId"),
    ("BillPositionIdType", "positionId"),
    ("BillValueIdType", "valueId"),
    ("BindingIdType", "bindingId"),
    ("CommodityIdType", "commodityId"),
    ("ConditionIdType", "conditionId"),
    ("DeviceConfigurationKeyIdType", "keyId"),
    ("ElectricalConnectionIdType", "electricalConnectionId"),
    (
        "ElectricalConnectionCharacteristicIdType",
        "characteristicId",
    ),
    ("ElectricalConnectionParameterIdType", "parameterId"),
    ("HvacOperationModeIdType", "operationModeId"),
    ("HvacOverrunIdType", "overrunId"),
    ("HvacSystemFunctionIdType", "systemFunctionId"),
    ("IdentificationIdType", "identificationId"),
    ("IncentiveIdType", "incentiveId"),
    ("LoadControlEventIdType", "eventId"),
    ("LoadControlLimitIdType", "limitId"),
    ("MeasurementIdType", "measurementId"),
    ("MessagingNumberType", "messagingNumber"),
    ("PowerSequenceIdType", "sequenceId"),
    ("PowerTimeSlotNumberType", "slotNumber"),
    ("SessionIdType", "sessionId"),
    ("SetpointIdType", "setpointId"),
    ("StateInformationIdType", "stateInformationId"),
    ("SubscriptionIdType", "subscriptionId"),
    ("TariffIdType", "tariffId"),
    ("TaskManagementJobIdType", "jobId"),
    ("ThresholdIdType", "thresholdId"),
    ("TierBoundaryIdType", "boundaryId"),
    ("TierIdType", "tierId"),
    ("TimeSeriesIdType", "timeSeriesId"),
    ("TimeSeriesSlotIdType", "timeSeriesSlotId"),
    ("TimeSlotIdType", "timeSlotId"),
    ("TimeTableIdType", "timeTableId"),
];

/// The leading run of elements that identify one entry of a list.
///
/// SPINE §3.4.2.1 names three kinds of identifier and only two of them are identity:
///
/// * **PRIMARY** and **SUB** identifiers address the entry — "one or more SUB IDENTIFIERS
///   MAY be used within a function to identify the further dimensions of list entries".
/// * A **FOREIGN** identifier refers to other functionality on the same entity and "is not
///   used to create further dimensions of list entries". `setpointDescriptionData` carries
///   a `measurementId` and a `timeTableId`, both marked FOREIGN by Table 117;
///   `hvacSystemFunctionData` carries a `currentOperationModeId` marked FOREIGN by
///   Table 64 — and that one is not even constant, it is *the mode the function is in*.
///   Counting it as identity makes a mode change look like a second system function.
///
/// The specification carries the distinction in prose, per function. Two mechanical tests
/// reproduce it, and between them they agree with every element-rules table checked:
///
/// 1. **The element is named what Table 358 says it is named.** `operationModeId` is an
///    identifier; `currentOperationModeId` is a reference to one. Likewise
///    `currentSetpointId` against `setpointId`.
/// 2. **A *following* identifier comes from the same SPINE class as the first.** The
///    first identifier is the function's PRIMARY wherever its type happens to be declared
///    — `nodeManagementBindingData` is keyed by a `BindingIdType` from BindingManagement,
///    `smartEnergyManagementPsPriceData` by a `PowerSequenceIdType` from PowerSequences —
///    so nothing constrains it. What a *further* dimension has to be is a further
///    dimension of the same thing: `{electricalConnectionId, parameterId}` are both
///    ElectricalConnection, `{systemFunctionId, operationModeId}` both HVAC,
///    `{timeTableId, timeSlotId}` both TimeTable. A `MeasurementIdType` after a
///    `SetpointIdType` is a link into another class, which is what FOREIGN means, and
///    Table 117 says so in as many words.
///
/// The run stops at the first element that fails either test, which is what keeps a
/// trailing FOREIGN identifier — the common shape — out of the identity.
fn leading_identifiers(
    ctx: &Ctx<'_>,
    owner: &str,
    def: &ComplexDef,
) -> Result<Vec<String>, String> {
    let _ = owner;
    let mut out = Vec::new();
    let mut class: Option<&String> = None;
    for particle in &def.particles {
        let Particle::Element {
            wire, ty, repeated, ..
        } = particle
        else {
            break;
        };
        if *repeated {
            break;
        }
        let resolved = resolve(ctx, ty)?;
        // Case-insensitively: the schema spells one of them `stateInformationIdType`,
        // with a lowercase initial, where Table 358 and every other declaration use an
        // initial capital. Matching exactly would drop `stateInformationListData`'s
        // identity over a typo in the specification's own XSD.
        let Some((_, element)) = IDENTIFIER_TYPES
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(resolved.as_str()))
        else {
            break;
        };
        if element != wire {
            // Table 358 records the element an identifier type is *used as*; anything
            // else carrying that type is a reference to one (§3.4.2.1, FOREIGN).
            break;
        }
        let owning = ctx.schema.module_of.get(&resolved);
        match class {
            // The first identifier is this function's own, whatever declares its type.
            None => class = owning,
            // A further dimension of the same thing, or a link out of it.
            Some(first) if owning == Some(first) => {}
            Some(_) => break,
        }
        out.push(field_name(wire));
    }
    Ok(out)
}
