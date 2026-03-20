mod debug;
mod info;

#[cfg(test)]
mod tests;

pub(crate) use debug::{DEBUG_HELP_TEXT, parse_debug_flags};
pub(crate) use info::{INFO_HELP_TEXT, parse_info_flags};
