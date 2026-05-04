use crate::cli::DocsArgs;

/// Options accepted by the `docs` command.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DocsOptions {
    /// Whether to open the generated documentation in a browser.
    pub open: bool,
    /// Whether to validate documentation snippets against workspace branding metadata.
    pub validate: bool,
}

impl From<DocsArgs> for DocsOptions {
    fn from(args: DocsArgs) -> Self {
        Self {
            open: args.open,
            validate: args.validate,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_args_default_options() {
        let args = DocsArgs {
            open: false,
            validate: false,
        };
        let options: DocsOptions = args.into();
        assert!(!options.open);
        assert!(!options.validate);
    }

    #[test]
    fn from_args_with_open_flag() {
        let args = DocsArgs {
            open: true,
            validate: false,
        };
        let options: DocsOptions = args.into();
        assert!(options.open);
        assert!(!options.validate);
    }

    #[test]
    fn from_args_with_validate_flag() {
        let args = DocsArgs {
            open: false,
            validate: true,
        };
        let options: DocsOptions = args.into();
        assert!(!options.open);
        assert!(options.validate);
    }
}
