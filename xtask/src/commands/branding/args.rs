use crate::cli::BrandingArgs;

/// Output format supported by the `branding` command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BrandingOutputFormat {
    /// Human-readable text report.
    #[default]
    Text,
    /// Structured JSON report suitable for automation.
    Json,
}

/// Options accepted by the `branding` command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BrandingOptions {
    /// Desired output format.
    pub format: BrandingOutputFormat,
}

impl From<BrandingArgs> for BrandingOptions {
    fn from(args: BrandingArgs) -> Self {
        Self {
            format: if args.json {
                BrandingOutputFormat::Json
            } else {
                BrandingOutputFormat::Text
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_args_default_uses_text_format() {
        let args = BrandingArgs { json: false };
        let options: BrandingOptions = args.into();
        assert_eq!(options.format, BrandingOutputFormat::Text);
    }

    #[test]
    fn from_args_json_flag_uses_json_format() {
        let args = BrandingArgs { json: true };
        let options: BrandingOptions = args.into();
        assert_eq!(options.format, BrandingOutputFormat::Json);
    }
}
