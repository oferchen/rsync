// Global-section directive parsing.
//
// Handles `key = value` directives that appear in the global section.

include!("global_directives/module_defaults.rs");

include!("global_directives/parse_state.rs");

include!("global_directives/dispatch.rs");
