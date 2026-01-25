use std::io::Write;

use core::message::Message;
use logging_sink::MessageSink;

use crate::frontend::write_message;

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

pub(super) fn fail_with_message<Err>(message: Message, stderr: &mut MessageSink<Err>) -> i32
where
    Err: Write,
{
    let brand = stderr.brand();
    let fallback = message.clone().with_brand(brand).to_string();
    emit_message_with_fallback(&message, &fallback, stderr);
    message.code().unwrap_or(1)
}

#[cfg(any(not(all(unix, feature = "acl")), not(all(unix, feature = "xattr"))))]
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
