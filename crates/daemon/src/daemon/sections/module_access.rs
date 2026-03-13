// Module access - handles client module requests, authentication, and transfer setup.
//
// Split into focused submodules:
// - `listing`: Module listing format and response
// - `authentication`: Challenge-response auth, secrets file verification
// - `client_args`: Client argument reading, server config building
// - `helpers`: Logging, sanitization, bandwidth, filter rules
// - `request`: Request context, error handling, main entry point
// - `transfer`: Stream setup, handshake, transfer execution
// - `tests`: Comprehensive test coverage

include!("module_access/listing.rs");

include!("module_access/authentication.rs");

include!("module_access/client_args.rs");

include!("module_access/helpers.rs");

include!("module_access/request.rs");

include!("module_access/transfer.rs");

include!("module_access/tests.rs");
