fn take_option_value<'a, I>(
    argument: &'a OsString,
    iter: &mut I,
    option: &str,
) -> Result<Option<OsString>, DaemonError>
where
    I: Iterator<Item = &'a OsString>,
{
    if argument == option {
        let value = iter
            .next()
            .cloned()
            .ok_or_else(|| missing_argument_value(option))?;
        return Ok(Some(value));
    }

    let text = argument.to_string_lossy();
    if let Some(rest) = text.strip_prefix(option)
        && let Some(value) = rest.strip_prefix('=') {
            return Ok(Some(OsString::from(value)));
        }

    Ok(None)
}

fn config_argument_present(arguments: &[OsString]) -> bool {
    for argument in arguments {
        if argument == "--config" {
            return true;
        }

        let text = argument.to_string_lossy();
        if let Some(rest) = text.strip_prefix("--config")
            && rest.starts_with('=') {
                return true;
            }
    }

    false
}

fn first_existing_path<I, P>(paths: I) -> Option<OsString>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    for candidate in paths {
        let candidate = candidate.as_ref();
        if candidate.is_file() {
            return Some(candidate.as_os_str().to_os_string());
        }
    }

    None
}

pub(crate) fn first_existing_config_path<I, P>(paths: I) -> Option<OsString>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    first_existing_path(paths)
}

fn environment_config_override() -> Option<OsString> {
    environment_path_override(BRANDED_CONFIG_ENV)
        .or_else(|| environment_path_override(LEGACY_CONFIG_ENV))
}

fn environment_secrets_override() -> Option<(OsString, &'static str)> {
    #[cfg(test)]
    if let Some(env) = TEST_SECRETS_ENV.with(|cell| cell.borrow().clone()) {
        if let Some(path) = env.branded.clone() {
            return Some((path, BRANDED_SECRETS_ENV));
        }

        if let Some(path) = env.legacy.clone() {
            return Some((path, LEGACY_SECRETS_ENV));
        }
    }

    if let Some(path) = environment_path_override(BRANDED_SECRETS_ENV) {
        return Some((path, BRANDED_SECRETS_ENV));
    }

    environment_path_override(LEGACY_SECRETS_ENV).map(|path| (path, LEGACY_SECRETS_ENV))
}

fn environment_path_override(name: &'static str) -> Option<OsString> {
    let value = env::var_os(name)?;
    if value.is_empty() { None } else { Some(value) }
}

fn default_config_path_if_present(brand: Brand) -> Option<OsString> {
    #[cfg(test)]
    if let Some(paths) = TEST_CONFIG_CANDIDATES.with(|cell| cell.borrow().clone()) {
        return first_existing_path(paths.iter().map(PathBuf::as_path));
    }

    first_existing_config_path(brand.config_path_candidate_strs())
}

pub(crate) fn default_secrets_path_if_present(brand: Brand) -> Option<OsString> {
    #[cfg(test)]
    if let Some(paths) = TEST_SECRETS_CANDIDATES.with(|cell| cell.borrow().clone()) {
        return first_existing_path(paths.iter());
    }

    first_existing_path(brand.secrets_path_candidate_strs())
}
