//! Privilege escalation arguments: super, fake-super, and trust-sender.

use super::{Arg, ArgAction, ClapCommand};

/// Adds privilege escalation and trust flags to the command.
pub(super) fn add_privilege_args(command: ClapCommand) -> ClapCommand {
    command
        .arg(
            Arg::new("super")
                .long("super")
                .help(
                    "Receiver attempts super-user activities (implies --owner, --group, and --perms).",
                )
                .action(ArgAction::SetTrue)
                .overrides_with("no-super"),
        )
        .arg(
            Arg::new("no-super")
                .long("no-super")
                .help("Disable super-user handling even when running as root.")
                .action(ArgAction::SetTrue)
                .overrides_with("super"),
        )
        .arg(
            Arg::new("fake-super")
                .long("fake-super")
                .help("Store/restore privileged attrs using xattrs instead of real permissions.")
                .action(ArgAction::SetTrue)
                // Self-override makes a repeated flag idempotent instead of an
                // error. A local transfer splices its -M values back into argv
                // (local_remote_option_argv), so passing --fake-super together
                // with -M--fake-super legitimately presents it twice; upstream parses
                // the same pair with POPT_ARG_VAL, which just re-sets the value.
                // upstream: options.c:672 {"fake-super", 0, POPT_ARG_VAL, ...}
                .overrides_with_all(["no-fake-super", "fake-super"]),
        )
        .arg(
            Arg::new("no-fake-super")
                .long("no-fake-super")
                .help("Disable fake-super mode.")
                .action(ArgAction::SetTrue)
                .overrides_with("fake-super"),
        )
        .arg(
            Arg::new("trust-sender")
                .long("trust-sender")
                .help("Trust the sender's file list without additional verification.")
                .action(ArgAction::SetTrue),
        )
}
