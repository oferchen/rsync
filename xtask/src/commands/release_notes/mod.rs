//! Release notes management commands.
//!
//! Subcommands for rendering release note templates, validating CVE
//! statuses against `SECURITY.md`, and publishing GitHub releases.

mod publish;
mod render;
mod validate;

use crate::cli::ReleaseNotesArgs;
use crate::error::TaskResult;
use std::path::Path;

pub use publish::PublishOptions;
pub use render::RenderOptions;
pub use validate::ValidateOptions;

/// Subcommand dispatch for release-notes operations.
#[derive(Debug, Clone)]
pub enum ReleaseNotesCommand {
    /// Render the release template with version placeholders.
    Render(RenderOptions),
    /// Validate CVE statuses between release body and `SECURITY.md`.
    Validate(ValidateOptions),
    /// Create or update a GitHub release.
    Publish(PublishOptions),
}

/// Options for the top-level `release-notes` command.
#[derive(Debug, Clone)]
pub struct ReleaseNotesOptions {
    /// The subcommand to execute.
    pub command: ReleaseNotesCommand,
}

impl From<ReleaseNotesArgs> for ReleaseNotesOptions {
    fn from(args: ReleaseNotesArgs) -> Self {
        use crate::cli::ReleaseNotesSubcommand;

        let command = match args.command {
            ReleaseNotesSubcommand::Render(render_args) => {
                ReleaseNotesCommand::Render(RenderOptions {
                    version: render_args.version,
                    output: render_args.output,
                })
            }
            ReleaseNotesSubcommand::Validate(validate_args) => {
                ReleaseNotesCommand::Validate(ValidateOptions {
                    body: validate_args.body,
                })
            }
            ReleaseNotesSubcommand::Publish(publish_args) => {
                ReleaseNotesCommand::Publish(PublishOptions {
                    tag: publish_args.tag,
                    body_file: publish_args.body_file,
                    draft: publish_args.draft,
                })
            }
        };

        Self { command }
    }
}

/// Executes the release-notes command.
pub fn execute(workspace: &Path, options: ReleaseNotesOptions) -> TaskResult<()> {
    match options.command {
        ReleaseNotesCommand::Render(opts) => render::execute(workspace, opts),
        ReleaseNotesCommand::Validate(opts) => validate::execute(workspace, opts),
        ReleaseNotesCommand::Publish(opts) => publish::execute(workspace, opts),
    }
}
