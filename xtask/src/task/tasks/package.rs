//! Task implementations for the package command.

use crate::task::Task;
use std::time::Duration;

use super::common::CargoBuildTask;

/// Root task for package command.
pub struct PackageTask {
    pub build_deb: bool,
    pub build_rpm: bool,
    pub build_tarball: bool,
    pub deb_variant: Option<String>,
}

impl Task for PackageTask {
    fn name(&self) -> &'static str {
        "package"
    }

    fn description(&self) -> &'static str {
        "Build distribution packages"
    }

    fn subtasks(&self) -> Vec<Box<dyn Task>> {
        let mut tasks: Vec<Box<dyn Task>> = Vec::new();

        // Always build binaries first if any package type is requested
        if self.build_deb || self.build_rpm || self.build_tarball {
            tasks.push(Box::new(CargoBuildTask::default()));
        }

        if self.build_deb {
            tasks.push(Box::new(BuildDebTask {
                variant: self.deb_variant.clone(),
            }));
        }

        if self.build_rpm {
            tasks.push(Box::new(BuildRpmTask));
        }

        if self.build_tarball {
            tasks.push(Box::new(BuildTarballTask));
        }

        tasks
    }
}

/// Builds Debian package with cargo-deb.
pub struct BuildDebTask {
    pub variant: Option<String>,
}

impl Task for BuildDebTask {
    fn name(&self) -> &'static str {
        "build-deb"
    }

    fn description(&self) -> &'static str {
        "Create Debian package with cargo-deb"
    }

    fn subtasks(&self) -> Vec<Box<dyn Task>> {
        let mut tasks: Vec<Box<dyn Task>> = vec![Box::new(InvokeCargoDebTask)];

        if self.variant.is_some() {
            tasks.push(Box::new(RenameDebTask));
        }

        tasks
    }
}

/// Invokes cargo deb tool.
struct InvokeCargoDebTask;

impl Task for InvokeCargoDebTask {
    fn name(&self) -> &'static str {
        "cargo-deb"
    }

    fn description(&self) -> &'static str {
        "Run cargo deb"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(15))
    }
}

/// Renames deb file with variant suffix.
struct RenameDebTask;

impl Task for RenameDebTask {
    fn name(&self) -> &'static str {
        "rename-deb"
    }

    fn description(&self) -> &'static str {
        "Add variant suffix to filename"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(1))
    }
}

/// Builds RPM package with cargo-rpm.
pub struct BuildRpmTask;

impl Task for BuildRpmTask {
    fn name(&self) -> &'static str {
        "build-rpm"
    }

    fn description(&self) -> &'static str {
        "Create RPM package with cargo-rpm"
    }

    fn subtasks(&self) -> Vec<Box<dyn Task>> {
        vec![Box::new(CheckRpmbuildTask), Box::new(InvokeCargoRpmTask)]
    }
}

/// Checks rpmbuild availability.
struct CheckRpmbuildTask;

impl Task for CheckRpmbuildTask {
    fn name(&self) -> &'static str {
        "check-rpmbuild"
    }

    fn description(&self) -> &'static str {
        "Verify rpmbuild is available"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(1))
    }
}

/// Invokes cargo rpm build.
struct InvokeCargoRpmTask;

impl Task for InvokeCargoRpmTask {
    fn name(&self) -> &'static str {
        "cargo-rpm"
    }

    fn description(&self) -> &'static str {
        "Run cargo rpm build"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(20))
    }
}

/// Builds tarball distribution.
pub struct BuildTarballTask;

impl Task for BuildTarballTask {
    fn name(&self) -> &'static str {
        "build-tarball"
    }

    fn description(&self) -> &'static str {
        "Create compressed tarball"
    }

    fn subtasks(&self) -> Vec<Box<dyn Task>> {
        vec![
            Box::new(CreateTarballDirTask),
            Box::new(CopyBinariesTask),
            Box::new(CompressTarballTask),
        ]
    }
}

/// Creates tarball staging directory.
struct CreateTarballDirTask;

impl Task for CreateTarballDirTask {
    fn name(&self) -> &'static str {
        "create-tarball-dir"
    }

    fn description(&self) -> &'static str {
        "Create staging directory"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(1))
    }
}

/// Copies binaries to tarball staging.
struct CopyBinariesTask;

impl Task for CopyBinariesTask {
    fn name(&self) -> &'static str {
        "copy-binaries"
    }

    fn description(&self) -> &'static str {
        "Copy binaries to staging"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(2))
    }
}

/// Compresses tarball.
struct CompressTarballTask;

impl Task for CompressTarballTask {
    fn name(&self) -> &'static str {
        "compress-tarball"
    }

    fn description(&self) -> &'static str {
        "Compress with gzip"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(5))
    }
}
