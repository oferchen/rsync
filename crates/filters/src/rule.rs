use crate::FilterAction;

/// User-visible filter rule consisting of an action and pattern.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilterRule {
    pub(crate) action: FilterAction,
    pub(crate) pattern: String,
    pub(crate) applies_to_sender: bool,
    pub(crate) applies_to_receiver: bool,
}

impl FilterRule {
    /// Creates an include rule for `pattern`.
    #[must_use]
    pub fn include(pattern: impl Into<String>) -> Self {
        Self {
            action: FilterAction::Include,
            pattern: pattern.into(),
            applies_to_sender: true,
            applies_to_receiver: true,
        }
    }

    /// Creates an exclude rule for `pattern`.
    #[must_use]
    pub fn exclude(pattern: impl Into<String>) -> Self {
        Self {
            action: FilterAction::Exclude,
            pattern: pattern.into(),
            applies_to_sender: true,
            applies_to_receiver: true,
        }
    }

    /// Creates a protect rule for `pattern`.
    #[must_use]
    pub fn protect(pattern: impl Into<String>) -> Self {
        Self {
            action: FilterAction::Protect,
            pattern: pattern.into(),
            applies_to_sender: false,
            applies_to_receiver: true,
        }
    }

    /// Creates a risk rule for `pattern`.
    #[must_use]
    pub fn risk(pattern: impl Into<String>) -> Self {
        Self {
            action: FilterAction::Risk,
            pattern: pattern.into(),
            applies_to_sender: false,
            applies_to_receiver: true,
        }
    }

    /// Clears all previously configured rules for the applicable transfer sides.
    #[must_use]
    #[doc(alias = "!")]
    pub fn clear() -> Self {
        Self {
            action: FilterAction::Clear,
            pattern: String::new(),
            applies_to_sender: true,
            applies_to_receiver: true,
        }
    }

    /// Creates a sender-only include rule equivalent to `show PATTERN`.
    ///
    /// # Examples
    /// ```
    /// use rsync_filters::FilterRule;
    /// let rule = FilterRule::show("logs/**");
    /// assert!(rule.applies_to_sender());
    /// assert!(!rule.applies_to_receiver());
    /// ```
    #[must_use]
    pub fn show(pattern: impl Into<String>) -> Self {
        Self {
            action: FilterAction::Include,
            pattern: pattern.into(),
            applies_to_sender: true,
            applies_to_receiver: false,
        }
    }

    /// Creates a sender-only exclude rule equivalent to `hide PATTERN`.
    ///
    /// # Examples
    /// ```
    /// use rsync_filters::FilterRule;
    /// let rule = FilterRule::hide("*.bak");
    /// assert!(rule.applies_to_sender());
    /// assert!(!rule.applies_to_receiver());
    /// ```
    #[must_use]
    pub fn hide(pattern: impl Into<String>) -> Self {
        Self {
            action: FilterAction::Exclude,
            pattern: pattern.into(),
            applies_to_sender: true,
            applies_to_receiver: false,
        }
    }

    /// Returns the rule action.
    #[must_use]
    pub const fn action(&self) -> FilterAction {
        self.action
    }

    /// Returns the pattern text associated with the rule.
    #[must_use]
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Returns whether the rule affects the sending side.
    #[must_use]
    pub const fn applies_to_sender(&self) -> bool {
        self.applies_to_sender
    }

    /// Returns whether the rule affects the receiving side.
    #[must_use]
    pub const fn applies_to_receiver(&self) -> bool {
        self.applies_to_receiver
    }

    /// Sets whether the rule applies on the sending side.
    #[must_use]
    pub const fn with_sender(mut self, applies: bool) -> Self {
        self.applies_to_sender = applies;
        self
    }

    /// Sets whether the rule applies on the receiving side.
    #[must_use]
    pub const fn with_receiver(mut self, applies: bool) -> Self {
        self.applies_to_receiver = applies;
        self
    }

    /// Updates both side flags at once.
    #[must_use]
    pub const fn with_sides(mut self, sender: bool, receiver: bool) -> Self {
        self.applies_to_sender = sender;
        self.applies_to_receiver = receiver;
        self
    }

    /// Anchors the pattern to the root of the transfer if it is not already.
    #[must_use]
    pub fn anchor_to_root(mut self) -> Self {
        if !self.pattern.starts_with('/') {
            self.pattern.insert(0, '/');
        }
        self
    }
}
