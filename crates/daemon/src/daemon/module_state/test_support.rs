use std::cell::RefCell;
use std::ffi::OsString;
use std::path::PathBuf;

thread_local! {
    pub(in crate::daemon) static TEST_CONFIG_CANDIDATES: RefCell<Option<Vec<PathBuf>>> =
        const { RefCell::new(Some(Vec::new())) };
}

thread_local! {
    pub(crate) static TEST_SECRETS_CANDIDATES: RefCell<Option<Vec<PathBuf>>> =
        const { RefCell::new(None) };
}

thread_local! {
    pub(crate) static TEST_SECRETS_ENV: RefCell<Option<TestSecretsEnvOverride>> =
        const { RefCell::new(None) };
}

/// Override for secrets-related environment variables in tests.
#[derive(Clone, Debug, Default)]
pub(crate) struct TestSecretsEnvOverride {
    /// Override for the branded secrets env var (`OC_RSYNC_SECRETS`).
    pub(crate) branded: Option<OsString>,
    /// Override for the legacy secrets env var (`RSYNCD_SECRETS`).
    pub(crate) legacy: Option<OsString>,
}
