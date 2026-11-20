//! Holding the line `#[non_exhaustive]` draws.
//!
//! An enum this crate closes — an error, an event, an outcome, a reason — grows as the crate
//! learns what is worth reporting, and adding a variant must not break a consumer's
//! `match`. An enum the specification closes — a state machine, a generated enumeration,
//! SPINE's `errorNumber` table — is exhaustive, so a new variant makes a consumer look.
//!
//! The first group is recognised by name, which is how the crate names them anyway, and
//! every public one outside the generated model must carry the attribute.

use std::fmt::Write as _;
use std::path::Path;

/// Name endings of the enums this crate closes.
const CRATE_CLOSED: &[&str] = &[
    "Error",
    "Event",
    "Outcome",
    "Reason",
    "Verdict",
    "Disconnect",
];

/// Ending in one of the above, but closed by the specification.
const SPECIFIED: &[&str] = &[
    // SPINE's errorNumber table (Resource Specification §5.2.6).
    "ErrorNumber",
    // SHIP §13.4.8's closeReason values.
    "ConnectionCloseReason",
];

pub(crate) fn run(root: &Path) -> Result<String, String> {
    let mut missing = Vec::new();
    let mut checked = 0;
    let mut stack = vec![root.join("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.is_dir() {
                if !path.ends_with("generated") {
                    stack.push(path);
                }
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let lines: Vec<&str> = text.lines().collect();
            for (index, line) in lines.iter().enumerate() {
                let Some(name) = line.trim().strip_prefix("pub enum ") else {
                    continue;
                };
                let name = name
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .next()
                    .unwrap_or("");
                if !CRATE_CLOSED.iter().any(|end| name.ends_with(end)) || SPECIFIED.contains(&name)
                {
                    continue;
                }
                checked += 1;
                // The attributes directly above the item, back to the doc comment.
                let attributed = lines[..index]
                    .iter()
                    .rev()
                    .take_while(|l| l.trim_start().starts_with("#["))
                    .any(|l| l.contains("non_exhaustive"));
                if !attributed {
                    let relative = path.strip_prefix(root).unwrap_or(&path);
                    missing.push(format!(
                        "{}:{}: pub enum {name}",
                        relative.display(),
                        index + 1
                    ));
                }
            }
        }
    }
    if missing.is_empty() {
        return Ok(format!(
            "✓ {checked} crate-closed public enums, every one #[non_exhaustive]"
        ));
    }
    missing.sort();
    let mut report = String::from(
        "these enums are closed by this crate and must be #[non_exhaustive] \
         (or, if the specification closes them, listed in xtask/src/enums.rs SPECIFIED):\n",
    );
    for m in missing {
        let _ = writeln!(report, "  {m}");
    }
    Err(report)
}
