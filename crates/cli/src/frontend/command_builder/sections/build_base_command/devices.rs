//! Device and special file arguments: archive-devices (-D), devices,
//! copy-devices, write-devices, and specials.

use super::{Arg, ArgAction, ClapCommand};

/// Adds device and special file preservation flags to the command.
pub(super) fn add_device_args(command: ClapCommand) -> ClapCommand {
    command
        .arg(
            Arg::new("archive-devices")
                .short('D')
                .help("Preserve device and special files (equivalent to --devices --specials).")
                .action(ArgAction::SetTrue)
                .overrides_with("no-archive-devices"),
        )
        .arg(
            Arg::new("no-archive-devices")
                .long("no-D")
                .help("Disable preservation of device and special files (negates -D).")
                .action(ArgAction::SetTrue)
                .overrides_with("archive-devices"),
        )
        .arg(
            Arg::new("devices")
                .long("devices")
                .help("Preserve device files.")
                .action(ArgAction::SetTrue)
                .conflicts_with("no-devices"),
        )
        .arg(
            Arg::new("no-devices")
                .long("no-devices")
                .help("Disable device file preservation.")
                .action(ArgAction::SetTrue)
                .conflicts_with("devices"),
        )
        .arg(
            Arg::new("copy-devices")
                .long("copy-devices")
                .help("Copy device files as regular files, transferring their contents.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("write-devices")
                .long("write-devices")
                .help("Write file data directly to device files instead of creating nodes.")
                .action(ArgAction::SetTrue)
                .conflicts_with("no-write-devices"),
        )
        .arg(
            Arg::new("no-write-devices")
                .long("no-write-devices")
                .help("Do not write file data directly to device files.")
                .action(ArgAction::SetTrue)
                .conflicts_with("write-devices"),
        )
        .arg(
            Arg::new("specials")
                .long("specials")
                .help("Preserve special files such as FIFOs.")
                .action(ArgAction::SetTrue)
                .conflicts_with("no-specials"),
        )
        .arg(
            Arg::new("no-specials")
                .long("no-specials")
                .help("Disable preservation of special files such as FIFOs.")
                .action(ArgAction::SetTrue)
                .conflicts_with("specials"),
        )
        // Paired with `overrides_with` rather than `conflicts_with` because
        // upstream declares both as POPT_ARG_VAL assignments to one variable
        // (options.c:688-689), so a repeated pair is last-wins, not an error -
        // the same shape `-D` / `--no-D` already uses above.
        .arg(
            Arg::new("drop-devices")
                .long("drop-D")
                .help("Receiver refuses to create device and special files.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-drop-devices"),
        )
        .arg(
            Arg::new("no-drop-devices")
                .long("no-drop-D")
                .help("Let the receiver create device and special files (negates --drop-D).")
                .action(ArgAction::SetTrue)
                .overrides_with("drop-devices"),
        )
}
