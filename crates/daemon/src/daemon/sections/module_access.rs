// Module access - client module requests, authentication, and transfer setup.
//
// upstream: clientserver.c - `rsync_module()` processes a client's module
// request after the `@RSYNCD:` greeting exchange.

include!("module_access/listing.rs");

include!("module_access/authentication.rs");

include!("module_access/client_args.rs");

include!("module_access/helpers.rs");

include!("module_access/request.rs");

include!("module_access/transfer.rs");

include!("module_access/tests.rs");
