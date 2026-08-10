/// Per-user access level override for rsync daemon authentication.
///
/// When a username in `auth users` includes a suffix (`:ro`, `:rw`, or `:deny`),
/// the access level overrides the module's default read_only/write_only settings.
///
/// upstream: loadparm.c - `auth users` parameter supports `user:ro`, `user:rw`,
/// and `user:deny` suffixes to control per-user access independently of the
/// module's `read only` and `write only` settings.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum UserAccessLevel {
    /// Use module's default access (read_only/write_only settings).
    #[default]
    Default,
    /// Read-only access regardless of module settings.
    ReadOnly,
    /// Read-write access regardless of module settings.
    ReadWrite,
    /// Deny access (authentication succeeds but access is blocked).
    Deny,
}

/// An authorized user with optional access level override.
///
/// Represents a user entry from the `auth users` directive in rsyncd.conf.
/// The access level defaults to `Default` (use module settings) unless
/// explicitly specified with a suffix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AuthUser {
    /// The username for authentication.
    pub(crate) username: String,
    /// Access level override for this user.
    pub(crate) access_level: UserAccessLevel,
}

impl AuthUser {
    /// Creates a new AuthUser with default access level.
    #[allow(dead_code)] // Used in tests and for future use
    pub(crate) fn new(username: String) -> Self {
        Self {
            username,
            access_level: UserAccessLevel::Default,
        }
    }

    /// Creates a new AuthUser with a specific access level.
    pub(crate) fn with_access(username: String, access_level: UserAccessLevel) -> Self {
        Self {
            username,
            access_level,
        }
    }
}

/// Resolves whether an authenticating user belongs to a named system group.
///
/// This is a seam over the system group database so that `auth users = @group`
/// matching can be exercised hermetically in tests. Production authentication
/// uses [`SystemGroupMembership`]; tests inject a deterministic resolver.
pub(crate) trait GroupMembership {
    /// Returns whether `user` is a member of the group named `group`.
    fn is_member(&self, user: &str, group: &str) -> bool;
}

/// Production [`GroupMembership`] backed by the host group database.
///
/// Membership is resolved from the group's member list (`getgrnam_r` on Unix,
/// `NetLocalGroupGetMembers` on Windows) via `platform::group`. On platforms
/// without a group database the lookup yields no members and every check is
/// `false`.
///
/// upstream: authenticate.c:276 `auth_server()` authorizes an `@group` token by
/// checking the authenticating user's group membership.
pub(crate) struct SystemGroupMembership;

impl GroupMembership for SystemGroupMembership {
    fn is_member(&self, user: &str, group: &str) -> bool {
        matches!(
            crate::daemon::lookup_group_members(group),
            Ok(Some(members)) if members.iter().any(|member| member == user)
        )
    }
}

/// Returns the first `auth users` entry that authorizes `username`, or `None`
/// when no entry matches.
///
/// Mirrors the token loop in upstream `auth_server`: entries are evaluated in
/// configuration order and the first match wins. A plain token matches the
/// username via shell-wildcard `wildmatch` (which subsumes an exact literal
/// match); a token starting with `@` authorizes any user who is a member of the
/// named group. The matched entry's access suffix (`:ro`/`:rw`/`:deny`) then
/// governs access downstream, so a matching `:deny` entry still denies.
///
/// upstream: authenticate.c:276 `auth_server()` - `if (wildmatch(tok, line))`
/// for plain tokens and group membership for `@group` tokens.
pub(crate) fn authorize_auth_user<'a, G: GroupMembership>(
    entries: &'a [AuthUser],
    username: &str,
    groups: &G,
) -> Option<&'a AuthUser> {
    entries
        .iter()
        .find(|entry| match entry.username.strip_prefix('@') {
            Some(group) => groups.is_member(username, group),
            None => filters::wildmatch(entry.username.as_bytes(), username.as_bytes()),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Deterministic [`GroupMembership`] for hermetic `@group` tests. Maps a
    /// group name to the set of member usernames, so authorization does not
    /// depend on the host's `/etc/group`.
    struct MockGroups(HashMap<&'static str, Vec<&'static str>>);

    impl GroupMembership for MockGroups {
        fn is_member(&self, user: &str, group: &str) -> bool {
            self.0
                .get(group)
                .is_some_and(|members| members.contains(&user))
        }
    }

    fn entries(tokens: &[&str]) -> Vec<AuthUser> {
        tokens
            .iter()
            .map(|token| AuthUser::new((*token).to_owned()))
            .collect()
    }

    /// Exact literal tokens still authorize the named user and reject others,
    /// matching upstream's `wildmatch("bob", line)` for a metacharacter-free
    /// token.
    #[test]
    fn exact_token_authorizes_only_that_user() {
        let list = entries(&["bob"]);
        let groups = MockGroups(HashMap::new());
        assert!(authorize_auth_user(&list, "bob", &groups).is_some());
        assert!(authorize_auth_user(&list, "alice", &groups).is_none());
    }

    /// A wildcard token authorizes matching usernames and rejects the rest,
    /// mirroring upstream `wildmatch("user*", line)`.
    #[test]
    fn wildcard_token_matches_by_pattern() {
        let list = entries(&["user*"]);
        let groups = MockGroups(HashMap::new());
        assert!(authorize_auth_user(&list, "user1", &groups).is_some());
        assert!(authorize_auth_user(&list, "user-admin", &groups).is_some());
        assert!(authorize_auth_user(&list, "admin", &groups).is_none());
    }

    /// An `@group` token authorizes members of the group and rejects
    /// non-members, mirroring upstream's group-membership check.
    #[test]
    fn group_token_authorizes_members_only() {
        let list = entries(&["@wheel"]);
        let groups = MockGroups(HashMap::from([("wheel", vec!["carol", "dave"])]));
        assert!(authorize_auth_user(&list, "carol", &groups).is_some());
        assert!(authorize_auth_user(&list, "mallory", &groups).is_none());
    }

    /// First match wins across a mixed list, and the matched entry's access
    /// level is the one returned, matching upstream's first-token-wins loop.
    #[test]
    fn first_match_wins_and_returns_matched_entry() {
        let list = vec![
            AuthUser::with_access("bob".to_owned(), UserAccessLevel::Deny),
            AuthUser::with_access("bob".to_owned(), UserAccessLevel::ReadWrite),
        ];
        let groups = MockGroups(HashMap::new());
        let matched = authorize_auth_user(&list, "bob", &groups).expect("bob matches");
        assert_eq!(matched.access_level, UserAccessLevel::Deny);
    }

    /// A non-matching user against a mixed exact/wildcard/@group list is
    /// rejected outright.
    #[test]
    fn non_matching_user_is_rejected() {
        let list = entries(&["alice", "team*", "@staff"]);
        let groups = MockGroups(HashMap::from([("staff", vec!["carol"])]));
        assert!(authorize_auth_user(&list, "intruder", &groups).is_none());
    }
}
