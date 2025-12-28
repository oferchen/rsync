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
    pub(crate) const fn owner(&self) -> Option<uid_t> {
        self.owner
    }

    pub(crate) const fn group(&self) -> Option<gid_t> {
        self.group
    }

    /// Returns the original spec string
    #[allow(dead_code)]
    pub(crate) const fn spec(&self) -> &OsString {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn parsed_chown_getters() {
        let parsed = ParsedChown {
            spec: OsString::from("1000:1000"),
            owner: Some(1000),
            group: Some(1000),
        };
        assert_eq!(parsed.owner(), Some(1000));
        assert_eq!(parsed.group(), Some(1000));
        assert_eq!(parsed.spec(), &OsString::from("1000:1000"));
    }

    #[test]
    fn parsed_chown_with_no_owner() {
        let parsed = ParsedChown {
            spec: OsString::from(":1000"),
            owner: None,
            group: Some(1000),
        };
        assert!(parsed.owner().is_none());
        assert_eq!(parsed.group(), Some(1000));
    }

    #[test]
    fn parsed_chown_with_no_group() {
        let parsed = ParsedChown {
            spec: OsString::from("1000"),
            owner: Some(1000),
            group: None,
        };
        assert_eq!(parsed.owner(), Some(1000));
        assert!(parsed.group().is_none());
    }

    #[test]
    fn parsed_chown_eq() {
        let a = ParsedChown {
            spec: OsString::from("1000:1000"),
            owner: Some(1000),
            group: Some(1000),
        };
        let b = ParsedChown {
            spec: OsString::from("1000:1000"),
            owner: Some(1000),
            group: Some(1000),
        };
        assert_eq!(a, b);
    }

    #[test]
    fn parsed_chown_clone() {
        let parsed = ParsedChown {
            spec: OsString::from("1000:1000"),
            owner: Some(1000),
            group: Some(1000),
        };
        let cloned = parsed.clone();
        assert_eq!(parsed, cloned);
    }

    #[test]
    fn parse_chown_argument_numeric_user_only() {
        let result = parse_chown_argument(OsStr::new("1000")).expect("parse");
        assert_eq!(result.owner(), Some(1000));
        assert!(result.group().is_none());
    }

    #[test]
    fn parse_chown_argument_numeric_group_only() {
        let result = parse_chown_argument(OsStr::new(":1000")).expect("parse");
        assert!(result.owner().is_none());
        assert_eq!(result.group(), Some(1000));
    }

    #[test]
    fn parse_chown_argument_numeric_user_and_group() {
        let result = parse_chown_argument(OsStr::new("1000:2000")).expect("parse");
        assert_eq!(result.owner(), Some(1000));
        assert_eq!(result.group(), Some(2000));
    }

    #[test]
    fn parse_chown_argument_empty_fails() {
        let result = parse_chown_argument(OsStr::new(""));
        assert!(result.is_err());
    }

    #[test]
    fn parse_chown_argument_whitespace_only_fails() {
        let result = parse_chown_argument(OsStr::new("   "));
        assert!(result.is_err());
    }

    #[test]
    fn parse_chown_argument_colon_only_fails() {
        let result = parse_chown_argument(OsStr::new(":"));
        assert!(result.is_err());
    }
}
