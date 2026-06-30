// Global-section directive parsing.
//
// Handles `key = value` directives that appear before any `[module]` header
// (the global section), including the `include` directive that triggers
// recursive config file parsing and result merging.

include!("global_directives/module_defaults.rs");

include!("global_directives/parse_state.rs");

include!("global_directives/dispatch.rs");
