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

/// Enumerates every system group an authenticating user belongs to.
///
/// This is a seam over the system group database so that `auth users = @group`
/// matching can be exercised hermetically in tests. Production authentication
/// uses [`SystemGroupMembership`]; tests inject a deterministic resolver.
///
/// The direction mirrors upstream: it enumerates the *user's* groups (primary +
/// supplementary) rather than looking up the members of a single named group,
/// because an `@group` token may be a wildcard (`@admin*`) that has to be
/// matched against every group the user is in.
pub(crate) trait GroupMembership {
    /// Returns the names of all groups `user` belongs to, primary group first.
    fn groups_of(&self, user: &str) -> Vec<String>;
}

/// Production [`GroupMembership`] backed by the host group database.
///
/// Resolves the user's uid and enumerates its full group set via
/// `metadata::id_lookup::groups_for_user` (`getpwnam` + `getgrouplist` +
/// `getgrgid`). On platforms without a POSIX group database the set is empty,
/// so no `@group` token can match - matching upstream, which compiles the
/// `@group` branch only when `HAVE_GETGROUPLIST` is defined.
///
/// upstream: authenticate.c:283-295 `auth_server()` resolves the authenticating
/// user's groups with `getallgroups()` and `wildmatch`es each against `tok+1`.
pub(crate) struct SystemGroupMembership;

impl GroupMembership for SystemGroupMembership {
    fn groups_of(&self, user: &str) -> Vec<String> {
        #[cfg(unix)]
        {
            metadata::id_lookup::groups_for_user(user).unwrap_or_default()
        }
        #[cfg(not(unix))]
        {
            let _ = user;
            Vec::new()
        }
    }
}

/// The `auth users` entry that authorized a client, plus the concrete system
/// group name that satisfied an `@group` token.
///
/// `group` carries the actual matched group name (e.g. `administrators` for a
/// `@admin*` token), not the token, because upstream hands that resolved name -
/// not the pattern - to `check_secret()` when looking up an `@group:` secret
/// line. It is `None` for a plain-username match.
///
/// upstream: authenticate.c:318 `auth_server()` -
/// `group_match >= 0 ? auth_uid_groups[group_match] : NULL`.
pub(crate) struct AuthMatch<'a> {
    /// The matched `auth users` entry, carrying its `:ro`/`:rw`/`:deny` suffix.
    pub(crate) user: &'a AuthUser,
    /// The resolved system group name for an `@group` match, else `None`.
    pub(crate) group: Option<String>,
}

/// Returns the first `auth users` entry that authorizes `username`, or `None`
/// when no entry matches.
///
/// Mirrors the token loop in upstream `auth_server`: entries are evaluated in
/// configuration order and the first match wins. A plain token matches the
/// username via shell-wildcard `wildmatch` (which subsumes an exact literal
/// match); a token starting with `@` authorizes the user when any group they
/// belong to `wildmatch`es the token (minus the `@`) - so `@admin*` matches a
/// member of `administrators`. The matched entry's access suffix
/// (`:ro`/`:rw`/`:deny`) governs access downstream, so a matching `:deny` entry
/// still denies.
///
/// The user's group set is resolved lazily and at most once, only when an
/// `@group` token is reached, mirroring upstream's cached `auth_uid_groups`.
///
/// upstream: authenticate.c:274-304 `auth_server()` - `wildmatch(tok, line)`
/// for plain tokens; for `@group`, `wildmatch(tok+1, auth_uid_groups[j])` over
/// the authenticating user's own groups.
pub(crate) fn authorize_auth_user<'a, G: GroupMembership>(
    entries: &'a [AuthUser],
    username: &str,
    groups: &G,
) -> Option<AuthMatch<'a>> {
    let mut user_groups: Option<Vec<String>> = None;
    for entry in entries {
        match entry.username.strip_prefix('@') {
            None => {
                if filters::wildmatch(entry.username.as_bytes(), username.as_bytes()) {
                    return Some(AuthMatch {
                        user: entry,
                        group: None,
                    });
                }
            }
            Some(token) => {
                let resolved = user_groups.get_or_insert_with(|| groups.groups_of(username));
                if let Some(name) = resolved
                    .iter()
                    .find(|name| filters::wildmatch(token.as_bytes(), name.as_bytes()))
                {
                    return Some(AuthMatch {
                        user: entry,
                        group: Some(name.clone()),
                    });
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Deterministic [`GroupMembership`] for hermetic `@group` tests. Maps each
    /// username to the groups it belongs to, so authorization does not depend on
    /// the host's `/etc/group`. This is the same direction upstream resolves
    /// (`getallgroups()` on the authenticating user), which is what lets a
    /// wildcard token like `@admin*` be matched against every one of the user's
    /// groups.
    struct MockGroups(HashMap<&'static str, Vec<&'static str>>);

    impl GroupMembership for MockGroups {
        fn groups_of(&self, user: &str) -> Vec<String> {
            self.0
                .get(user)
                .into_iter()
                .flatten()
                .map(|group| (*group).to_owned())
                .collect()
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

    /// An exact `@group` token authorizes members of that group and rejects
    /// non-members. The resolved group name (not the token) is reported so the
    /// downstream secrets lookup keys off the concrete group, mirroring
    /// upstream's `auth_uid_groups[group_match]`.
    #[test]
    fn group_token_authorizes_members_only() {
        let list = entries(&["@wheel"]);
        let groups = MockGroups(HashMap::from([
            ("carol", vec!["wheel"]),
            ("dave", vec!["users", "wheel"]),
        ]));
        let carol = authorize_auth_user(&list, "carol", &groups).expect("carol is in wheel");
        assert_eq!(carol.group.as_deref(), Some("wheel"));
        assert!(authorize_auth_user(&list, "dave", &groups).is_some());
        assert!(authorize_auth_user(&list, "mallory", &groups).is_none());
    }

    /// A wildcard `@group` token authorizes a user whose *own* group set
    /// contains a group matching the pattern, and rejects a user with no
    /// matching group. This is the core upstream behaviour: `wildmatch(tok+1,
    /// group_name)` over `getallgroups(user)`, so `@admin*` matches a member of
    /// `administrators` or `admin-team`. A literal `getgrnam("admin*")` would
    /// find nothing and wrongly deny - this test fails if the resolution
    /// regresses to that direction.
    #[test]
    fn wildcard_group_token_matches_users_groups() {
        let list = entries(&["@admin*"]);
        let groups = MockGroups(HashMap::from([
            ("alice", vec!["users", "administrators"]),
            ("bob", vec!["admin-team"]),
            ("mallory", vec!["users", "staff"]),
        ]));

        let alice = authorize_auth_user(&list, "alice", &groups).expect("alice in administrators");
        // The concrete matched group, not the `admin*` pattern, is reported.
        assert_eq!(alice.group.as_deref(), Some("administrators"));

        let bob = authorize_auth_user(&list, "bob", &groups).expect("bob in admin-team");
        assert_eq!(bob.group.as_deref(), Some("admin-team"));

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
        assert_eq!(matched.user.access_level, UserAccessLevel::Deny);
        assert_eq!(matched.group, None);
    }

    /// A non-matching user against a mixed exact/wildcard/@group list is
    /// rejected outright.
    #[test]
    fn non_matching_user_is_rejected() {
        let list = entries(&["alice", "team*", "@staff"]);
        let groups = MockGroups(HashMap::from([("carol", vec!["staff"])]));
        assert!(authorize_auth_user(&list, "intruder", &groups).is_none());
    }
}
