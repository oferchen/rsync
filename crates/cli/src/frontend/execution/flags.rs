use std::ffi::OsString;

use core::{
    message::{Message, Role},
    rsync_error,
};

use super::super::progress::{NameOutputLevel, ProgressSetting};

impl Default for ProgressSetting {
    fn default() -> Self {
        Self::Unspecified
    }
}

#[derive(Default)]
pub(crate) struct InfoFlagSettings {
    pub(crate) progress: ProgressSetting,
    pub(crate) stats: Option<bool>,
    pub(crate) name: Option<NameOutputLevel>,
    pub(crate) help_requested: bool,
}

impl InfoFlagSettings {
    fn enable_all(&mut self) {
        self.progress = ProgressSetting::PerFile;
        self.stats = Some(true);
        self.name = Some(NameOutputLevel::UpdatedAndUnchanged);
    }

    fn disable_all(&mut self) {
        self.progress = ProgressSetting::Disabled;
        self.stats = Some(false);
        self.name = Some(NameOutputLevel::Disabled);
    }

    fn apply(&mut self, token: &str, display: &str) -> Result<(), Message> {
        let lower = token.to_ascii_lowercase();
        match lower.as_str() {
            "help" => {
                self.help_requested = true;
                Ok(())
            }
            "all" | "1" => {
                self.enable_all();
                Ok(())
            }
            "none" | "0" => {
                self.disable_all();
                Ok(())
            }
            "progress" | "progress1" => {
                self.progress = ProgressSetting::PerFile;
                Ok(())
            }
            "progress2" => {
                self.progress = ProgressSetting::Overall;
                Ok(())
            }
            "progress0" | "noprogress" | "-progress" => {
                self.progress = ProgressSetting::Disabled;
                Ok(())
            }
            "stats" | "stats1" => {
                self.stats = Some(true);
                Ok(())
            }
            "stats0" | "nostats" | "-stats" => {
                self.stats = Some(false);
                Ok(())
            }
            _ if lower.starts_with("name") => {
                let level = &lower[4..];
                let parsed = if level.is_empty() || level == "1" {
                    Some(NameOutputLevel::UpdatedOnly)
                } else if level == "0" {
                    Some(NameOutputLevel::Disabled)
                } else if level.chars().all(|ch| ch.is_ascii_digit()) {
                    Some(NameOutputLevel::UpdatedAndUnchanged)
                } else {
                    None
                };

                match parsed {
                    Some(level) => {
                        self.name = Some(level);
                        Ok(())
                    }
                    None => Err(info_flag_error(display)),
                }
            }
            _ => Err(info_flag_error(display)),
        }
    }
}

fn info_flag_error(display: &str) -> Message {
    rsync_error!(
        1,
        format!(
            "invalid --info flag '{display}': supported flags are help, all, none, 0, 1, name, name0, name1, name2, progress, progress0, progress1, progress2, stats, stats0, and stats1"
        )
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

pub(crate) struct DebugFlagSettings {
    pub(crate) flags: Vec<OsString>,
    pub(crate) help_requested: bool,
}

impl DebugFlagSettings {
    fn push_flag(&mut self, flag: &str) {
        self.flags.push(OsString::from(flag));
    }
}

pub(crate) fn parse_debug_flags(values: &[OsString]) -> Result<DebugFlagSettings, Message> {
    let mut settings = DebugFlagSettings {
        flags: Vec::new(),
        help_requested: false,
    };

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
                settings.push_flag(token);
            }
        }
    }

    Ok(settings)
}

fn debug_flag_empty_error() -> Message {
    rsync_error!(1, "--debug flag must not be empty").with_role(Role::Client)
}

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
    name        Mention updated file and directory names.\n\
    name2       Mention updated and unchanged file and directory names.\n\
    name0       Disable file and directory name output.\n\
    progress    Enable per-file progress updates.\n\
    progress2   Enable overall transfer progress.\n\
    progress0   Disable progress reporting.\n\
    stats       Enable transfer statistics.\n\
    stats0      Disable transfer statistics.\n\
Flags may also be written with 'no' prefixes (for example, --info=noprogress).\n";

pub(crate) const DEBUG_HELP_TEXT: &str = "The following --debug flags are supported:\n\
    all         Enable all diagnostic categories currently implemented.\n\
    none        Disable diagnostic output.\n\
    checksum    Trace checksum calculations and verification.\n\
    deltas      Trace delta-transfer generation and token handling.\n\
    events      Trace file-list discovery and generator events.\n\
    fs          Trace filesystem metadata operations.\n\
    io          Trace I/O buffering and transport exchanges.\n\
    socket      Trace socket setup, negotiation, and pacing decisions.\n\
Flags may be prefixed with 'no' or '-' to disable a category. Multiple flags\n\
may be combined by separating them with commas.\n";
