//! Publishes the Restricted Function Exchange table as a documentation page.
//!
//! SPINE admits thousands of implementation variations, and the one honest answer to
//! "which of them does this implement" is the table the engine actually consults. It
//! exists — the generator resolves the schemas' naming convention into it and
//! `possibleOperations` is derived from it — and until now it was readable only by opening
//! a generated Rust file.
//!
//! The source here is that generated file rather than the schemas, deliberately: the
//! schemas are not redistributable and this task has to run without them, and the
//! committed macro invocation *is* what the crate compiles. A page generated from anything
//! else would be a second claim to keep in step.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

/// Where the table lives, and where the page goes.
const MODEL: &str = "src/model/generated/command_frame.rs";
const PAGE: &str = "site/content/docs/functions.md";

/// What the table says about one function.
struct Row {
    /// The wire name, e.g. `loadControlLimitListData`.
    wire: String,
    /// A list of identified entries, or a single value.
    list: bool,
    /// The schemas give it a selectors filter.
    selectors: bool,
    /// The schemas give it an elements filter.
    elements: bool,
}

impl Row {
    /// What a peer may ask for, in the words `possibleOperations` uses.
    fn exchange(&self) -> &'static str {
        match (self.selectors, self.elements) {
            (true, true) => "entries and elements",
            (true, false) => "entries",
            (false, true) => "elements",
            (false, false) => "whole function only",
        }
    }
}

pub fn run(root: &Path) -> Result<String, String> {
    let source = std::fs::read_to_string(root.join(MODEL))
        .map_err(|e| format!("{MODEL} is not readable: {e}"))?;

    let all = functions(&source)?;
    let restrictable = restrictable(&source)?;
    if restrictable.is_empty() {
        return Err(format!("no `eebus_restrict!` table found in {MODEL}"));
    }

    let page = render(&all, &restrictable);
    let path = root.join(PAGE);
    let unchanged = std::fs::read_to_string(&path).is_ok_and(|held| held == page);
    std::fs::write(&path, page).map_err(|e| format!("{PAGE} is not writable: {e}"))?;

    Ok(format!(
        "{PAGE}: {} of {} functions can be exchanged in part{}",
        restrictable.len(),
        all.len(),
        if unchanged { " (unchanged)" } else { "" }
    ))
}

/// Every function the payload choice defines, from the `eebus_choice!` block.
fn functions(source: &str) -> Result<Vec<String>, String> {
    let block = block(source, "crate::eebus_choice! {", "pub enum CmdData {")
        .ok_or_else(|| format!("no `CmdData` choice in {MODEL}"))?;
    Ok(block
        .lines()
        .filter_map(|line| {
            quoted(
                line.trim()
                    .strip_prefix("plain ")
                    .or_else(|| line.trim().strip_prefix("list "))?,
            )
        })
        .collect())
}

/// The functions the RFE table covers, from the `eebus_restrict!` block.
fn restrictable(source: &str) -> Result<BTreeMap<String, Row>, String> {
    let block = block(
        source,
        "crate::eebus_restrict! {",
        "CmdData by FilterSelectors / FilterElements {",
    )
    .ok_or_else(|| format!("no `eebus_restrict!` table in {MODEL}"))?;

    let mut rows = BTreeMap::new();
    for line in block.lines() {
        let line = line.trim().trim_end_matches(';');
        let Some(rest) = line
            .strip_prefix("list ")
            .map(|r| (true, r))
            .or_else(|| line.strip_prefix("plain ").map(|r| (false, r)))
        else {
            continue;
        };
        let (list, rest) = rest;
        let Some(wire) = quoted(rest) else { continue };
        rows.insert(
            wire.clone(),
            Row {
                wire,
                list,
                selectors: rest.contains(" sel "),
                elements: rest.contains(" el "),
            },
        );
    }
    Ok(rows)
}

/// The body of a macro invocation, from the line after `open` to its closing brace.
fn block<'a>(source: &'a str, macro_start: &str, open: &str) -> Option<&'a str> {
    let start = source.find(macro_start)?;
    let inner = source[start..].find(open)? + start + open.len();
    let end = source[inner..].find("\n    }")? + inner;
    Some(&source[inner..end])
}

/// The first double-quoted string on a line.
fn quoted(line: &str) -> Option<String> {
    let start = line.find('"')? + 1;
    let end = line[start..].find('"')? + start;
    Some(line[start..end].to_string())
}

fn render(all: &[String], rows: &BTreeMap<String, Row>) -> String {
    let mut out = String::with_capacity(48 * 1024);
    let lists = rows.values().filter(|r| r.list).count();

    out.push_str(
        "+++\n\
         title = \"Which functions can be exchanged in part\"\n\
         description = \"The generated Restricted Function Exchange table: every SPINE \
         function this crate can serve a filtered read, a partial write or a filtered \
         delete on, and every function it cannot.\"\n\
         weight = 65\n\
         [extra]\n\
         group = \"Protocol\"\n\
         +++\n\n",
    );

    let _ = write!(
        out,
        "SPINE admits thousands of implementation variations, and \"which ones does this \
         implement\" is the first question an integrator has to answer. For Restricted \
         Function Exchange the answer is a table, and this is it — **generated from the \
         same source the compiler reads**, by `cargo xtask rfe-table`, so it cannot drift \
         from what the engine does.\n\n\
         Of the **{total}** functions the SPINE payload defines, **{covered}** can be \
         exchanged in part: {lists} are lists of identified entries and {plains} are single \
         values.\n\n",
        total = all.len(),
        covered = rows.len(),
        lists = lists,
        plains = rows.len() - lists,
    );

    out.push_str(
        "## What the columns mean\n\n\
         Nothing in the XML Schemas links a data type to its selectors and elements \
         filters — the link is a naming convention, which the generator resolves. \
         **Selectors** choose which entries of a list a command addresses; **elements** \
         choose which parts of them. A function with both can be read, written and deleted \
         at either granularity; one with neither is exchanged whole.\n\n\
         This table is also what `possibleOperations` is derived from. A feature announces \
         `read.partial` only for a function listed here, and a filtered request for \
         anything else is answered `errorNumber` 8 rather than approximately — see \
         [Conformance](@/docs/conformance.md).\n\n\
         A **delete** is a restricted exchange too: its `selectors` choose the entries and \
         its `elements` choose the parts of them to remove, and a command may carry a \
         delete filter and a partial-update filter at once. See \
         [Restricted Function Exchange](@/docs/spine.md#restricted-function-exchange).\n\n\
         ## Exchangeable in part\n\n\
         | Function | Shape | Addressable by |\n|---|---|---|\n",
    );
    for row in rows.values() {
        let _ = writeln!(
            out,
            "| `{}` | {} | {} |",
            row.wire,
            if row.list { "list" } else { "value" },
            row.exchange(),
        );
    }

    let uncovered: Vec<&String> = all
        .iter()
        .filter(|wire| !rows.contains_key(*wire))
        .collect();
    let _ = write!(
        out,
        "\n## Whole function only\n\n\
         The schemas give {} no selectors and no elements filter, so there is nothing to \
         narrow {} by. A feature serving one announces `read` without `partial`, and a peer \
         that sends a filter for it is answered `errorNumber` 8 — the honest answer, and the \
         one that stops a client acting on a reply it thinks was filtered.\n\n",
        match uncovered.len() {
            1 => "one function".to_string(),
            n => format!("these **{n}** functions"),
        },
        if uncovered.len() == 1 { "it" } else { "them" },
    );
    for wire in &uncovered {
        let _ = writeln!(out, "- `{wire}`");
    }
    out
}
