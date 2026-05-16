// Module definition builder for rsyncd.conf module sections.
//
// Constructs validated `ModuleDefinition` instances from per-module directives.
// Each setter enforces duplicate detection so the same directive cannot appear
// twice within a single module section.

include!("module_definition/builder.rs");

include!("module_definition/setters.rs");

include!("module_definition/finish.rs");

// These tests use Unix-style paths like /data and /etc/secrets
#[cfg(all(test, unix))]
#[path = "module_definition/tests.rs"]
mod module_definition_builder_tests;
