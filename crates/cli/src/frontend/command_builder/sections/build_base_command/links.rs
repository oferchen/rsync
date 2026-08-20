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
        // upstream: options.c:697-698 - a POPT_ARG_VAL pair that sets
        // `insecure_links` to 1/0; :134 defaults it to 0. It is a LOCAL-ONLY
        // opt-out: options.c:3068 deliberately does not forward it, and
        // options.c:1084 makes the daemon hard-refuse it, so a client can never
        // relax a server's path confinement with it.
        .arg(
            Arg::new("insecure-links")
                .long("insecure-links")
                .help("Follow operator-supplied path symlinks without the ownership check.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-insecure-links"),
        )
        .arg(
            Arg::new("no-insecure-links")
                .long("no-insecure-links")
                .help("Restore the ownership check on operator-supplied path symlinks.")
                .action(ArgAction::SetTrue)
                .overrides_with("insecure-links"),
        )
        // upstream: options.c:690 - `--confine-root=DIR` is POPT_ARG_STRING.
        // Declared here as well as in the server table so the client PARSES it
        // instead of passing it through as a positional operand.
        .arg(
            Arg::new("confine-root")
                .long("confine-root")
                .value_name("DIR")
                .help("Confine every operator- and peer-supplied path beneath DIR.")
                .action(ArgAction::Set),
        )
}
