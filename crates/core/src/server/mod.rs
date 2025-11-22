//! Server orchestration entry points mirroring the client facade.

mod config;
mod role;

pub use self::config::ServerConfig;
pub use self::role::ServerRole;

#[cfg(test)]
mod tests;
