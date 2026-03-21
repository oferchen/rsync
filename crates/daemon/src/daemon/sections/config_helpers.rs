// Daemon configuration helper functions.
//
// Provides parsing utilities for config directive values (booleans, numeric
// identifiers, timeouts, user lists, option lists), secrets file validation
// with cross-platform permission checks, and host pattern matching for
// allow/deny access control lists.
//
// Split into focused submodules:
// - `value_parsing`: Config value parsers (booleans, numbers, lists)
// - `secrets_validation`: Secrets file path and permission validation
// - `host_pattern`: Host pattern types and matching (IP CIDR, hostname, wildcard)
// - `tests`: Comprehensive test coverage

include!("config_helpers/value_parsing.rs");

include!("config_helpers/secrets_validation.rs");

include!("config_helpers/host_pattern.rs");

include!("config_helpers/tests.rs");
