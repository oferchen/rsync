use super::cli::DocsOptions;
use super::validation;
use crate::error::{TaskError, TaskResult};
use crate::util::run_cargo_tool;
use cargo_metadata::{MetadataCommand, Package, Target};
use std::collections::HashSet;
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
    let packages = library_packages(workspace)?;
    if packages.is_empty() {
        println!("Skipping doctests; no library targets were found in the workspace");
    } else {
        let mut test_args = vec![
            OsString::from("test"),
            OsString::from("--doc"),
            OsString::from("--locked"),
        ];

        for package in &packages {
            test_args.push(OsString::from("--package"));
            test_args.push(OsString::from(package));
        }

        run_cargo_tool(
            workspace,
            test_args,
            "cargo test --doc",
            "ensure the Rust toolchain is installed",
        )?;
    }

    if options.validate {
        println!("Validating documentation branding");
        validation::validate_documents(workspace)?;
    }

    Ok(())
}

fn library_packages(workspace: &Path) -> TaskResult<Vec<String>> {
    let mut command = MetadataCommand::new();
    command.current_dir(workspace);
    let metadata = command.exec().map_err(|error| {
        TaskError::Metadata(format!("failed to load workspace metadata: {error}"))
    })?;

    let members: HashSet<_> = metadata.workspace_members.into_iter().collect();
    let mut packages: Vec<_> = metadata
        .packages
        .into_iter()
        .filter(|package| members.contains(&package.id))
        .filter(has_library_target)
        .map(|package| package.name)
        .collect();

    packages.sort();
    packages.dedup();

    Ok(packages)
}

fn has_library_target(package: &Package) -> bool {
    package.targets.iter().any(target_has_library_kind)
}

fn target_has_library_kind(target: &Target) -> bool {
    target.kind.iter().any(|kind| is_library_kind(kind))
}

fn is_library_kind(kind: &str) -> bool {
    matches!(
        kind,
        "lib" | "proc-macro" | "rlib" | "dylib" | "cdylib" | "staticlib"
    )
}

#[cfg(test)]
mod tests {
    use super::is_library_kind;

    #[test]
    fn detects_library_kinds() {
        assert!(is_library_kind("lib"));
        assert!(is_library_kind("proc-macro"));
        assert!(is_library_kind("staticlib"));
        assert!(!is_library_kind("bin"));
        assert!(!is_library_kind("example"));
    }
}
