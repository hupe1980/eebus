//! Repository automation.
//!
//! `cargo xtask codegen` regenerates `src/model/generated/` from the SPINE 1.3.0 XML
//! Schemas. The schemas are not redistributable, so they live in the git-ignored
//! `specs/` directory and the *generated* Rust is what gets committed: building the
//! crate never requires access to the specification.

use std::path::{Path, PathBuf};

mod codegen;
mod fixtures;
mod naming;
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
        other => {
            eprintln!("usage: cargo xtask <codegen|fixtures>");
            if let Some(o) = other {
                eprintln!("unknown task: {o}");
            }
            std::process::exit(2);
        }
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the workspace root")
        .to_path_buf()
}
