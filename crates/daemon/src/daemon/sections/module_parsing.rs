// Daemon module and argument parsing.
//
// Parses module definitions (CLI `NAME=PATH` specs and inline options),
// evaluates `refuse options` rules against client-requested options, parses the
// daemon's scalar CLI argument values, and constructs the configuration and
// network `DaemonError` instances used across daemon parsing.

include!("module_parsing/refuse_options.rs");

include!("module_parsing/module_spec.rs");

include!("module_parsing/errors.rs");

#[cfg(test)]
mod tests;
