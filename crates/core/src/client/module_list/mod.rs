mod address;
mod auth;
mod connect;
mod errors;
mod options;
mod program;
mod proxy;
mod request;
mod response;
mod util;

pub use address::DaemonAddress;
pub use connect::{
    run_module_list, run_module_list_with_options, run_module_list_with_password,
    run_module_list_with_password_and_options,
};
pub use options::ModuleListOptions;
pub use request::ModuleListRequest;
pub use response::{ModuleList, ModuleListEntry};

#[cfg(test)]
pub(crate) use {
    auth::{
        DaemonAuthContext, SensitiveBytes, compute_daemon_auth_response, set_test_daemon_password,
    },
    connect::{
        connect_direct, connect_via_proxy, establish_proxy_tunnel, resolve_connect_timeout,
        resolve_daemon_addresses,
    },
    errors::map_daemon_handshake_error,
    program::ConnectProgramConfig,
    proxy::{ProxyConfig, parse_proxy_spec},
};
