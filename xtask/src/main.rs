//! Repository automation.
//!
//! `cargo xtask codegen` regenerates `src/model/generated/` from the SPINE 1.3.0 XML
//! Schemas. The schemas are not redistributable, so they live in a git-ignored
//! reference directory (see [`reference_dir`]) and the *generated* Rust is what gets
//! committed: building the crate never requires access to the specification. Beside the
//! generated code, `schemas.sha256` records which schemas it came from, and
//! `cargo xtask check-schemas` compares the ones on this machine against it.

use std::path::{Path, PathBuf};

mod codegen;
mod devices;
mod fixtures;
mod floats;
mod naming;
mod rfe;
mod schemas;
mod simulators;
mod xsd;

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("codegen") => {
            let root = repo_root();
            match codegen::run(&root) {
                Ok(report) => println!("{report}"),
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        Some("fixtures") => {
            let root = repo_root();
            match fixtures::run(&root) {
                Ok(report) => println!("{report}"),
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        Some("devices") => {
            let root = repo_root();
            let Some(corpus) = args.next() else {
                eprintln!("usage: cargo xtask devices <path-to-enbility/devices checkout>");
                std::process::exit(2);
            };
            match devices::run(&root, std::path::Path::new(&corpus)) {
                Ok(report) => println!("{report}"),
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        Some("rfe-table") => {
            let root = repo_root();
            match rfe::run(&root) {
                Ok(report) => println!("{report}"),
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        Some("simulators") => {
            let root = repo_root();
            match simulators::run(&root) {
                Ok(report) => println!("{report}"),
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        Some("check-schemas") => {
            let root = repo_root();
            match schemas::check(&root) {
                Ok(report) => println!("{report}"),
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        Some("check-floats") => {
            let root = repo_root();
            match floats::run(&root) {
                Ok(report) => println!("{report}"),
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        other => {
            eprintln!(
                "usage: cargo xtask <codegen|fixtures|devices|rfe-table|check-floats|check-schemas>"
            );
            if let Some(o) = other {
                eprintln!("unknown task: {o}");
            }
            std::process::exit(2);
        }
    }
}

/// Where the unpacked EEBUS specifications live: `$EEBUS_REFERENCE` when set, else the
/// git-ignored `concepts/reference/` of this checkout. Either way the SPINE bundle is
/// expected under `spine/` and the SHIP bundle under `ship/`.
pub(crate) fn reference_dir(root: &Path) -> PathBuf {
    std::env::var_os("EEBUS_REFERENCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("concepts/reference"))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the workspace root")
        .to_path_buf()
}
