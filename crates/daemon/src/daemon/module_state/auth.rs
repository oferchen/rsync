/// Per-user access level override for rsync daemon authentication.
///
/// When a username in `auth users` includes a suffix (`:ro`, `:rw`, or `:deny`),
/// the access level overrides the module's default read_only/write_only settings.
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
