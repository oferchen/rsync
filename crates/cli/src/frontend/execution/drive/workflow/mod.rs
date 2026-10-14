//! Client transfer workflow: operand checks, preflight validation and the run.

#![deny(unsafe_code)]

mod operands;
mod preflight;
mod run;

pub(crate) use run::execute;

#[cfg(test)]
pub(crate) use operands::render_missing_operands_stdout;
