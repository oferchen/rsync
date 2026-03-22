// Module access - handles client module requests, authentication, and transfer setup.
//
// upstream: clientserver.c - `rsync_module()` is the main entry point for
// processing a client's module request after the `@RSYNCD:` greeting exchange.
//
// Split into focused submodules:
// - `listing`: Module listing format and response (upstream: clientserver.c:1246-1254)
// - `authentication`: Challenge-response auth, secrets file verification (upstream: authenticate.c)
// - `client_args`: Client argument reading, server config building (upstream: io.c:1292, options.c:2737)
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
