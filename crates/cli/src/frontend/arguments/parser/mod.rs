//! Translates `clap` matches into the strongly-typed [`ParsedArgs`] struct
//! consumed by the rest of the frontend.
//!
//! The [`parse_args`] entry point lives in [`entry`]; the focused helpers it
//! orchestrates are split across [`flags`] (tri-state flag pairs), [`values`]
//! (repeatable `OsString` joining), [`coerce`] (numeric/sized value
//! validation), and [`cow`] (copy-on-write policy resolution).

mod coerce;
mod cow;
mod entry;
mod flags;
mod values;

#[cfg(test)]
mod tests;

pub use coerce::ChecksumThreadsSetting;
pub use entry::parse_args;

use super::{
    BandwidthArgument, ParsedArgs, detect_program_name, env_iconv_default, env_max_alloc_default,
    env_protect_args_default,
};
