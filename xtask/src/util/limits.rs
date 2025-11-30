use crate::error::{TaskError, TaskResult};
use std::env;

/// Reads an environment variable that stores a positive integer, returning
/// `Ok(None)` when the variable is not set.
pub fn read_limit_env_var(name: &str) -> TaskResult<Option<usize>> {
    match env::var(name) {
        Ok(value) => {
            if value.is_empty() {
                return Err(TaskError::Validation(format!(
                    "{name} must be a positive integer, found an empty value"
                )));
            }

            let parsed = parse_positive_usize_from_env(name, &value)?;
            Ok(Some(parsed))
        }
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(TaskError::Validation(format!(
            "{name} must contain a UTF-8 encoded positive integer"
        ))),
    }
}

fn parse_positive_usize_from_env(name: &str, value: &str) -> TaskResult<usize> {
    let parsed = value.parse::<usize>().map_err(|_| {
        TaskError::Validation(format!(
            "{name} must be a positive integer, found '{value}'"
        ))
    })?;

    if parsed == 0 {
        return Err(TaskError::Validation(format!(
            "{name} must be greater than zero, found '{value}'"
        )));
    }

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::{parse_positive_usize_from_env, read_limit_env_var};
    use crate::error::TaskError;
    use crate::util::test_env::EnvGuard;

    #[test]
    fn read_limit_env_var_parses_positive_values() {
        let mut guard = EnvGuard::new();
        guard.set("TEST_LIMIT", "42");
        assert_eq!(
            read_limit_env_var("TEST_LIMIT").expect("read succeeds"),
            Some(42)
        );
    }

    #[test]
    fn read_limit_env_var_handles_missing_and_invalid_values() {
        {
            let mut guard = EnvGuard::new();
            guard.remove("MISSING_LIMIT");
            assert!(
                read_limit_env_var("MISSING_LIMIT")
                    .expect("missing is ok")
                    .is_none()
            );
        }

        {
            let mut zero = EnvGuard::new();
            zero.set("ZERO_LIMIT", "0");
            let zero_err = read_limit_env_var("ZERO_LIMIT").unwrap_err();
            assert!(matches!(
                zero_err,
                TaskError::Validation(message) if message.contains("ZERO_LIMIT")
            ));
        }

        let mut invalid = EnvGuard::new();
        invalid.set("BAD_LIMIT", "not-a-number");
        let invalid_err = read_limit_env_var("BAD_LIMIT").unwrap_err();
        assert!(matches!(
            invalid_err,
            TaskError::Validation(message) if message.contains("BAD_LIMIT")
        ));
    }

    #[test]
    fn parse_positive_usize_from_env_rejects_zero_and_negative() {
        let err = parse_positive_usize_from_env("VALUE", "0").unwrap_err();
        assert!(matches!(err, TaskError::Validation(message) if message.contains("VALUE")));

        let err = parse_positive_usize_from_env("VALUE", "-1").unwrap_err();
        assert!(matches!(err, TaskError::Validation(message) if message.contains("VALUE")));

        assert_eq!(
            parse_positive_usize_from_env("VALUE", "7").expect("parse succeeds"),
            7
        );
    }
}
