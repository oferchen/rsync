//! Task decomposition and tree visualization for xtask commands.

mod renderer;
pub mod tasks;

pub use renderer::TreeRenderer;

use std::time::Duration;

/// A decomposable task with metadata for tree visualization.
pub trait Task {
    /// Short identifier (e.g., "build-binaries").
    fn name(&self) -> &'static str;

    /// Human-readable description.
    fn description(&self) -> &'static str;

    /// Explicit duration override. Returns `None` to use sum of subtasks.
    fn explicit_duration(&self) -> Option<Duration> {
        None
    }

    /// Estimated duration for display.
    ///
    /// Returns explicit duration if set, otherwise sums subtask durations.
    fn estimated_duration(&self) -> Duration {
        self.explicit_duration().unwrap_or_else(|| {
            let subtasks = self.subtasks();
            if subtasks.is_empty() {
                Duration::ZERO
            } else {
                subtasks.iter().map(|t| t.estimated_duration()).sum()
            }
        })
    }

    /// Child tasks executed by this task.
    fn subtasks(&self) -> Vec<Box<dyn Task>> {
        Vec::new()
    }
}

/// Counts total tasks in a tree (including root).
pub fn count_tasks(task: &dyn Task) -> usize {
    1 + task.subtasks().iter().map(|t| count_tasks(t.as_ref())).sum::<usize>()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct LeafTask;
    impl Task for LeafTask {
        fn name(&self) -> &'static str { "leaf" }
        fn description(&self) -> &'static str { "A leaf task" }
        fn explicit_duration(&self) -> Option<Duration> {
            Some(Duration::from_secs(10))
        }
    }

    struct ParentTask;
    impl Task for ParentTask {
        fn name(&self) -> &'static str { "parent" }
        fn description(&self) -> &'static str { "A parent task" }
        fn subtasks(&self) -> Vec<Box<dyn Task>> {
            vec![Box::new(LeafTask), Box::new(LeafTask)]
        }
    }

    #[test]
    fn leaf_task_uses_explicit_duration() {
        let task = LeafTask;
        assert_eq!(task.estimated_duration(), Duration::from_secs(10));
    }

    #[test]
    fn parent_task_sums_subtask_durations() {
        let task = ParentTask;
        assert_eq!(task.estimated_duration(), Duration::from_secs(20));
    }

    #[test]
    fn count_tasks_counts_all_nodes() {
        let task = ParentTask;
        assert_eq!(count_tasks(&task), 3); // parent + 2 leaves
    }
}
