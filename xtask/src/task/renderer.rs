//! Tree rendering with Unicode box-drawing and ANSI colors.

use super::{Task, count_tasks};
use anstyle::{AnsiColor, Color, Style};
use std::io::{self, Write};
use std::time::Duration;

/// Box-drawing characters for tree visualization.
mod box_chars {
    pub const BRANCH: &str = "├── ";
    pub const LAST: &str = "└── ";
    pub const VERTICAL: &str = "│   ";
    pub const SPACE: &str = "    ";
    pub const HORIZONTAL: char = '─';
}

/// Renders a task tree to a writer with optional colors.
pub struct TreeRenderer<W: Write> {
    writer: W,
    use_color: bool,
    name_style: Style,
    desc_style: Style,
    duration_style: Style,
    dim_style: Style,
}

impl<W: Write> TreeRenderer<W> {
    /// Creates a new renderer.
    pub fn new(writer: W, use_color: bool) -> Self {
        let name_style = if use_color {
            Style::new().fg_color(Some(Color::Ansi(AnsiColor::Cyan)))
        } else {
            Style::new()
        };

        let desc_style = if use_color {
            Style::new().dimmed()
        } else {
            Style::new()
        };

        let duration_style = if use_color {
            Style::new().fg_color(Some(Color::Ansi(AnsiColor::Yellow)))
        } else {
            Style::new()
        };

        let dim_style = if use_color {
            Style::new().dimmed()
        } else {
            Style::new()
        };

        Self {
            writer,
            use_color,
            name_style,
            desc_style,
            duration_style,
            dim_style,
        }
    }

    /// Renders the complete task tree with summary.
    pub fn render(&mut self, task: &dyn Task) -> io::Result<()> {
        let metrics = TreeMetrics::compute(task);
        self.render_node(task, &[], true, &metrics)?;
        self.write_summary(task, &metrics)?;
        Ok(())
    }

    fn render_node(
        &mut self,
        task: &dyn Task,
        ancestors: &[bool], // true = ancestor needs continuation line
        is_last: bool,
        metrics: &TreeMetrics,
    ) -> io::Result<()> {
        self.write_prefix(ancestors, is_last)?;
        self.write_task_line(task, metrics)?;

        let subtasks = task.subtasks();
        let child_count = subtasks.len();

        for (i, subtask) in subtasks.into_iter().enumerate() {
            let is_last_child = i + 1 == child_count;
            let mut new_ancestors = ancestors.to_vec();
            new_ancestors.push(!is_last);
            self.render_node(subtask.as_ref(), &new_ancestors, is_last_child, metrics)?;
        }

        Ok(())
    }

    fn write_prefix(&mut self, ancestors: &[bool], is_last: bool) -> io::Result<()> {
        for &needs_line in ancestors {
            let segment = if needs_line {
                box_chars::VERTICAL
            } else {
                box_chars::SPACE
            };
            write!(self.writer, "{}", self.style_dim(segment))?;
        }

        if !ancestors.is_empty() || !is_last {
            let connector = if is_last {
                box_chars::LAST
            } else {
                box_chars::BRANCH
            };
            write!(self.writer, "{}", self.style_dim(connector))?;
        }

        Ok(())
    }

    fn write_task_line(&mut self, task: &dyn Task, metrics: &TreeMetrics) -> io::Result<()> {
        let name = task.name();
        let desc = task.description();
        let duration = format_duration(task.estimated_duration());

        let name_padding = metrics.max_name_width.saturating_sub(name.len());
        let desc_padding = metrics.max_desc_width.saturating_sub(desc.len());

        writeln!(
            self.writer,
            "{}{:name_pad$}  {}{:desc_pad$}  {}",
            self.style_name(name),
            "",
            self.style_desc(desc),
            "",
            self.style_duration(&duration),
            name_pad = name_padding,
            desc_pad = desc_padding,
        )
    }

    fn write_summary(&mut self, task: &dyn Task, metrics: &TreeMetrics) -> io::Result<()> {
        let total_width = metrics.max_name_width + metrics.max_desc_width + 20;
        let separator: String = std::iter::repeat_n(box_chars::HORIZONTAL, total_width).collect();

        let task_count = count_tasks(task);
        let total_duration = format_duration(task.estimated_duration());
        let task_label = if task_count == 1 { "task" } else { "tasks" };

        writeln!(self.writer, "{}", self.style_dim(&separator))?;
        writeln!(
            self.writer,
            "Total: {task_count} {task_label}  {:>width$}",
            self.style_duration(&total_duration),
            width = total_width - 10 - task_count.to_string().len() - task_label.len(),
        )
    }

    fn style_name(&self, s: &str) -> String {
        if self.use_color {
            format!("{}{}{}", self.name_style.render(), s, self.name_style.render_reset())
        } else {
            s.to_string()
        }
    }

    fn style_desc(&self, s: &str) -> String {
        if self.use_color {
            format!("{}{}{}", self.desc_style.render(), s, self.desc_style.render_reset())
        } else {
            s.to_string()
        }
    }

    fn style_duration(&self, s: &str) -> String {
        if self.use_color {
            format!("{}{}{}", self.duration_style.render(), s, self.duration_style.render_reset())
        } else {
            s.to_string()
        }
    }

    fn style_dim(&self, s: &str) -> String {
        if self.use_color {
            format!("{}{}{}", self.dim_style.render(), s, self.dim_style.render_reset())
        } else {
            s.to_string()
        }
    }
}

/// Precomputed metrics for column alignment.
struct TreeMetrics {
    max_name_width: usize,
    max_desc_width: usize,
}

impl TreeMetrics {
    fn compute(task: &dyn Task) -> Self {
        let mut metrics = Self {
            max_name_width: 0,
            max_desc_width: 0,
        };
        metrics.visit(task, 0);
        metrics
    }

    fn visit(&mut self, task: &dyn Task, depth: usize) {
        let indent = depth * 4; // box char width
        let effective_name_width = task.name().len() + indent;

        self.max_name_width = self.max_name_width.max(effective_name_width);
        self.max_desc_width = self.max_desc_width.max(task.description().len());

        for subtask in task.subtasks() {
            self.visit(subtask.as_ref(), depth + 1);
        }
    }
}

/// Formats a duration for display.
pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    match secs {
        0 => String::from("<1s"),
        1..=59 => format!("~{secs}s"),
        60..=3599 => {
            let mins = secs / 60;
            let remainder = secs % 60;
            if remainder == 0 {
                format!("~{mins}m")
            } else {
                format!("~{mins}m {remainder}s")
            }
        }
        _ => {
            let hours = secs / 3600;
            let mins = (secs % 3600) / 60;
            if mins == 0 {
                format!("~{hours}h")
            } else {
                format!("~{hours}h {mins}m")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_duration_subsecond() {
        assert_eq!(format_duration(Duration::from_millis(500)), "<1s");
        assert_eq!(format_duration(Duration::ZERO), "<1s");
    }

    #[test]
    fn format_duration_seconds() {
        assert_eq!(format_duration(Duration::from_secs(1)), "~1s");
        assert_eq!(format_duration(Duration::from_secs(30)), "~30s");
        assert_eq!(format_duration(Duration::from_secs(59)), "~59s");
    }

    #[test]
    fn format_duration_minutes() {
        assert_eq!(format_duration(Duration::from_secs(60)), "~1m");
        assert_eq!(format_duration(Duration::from_secs(90)), "~1m 30s");
        assert_eq!(format_duration(Duration::from_secs(120)), "~2m");
        assert_eq!(format_duration(Duration::from_secs(3599)), "~59m 59s");
    }

    #[test]
    fn format_duration_hours() {
        assert_eq!(format_duration(Duration::from_secs(3600)), "~1h");
        assert_eq!(format_duration(Duration::from_secs(3660)), "~1h 1m");
        assert_eq!(format_duration(Duration::from_secs(7200)), "~2h");
    }

    struct TestTask {
        name: &'static str,
        desc: &'static str,
        duration: Duration,
        children: Vec<Box<dyn Task>>,
    }

    impl Task for TestTask {
        fn name(&self) -> &'static str { self.name }
        fn description(&self) -> &'static str { self.desc }
        fn explicit_duration(&self) -> Option<Duration> {
            if self.children.is_empty() {
                Some(self.duration)
            } else {
                None
            }
        }
        fn subtasks(&self) -> Vec<Box<dyn Task>> {
            self.children.iter().map(|_| {
                Box::new(TestTask {
                    name: "child",
                    desc: "Child task",
                    duration: Duration::from_secs(5),
                    children: vec![],
                }) as Box<dyn Task>
            }).collect()
        }
    }

    #[test]
    fn renderer_produces_output() {
        let task = TestTask {
            name: "root",
            desc: "Root task",
            duration: Duration::from_secs(10),
            children: vec![],
        };

        let mut output = Vec::new();
        let mut renderer = TreeRenderer::new(&mut output, false);
        renderer.render(&task).expect("render succeeds");

        let result = String::from_utf8(output).expect("valid utf8");
        assert!(result.contains("root"));
        assert!(result.contains("Root task"));
        assert!(result.contains("Total:"));
    }
}
