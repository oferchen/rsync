//! Symlink and hard link arguments: links, copy-links, copy-unsafe-links,
//! hard-links, copy-dirlinks, keep-dirlinks, safe-links, and munge-links.

use super::{Arg, ArgAction, ClapCommand};

/// Adds symlink and hard link preservation flags to the command.
pub(super) fn add_link_args(command: ClapCommand) -> ClapCommand {
    command
        .arg(
            Arg::new("links")
                .long("links")
                .short('l')
                .help("Copy symlinks as symlinks.")
                .action(ArgAction::SetTrue)
                .conflicts_with_all(["copy-links", "no-links"]),
        )
        .arg(
            Arg::new("no-links")
                .long("no-links")
                .visible_alias("no-l")
                .help("Do not copy symlinks as symlinks.")
                .action(ArgAction::SetTrue)
                .conflicts_with_all(["links", "copy-links"]),
        )
        .arg(
            Arg::new("copy-links")
                .long("copy-links")
                .short('L')
                .help("Transform symlinks into referent files/directories.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("copy-unsafe-links")
                .long("copy-unsafe-links")
                .help("Transform unsafe symlinks into referent files/directories.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("hard-links")
                .long("hard-links")
                .short('H')
                .help("Preserve hard links between files.")
                .action(ArgAction::SetTrue)
                .conflicts_with("no-hard-links"),
        )
        .arg(
            Arg::new("no-hard-links")
                .long("no-hard-links")
                .visible_alias("no-H")
                .help("Disable hard link preservation.")
                .action(ArgAction::SetTrue)
                .conflicts_with("hard-links"),
        )
        .arg(
            Arg::new("copy-dirlinks")
                .long("copy-dirlinks")
                .short('k')
                .help("Transform symlinked directories into referent directories.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("keep-dirlinks")
                .long("keep-dirlinks")
                .short('K')
                .help("Treat existing destination symlinks to directories as directories.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("safe-links")
                .long("safe-links")
                .help("Skip symlinks that point outside the transfer root.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("munge-links")
                .long("munge-links")
                .help("Munge symlinks to make them safe in daemon mode.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-munge-links"),
        )
        .arg(
            Arg::new("no-munge-links")
                .long("no-munge-links")
                .help("Disable symlink munging.")
                .action(ArgAction::SetTrue)
                .overrides_with("munge-links"),
        )
}
