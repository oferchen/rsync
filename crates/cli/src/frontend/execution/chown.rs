use std::ffi::{OsStr, OsString};

use crate::platform::{gid_t, uid_t};

use core::{
    message::{Message, Role},
    rsync_error,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ParsedChown {
    spec: OsString,
    owner: Option<uid_t>,
    group: Option<gid_t>,
}

impl ParsedChown {
    pub(crate) fn owner(&self) -> Option<uid_t> {
        self.owner
    }

    pub(crate) fn group(&self) -> Option<gid_t> {
        self.group
    }

    /// Returns the original spec string
    #[allow(dead_code)]
    pub(crate) fn spec(&self) -> &OsString {
        &self.spec
    }
}

pub(crate) fn parse_chown_argument(value: &OsStr) -> Result<ParsedChown, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim();

    if trimmed.is_empty() {
        return Err(
            rsync_error!(1, "--chown requires a non-empty USER and/or GROUP")
                .with_role(Role::Client),
        );
    }

    let (user_part, group_part) = match trimmed.split_once(':') {
        Some((user, group)) => (user, group),
        None => (trimmed, ""),
    };

    let owner = if user_part.is_empty() {
        None
    } else {
        Some(resolve_chown_user(user_part)?)
    };
    let group = if group_part.is_empty() {
        None
    } else {
        Some(resolve_chown_group(group_part)?)
    };

    if owner.is_none() && group.is_none() {
        return Err(rsync_error!(1, "--chown requires a user and/or group").with_role(Role::Client));
    }

    Ok(ParsedChown {
        spec: OsString::from(trimmed),
        owner,
        group,
    })
}

fn resolve_chown_user(input: &str) -> Result<uid_t, Message> {
    if let Ok(id) = input.parse::<uid_t>() {
        return Ok(id);
    }

    if let Some(uid) = crate::platform::lookup_user_by_name(input) {
        return Ok(uid);
    }

    if crate::platform::supports_user_name_lookup() {
        Err(
            rsync_error!(1, "unknown user '{}' specified for --chown", input)
                .with_role(Role::Client),
        )
    } else {
        Err(rsync_error!(
            1,
            "user name '{}' specified for --chown requires a numeric ID on this platform",
            input
        )
        .with_role(Role::Client))
    }
}

fn resolve_chown_group(input: &str) -> Result<gid_t, Message> {
    if let Ok(id) = input.parse::<gid_t>() {
        return Ok(id);
    }

    if let Some(gid) = crate::platform::lookup_group_by_name(input) {
        return Ok(gid);
    }

    if crate::platform::supports_group_name_lookup() {
        Err(
            rsync_error!(1, "unknown group '{}' specified for --chown", input)
                .with_role(Role::Client),
        )
    } else {
        Err(rsync_error!(
            1,
            "group name '{}' specified for --chown requires a numeric ID on this platform",
            input
        )
        .with_role(Role::Client))
    }
}
