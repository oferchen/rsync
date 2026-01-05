//! Task implementations for the preflight command.

use crate::task::Task;

use super::common::{
    CargoClippyTask, CargoFmtTask, CargoTestTask, ValidateBrandingTask, ValidateCiTask,
    ValidateReadmeTask,
};
use super::release::{EnforceLimitsTask, NoBinariesTask, NoPlaceholdersTask};

/// Root task for preflight command.
pub struct PreflightTask;

impl Task for PreflightTask {
    fn name(&self) -> &'static str {
        "preflight"
    }

    fn description(&self) -> &'static str {
        "Run packaging preflight checks"
    }

    fn subtasks(&self) -> Vec<Box<dyn Task>> {
        vec![
            Box::new(CargoFmtTask),
            Box::new(CargoClippyTask),
            Box::new(CargoTestTask::default()),
            Box::new(EnforceLimitsTask),
            Box::new(NoPlaceholdersTask),
            Box::new(NoBinariesTask),
            Box::new(ValidateReadmeTask),
            Box::new(ValidateCiTask),
            Box::new(ValidateBrandingTask),
        ]
    }
}
