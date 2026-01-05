//! Common reusable leaf tasks.

use crate::task::Task;
use std::time::Duration;

/// Compiles workspace binaries with cargo build.
#[allow(dead_code)]
pub struct CargoBuildTask {
    pub profile: Option<&'static str>,
    pub target: Option<&'static str>,
}

impl Default for CargoBuildTask {
    fn default() -> Self {
        Self {
            profile: Some("dist"),
            target: None,
        }
    }
}

impl Task for CargoBuildTask {
    fn name(&self) -> &'static str {
        "build-binaries"
    }

    fn description(&self) -> &'static str {
        "Compile workspace with cargo build"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }
}

/// Invokes an external cargo tool.
pub struct CargoToolTask {
    pub tool_name: &'static str,
    pub description: &'static str,
    pub duration_secs: u64,
}

impl Task for CargoToolTask {
    fn name(&self) -> &'static str {
        self.tool_name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(self.duration_secs))
    }
}

/// Runs cargo fmt check.
pub struct CargoFmtTask;

impl Task for CargoFmtTask {
    fn name(&self) -> &'static str {
        "cargo-fmt"
    }

    fn description(&self) -> &'static str {
        "Check code formatting"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(5))
    }
}

/// Runs cargo clippy.
pub struct CargoClippyTask;

impl Task for CargoClippyTask {
    fn name(&self) -> &'static str {
        "cargo-clippy"
    }

    fn description(&self) -> &'static str {
        "Run clippy lints"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(30))
    }
}

/// Runs cargo nextest or cargo test.
pub struct CargoTestTask {
    pub use_nextest: bool,
}

impl Default for CargoTestTask {
    fn default() -> Self {
        Self { use_nextest: true }
    }
}

impl Task for CargoTestTask {
    fn name(&self) -> &'static str {
        if self.use_nextest {
            "cargo-nextest"
        } else {
            "cargo-test"
        }
    }

    fn description(&self) -> &'static str {
        "Run test suite"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(120))
    }
}

/// Generates documentation with cargo doc.
pub struct CargoDocTask;

impl Task for CargoDocTask {
    fn name(&self) -> &'static str {
        "cargo-doc"
    }

    fn description(&self) -> &'static str {
        "Generate API documentation"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(45))
    }
}

/// Validates README content.
pub struct ValidateReadmeTask;

impl Task for ValidateReadmeTask {
    fn name(&self) -> &'static str {
        "validate-readme"
    }

    fn description(&self) -> &'static str {
        "Validate README structure and links"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(2))
    }
}

/// Validates CI workflow files.
pub struct ValidateCiTask;

impl Task for ValidateCiTask {
    fn name(&self) -> &'static str {
        "validate-ci"
    }

    fn description(&self) -> &'static str {
        "Validate CI workflow configuration"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(3))
    }
}

/// Validates branding consistency.
pub struct ValidateBrandingTask;

impl Task for ValidateBrandingTask {
    fn name(&self) -> &'static str {
        "validate-branding"
    }

    fn description(&self) -> &'static str {
        "Check branding consistency"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(1))
    }
}

/// Creates a file or artifact.
pub struct CreateFileTask {
    pub name: &'static str,
    pub description: &'static str,
    pub duration_secs: u64,
}

impl Task for CreateFileTask {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(self.duration_secs))
    }
}
