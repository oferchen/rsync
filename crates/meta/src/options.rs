use crate::chmod::ChmodModifiers;

/// Options that control metadata preservation during copy operations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataOptions {
    preserve_owner: bool,
    preserve_group: bool,
    preserve_permissions: bool,
    preserve_times: bool,
    numeric_ids: bool,
    owner_override: Option<u32>,
    group_override: Option<u32>,
    chmod: Option<ChmodModifiers>,
}

impl MetadataOptions {
    /// Creates a new [`MetadataOptions`] value with defaults applied.
    ///
    /// By default the options preserve permissions and timestamps while leaving
    /// ownership disabled so callers can opt-in as needed.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            preserve_owner: false,
            preserve_group: false,
            preserve_permissions: true,
            preserve_times: true,
            numeric_ids: false,
            owner_override: None,
            group_override: None,
            chmod: None,
        }
    }

    /// Requests that ownership be preserved when applying metadata.
    #[must_use]
    pub const fn preserve_owner(mut self, preserve: bool) -> Self {
        self.preserve_owner = preserve;
        self
    }

    /// Requests that the group be preserved when applying metadata.
    #[must_use]
    pub const fn preserve_group(mut self, preserve: bool) -> Self {
        self.preserve_group = preserve;
        self
    }

    /// Requests that permissions be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--perms")]
    pub const fn preserve_permissions(mut self, preserve: bool) -> Self {
        self.preserve_permissions = preserve;
        self
    }

    /// Requests that timestamps be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--times")]
    pub const fn preserve_times(mut self, preserve: bool) -> Self {
        self.preserve_times = preserve;
        self
    }

    /// Requests that UID/GID preservation use numeric identifiers instead of mapping by name.
    #[must_use]
    #[doc(alias = "--numeric-ids")]
    pub const fn numeric_ids(mut self, numeric: bool) -> Self {
        self.numeric_ids = numeric;
        self
    }

    /// Applies an explicit ownership override using numeric identifiers.
    ///
    /// When set, the override takes precedence over [`Self::preserve_owner`]
    /// and [`Self::numeric_ids`] by forcing the supplied UID regardless of the
    /// source metadata. This mirrors the behaviour of rsync's `--chown`
    /// receiver-side handling.
    #[must_use]
    pub const fn with_owner_override(mut self, owner: Option<u32>) -> Self {
        self.owner_override = owner;
        self
    }

    /// Applies an explicit group override using numeric identifiers.
    ///
    /// When set, the override takes precedence over [`Self::preserve_group`]
    /// and [`Self::numeric_ids`] by forcing the supplied GID regardless of the
    /// source metadata. This mirrors the behaviour of rsync's `--chown`
    /// receiver-side handling.
    #[must_use]
    pub const fn with_group_override(mut self, group: Option<u32>) -> Self {
        self.group_override = group;
        self
    }

    /// Supplies chmod modifiers that should be applied after metadata is
    /// preserved.
    #[must_use]
    pub fn with_chmod(mut self, modifiers: Option<ChmodModifiers>) -> Self {
        self.chmod = modifiers;
        self
    }

    /// Reports whether ownership should be preserved.
    #[must_use]
    pub const fn owner(&self) -> bool {
        self.preserve_owner
    }

    /// Reports whether the group should be preserved.
    #[must_use]
    pub const fn group(&self) -> bool {
        self.preserve_group
    }

    /// Reports whether permissions should be preserved.
    #[must_use]
    pub const fn permissions(&self) -> bool {
        self.preserve_permissions
    }

    /// Reports whether timestamps should be preserved.
    #[must_use]
    pub const fn times(&self) -> bool {
        self.preserve_times
    }

    /// Reports whether numeric UID/GID preservation was requested.
    #[must_use]
    pub const fn numeric_ids_enabled(&self) -> bool {
        self.numeric_ids
    }

    /// Reports the configured ownership override if any.
    #[must_use]
    pub const fn owner_override(&self) -> Option<u32> {
        self.owner_override
    }

    /// Reports the configured group override if any.
    #[must_use]
    pub const fn group_override(&self) -> Option<u32> {
        self.group_override
    }

    /// Returns the chmod modifiers, if any.
    #[must_use]
    pub fn chmod(&self) -> Option<&ChmodModifiers> {
        self.chmod.as_ref()
    }
}

impl Default for MetadataOptions {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chmod::ChmodModifiers;

    #[test]
    fn defaults_match_expected_configuration() {
        let options = MetadataOptions::new();

        assert!(!options.owner());
        assert!(!options.group());
        assert!(options.permissions());
        assert!(options.times());
        assert!(!options.numeric_ids_enabled());
        assert!(options.owner_override().is_none());
        assert!(options.group_override().is_none());
        assert!(options.chmod().is_none());

        assert_eq!(MetadataOptions::default(), options);
    }

    #[test]
    fn builder_methods_apply_requested_flags() {
        let modifiers = ChmodModifiers::parse("u=rw").expect("parse modifiers");

        let options = MetadataOptions::new()
            .preserve_owner(true)
            .preserve_group(true)
            .preserve_permissions(false)
            .preserve_times(false)
            .numeric_ids(true)
            .with_owner_override(Some(42))
            .with_group_override(Some(7))
            .with_chmod(Some(modifiers.clone()));

        assert!(options.owner());
        assert!(options.group());
        assert!(!options.permissions());
        assert!(!options.times());
        assert!(options.numeric_ids_enabled());
        assert_eq!(options.owner_override(), Some(42));
        assert_eq!(options.group_override(), Some(7));
        assert_eq!(options.chmod(), Some(&modifiers));
    }

    #[test]
    fn overrides_and_chmod_can_be_cleared() {
        let base = MetadataOptions::new()
            .with_owner_override(Some(13))
            .with_group_override(Some(24))
            .with_chmod(Some(ChmodModifiers::parse("g+x").expect("parse modifiers")));

        let cleared = base
            .with_owner_override(None)
            .with_group_override(None)
            .with_chmod(None)
            .preserve_owner(false)
            .preserve_group(false)
            .preserve_permissions(true)
            .preserve_times(true)
            .numeric_ids(false);

        assert!(!cleared.owner());
        assert!(!cleared.group());
        assert!(cleared.permissions());
        assert!(cleared.times());
        assert!(!cleared.numeric_ids_enabled());
        assert!(cleared.owner_override().is_none());
        assert!(cleared.group_override().is_none());
        assert!(cleared.chmod().is_none());
    }
}
