use std::io::Write;

use core::message::Message;
use logging_sink::MessageSink;

use crate::frontend::write_message;

/// Writes a message to stderr, falling back to a plain string on failure.
pub(super) fn emit_message_with_fallback<Err>(
    message: &Message,
    fallback: &str,
    stderr: &mut MessageSink<Err>,
) where
    Err: Write,
{
    if write_message(message, stderr).is_err() {
        let _ = writeln!(stderr.writer_mut(), "{fallback}");
    }
}

/// Emits an error message to stderr and returns the associated exit code.
pub(super) fn fail_with_message<Err>(message: Message, stderr: &mut MessageSink<Err>) -> i32
where
    Err: Write,
{
    let brand = stderr.brand();
    let fallback = message.clone().with_brand(brand).to_string();
    emit_message_with_fallback(&message, &fallback, stderr);
    message.code().unwrap_or(1)
}

#[cfg(any(
    not(all(any(unix, windows), feature = "acl")),
    not(all(unix, feature = "xattr"))
))]
/// Emits an error message with a caller-supplied fallback string and returns the exit code.
pub(super) fn fail_with_custom_fallback<Err>(
    message: Message,
    fallback: String,
    stderr: &mut MessageSink<Err>,
) -> i32
where
    Err: Write,
{
    emit_message_with_fallback(&message, &fallback, stderr);
    message.code().unwrap_or(1)
}
