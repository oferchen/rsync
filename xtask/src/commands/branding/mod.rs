mod args;
mod render;
mod validate;

pub use args::{BrandingOptions, parse_args};

use crate::error::TaskResult;
use crate::workspace::{WorkspaceBranding, load_workspace_branding};
use std::path::Path;

use render::render_branding;
use validate::validate_branding;

/// Executes the `branding` command using the workspace manifest.
pub fn execute(workspace: &Path, options: BrandingOptions) -> TaskResult<()> {
    let branding = load_workspace_branding(workspace)?;
    validate_branding(&branding)?;
    emit_branding_report(&branding, options)
}

fn emit_branding_report(branding: &WorkspaceBranding, options: BrandingOptions) -> TaskResult<()> {
    let output = render_branding(branding, options.format)?;
    println!("{output}");
    Ok(())
}

#[cfg(test)]
mod tests;
