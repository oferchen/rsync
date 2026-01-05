//! Task implementations for the docs command.

use crate::task::Task;
use std::time::Duration;

use super::common::{CargoDocTask, ValidateBrandingTask};

/// Root task for docs command.
pub struct DocsTask {
    pub open: bool,
    pub validate: bool,
}

impl Task for DocsTask {
    fn name(&self) -> &'static str {
        "docs"
    }

    fn description(&self) -> &'static str {
        "Build API documentation"
    }

    fn subtasks(&self) -> Vec<Box<dyn Task>> {
        let mut tasks: Vec<Box<dyn Task>> = vec![Box::new(CargoDocTask)];

        if self.validate {
            tasks.push(Box::new(ValidateBrandingTask));
        }

        if self.open {
            tasks.push(Box::new(OpenDocsTask));
        }

        tasks
    }
}

/// Opens generated documentation in browser.
struct OpenDocsTask;

impl Task for OpenDocsTask {
    fn name(&self) -> &'static str {
        "open-docs"
    }

    fn description(&self) -> &'static str {
        "Open documentation in browser"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(1))
    }
}

/// Root task for doc-package command.
pub struct DocPackageTask {
    pub open: bool,
}

impl Task for DocPackageTask {
    fn name(&self) -> &'static str {
        "doc-package"
    }

    fn description(&self) -> &'static str {
        "Package documentation for distribution"
    }

    fn subtasks(&self) -> Vec<Box<dyn Task>> {
        let mut tasks: Vec<Box<dyn Task>> = vec![
            Box::new(CargoDocTask),
            Box::new(CreateDocTarballTask),
        ];

        if self.open {
            tasks.push(Box::new(OpenDocsTask));
        }

        tasks
    }
}

/// Creates documentation tarball.
struct CreateDocTarballTask;

impl Task for CreateDocTarballTask {
    fn name(&self) -> &'static str {
        "create-doc-tarball"
    }

    fn description(&self) -> &'static str {
        "Create documentation tarball"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(5))
    }
}
