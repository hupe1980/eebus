//! The counts the documentation quotes, held to the generated code.
//!
//! The README and four pages of the site state how many types `cargo xtask codegen`
//! emits and how many functions the Restricted Function Exchange table covers. Those
//! numbers move whenever the generator or the schemas do; a failure here names the files
//! to correct.

use std::fs;
use std::path::Path;

/// SPINE types emitted into `src/model/generated`.
const SPINE_TYPES: usize = 830;

/// SHIP message types emitted into `src/ship/generated`.
const SHIP_TYPES: usize = 47;

/// Functions the Restricted Function Exchange table covers.
const RESTRICTED_FUNCTIONS: usize = 141;

/// Where each count is quoted, so a failure says what to edit.
const QUOTED_IN: &str = "README.md, src/lib.rs, site/content/_index.md, \
site/content/docs/architecture.md, site/content/docs/codegen.md";

/// The macros that each define one public type.
const TYPE_MACROS: &[&str] = &[
    "crate::eebus_struct!",
    "crate::eebus_enum!",
    "crate::eebus_enum_ext!",
    "crate::eebus_id!",
    "crate::eebus_str!",
    "crate::eebus_choice!",
];

fn count_types(dir: &str) -> usize {
    let mut total = 0;
    for entry in fs::read_dir(dir).expect("the generated directory is committed") {
        let path = entry.expect("a directory entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let source = fs::read_to_string(&path).expect("readable");
        for line in source.lines() {
            // Only an invocation at the start of a line defines a type; the macros also
            // appear indented inside each other's expansions.
            if TYPE_MACROS.iter().any(|m| line.starts_with(m)) {
                total += 1;
            }
        }
    }
    total
}

#[test]
fn the_generated_spine_model_is_the_size_the_documentation_claims() {
    let counted = count_types("src/model/generated");
    assert_eq!(
        counted, SPINE_TYPES,
        "the generated SPINE model now holds {counted} types, not {SPINE_TYPES}. \
         Update the count in: {QUOTED_IN}"
    );
}

#[test]
fn the_generated_ship_messages_are_the_size_the_documentation_claims() {
    let counted = count_types("src/ship/generated");
    assert_eq!(
        counted, SHIP_TYPES,
        "the generated SHIP messages now hold {counted} types, not {SHIP_TYPES}. \
         Update the count in: {QUOTED_IN}"
    );
}

#[test]
fn the_restricted_function_exchange_table_covers_what_the_documentation_claims() {
    let source = fs::read_to_string(Path::new("src/model/generated/command_frame.rs"))
        .expect("the command frame is committed");
    let table = source
        .split_once("crate::eebus_restrict! {")
        .expect("the table is generated")
        .1;
    let counted = table
        .lines()
        .filter(|line| {
            let line = line.trim_start();
            line.starts_with("list \"") || line.starts_with("plain \"")
        })
        .count();
    assert_eq!(
        counted, RESTRICTED_FUNCTIONS,
        "the Restricted Function Exchange table now covers {counted} functions, \
         not {RESTRICTED_FUNCTIONS}. Update the count in: {QUOTED_IN}"
    );
}
