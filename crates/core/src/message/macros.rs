/// Captures the current source location for rsync diagnostics.
#[macro_export]
macro_rules! message_source {
    () => {
        $crate::message::SourceLocation::from_parts(env!("CARGO_MANIFEST_DIR"), file!(), line!())
    };
}

/// Builds a [`SourceLocation`](crate::message::SourceLocation) from an explicit
/// [`std::panic::Location`].
///
/// This macro is useful when the caller already captured a location through
/// `#[track_caller]` and wishes to convert it into the repo-relative form used
/// by the message subsystem.
///
/// # Examples
///
/// ```
/// use rsync_core::{message::SourceLocation, message_source_from};
///
/// let caller = std::panic::Location::caller();
/// let location: SourceLocation = message_source_from!(caller);
/// assert_eq!(location.line(), caller.line());
/// ```
#[macro_export]
macro_rules! message_source_from {
    ($location:expr) => {{
        let location = $location;
        $crate::message::SourceLocation::from_parts(
            env!("CARGO_MANIFEST_DIR"),
            location.file(),
            location.line(),
        )
    }};
}

/// Captures a [`SourceLocation`](crate::message::SourceLocation) that honours
/// `#[track_caller]` propagation.
///
/// Unlike [`macro@message_source`], this macro calls [`std::panic::Location::caller`]
/// so that helper functions annotated with `#[track_caller]` report the
/// location of their caller rather than their own body.
///
/// # Examples
///
/// ```
/// use rsync_core::{message::SourceLocation, tracked_message_source};
///
/// #[track_caller]
/// fn helper() -> SourceLocation {
///     tracked_message_source!()
/// }
///
/// let location = helper();
/// assert!(location.path().ends_with(".rs"));
/// ```
#[macro_export]
macro_rules! tracked_message_source {
    () => {{ $crate::message_source_from!(::std::panic::Location::caller()) }};
}

/// Constructs an error [`Message`](crate::message::Message) with the provided
/// exit code and payload.
///
/// The macro mirrors upstream diagnostics by automatically attaching the
/// call-site [`SourceLocation`](crate::message::SourceLocation) using
/// [`macro@crate::tracked_message_source`]. It
/// accepts either a
/// string literal/`Cow<'static, str>` payload or a [`format!`] style template.
/// Callers can further customise the returned
/// [`Message`](crate::message::Message) by chaining helpers such as
/// [`Message::with_role`](crate::message::Message::with_role).
///
/// # Examples
///
/// ```
/// use rsync_core::{message::Role, rsync_error};
///
/// let rendered = rsync_error!(23, "delta-transfer failure")
///     .with_role(Role::Sender)
///     .to_string();
///
/// assert!(rendered.contains("rsync error: delta-transfer failure (code 23)"));
/// assert!(rendered.contains(&format!(
///     "[sender={}]",
///     rsync_core::version::RUST_VERSION
/// )));
/// ```
///
/// Formatting arguments are forwarded to [`format!`]:
///
/// ```
/// use rsync_core::rsync_error;
///
/// let path = "src/lib.rs";
/// let rendered = rsync_error!(11, "failed to read {path}", path = path).to_string();
///
/// assert!(rendered.contains("failed to read src/lib.rs"));
/// ```
#[macro_export]
macro_rules! rsync_error {
    ($code:expr, $text:expr $(,)?) => {{
        $crate::message::Message::error($code, $text)
            .with_source($crate::tracked_message_source!())
    }};
    ($code:expr, $fmt:expr, $($arg:tt)+ $(,)?) => {{
        $crate::message::Message::error($code, format!($fmt, $($arg)+))
            .with_source($crate::tracked_message_source!())
    }};
}

/// Constructs a warning [`Message`](crate::message::Message) from the provided
/// payload.
///
/// Like [`macro@rsync_error`], the macro records the call-site source location so
/// diagnostics include repo-relative paths. The macro relies on
/// [`macro@crate::tracked_message_source`], meaning callers annotated with
/// `#[track_caller]` automatically propagate their invocation site. Additional
/// context, such as exit codes, can be attached using
/// [`Message::with_code`](crate::message::Message::with_code).
///
/// # Examples
///
/// ```
/// use rsync_core::rsync_warning;
///
/// let rendered = rsync_warning!("some files vanished")
///     .with_code(24)
///     .to_string();
///
/// assert!(rendered.starts_with("rsync warning:"));
/// assert!(rendered.contains("(code 24)"));
/// ```
#[macro_export]
macro_rules! rsync_warning {
    ($text:expr $(,)?) => {{
        $crate::message::Message::warning($text)
            .with_source($crate::tracked_message_source!())
    }};
    ($fmt:expr, $($arg:tt)+ $(,)?) => {{
        $crate::message::Message::warning(format!($fmt, $($arg)+))
            .with_source($crate::tracked_message_source!())
    }};
}

/// Constructs an informational [`Message`](crate::message::Message) from the
/// provided payload.
///
/// The macro is a convenience wrapper around
/// [`Message::info`](crate::message::Message::info) that automatically
/// attaches the call-site source location. Upstream rsync typically omits source
/// locations for informational output, but retaining the metadata simplifies
/// debugging and keeps the API consistent across severities. As with the other
/// message macros, [`macro@crate::tracked_message_source`] ensures
/// `#[track_caller]` annotations propagate the original invocation site into
/// diagnostics.
///
/// # Examples
///
/// ```
/// use rsync_core::rsync_info;
///
/// let rendered = rsync_info!("negotiation complete").to_string();
///
/// assert!(rendered.starts_with("rsync info:"));
/// ```
#[macro_export]
macro_rules! rsync_info {
    ($text:expr $(,)?) => {{
        $crate::message::Message::info($text)
            .with_source($crate::tracked_message_source!())
    }};
    ($fmt:expr, $($arg:tt)+ $(,)?) => {{
        $crate::message::Message::info(format!($fmt, $($arg)+))
            .with_source($crate::tracked_message_source!())
    }};
}

/// Constructs a [`Message`](crate::message::Message) using the canonical
/// wording for a known exit code.
///
/// The macro delegates to
/// [`Message::from_exit_code`](crate::message::Message::from_exit_code) and
/// attaches the caller's
/// source location via [`macro@crate::tracked_message_source`]. Returning an
/// [`Option`] mirrors the underlying helper:
/// upstream rsync only assigns stock diagnostics to a fixed set of exit codes. Callers can use
/// [`Option::unwrap_or_else`] to supply bespoke text when the exit code is not recognised.
///
/// Because the macro relies on [`macro@crate::tracked_message_source`], functions
/// annotated with `#[track_caller]` propagate their call-site into the rendered
/// diagnostic just like the other
/// `rsync_*` macros.
///
/// # Examples
///
/// Emit the canonical message for exit code 23 and assert that it carries the standard wording
/// and caller location:
///
/// ```
/// use rsync_core::{message::Role, rsync_exit_code};
///
/// fn render() -> rsync_core::message::Message {
///     rsync_exit_code!(23).expect("exit code 23 is defined").with_role(Role::Sender)
/// }
///
/// let message = render();
/// assert_eq!(message.code(), Some(23));
/// let rendered = message.to_string();
/// assert!(rendered.contains("rsync error: some files/attrs were not transferred"));
/// assert!(rendered.contains("(code 23)"));
/// assert!(message.source().is_some());
/// ```
#[macro_export]
macro_rules! rsync_exit_code {
    ($code:expr $(,)?) => {{
        match $crate::message::Message::from_exit_code($code) {
            Some(message) => Some(message.with_source($crate::tracked_message_source!())),
            None => None,
        }
    }};
}
