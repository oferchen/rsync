pub(super) fn extract_job_section(contents: &str, job_name: &str) -> Option<String> {
    let mut section_lines = Vec::new();
    let mut collecting = false;
    let mut job_indent = 0usize;
    let target = format!("{job_name}:");

    for line in contents.lines() {
        let indent = line.chars().take_while(|c| *c == ' ').count();
        let trimmed = line[indent..].trim_end();

        if !collecting {
            if indent == 2 && trimmed == target {
                collecting = true;
                job_indent = indent;
                section_lines.push(line.to_string());
            }
            continue;
        }

        if trimmed.is_empty() {
            section_lines.push(line.to_string());
            continue;
        }

        if indent <= job_indent && trimmed.ends_with(':') && !trimmed.starts_with('-') {
            break;
        }

        section_lines.push(line.to_string());
    }

    if collecting {
        Some(section_lines.join("\n"))
    } else {
        None
    }
}

pub(super) fn find_yaml_scalar(section: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    for line in section.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix(&prefix) {
            let value = rest.split('#').next().unwrap_or("").trim();
            if value.is_empty() {
                return Some(String::new());
            }
            let value = value.trim_matches('"');
            return Some(value.to_string());
        }
    }

    None
}
