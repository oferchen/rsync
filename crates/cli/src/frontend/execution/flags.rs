use std::ffi::OsString;

use core::{
    message::{Message, Role},
    rsync_error,
};

use super::super::progress::{NameOutputLevel, ProgressSetting};

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

    fn disable_all(&mut self) {
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

    fn apply(&mut self, token: &str, display: &str) -> Result<(), Message> {
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

    fn parse_flag_and_level<'a>(&self, input: &'a str) -> (&'a str, u8) {
        let stripped = input
            .strip_prefix("no")
            .or_else(|| input.strip_prefix('-'))
            .unwrap_or(input);

        if stripped != input {
            return (stripped, 0);
        }

        let base = input.trim_end_matches(|c: char| c.is_ascii_digit());
        if base == input {
            return (input, 1);
        }

        let suffix = &input[base.len()..];
        let level = suffix.parse::<u8>().unwrap_or(1);
        (base, level)
    }
}

fn info_flag_error(display: &str) -> Message {
    rsync_error!(
        1,
        format!("invalid --info flag '{display}': use --info=help for supported flags")
    )
    .with_role(Role::Client)
}

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

#[derive(Default)]
pub(crate) struct DebugFlagSettings {
    pub(crate) acl: Option<u8>,
    pub(crate) backup: Option<u8>,
    pub(crate) bind: Option<u8>,
    pub(crate) chdir: Option<u8>,
    pub(crate) connect: Option<u8>,
    pub(crate) cmd: Option<u8>,
    pub(crate) del: Option<u8>,
    pub(crate) deltasum: Option<u8>,
    pub(crate) dup: Option<u8>,
    pub(crate) exit: Option<u8>,
    pub(crate) filter: Option<u8>,
    pub(crate) flist: Option<u8>,
    pub(crate) fuzzy: Option<u8>,
    pub(crate) genr: Option<u8>,
    pub(crate) hash: Option<u8>,
    pub(crate) hlink: Option<u8>,
    pub(crate) iconv: Option<u8>,
    pub(crate) io: Option<u8>,
    pub(crate) nstr: Option<u8>,
    pub(crate) own: Option<u8>,
    pub(crate) proto: Option<u8>,
    pub(crate) recv: Option<u8>,
    pub(crate) send: Option<u8>,
    pub(crate) time: Option<u8>,
    pub(crate) help_requested: bool,
}

impl DebugFlagSettings {
    fn enable_all(&mut self) {
        self.acl = Some(1);
        self.backup = Some(1);
        self.bind = Some(1);
        self.chdir = Some(1);
        self.connect = Some(1);
        self.cmd = Some(1);
        self.del = Some(1);
        self.deltasum = Some(1);
        self.dup = Some(1);
        self.exit = Some(1);
        self.filter = Some(1);
        self.flist = Some(1);
        self.fuzzy = Some(1);
        self.genr = Some(1);
        self.hash = Some(1);
        self.hlink = Some(1);
        self.iconv = Some(1);
        self.io = Some(1);
        self.nstr = Some(1);
        self.own = Some(1);
        self.proto = Some(1);
        self.recv = Some(1);
        self.send = Some(1);
        self.time = Some(1);
    }

    fn disable_all(&mut self) {
        self.acl = Some(0);
        self.backup = Some(0);
        self.bind = Some(0);
        self.chdir = Some(0);
        self.connect = Some(0);
        self.cmd = Some(0);
        self.del = Some(0);
        self.deltasum = Some(0);
        self.dup = Some(0);
        self.exit = Some(0);
        self.filter = Some(0);
        self.flist = Some(0);
        self.fuzzy = Some(0);
        self.genr = Some(0);
        self.hash = Some(0);
        self.hlink = Some(0);
        self.iconv = Some(0);
        self.io = Some(0);
        self.nstr = Some(0);
        self.own = Some(0);
        self.proto = Some(0);
        self.recv = Some(0);
        self.send = Some(0);
        self.time = Some(0);
    }

    fn apply(&mut self, token: &str, display: &str) -> Result<(), Message> {
        let lower = token.to_ascii_lowercase();

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
            "acl" => {
                self.acl = Some(level);
                Ok(())
            }
            "backup" => {
                if level > 2 {
                    return Err(debug_flag_error(display));
                }
                self.backup = Some(level);
                Ok(())
            }
            "bind" => {
                self.bind = Some(level);
                Ok(())
            }
            "chdir" => {
                self.chdir = Some(level);
                Ok(())
            }
            "connect" => {
                if level > 2 {
                    return Err(debug_flag_error(display));
                }
                self.connect = Some(level);
                Ok(())
            }
            "cmd" => {
                if level > 2 {
                    return Err(debug_flag_error(display));
                }
                self.cmd = Some(level);
                Ok(())
            }
            "del" => {
                if level > 3 {
                    return Err(debug_flag_error(display));
                }
                self.del = Some(level);
                Ok(())
            }
            "deltasum" => {
                if level > 4 {
                    return Err(debug_flag_error(display));
                }
                self.deltasum = Some(level);
                Ok(())
            }
            "dup" => {
                self.dup = Some(level);
                Ok(())
            }
            "exit" => {
                if level > 3 {
                    return Err(debug_flag_error(display));
                }
                self.exit = Some(level);
                Ok(())
            }
            "filter" => {
                if level > 3 {
                    return Err(debug_flag_error(display));
                }
                self.filter = Some(level);
                Ok(())
            }
            "flist" => {
                if level > 4 {
                    return Err(debug_flag_error(display));
                }
                self.flist = Some(level);
                Ok(())
            }
            "fuzzy" => {
                if level > 2 {
                    return Err(debug_flag_error(display));
                }
                self.fuzzy = Some(level);
                Ok(())
            }
            "genr" => {
                self.genr = Some(level);
                Ok(())
            }
            "hash" => {
                self.hash = Some(level);
                Ok(())
            }
            "hlink" => {
                if level > 3 {
                    return Err(debug_flag_error(display));
                }
                self.hlink = Some(level);
                Ok(())
            }
            "iconv" => {
                if level > 2 {
                    return Err(debug_flag_error(display));
                }
                self.iconv = Some(level);
                Ok(())
            }
            "io" => {
                if level > 4 {
                    return Err(debug_flag_error(display));
                }
                self.io = Some(level);
                Ok(())
            }
            "nstr" => {
                self.nstr = Some(level);
                Ok(())
            }
            "own" => {
                if level > 2 {
                    return Err(debug_flag_error(display));
                }
                self.own = Some(level);
                Ok(())
            }
            "proto" => {
                self.proto = Some(level);
                Ok(())
            }
            "recv" => {
                self.recv = Some(level);
                Ok(())
            }
            "send" => {
                self.send = Some(level);
                Ok(())
            }
            "time" => {
                if level > 2 {
                    return Err(debug_flag_error(display));
                }
                self.time = Some(level);
                Ok(())
            }
            _ => Err(debug_flag_error(display)),
        }
    }

    fn parse_flag_and_level<'a>(&self, input: &'a str) -> (&'a str, u8) {
        let stripped = input
            .strip_prefix("no")
            .or_else(|| input.strip_prefix('-'))
            .unwrap_or(input);

        if stripped != input {
            return (stripped, 0);
        }

        let base = input.trim_end_matches(|c: char| c.is_ascii_digit());
        if base == input {
            return (input, 1);
        }

        let suffix = &input[base.len()..];
        let level = suffix.parse::<u8>().unwrap_or(1);
        (base, level)
    }
}

pub(crate) fn parse_debug_flags(values: &[OsString]) -> Result<DebugFlagSettings, Message> {
    let mut settings = DebugFlagSettings::default();

    for value in values {
        let text = value.to_string_lossy();
        let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());

        if trimmed.is_empty() {
            return Err(debug_flag_empty_error());
        }

        for token in trimmed.split(',') {
            let token = token.trim_matches(|ch: char| ch.is_ascii_whitespace());
            if token.is_empty() {
                return Err(debug_flag_empty_error());
            }

            if token.eq_ignore_ascii_case("help") {
                settings.help_requested = true;
            } else {
                settings.apply(token, token)?;
            }
        }
    }

    Ok(settings)
}

fn debug_flag_empty_error() -> Message {
    rsync_error!(1, "--debug flag must not be empty").with_role(Role::Client)
}

fn debug_flag_error(display: &str) -> Message {
    rsync_error!(
        1,
        format!("invalid --debug flag '{display}': use --debug=help for supported flags")
    )
    .with_role(Role::Client)
}

/// Deprecated: Kept for reference, will be removed once native SSH is fully validated
#[allow(dead_code)]
pub(crate) fn info_flags_include_progress(flags: &[OsString]) -> bool {
    flags.iter().any(|value| {
        value
            .to_string_lossy()
            .split(',')
            .map(|token| token.trim())
            .filter(|token| !token.is_empty())
            .any(|token| {
                let normalized = token.to_ascii_lowercase();
                let without_dash = normalized.strip_prefix('-').unwrap_or(&normalized);
                let stripped = without_dash
                    .strip_prefix("no-")
                    .or_else(|| without_dash.strip_prefix("no"))
                    .unwrap_or(without_dash);
                stripped.starts_with("progress")
            })
    })
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

pub(crate) const DEBUG_HELP_TEXT: &str = "The following --debug flags are supported:\n\
    all         Enable all diagnostic categories currently implemented.\n\
    none        Disable diagnostic output.\n\
    ACL         Debug extra ACL info.\n\
    BACKUP      Debug backup actions (levels 1-2).\n\
    BIND        Debug socket bind actions.\n\
    CHDIR       Debug when the current directory changes.\n\
    CONNECT     Debug connection events (levels 1-2).\n\
    CMD         Debug commands+options that are issued (levels 1-2).\n\
    DEL         Debug delete actions (levels 1-3).\n\
    DELTASUM    Debug delta-transfer checksumming (levels 1-4).\n\
    DUP         Debug weeding of duplicate names.\n\
    EXIT        Debug exit events (levels 1-3).\n\
    FILTER      Debug filter actions (levels 1-3).\n\
    FLIST       Debug file-list operations (levels 1-4).\n\
    FUZZY       Debug fuzzy scoring (levels 1-2).\n\
    GENR        Debug generator functions.\n\
    HASH        Debug hashtable code.\n\
    HLINK       Debug hard-link actions (levels 1-3).\n\
    ICONV       Debug iconv character conversions (levels 1-2).\n\
    IO          Debug I/O routines (levels 1-4).\n\
    NSTR        Debug negotiation strings.\n\
    OWN         Debug ownership changes in users & groups (levels 1-2).\n\
    PROTO       Debug protocol information.\n\
    RECV        Debug receiver functions.\n\
    SEND        Debug sender functions.\n\
    TIME        Debug setting of modified times (levels 1-2).\n\
\n\
Flags may be prefixed with 'no' or '-' to disable a category. Multiple flags\n\
may be combined by separating them with commas. Level suffixes may be used\n\
(for example, --debug=io2 or --debug=flist0).\n";
