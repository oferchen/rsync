use std::ffi::OsString;

use core::{
    message::{Message, Role},
    rsync_error,
};

use super::super::super::progress::{NameOutputLevel, ProgressSetting};
use super::output_words::{self, OutputWord};

/// Per-flag descriptor for an `--info=` token.
///
/// upstream: options.c `output_struct` (rsync-3.4.1:259-266) plus the
/// `info_verbosity[]` grouping at options.c:239-243. `max_level` is the
/// per-flag ceiling used when `--info=all<N>` fans out
/// across every flag (it caps each flag at its highest meaningful level,
/// mirroring upstream's runtime `INFO_GTE(...)` checks). `priority`
/// records the upstream verbosity group at which the flag is auto-enabled
/// from `-v` (NONREG=0, level-1 group covers COPY/DEL/FLIST/MISC/NAME/
/// STATS/SYMSAFE=1, level-2 group covers BACKUP/MOUNT/REMOVE/SKIP=2). It
/// is currently informational, exposed for the daemon-side limit handling
/// tracked by audit I16 (`limit_output_verbosity()`, options.c:527-552).
#[derive(Clone, Copy)]
pub(crate) struct InfoFlagSpec {
    pub(crate) name: &'static str,
    pub(crate) max_level: u8,
    // Exposed for future daemon-side `limit_output_verbosity()` parity. The
    // current client-side `enable_all_at_level()` clamps by max_level rather
    // than priority, mirroring upstream's `parse_output_words` behavior.
    #[allow(dead_code)]
    pub(crate) priority: u8,
}

/// Every `--info` flag, with the verbosity group that enables it.
///
/// upstream: options.c info_verbosity[] (rsync-3.4.1:239-243) - priority is
/// the verbosity-group index. NONREG sits in group 0 (always-on default),
/// the level-1 group covers COPY/DEL/FLIST/MISC/NAME/STATS/SYMSAFE, and the
/// level-2 group covers BACKUP/MOUNT/REMOVE/SKIP. PROGRESS has no upstream
/// verbosity-group entry; oc-rsync treats it as priority 1 so `--info=all1`
/// enables per-file progress, matching upstream `-v` parity for `--info`.
#[rustfmt::skip]
pub(crate) const INFO_FLAG_SPECS: &[InfoFlagSpec] = &[
    InfoFlagSpec { name: "backup",   max_level: 1, priority: 2 },
    InfoFlagSpec { name: "copy",     max_level: 1, priority: 1 },
    InfoFlagSpec { name: "del",      max_level: 1, priority: 1 },
    InfoFlagSpec { name: "flist",    max_level: 2, priority: 1 },
    InfoFlagSpec { name: "misc",     max_level: 2, priority: 1 },
    InfoFlagSpec { name: "mount",    max_level: 1, priority: 2 },
    InfoFlagSpec { name: "name",     max_level: 2, priority: 1 },
    InfoFlagSpec { name: "nonreg",   max_level: 1, priority: 0 },
    InfoFlagSpec { name: "progress", max_level: 2, priority: 1 },
    InfoFlagSpec { name: "remove",   max_level: 1, priority: 2 },
    InfoFlagSpec { name: "skip",     max_level: 2, priority: 2 },
    InfoFlagSpec { name: "stats",    max_level: 3, priority: 1 },
    InfoFlagSpec { name: "symsafe",  max_level: 1, priority: 1 },
];

/// Parsed `--info` flag settings controlling informational output levels.
#[derive(Debug, Default)]
pub(crate) struct InfoFlagSettings {
    pub(crate) progress: ProgressSetting,
    pub(crate) stats: Option<u8>,
    pub(crate) name: Option<NameOutputLevel>,
    pub(crate) backup: Option<u8>,
    pub(crate) copy: Option<u8>,
    pub(crate) del: Option<u8>,
    pub(crate) flist: Option<u8>,
    pub(crate) misc: Option<u8>,
    pub(crate) mount: Option<u8>,
    pub(crate) nonreg: Option<u8>,
    pub(crate) remove: Option<u8>,
    pub(crate) skip: Option<u8>,
    pub(crate) symsafe: Option<u8>,
    pub(crate) help_requested: bool,
}

impl InfoFlagSettings {
    // upstream: options.c parse_output_words - the "all<N>" token sets every
    // flag to level `min(N, spec.max_level)`. The `priority` field on each
    // spec records the upstream verbosity-group ordering (NONREG=0, level-1
    // group=1, level-2 group=2) and is exposed via `InfoFlagSpec::priority`
    // for future daemon-side `limit_output_verbosity()` parity (audit I16).
    fn enable_all_at_level(&mut self, level: u8) {
        for spec in INFO_FLAG_SPECS {
            let effective = level.min(spec.max_level);
            self.assign(spec.name, effective);
        }
    }

    fn assign(&mut self, name: &str, level: u8) {
        match name {
            "progress" => {
                self.progress = match level {
                    0 => ProgressSetting::Disabled,
                    1 => ProgressSetting::PerFile,
                    _ => ProgressSetting::Overall,
                }
            }
            "stats" => self.stats = Some(level),
            "name" => {
                self.name = Some(match level {
                    0 => NameOutputLevel::Disabled,
                    1 => NameOutputLevel::UpdatedOnly,
                    _ => NameOutputLevel::UpdatedAndUnchanged,
                })
            }
            "backup" => self.backup = Some(level),
            "copy" => self.copy = Some(level),
            "del" => self.del = Some(level),
            "flist" => self.flist = Some(level),
            "misc" => self.misc = Some(level),
            "mount" => self.mount = Some(level),
            "nonreg" => self.nonreg = Some(level),
            "remove" => self.remove = Some(level),
            "skip" => self.skip = Some(level),
            "symsafe" => self.symsafe = Some(level),
            _ => {}
        }
    }

    /// Apply resolved settings to the thread-local verbosity config.
    ///
    /// For each subcategory that was explicitly set (non-None), this calls
    /// `logging::apply_info_flag()` with the resolved level. This correctly
    /// handles composite tokens like "all", "none", and bare numeric levels
    /// that `apply_info_flag()` alone cannot parse, because
    /// `InfoFlagSettings` has already resolved them into per-flag levels.
    ///
    /// upstream: options.c set_output_verbosity - cumulative flag application
    pub(crate) fn apply_to_thread_local(&self) {
        let apply = |name: &str, level: Option<u8>| {
            if let Some(lvl) = level {
                let _ = logging::apply_info_flag(&format!("{name}{lvl}"));
            }
        };

        // progress level comes from the ProgressSetting enum
        match self.progress {
            ProgressSetting::Unspecified => {}
            ProgressSetting::Disabled => {
                let _ = logging::apply_info_flag("progress0");
            }
            ProgressSetting::PerFile => {
                let _ = logging::apply_info_flag("progress1");
            }
            ProgressSetting::Overall => {
                let _ = logging::apply_info_flag("progress2");
            }
        }

        // name level comes from the NameOutputLevel enum
        if let Some(ref level) = self.name {
            let lvl = match level {
                NameOutputLevel::Disabled => 0,
                NameOutputLevel::UpdatedOnly => 1,
                NameOutputLevel::UpdatedAndUnchanged => 2,
            };
            let _ = logging::apply_info_flag(&format!("name{lvl}"));
        }

        apply("stats", self.stats);
        apply("backup", self.backup);
        apply("copy", self.copy);
        apply("del", self.del);
        apply("flist", self.flist);
        apply("misc", self.misc);
        apply("mount", self.mount);
        apply("nonreg", self.nonreg);
        apply("remove", self.remove);
        apply("skip", self.skip);
        apply("symsafe", self.symsafe);
    }

    fn spec_for(name: &str) -> Option<&'static InfoFlagSpec> {
        INFO_FLAG_SPECS.iter().find(|spec| spec.name == name)
    }

    /// Returns the explicitly-set `(name, level)` info categories in upstream
    /// `info_words[]` order, for forwarding to a remote peer.
    ///
    /// `stats` is intentionally omitted: oc conflates `--stats` and
    /// `--info=stats` into a single stats level and forwards it via the
    /// standalone `--stats` flag (upstream `if (do_stats) --stats`,
    /// options.c:2856), so re-emitting `--info=stats` here would double-send.
    /// The remote builders apply upstream's role `where` filter to this list.
    pub(crate) fn iter_enabled_flags(&self) -> Vec<(&'static str, u8)> {
        let mut out: Vec<(&'static str, u8)> = Vec::new();
        let mut push = |name: &'static str, level: Option<u8>| {
            if let Some(level) = level {
                out.push((name, level));
            }
        };
        // upstream: options.c:280-294 info_words[] order.
        push("backup", self.backup);
        push("copy", self.copy);
        push("del", self.del);
        push("flist", self.flist);
        push("misc", self.misc);
        push("mount", self.mount);
        let name_level = self.name.as_ref().map(|level| match level {
            NameOutputLevel::Disabled => 0,
            NameOutputLevel::UpdatedOnly => 1,
            NameOutputLevel::UpdatedAndUnchanged => 2,
        });
        push("name", name_level);
        push("nonreg", self.nonreg);
        let progress_level = match self.progress {
            ProgressSetting::Unspecified => None,
            ProgressSetting::Disabled => Some(0),
            ProgressSetting::PerFile => Some(1),
            ProgressSetting::Overall => Some(2),
        };
        push("progress", progress_level);
        push("remove", self.remove);
        push("skip", self.skip);
        push("symsafe", self.symsafe);
        out
    }

    #[cfg(test)]
    pub(super) fn apply(&mut self, token: &str, display: &str) -> Result<(), Message> {
        self.apply_with_mode(token, display, false)
    }

    /// Applies a single `--info` token.
    ///
    /// When `am_server` is `true`, an unrecognised token is silently
    /// accepted instead of producing an error, mirroring upstream rsync's
    /// `parse_output_words()` (`options.c:465`) where the
    /// `if (len && !words[j].name && !am_server)` guard skips the
    /// `RERR_SYNTAX` exit. This preserves cross-version compatibility when a
    /// newer client forwards info tokens this server build does not know.
    ///
    /// upstream: options.c parse_output_words
    pub(super) fn apply_with_mode(
        &mut self,
        token: &str,
        display: &str,
        am_server: bool,
    ) -> Result<(), Message> {
        let lower = token.to_ascii_lowercase();

        match output_words::classify(&lower) {
            OutputWord::Help => self.help_requested = true,
            OutputWord::Every(level) => self.enable_all_at_level(level),
            OutputWord::Named { name, level } => {
                // The `am_server` branch mirrors upstream's
                // `if (len && !words[j].name && !am_server)` guard: a newer
                // client may forward info words this build does not know, and
                // the server must accept them so the connection survives the
                // version skew. The client still rejects, so a typo surfaces
                // at the source.
                let Some(spec) = Self::spec_for(name) else {
                    return if am_server {
                        Ok(())
                    } else {
                        Err(info_flag_error(display))
                    };
                };
                self.assign(spec.name, level);
            }
        }

        Ok(())
    }
}

fn info_flag_error(display: &str) -> Message {
    rsync_error!(
        1,
        format!("invalid --info flag '{display}': use --info=help for supported flags")
    )
    .with_role(Role::Client)
}

/// Parses `--info` flag values into resolved settings.
pub(crate) fn parse_info_flags(values: &[OsString]) -> Result<InfoFlagSettings, Message> {
    parse_info_flags_inner(values, false)
}

/// Parses `--info` flag values in server mode, silently ignoring unknown tokens.
///
/// Upstream rsync's `parse_output_words()` checks `!am_server` before raising
/// `Unknown --info item` (`options.c:465`). The server side accepts whatever
/// the (possibly newer) client forwards so the connection survives across
/// version skew. The client-side parser still rejects unknown tokens via
/// [`parse_info_flags`] so typos surface at the source.
///
/// upstream: options.c parse_output_words
pub(crate) fn parse_info_flags_server(values: &[OsString]) -> Result<InfoFlagSettings, Message> {
    parse_info_flags_inner(values, true)
}

fn parse_info_flags_inner(
    values: &[OsString],
    am_server: bool,
) -> Result<InfoFlagSettings, Message> {
    let mut settings = InfoFlagSettings::default();
    for value in values {
        let text = value.to_string_lossy();
        output_words::for_each_token(&text, |token| {
            settings.apply_with_mode(token, token, am_server)
        })?;
    }

    Ok(settings)
}

/// Body of `--info=help`, reproduced byte-for-byte from upstream.
///
/// upstream: options.c output_item_help (rsync-3.4.1:474-510). The text is
/// reproduced byte-for-byte from upstream's runtime output so `--info=help`
/// matches `rsync --info=help`. The format string is `"%-10s %s\n"`: each
/// name is left-padded to 10 columns, followed by a single space and the
/// help text. ALL/NONE descriptions inline the sentinel's `--info` help
/// (options.c:489-495). The per-verbosity summary block is rendered by
/// upstream's `make_output_option` over `info_verbosity[]` (options.c:239-
/// 243) and emits one line per non-empty level in `info_words[]` order.
pub(crate) const INFO_HELP_TEXT: &str = "\
Use OPT or OPT1 for level 1 output, OPT2 for level 2, etc.; OPT0 silences.\n\
\n\
BACKUP     Mention files backed up\n\
COPY       Mention files copied locally on the receiving side\n\
DEL        Mention deletions on the receiving side\n\
FLIST      Mention file-list receiving/sending (levels 1-2)\n\
MISC       Mention miscellaneous information (levels 1-2)\n\
MOUNT      Mention mounts that were found or skipped\n\
NAME       Mention 1) updated file/dir names, 2) unchanged names\n\
NONREG     Mention skipped non-regular files (default 1, 0 disables)\n\
PROGRESS   Mention 1) per-file progress or 2) total transfer progress\n\
REMOVE     Mention files removed on the sending side\n\
SKIP       Mention files skipped due to transfer overrides (levels 1-2)\n\
STATS      Mention statistics at end of run (levels 1-3)\n\
SYMSAFE    Mention symlinks that are unsafe\n\
\n\
ALL        Set all --info options (e.g. all4)\n\
NONE       Silence all --info options (same as all0)\n\
HELP       Output this help message\n\
\n\
Options added at each level of verbosity:\n\
0) NONREG\n\
1) COPY,DEL,FLIST,MISC,NAME,STATS,SYMSAFE\n\
2) BACKUP,MISC2,MOUNT,NAME2,REMOVE,SKIP\n";
