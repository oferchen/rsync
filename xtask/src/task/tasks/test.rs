//! Task implementations for the test command.

use crate::task::Task;

use super::common::CargoTestTask;

/// Root task for test command.
pub struct TestTask {
    pub use_nextest: bool,
}

impl Task for TestTask {
    fn name(&self) -> &'static str {
        "test"
    }

    fn description(&self) -> &'static str {
        "Run workspace test suite"
    }

    fn subtasks(&self) -> Vec<Box<dyn Task>> {
        vec![Box::new(CargoTestTask {
            use_nextest: self.use_nextest,
        })]
    }
}
