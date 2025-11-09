/// Controls whether a [`MessageSink`](crate::MessageSink) appends a trailing newline when writing messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineMode {
    /// Append a newline terminator after each rendered message.
    WithNewline,
    /// Emit the rendered message without a trailing newline.
    WithoutNewline,
}

impl LineMode {
    /// Reports whether the mode appends a trailing newline when rendering a message.
    ///
    /// The helper mirrors the terminology used throughout the workspace where
    /// [`LineMode::WithNewline`] matches upstream rsync's default of emitting
    /// each diagnostic on its own line. Exposing the behaviour as a method
    /// avoids requiring callers to pattern-match on the enum, simplifying
    /// integrations that need to mirror the sink's newline policy when routing
    /// messages to multiple destinations.
    ///
    /// # Examples
    ///
    /// ```
    /// use logging::LineMode;
    ///
    /// assert!(LineMode::WithNewline.append_newline());
    /// assert!(!LineMode::WithoutNewline.append_newline());
    /// ```
    #[must_use]
    pub const fn append_newline(self) -> bool {
        matches!(self, Self::WithNewline)
    }
}

impl Default for LineMode {
    fn default() -> Self {
        Self::WithNewline
    }
}

impl From<bool> for LineMode {
    /// Converts a boolean flag describing whether a trailing newline should be appended into a [`LineMode`].
    ///
    /// `true` maps to [`LineMode::WithNewline`] while `false` selects [`LineMode::WithoutNewline`],
    /// mirroring the terminology used throughout the workspace. This allows call sites that already
    /// compute newline behaviour as a boolean (for example, when matching upstream format tables) to
    /// adopt [`MessageSink`](crate::MessageSink) without branching on the enum variants themselves.
    ///
    /// # Examples
    ///
    /// ```
    /// use logging::LineMode;
    ///
    /// assert_eq!(LineMode::from(true), LineMode::WithNewline);
    /// assert_eq!(LineMode::from(false), LineMode::WithoutNewline);
    /// ```
    fn from(append_newline: bool) -> Self {
        if append_newline {
            Self::WithNewline
        } else {
            Self::WithoutNewline
        }
    }
}

impl From<LineMode> for bool {
    /// Converts a [`LineMode`] back into a boolean flag describing whether a trailing newline is appended.
    ///
    /// The conversion delegates to [`LineMode::append_newline`], ensuring the mapping remains consistent even
    /// if future variants are introduced. This is primarily useful in formatting pipelines that need to feed
    /// newline preferences into APIs expecting a boolean without reimplementing the enum-to-bool logic.
    ///
    /// # Examples
    ///
    /// ```
    /// use logging::LineMode;
    ///
    /// let append_newline: bool = LineMode::WithNewline.into();
    /// assert!(append_newline);
    ///
    /// let append_newline: bool = LineMode::WithoutNewline.into();
    /// assert!(!append_newline);
    /// ```
    fn from(mode: LineMode) -> Self {
        mode.append_newline()
    }
}

#[cfg(test)]
mod tests;
