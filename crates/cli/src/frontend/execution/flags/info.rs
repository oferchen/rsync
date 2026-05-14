use std::ffi::OsString;

use core::{
    message::{Message, Role},
    rsync_error,
};

use super::super::super::progress::{NameOutputLevel, ProgressSetting};

/// Per-flag descriptor for an `--info=` token.
///
/// upstream: options.c `output_struct` (rsync-3.4.1:259-266) plus the
/// `info_verbosity[]` grouping at options.c:239-243. `max_level` is the
/// per-flag ceiling used when `--info=all<N>` or `--info=N` fans out
/// across every flag (it caps each flag at its highest meaningful level,
/// mirroring upstream's runtime `INFO_GTE(...)` checks). `priority`
/// records the upstream verbosity group at which the flag is auto-enabled
/// from `-v` (NONREG=0, level-1 group covers COPY/DEL/FLIST/MISC/NAME/
/// STATS/SYMSAFE=1, level-2 group covers BACKUP/MOUNT/REMOVE/SKIP=2). It
/// is currently informational, exposed for the daemon-side limit handling
/// tracked by audit I16 (`limit_output_verbosity()`, options.c:527-552).
/// `strict_cap` controls whether a per-token level above `max_level` is
/// rejected (`--info=flist3`) or silently capped (`--info=backup5`),
/// preserving oc-rsync's historical usability split.
#[derive(Clone, Copy)]
pub(crate) struct InfoFlagSpec {
    pub(crate) name: &'static str,
    pub(crate) max_level: u8,
    pub(crate) priority: u8,
    pub(crate) strict_cap: bool,
}

// upstream: options.c info_verbosity[] (rsync-3.4.1:239-243) - priority is
// the verbosity-group index. NONREG sits in group 0 (always-on default),
// the level-1 group covers COPY/DEL/FLIST/MISC/NAME/STATS/SYMSAFE, and the
// level-2 group covers BACKUP/MOUNT/REMOVE/SKIP. PROGRESS has no upstream
// verbosity-group entry; oc-rsync treats it as priority 1 so `--info=1`
// enables per-file progress, matching upstream `-v` parity for `--info`.
#[rustfmt::skip]
pub(crate) const INFO_FLAG_SPECS: &[InfoFlagSpec] = &[
    InfoFlagSpec { name: "backup",   max_level: 1, priority: 2, strict_cap: false },
    InfoFlagSpec { name: "copy",     max_level: 1, priority: 1, strict_cap: false },
    InfoFlagSpec { name: "del",      max_level: 1, priority: 1, strict_cap: false },
    InfoFlagSpec { name: "flist",    max_level: 2, priority: 1, strict_cap: true  },
    InfoFlagSpec { name: "misc",     max_level: 2, priority: 1, strict_cap: true  },
    InfoFlagSpec { name: "mount",    max_level: 1, priority: 2, strict_cap: false },
    InfoFlagSpec { name: "name",     max_level: 2, priority: 1, strict_cap: false },
    InfoFlagSpec { name: "nonreg",   max_level: 1, priority: 0, strict_cap: false },
    InfoFlagSpec { name: "progress", max_level: 2, priority: 1, strict_cap: true  },
    InfoFlagSpec { name: "remove",   max_level: 1, priority: 2, strict_cap: false },
    InfoFlagSpec { name: "skip",     max_level: 2, priority: 2, strict_cap: true  },
    InfoFlagSpec { name: "stats",    max_level: 3, priority: 1, strict_cap: true  },
    InfoFlagSpec { name: "symsafe",  max_level: 1, priority: 1, strict_cap: false },
];

/// Parsed `--info` flag settings controlling informational output levels.
#[derive(Default)]
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
    fn enable_all(&mut self) {
        self.enable_all_at_level(1);
    }

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

    fn disable_all(&mut self) {
        for spec in INFO_FLAG_SPECS {
            self.assign(spec.name, 0);
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

    fn spec_for(name: &str) -> Option<&'static InfoFlagSpec> {
        INFO_FLAG_SPECS.iter().find(|spec| spec.name == name)
    }

    pub(super) fn apply(&mut self, token: &str, display: &str) -> Result<(), Message> {
        let lower = token.to_ascii_lowercase();

        if lower == "help" {
            self.help_requested = true;
            return Ok(());
        }

        if lower == "all" {
            self.enable_all();
            return Ok(());
        }

        if lower == "none" {
            self.disable_all();
            return Ok(());
        }

        // upstream: options.c parse_output_words accepts "all<N>" (e.g. "all2")
        // to set every flag to level N. As a usability extension, oc-rsync
        // also accepts a bare integer token like "--info=2" with the same
        // semantics. Per-flag caps are applied by `enable_all_at_level`.
        if !lower.is_empty() && lower.bytes().all(|b| b.is_ascii_digit()) {
            let level = lower.parse::<u8>().unwrap_or(u8::MAX);
            self.enable_all_at_level(level);
            return Ok(());
        }

        let (normalized, level) = self.parse_flag_and_level(&lower);

        // upstream: options.c output_msg / parse_output_words clamps levels to
        // MAX_OUT_LEVEL=4 but never rejects them. oc-rsync rejects only when the
        // spec's `strict_cap` is set (progress/stats/flist/misc/skip) and the
        // user-supplied level exceeds `max_level`; other flags accept any level
        // verbatim for forward-compatibility with hypothetical future emit sites.
        let spec = Self::spec_for(normalized).ok_or_else(|| info_flag_error(display))?;
        if spec.strict_cap && level > spec.max_level {
            return Err(info_flag_error(display));
        }
        self.assign(spec.name, level);
        Ok(())
    }

    pub(super) fn parse_flag_and_level<'a>(&self, input: &'a str) -> (&'a str, u8) {
        let base = input.trim_end_matches(|c: char| c.is_ascii_digit());
        if base != input {
            if Self::spec_for(base).is_some() {
                let suffix = &input[base.len()..];
                let level = suffix.parse::<u8>().unwrap_or(1);
                return (base, level);
            }
        } else if Self::spec_for(input).is_some() {
            return (input, 1);
        }

        let stripped = input.strip_prefix("no").or_else(|| input.strip_prefix('-'));

        if let Some(stripped) = stripped {
            return (stripped, 0);
        }

        (input, 1)
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
    let mut settings = InfoFlagSettings::default();
    for value in values {
        let text = value.to_string_lossy();
        let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());

        if trimmed.is_empty() {
            return Err(rsync_error!(1, "--info flag must not be empty").with_role(Role::Client));
        }

        for token in trimmed.split(',') {
            let token = token.trim_matches(|ch: char| ch.is_ascii_whitespace());
            if token.is_empty() {
                return Err(
                    rsync_error!(1, "--info flag must not be empty").with_role(Role::Client)
                );
            }

            settings.apply(token, token)?;
        }
    }

    Ok(settings)
}

pub(crate) const INFO_HELP_TEXT: &str = "The following --info flags are supported:\n\
    all         Enable all informational output currently implemented.\n\
    none        Disable all informational output handled by this build.\n\
    BACKUP      Mention files backed up.\n\
    COPY        Mention files copied locally on the receiving side.\n\
    DEL         Mention deletions on the receiving side.\n\
    FLIST       Mention file-list receiving/sending (levels 1-2).\n\
    MISC        Mention miscellaneous information (levels 1-2).\n\
    MOUNT       Mention mounts that were found or skipped.\n\
    NAME        Mention 1) updated file/dir names, 2) unchanged names.\n\
    NONREG      Mention skipped non-regular files (default 1, 0 disables).\n\
    PROGRESS    Mention 1) per-file progress or 2) total transfer progress.\n\
    REMOVE      Mention files removed on the sending side.\n\
    SKIP        Mention files skipped due to transfer overrides (levels 1-2).\n\
    STATS       Mention statistics at end of run (levels 1-3).\n\
    SYMSAFE     Mention symlinks that are unsafe.\n\
\n\
Flags may be written with 'no' or '-' prefixes (for example, --info=noprogress).\n\
Level suffixes may be used (for example, --info=stats2 or --info=flist0).\n";
