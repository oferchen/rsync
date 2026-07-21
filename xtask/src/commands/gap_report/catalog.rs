#![deny(unsafe_code)]

//! Curated catalog of upstream receiver-applied options.
//!
//! Upstream `options.c` `server_options()` builds the remote argv. Options
//! wrapped in `if (am_sender)` are never sent over the wire on a pull, so the
//! local client (which is the receiver) must apply them itself by carrying them
//! onto its `ServerConfig`. This catalog is the single source of truth for that
//! set: each entry records the upstream option, the `options.c` reference used
//! to re-verify it, the `ClientConfig` getter that reads the value, and the
//! `ServerConfig` leaf field the value should land on.
//!
//! Entries with no leaf field (`server_field` is `None`) are propagated by a
//! different mechanism (wire argv, the compact flag string, or the local-copy
//! path), or have no receiver handling yet. They are reported as `NO-FIELD` and
//! never counted as gaps.
//!
//! Regenerate on an upstream version bump: diff the new `options.c`
//! `server_options()` against this table, add any newly `am_sender`-gated
//! option, and refresh the line references below.

/// A receiver config builder that constructs a `ServerConfig` for a pull.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builder {
    /// External `ssh`/`rsh` transport (`ssh_transfer/server_config.rs`).
    Ssh,
    /// Embedded SSH (`russh`, `ssh://`) transport (`embedded_ssh_transfer.rs`).
    EmbeddedSsh,
    /// `rsync://` daemon transport
    /// (`daemon_transfer/orchestration/server_config.rs`).
    Daemon,
}

impl Builder {
    /// Every receiver builder, in report order.
    pub const ALL: [Builder; 3] = [Builder::Ssh, Builder::EmbeddedSsh, Builder::Daemon];

    /// Stable machine identifier used in the baseline file.
    pub const fn id(self) -> &'static str {
        match self {
            Builder::Ssh => "ssh",
            Builder::EmbeddedSsh => "embedded-ssh",
            Builder::Daemon => "daemon",
        }
    }

    /// Workspace-relative path to the builder source file.
    pub const fn relative_path(self) -> &'static str {
        match self {
            Builder::Ssh => "crates/core/src/client/remote/ssh_transfer/server_config.rs",
            Builder::EmbeddedSsh => "crates/core/src/client/remote/embedded_ssh_transfer.rs",
            Builder::Daemon => {
                "crates/core/src/client/remote/daemon_transfer/orchestration/server_config.rs"
            }
        }
    }
}

/// The receiver builders on which an option's `ServerConfig` field is expected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applicability {
    /// Every SSH and daemon receiver builder must propagate the field.
    AllReceivers,
    /// Daemon-only receiver concern; SSH transports do not apply it.
    DaemonOnly,
}

impl Applicability {
    /// Returns `true` when `builder` must propagate the option's field.
    pub fn includes(self, builder: Builder) -> bool {
        match self {
            Applicability::AllReceivers => true,
            Applicability::DaemonOnly => builder == Builder::Daemon,
        }
    }
}

/// One curated upstream receiver-applied option.
#[derive(Debug, Clone, Copy)]
pub struct OptionSpec {
    /// Canonical upstream option token (single word, e.g. `--list-only`).
    pub upstream_name: &'static str,
    /// `options.c` reference used to re-verify the `am_sender` gating.
    pub options_c: &'static str,
    /// `ClientConfig` getter that reads the value (documentation only).
    pub getter: &'static str,
    /// `ServerConfig` leaf field the value lands on, or `None` when the option
    /// is propagated by another mechanism or not yet handled.
    pub server_field: Option<&'static str>,
    /// Builders on which the leaf field is expected (unused when no field).
    pub applicability: Applicability,
    /// Human note: mechanism, rationale, or the reason there is no field.
    pub note: &'static str,
}

use Applicability::{AllReceivers, DaemonOnly};

/// The curated catalog of upstream receiver-applied (`am_sender`-gated)
/// options relevant to a pull, mirrored from rsync 3.4.4 `options.c`.
pub const CATALOG: &[OptionSpec] = &[
    OptionSpec {
        upstream_name: "--link-dest",
        options_c: "options.c:2933",
        getter: "reference_directories",
        server_field: Some("reference_directories"),
        applicability: AllReceivers,
        note: "alt-dest family: --compare-dest/--copy-dest/--link-dest (basis_dir)",
    },
    OptionSpec {
        upstream_name: "--backup-dir",
        options_c: "options.c:2805",
        getter: "backup_directory",
        server_field: Some("backup_dir"),
        applicability: AllReceivers,
        note: "long-form value finalized in local popt parse",
    },
    OptionSpec {
        upstream_name: "--suffix",
        options_c: "options.c:2813",
        getter: "backup_suffix",
        server_field: Some("backup_suffix"),
        applicability: AllReceivers,
        note: "without it effective_backup_suffix() falls back to '~'",
    },
    OptionSpec {
        upstream_name: "--chmod",
        options_c: "options.c:1762",
        getter: "chmod",
        server_field: Some("chmod"),
        applicability: AllReceivers,
        note: "parsed into chmod_modes; never placed in server_options",
    },
    OptionSpec {
        upstream_name: "--ignore-existing",
        options_c: "options.c:2918",
        getter: "ignore_existing",
        server_field: Some("ignore_existing"),
        applicability: AllReceivers,
        note: "receiver skips files already present (generator.c:1395)",
    },
    OptionSpec {
        upstream_name: "--existing",
        options_c: "options.c:2922",
        getter: "existing_only",
        server_field: Some("existing_only"),
        applicability: AllReceivers,
        note: "ignore_non_existing; receiver never creates absent files",
    },
    OptionSpec {
        upstream_name: "--prune-empty-dirs",
        options_c: "options.c:2644",
        getter: "prune_empty_dirs",
        server_field: Some("prune_empty_dirs"),
        applicability: AllReceivers,
        note: "flist_sort_and_clean prunes on the receiver",
    },
    OptionSpec {
        upstream_name: "--delay-updates",
        options_c: "options.c:2891",
        getter: "delay_updates",
        server_field: Some("delay_updates"),
        applicability: AllReceivers,
        note: "receiver stages then renames updates at the end",
    },
    OptionSpec {
        upstream_name: "--size-only",
        options_c: "options.c:2854",
        getter: "size_only",
        server_field: Some("size_only"),
        applicability: AllReceivers,
        note: "quick-check variation applied by the receiver",
    },
    OptionSpec {
        upstream_name: "--numeric-ids",
        options_c: "options.c:2905",
        getter: "numeric_ids",
        server_field: Some("numeric_ids"),
        applicability: AllReceivers,
        note: "receiver skips id->name mapping",
    },
    OptionSpec {
        upstream_name: "--delete",
        options_c: "options.c:2845",
        getter: "delete_mode",
        server_field: Some("delete"),
        applicability: AllReceivers,
        note: "delete decision runs on the receiver/generator",
    },
    OptionSpec {
        upstream_name: "--partial",
        options_c: "options.c:2894",
        getter: "partial",
        server_field: Some("partial"),
        applicability: AllReceivers,
        note: "keep_partial; receiver retains partial transfers",
    },
    OptionSpec {
        upstream_name: "--devices",
        options_c: "options.c:2761",
        getter: "preserve_devices",
        server_field: Some("devices"),
        applicability: AllReceivers,
        note: "'D' now tracks devices only in the compact flag string",
    },
    OptionSpec {
        upstream_name: "--specials",
        options_c: "options.c:2766",
        getter: "preserve_specials",
        server_field: Some("specials"),
        applicability: AllReceivers,
        note: "specials carried separately from devices",
    },
    OptionSpec {
        upstream_name: "--list-only",
        options_c: "options.c:2747",
        getter: "list_only",
        server_field: Some("list_only"),
        applicability: AllReceivers,
        note: "long-form-only flag absent from the compact letter string",
    },
    OptionSpec {
        upstream_name: "--fsync",
        options_c: "options.c:2930",
        getter: "fsync",
        server_field: Some("fsync"),
        applicability: DaemonOnly,
        note: "receiver fsync wired only in the daemon write path",
    },
    OptionSpec {
        upstream_name: "--usermap",
        options_c: "options.c:2912",
        getter: "user_mapping",
        server_field: None,
        applicability: AllReceivers,
        note: "applied via invocation argv (invocation/builder.rs), no ServerConfig field",
    },
    OptionSpec {
        upstream_name: "--groupmap",
        options_c: "options.c:2915",
        getter: "group_mapping",
        server_field: None,
        applicability: AllReceivers,
        note: "applied via invocation argv (invocation/builder.rs), no ServerConfig field",
    },
    OptionSpec {
        upstream_name: "--temp-dir",
        options_c: "options.c:2926",
        getter: "temp_directory",
        server_field: None,
        applicability: AllReceivers,
        note: "no receiver ServerConfig field yet",
    },
    OptionSpec {
        upstream_name: "--mkpath",
        options_c: "options.c:2996",
        getter: "mkpath",
        server_field: None,
        applicability: AllReceivers,
        note: "no receiver ServerConfig field yet",
    },
    OptionSpec {
        upstream_name: "--omit-dir-times",
        options_c: "options.c:2646",
        getter: "omit_dir_times",
        server_field: None,
        applicability: AllReceivers,
        note: "no receiver ServerConfig field yet",
    },
    OptionSpec {
        upstream_name: "--omit-link-times",
        options_c: "options.c:2648",
        getter: "omit_link_times",
        server_field: None,
        applicability: AllReceivers,
        note: "no receiver ServerConfig field yet",
    },
    OptionSpec {
        upstream_name: "--write-devices",
        options_c: "options.c:2979",
        getter: "write_devices",
        server_field: None,
        applicability: AllReceivers,
        note: "no receiver ServerConfig field yet",
    },
    OptionSpec {
        upstream_name: "--keep-dirlinks",
        options_c: "options.c:2642",
        getter: "keep_dirlinks",
        server_field: None,
        applicability: AllReceivers,
        note: "no receiver ServerConfig field yet",
    },
    OptionSpec {
        upstream_name: "--fuzzy",
        options_c: "options.c:2650",
        getter: "fuzzy",
        server_field: None,
        applicability: AllReceivers,
        note: "no receiver ServerConfig field yet",
    },
    OptionSpec {
        upstream_name: "--force",
        options_c: "options.c:2848",
        getter: "force_replacements",
        server_field: None,
        applicability: AllReceivers,
        note: "force_delete; no receiver ServerConfig field yet",
    },
    OptionSpec {
        upstream_name: "--preserve-executability",
        options_c: "options.c:2692",
        getter: "preserve_executability",
        server_field: None,
        applicability: AllReceivers,
        note: "no receiver ServerConfig field yet",
    },
    OptionSpec {
        upstream_name: "--super",
        options_c: "options.c:2853",
        getter: "super_user",
        server_field: None,
        applicability: AllReceivers,
        note: "emitted as a wire arg; no receiver ServerConfig field",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_names_are_single_tokens_and_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for spec in CATALOG {
            assert!(
                !spec.upstream_name.contains(char::is_whitespace),
                "option token must be a single word: {}",
                spec.upstream_name
            );
            assert!(
                seen.insert(spec.upstream_name),
                "duplicate option token: {}",
                spec.upstream_name
            );
        }
    }

    #[test]
    fn options_c_references_are_populated() {
        for spec in CATALOG {
            assert!(
                spec.options_c.starts_with("options.c:"),
                "expected options.c reference for {}, got {}",
                spec.upstream_name,
                spec.options_c
            );
        }
    }

    #[test]
    fn daemon_only_applies_to_daemon_only() {
        assert!(DaemonOnly.includes(Builder::Daemon));
        assert!(!DaemonOnly.includes(Builder::Ssh));
        assert!(!DaemonOnly.includes(Builder::EmbeddedSsh));
    }

    #[test]
    fn all_receivers_applies_everywhere() {
        for builder in Builder::ALL {
            assert!(AllReceivers.includes(builder));
        }
    }

    #[test]
    fn builder_ids_are_stable_and_distinct() {
        let ids: Vec<&str> = Builder::ALL.iter().map(|b| b.id()).collect();
        assert_eq!(ids, ["ssh", "embedded-ssh", "daemon"]);
    }
}
