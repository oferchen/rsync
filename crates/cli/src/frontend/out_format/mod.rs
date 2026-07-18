#![deny(unsafe_code)]

//! Parsing and rendering of `--out-format` / `--log-format` specifications.

mod parser;
mod render;
mod tokens;

pub(crate) use parser::{log_format_has, parse_out_format};
pub(crate) use render::emit_out_format;
pub(crate) use tokens::{OutFormat, OutFormatContext};
