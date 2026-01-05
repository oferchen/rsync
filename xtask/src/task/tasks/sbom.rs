//! Task implementations for the sbom command.

use crate::task::Task;
use std::time::Duration;

/// Root task for sbom command.
pub struct SbomTask;

impl Task for SbomTask {
    fn name(&self) -> &'static str {
        "sbom"
    }

    fn description(&self) -> &'static str {
        "Generate CycloneDX SBOM"
    }

    fn subtasks(&self) -> Vec<Box<dyn Task>> {
        vec![
            Box::new(CollectDependenciesTask),
            Box::new(GenerateSbomTask),
            Box::new(WriteSbomTask),
        ]
    }
}

/// Collects workspace dependencies.
struct CollectDependenciesTask;

impl Task for CollectDependenciesTask {
    fn name(&self) -> &'static str {
        "collect-dependencies"
    }

    fn description(&self) -> &'static str {
        "Collect workspace dependencies"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(3))
    }
}

/// Generates SBOM document.
struct GenerateSbomTask;

impl Task for GenerateSbomTask {
    fn name(&self) -> &'static str {
        "generate-sbom"
    }

    fn description(&self) -> &'static str {
        "Generate SBOM document"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(2))
    }
}

/// Writes SBOM to file.
struct WriteSbomTask;

impl Task for WriteSbomTask {
    fn name(&self) -> &'static str {
        "write-sbom"
    }

    fn description(&self) -> &'static str {
        "Write SBOM to output file"
    }

    fn explicit_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(1))
    }
}
