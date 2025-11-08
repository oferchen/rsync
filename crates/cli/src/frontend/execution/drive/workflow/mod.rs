#![deny(unsafe_code)]

mod fallback_plan;
mod operands;
mod preflight;
mod run;

pub(crate) use run::execute;

#[cfg(test)]
pub(crate) use operands::render_missing_operands_stdout;
