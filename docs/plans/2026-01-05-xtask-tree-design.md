# XTask Tree Visualization Design

## Overview

Add a `--tree` flag to all xtask commands that displays a colored, indented task hierarchy with names, descriptions, and estimated durations.

## Requirements

- Display task tree without executing
- Show: task name, description, estimated duration
- Colored output with Unicode box-drawing
- Indentation to show parent-child relationships
- Available on all xtask commands via global `--tree` flag

## Architecture

### Core Task Trait

```rust
// xtask/src/task/mod.rs

use std::time::Duration;

pub trait Task {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;

    fn explicit_duration(&self) -> Option<Duration> {
        None
    }

    fn estimated_duration(&self) -> Duration {
        self.explicit_duration().unwrap_or_else(|| {
            self.subtasks()
                .iter()
                .map(|t| t.estimated_duration())
                .sum()
        })
    }

    fn subtasks(&self) -> Vec<Box<dyn Task>> {
        Vec::new()
    }
}
```

### Tree Renderer

```rust
// xtask/src/task/renderer.rs

use anstyle::{AnsiColor, Style};
use std::io::{self, Write};

pub struct TreeRenderer<W: Write> {
    writer: W,
    use_color: bool,
}

impl<W: Write> TreeRenderer<W> {
    pub fn new(writer: W, use_color: bool) -> Self {
        Self { writer, use_color }
    }

    pub fn render(&mut self, task: &dyn Task) -> io::Result<()> {
        self.render_node(task, &[], true)
    }

    fn render_node(
        &mut self,
        task: &dyn Task,
        prefix: &[bool],
        is_last: bool,
    ) -> io::Result<()>;
}

fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    match secs {
        0 => String::from("<1s"),
        1..=59 => format!("~{}s", secs),
        60..=3599 => format!("~{}m {}s", secs / 60, secs % 60),
        _ => format!("~{}h {}m", secs / 3600, (secs % 3600) / 60),
    }
}
```

**Color scheme:**
- Cyan: task name
- Dim white: description
- Yellow: duration estimate

**Box characters:** `├`, `└`, `│`, `─`

### CLI Integration

```rust
// xtask/src/cli.rs

#[derive(Parser)]
pub struct Cli {
    #[arg(long, global = true)]
    pub tree: bool,

    #[command(subcommand)]
    pub command: Command,
}
```

```rust
// xtask/src/main.rs

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.tree {
        let task = cli.command.as_task();
        let use_color = std::io::stdout().is_terminal();
        let mut renderer = TreeRenderer::new(std::io::stdout(), use_color);
        renderer.render(task.as_ref())?;
        return Ok(());
    }

    cli.command.execute()
}
```

### Command Extension Trait

```rust
// xtask/src/commands/mod.rs

pub trait CommandExt {
    fn as_task(&self) -> Box<dyn Task>;
    fn execute(self) -> Result<()>;
}
```

### Concrete Task Example

```rust
// xtask/src/task/tasks/package.rs

pub struct PackageTask {
    pub build_deb: bool,
    pub build_rpm: bool,
    pub build_tarball: bool,
    pub deb_variant: Option<String>,
}

impl Task for PackageTask {
    fn name(&self) -> &'static str { "package" }
    fn description(&self) -> &'static str { "Build distribution packages" }
    fn estimated_duration(&self) -> Duration { Duration::from_secs(120) }

    fn subtasks(&self) -> Vec<Box<dyn Task>> {
        let mut tasks: Vec<Box<dyn Task>> = vec![
            Box::new(BuildBinariesTask),
        ];
        if self.build_deb {
            tasks.push(Box::new(BuildDebTask {
                variant: self.deb_variant.clone()
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
```

## Module Structure

```
xtask/src/
├── task/
│   ├── mod.rs              # Task trait, re-exports
│   ├── renderer.rs         # TreeRenderer implementation
│   └── tasks/
│       ├── mod.rs          # Re-exports
│       ├── package.rs      # PackageTask, BuildDebTask, etc.
│       ├── release.rs      # ReleaseTask and subtasks
│       ├── docs.rs         # DocsTask and subtasks
│       ├── test.rs         # TestTask
│       ├── preflight.rs    # PreflightTask and checks
│       └── common.rs       # Shared leaf tasks
├── commands/               # Existing (unchanged)
├── cli.rs                  # Add --tree flag
└── main.rs                 # Add tree rendering branch
```

## Example Output

```
package                      Build distribution packages           ~2m 0s
├── build-binaries           Compile workspace with cargo build    ~1m 0s
├── build-deb                Create Debian package (focal)           ~20s
│   └── rename-deb           Add variant suffix to filename           ~1s
└── build-tarball            Create compressed tarball               ~10s
─────────────────────────────────────────────────────────────────────────
Total: 4 tasks                                                    ~2m 31s
```

## Dependencies

- `anstyle` - Already available via clap (zero new dependencies)
- `std::io::IsTerminal` - Stable since Rust 1.70

## Design Principles

- **Single Responsibility**: Each task file handles one command
- **Open/Closed**: New commands add files without modifying existing ones
- **DRY**: Common leaf tasks shared in `common.rs`
- **Zero new dependencies**: Reuses `anstyle` from clap
