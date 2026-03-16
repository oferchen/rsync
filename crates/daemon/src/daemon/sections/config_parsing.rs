// Daemon configuration file parsing.
//
// Parses `rsyncd.conf` files into structured module definitions and global
// settings. Supports the full upstream rsyncd.conf syntax including module
// sections, global directives, include directives with recursive detection,
// and per-module settings.
//
// Split into focused submodules:
// - `types`: Core data structures (ConfigDirectiveOrigin, ParsedConfigModules)
// - `module_directives`: Per-module `[section]` directive dispatch
// - `global_directives`: Global-section directive dispatch
// - `include_merge`: Recursive include handling and result merging
// - `parser`: Main parse loop, entry point, and path resolution
// - `tests`: Comprehensive test coverage

include!("config_parsing/types.rs");

include!("config_parsing/global_directives.rs");

include!("config_parsing/include_merge.rs");

include!("config_parsing/module_directives.rs");

include!("config_parsing/parser.rs");

include!("config_parsing/tests.rs");
