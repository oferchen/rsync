//! Newline behaviour control for message rendering.
//!
//! The [`LineMode`] enum determines whether a [`MessageSink`](crate::MessageSink)
//! appends a trailing newline after each rendered message, matching upstream
//! rsync's default of printing each diagnostic on its own line.

/// Controls whether a [`MessageSink`](crate::MessageSink) appends a trailing newline when writing messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum LineMode {
    /// Append a newline terminator after each rendered message.
    #[default]
    WithNewline,
    /// Emit the rendered message without a trailing newline.
    WithoutNewline,
}

impl LineMode {
    /// Reports whether the mode appends a trailing newline when rendering a message.
    ///
    /// [`LineMode::WithNewline`] matches upstream rsync's default of emitting
    /// each diagnostic on its own line.
    ///
    /// # Examples
    ///
    /// ```
    /// use logging_sink::LineMode;
    ///
    /// assert!(LineMode::WithNewline.append_newline());
    /// assert!(!LineMode::WithoutNewline.append_newline());
    /// ```
    #[must_use]
    pub const fn append_newline(self) -> bool {
        matches!(self, Self::WithNewline)
    }
}

impl From<bool> for LineMode {
    /// `true` maps to [`LineMode::WithNewline`]; `false` selects [`LineMode::WithoutNewline`].
    ///
    /// # Examples
    ///
    /// ```
    /// use logging_sink::LineMode;
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
    /// Delegates to [`LineMode::append_newline`].
    ///
    /// # Examples
    ///
    /// ```
    /// use logging_sink::LineMode;
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
