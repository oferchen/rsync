//! Task implementations for the release command.

use crate::task::Task;
use std::time::Duration;

use super::common::{
    CargoClippyTask, CargoDocTask, CargoFmtTask, CargoTestTask, ValidateBrandingTask,
    ValidateCiTask, ValidateReadmeTask,
};
use super::package::PackageTask;

/// Root task for release command.
pub struct ReleaseTask {
    pub skip_docs: bool,
    pub skip_hygiene: bool,
    pub skip_placeholder_scan: bool,
    pub skip_binary_scan: bool,
    pub skip_packages: bool,
    pub skip_upload: bool,
}

impl Task for ReleaseTask {
    fn name(&self) -> &'static str {
        "release"
    }

    fn description(&self) -> &'static str {
        "Run release-readiness checks"
    }

    fn subtasks(&self) -> Vec<Box<dyn Task>> {
        let mut tasks: Vec<Box<dyn Task>> = Vec::new();

        // Code quality checks
        tasks.push(Box::new(CargoFmtTask));
        tasks.push(Box::new(CargoClippyTask));

        // Tests
        tasks.push(Box::new(CargoTestTask::default()));

        // Documentation
        if !self.skip_docs {
            tasks.push(Box::new(CargoDocTask));
        }

        // Hygiene checks
        if !self.skip_hygiene {
            tasks.push(Box::new(EnforceLimitsTask));
        }

        // Placeholder scan
        if !self.skip_placeholder_scan {
            tasks.push(Box::new(NoPlaceholdersTask));
        }

        // Binary scan
        if !self.skip_binary_scan {
            tasks.push(Box::new(NoBinariesTask));
        }

        // Validation tasks
        tasks.push(Box::new(ValidateReadmeTask));
        tasks.push(Box::new(ValidateCiTask));
        tasks.push(Box::new(ValidateBrandingTask));

        // Packaging
        if !self.skip_packages {
            tasks.push(Box::new(PackageTask {
                build_deb: true,
                build_rpm: true,
                build_tarball: true,
                deb_variant: None,
            }));
        }

        // Upload
        if !self.skip_upload && !self.skip_packages {
            tasks.push(Box::new(UploadReleaseTask));
        }

        tasks
    }
}

/// Enforces source line limits.
pub struct EnforceLimitsTask;

impl Task for EnforceLimitsTask {
    fn name(&self) -> &'static str {
        "enforce-limits"
    }

    fn description(&self) -> &'static str {
        "Check source line limits"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(5))
    }
}

/// Scans for placeholder code.
pub struct NoPlaceholdersTask;

impl Task for NoPlaceholdersTask {
    fn name(&self) -> &'static str {
        "no-placeholders"
    }

    fn description(&self) -> &'static str {
        "Scan for placeholder code"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(3))
    }
}

/// Scans for binary files in git.
pub struct NoBinariesTask;

impl Task for NoBinariesTask {
    fn name(&self) -> &'static str {
        "no-binaries"
    }

    fn description(&self) -> &'static str {
        "Scan for binary files in git"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(2))
    }
}

/// Uploads release artifacts.
struct UploadReleaseTask;

impl Task for UploadReleaseTask {
    fn name(&self) -> &'static str {
        "upload-release"
    }

    fn description(&self) -> &'static str {
        "Upload artifacts to GitHub"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(30))
    }
}
