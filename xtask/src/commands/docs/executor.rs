use super::cli::DocsOptions;
use super::validation;
use crate::error::TaskResult;
use crate::util::run_cargo_tool;
use std::ffi::OsString;
use std::path::Path;

/// Executes the `docs` command.
pub fn execute(workspace: &Path, options: DocsOptions) -> TaskResult<()> {
    println!("Building API documentation");
    let mut doc_args = vec![
        OsString::from("doc"),
        OsString::from("--workspace"),
        OsString::from("--no-deps"),
        OsString::from("--locked"),
    ];
    if options.open {
        doc_args.push(OsString::from("--open"));
    }

    run_cargo_tool(
        workspace,
        doc_args,
        "cargo doc",
        "ensure the Rust toolchain is installed",
    )?;

    println!("Running doctests");
    let test_args = vec![
        OsString::from("test"),
        OsString::from("--doc"),
        OsString::from("--workspace"),
        OsString::from("--locked"),
    ];

    run_cargo_tool(
        workspace,
        test_args,
        "cargo test --doc",
        "ensure the Rust toolchain is installed",
    )?;

    if options.validate {
        println!("Validating documentation branding");
        validation::validate_documents(workspace)?;
    }

    Ok(())
}
