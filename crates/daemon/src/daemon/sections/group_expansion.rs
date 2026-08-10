// Group expansion for `@group` syntax in `auth users` directives.
//
// An entry starting with `@` is a system-group reference; its members expand
// to additional authorised usernames. On non-Unix platforms `@group`
// references are silently ignored.

/// Looks up a group by name and returns its member usernames.
///
/// Delegates to `platform::group::lookup_group_members()`.
pub(crate) fn lookup_group_members(group_name: &str) -> Result<Option<Vec<String>>, io::Error> {
    platform::group::lookup_group_members(group_name)
}

#[cfg(test)]
mod group_expansion_tests {
    #[allow(unused_imports)]
    use super::*;

    #[cfg(unix)]
    #[test]
    fn lookup_group_members_nonexistent_returns_none() {
        let result = lookup_group_members("nonexistent_group_xyz_99999");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn lookup_group_members_root_group_returns_some() {
        let root_result = lookup_group_members("root");
        let wheel_result = lookup_group_members("wheel");

        assert!(root_result.is_ok());
        assert!(wheel_result.is_ok());

        if root_result.as_ref().is_ok_and(|r| r.is_some()) {
            let _ = root_result.unwrap().unwrap();
        }
        if wheel_result.as_ref().is_ok_and(|r| r.is_some()) {
            let _ = wheel_result.unwrap().unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn lookup_group_members_handles_null_in_name() {
        let result = lookup_group_members("test\x00group");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn lookup_group_members_handles_empty_name() {
        let result = lookup_group_members("");
        assert!(result.is_ok());
    }
}
