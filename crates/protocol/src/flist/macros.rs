//! Macros for file list reader/writer builder methods.
//!
//! This module contains macros that generate common builder methods shared
//! between `FileListReader` and `FileListWriter`. This eliminates duplication
//! while maintaining the same API for both types.

/// Generates preserve-style builder methods for file list types.
///
/// This macro generates a set of builder methods that follow the pattern
/// `with_preserve_X(mut self, preserve: bool) -> Self` for configuring
/// which file attributes should be read from or written to the wire.
///
/// # Generated Methods
///
/// For each field specified, generates a method with:
/// - `#[inline]` for optimization
/// - `#[must_use]` to ensure the returned builder is used
/// - `pub const fn` for compile-time evaluation when possible
/// - A rustdoc comment describing the method
///
/// # Example
///
/// ```ignore
/// impl_preserve_builders! {
///     /// Sets whether UID values should be processed.
///     uid => preserve_uid,
///     /// Sets whether GID values should be processed.
///     gid => preserve_gid,
/// }
/// ```
///
/// Generates:
/// ```ignore
/// /// Sets whether UID values should be processed.
/// #[inline]
/// #[must_use]
/// pub const fn with_preserve_uid(mut self, preserve: bool) -> Self {
///     self.preserve_uid = preserve;
///     self
/// }
/// ```
#[allow(unused_macros)]
macro_rules! impl_preserve_builders {
    (
        $(
            #[doc = $doc:expr]
            $name:ident => $field:ident
        ),* $(,)?
    ) => {
        $(
            #[doc = $doc]
            #[inline]
            #[must_use]
            pub const fn $name(mut self, preserve: bool) -> Self {
                self.$field = preserve;
                self
            }
        )*
    };
}

#[allow(unused_imports)]
pub(crate) use impl_preserve_builders;
