// Daemon configuration file parsing.
//
// Parses `rsyncd.conf` files into structured module definitions and global
// settings. Supports the full upstream rsyncd.conf syntax including module
// sections, global directives, and `include` directives with recursive
// detection.

include!("config_parsing/types.rs");

include!("config_parsing/global_directives.rs");

include!("config_parsing/include_merge.rs");

include!("config_parsing/module_directives.rs");

include!("config_parsing/parser.rs");

include!("config_parsing/tests.rs");
