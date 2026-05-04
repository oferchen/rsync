use std::ffi::OsString;

use core::{
    message::{Message, Role},
    rsync_error,
};

use super::super::super::progress::{NameOutputLevel, ProgressSetting};

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
    const fn enable_all(&mut self) {
        self.progress = ProgressSetting::PerFile;
        self.stats = Some(1);
        self.name = Some(NameOutputLevel::UpdatedOnly);
        self.backup = Some(1);
        self.copy = Some(1);
        self.del = Some(1);
        self.flist = Some(1);
        self.misc = Some(1);
        self.mount = Some(1);
        self.nonreg = Some(1);
        self.remove = Some(1);
        self.skip = Some(1);
        self.symsafe = Some(1);
    }

    const fn disable_all(&mut self) {
        self.progress = ProgressSetting::Disabled;
        self.stats = Some(0);
        self.name = Some(NameOutputLevel::Disabled);
        self.backup = Some(0);
        self.copy = Some(0);
        self.del = Some(0);
        self.flist = Some(0);
        self.misc = Some(0);
        self.mount = Some(0);
        self.nonreg = Some(0);
        self.remove = Some(0);
        self.skip = Some(0);
        self.symsafe = Some(0);
    }

    pub(super) fn apply(&mut self, token: &str, display: &str) -> Result<(), Message> {
        let lower = token.to_ascii_lowercase();

        if lower == "help" {
            self.help_requested = true;
            return Ok(());
        }

        if lower == "all" || lower == "1" {
            self.enable_all();
            return Ok(());
        }

        if lower == "none" || lower == "0" {
            self.disable_all();
            return Ok(());
        }

        let (normalized, level) = self.parse_flag_and_level(&lower);

        match normalized {
            "progress" => {
                self.progress = match level {
                    0 => ProgressSetting::Disabled,
                    1 => ProgressSetting::PerFile,
                    2 => ProgressSetting::Overall,
                    _ => return Err(info_flag_error(display)),
                };
                Ok(())
            }
            "stats" => {
                if level > 3 {
                    return Err(info_flag_error(display));
                }
                self.stats = Some(level);
                Ok(())
            }
            "name" => {
                let name_level = if level == 0 {
                    NameOutputLevel::Disabled
                } else if level == 1 {
                    NameOutputLevel::UpdatedOnly
                } else if level >= 2 {
                    NameOutputLevel::UpdatedAndUnchanged
                } else {
                    return Err(info_flag_error(display));
                };
                self.name = Some(name_level);
                Ok(())
            }
            "backup" => {
                self.backup = Some(level);
                Ok(())
            }
            "copy" => {
                self.copy = Some(level);
                Ok(())
            }
            "del" => {
                self.del = Some(level);
                Ok(())
            }
            "flist" => {
                if level > 2 {
                    return Err(info_flag_error(display));
                }
                self.flist = Some(level);
                Ok(())
            }
            "misc" => {
                if level > 2 {
                    return Err(info_flag_error(display));
                }
                self.misc = Some(level);
                Ok(())
            }
            "mount" => {
                self.mount = Some(level);
                Ok(())
            }
            "nonreg" => {
                self.nonreg = Some(level);
                Ok(())
            }
            "remove" => {
                self.remove = Some(level);
                Ok(())
            }
            "skip" => {
                if level > 2 {
                    return Err(info_flag_error(display));
                }
                self.skip = Some(level);
                Ok(())
            }
            "symsafe" => {
                self.symsafe = Some(level);
                Ok(())
            }
            _ => Err(info_flag_error(display)),
        }
    }

    /// Known info flag names for disambiguating `no-` prefix vs flag names
    /// that start with "no" (e.g., "nonreg").
    const KNOWN_FLAGS: &'static [&'static str] = &[
        "backup", "copy", "del", "flist", "misc", "mount", "name", "nonreg", "progress", "remove",
        "skip", "stats", "symsafe",
    ];

    pub(super) fn parse_flag_and_level<'a>(&self, input: &'a str) -> (&'a str, u8) {
        let base = input.trim_end_matches(|c: char| c.is_ascii_digit());
        if base != input {
            if Self::KNOWN_FLAGS.contains(&base) {
                let suffix = &input[base.len()..];
                let level = suffix.parse::<u8>().unwrap_or(1);
                return (base, level);
            }
        } else if Self::KNOWN_FLAGS.contains(&input) {
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
