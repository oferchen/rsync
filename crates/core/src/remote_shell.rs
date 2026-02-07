//! Remote shell command construction and SSH argument parsing.
//!
//! This module implements upstream rsync's --rsh/-e remote shell behavior,
//! handling SSH command construction, argument parsing, and URI handling.
//!
//! # Examples
//!
//! ```
//! use core::remote_shell::{RemoteShell, SshConfig};
//!
//! // Default SSH shell
//! let shell = RemoteShell::default();
//! assert_eq!(shell.program(), "ssh");
//!
//! // Custom SSH with options
//! let shell = RemoteShell::new("ssh -p 2222 -o StrictHostKeyChecking=no");
//! assert_eq!(shell.program(), "ssh");
//! assert_eq!(shell.args(), &["-p", "2222", "-o", "StrictHostKeyChecking=no"]);
//!
//! // Build full command
//! let cmd = shell.build_command("example.com", "rsync", &["--server", "--sender"]);
//! assert_eq!(cmd[0], "ssh");
//! assert_eq!(cmd[1], "-p");
//! assert_eq!(cmd[2], "2222");
//! ```
//!
//! # Upstream Reference
//!
//! This module mirrors the shell command parsing in upstream rsync's `options.c`
//! and the remote shell execution logic in `main.c`.

use std::borrow::Cow;

/// Remote shell configuration for executing rsync on remote hosts.
///
/// Handles parsing of shell command strings (e.g., from --rsh/-e flag) and
/// construction of full command lines for remote execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteShell {
    program: String,
    args: Vec<String>,
}

impl RemoteShell {
    /// Create a new remote shell from a command string.
    ///
    /// Parses the command string to extract the program and any arguments.
    /// Handles quoted arguments and whitespace properly.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::remote_shell::RemoteShell;
    ///
    /// let shell = RemoteShell::new("ssh");
    /// assert_eq!(shell.program(), "ssh");
    /// assert!(shell.args().is_empty());
    ///
    /// let shell = RemoteShell::new("ssh -p 2222");
    /// assert_eq!(shell.program(), "ssh");
    /// assert_eq!(shell.args(), &["-p", "2222"]);
    /// ```
    pub fn new(command: &str) -> Self {
        let parts = parse_shell_command(command);
        if parts.is_empty() {
            return Self::default();
        }

        Self {
            program: parts[0].clone(),
            args: parts[1..].to_vec(),
        }
    }

    /// Get the shell program name.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::remote_shell::RemoteShell;
    ///
    /// let shell = RemoteShell::new("ssh");
    /// assert_eq!(shell.program(), "ssh");
    /// ```
    pub fn program(&self) -> &str {
        &self.program
    }

    /// Get the additional shell arguments.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::remote_shell::RemoteShell;
    ///
    /// let shell = RemoteShell::new("ssh -p 2222 -v");
    /// assert_eq!(shell.args(), &["-p", "2222", "-v"]);
    /// ```
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// Build a complete command line for remote execution.
    ///
    /// Constructs the full command by combining the shell program, its arguments,
    /// the target host, and the rsync server command with its arguments.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::remote_shell::RemoteShell;
    ///
    /// let shell = RemoteShell::new("ssh -p 2222");
    /// let cmd = shell.build_command("example.com", "rsync", &["--server", "."]);
    /// assert_eq!(cmd[0], "ssh");
    /// assert_eq!(cmd[1], "-p");
    /// assert_eq!(cmd[2], "2222");
    /// assert_eq!(cmd[3], "example.com");
    /// assert_eq!(cmd[4], "rsync");
    /// assert_eq!(cmd[5], "--server");
    /// assert_eq!(cmd[6], ".");
    /// ```
    pub fn build_command(&self, host: &str, rsync_path: &str, server_args: &[&str]) -> Vec<String> {
        let mut command = Vec::new();

        // Add shell program
        command.push(self.program.clone());

        // Add shell arguments
        command.extend(self.args.iter().cloned());

        // Add host
        command.push(host.to_string());

        // Add rsync path
        command.push(rsync_path.to_string());

        // Add server arguments
        command.extend(server_args.iter().map(|s| s.to_string()));

        command
    }
}

impl Default for RemoteShell {
    /// Default remote shell is "ssh" with no additional arguments.
    fn default() -> Self {
        Self {
            program: "ssh".to_string(),
            args: Vec::new(),
        }
    }
}

/// SSH-specific configuration options.
///
/// Represents common SSH command-line options that can be converted
/// to argument arrays for use with SSH commands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SshConfig {
    /// SSH port number (e.g., 2222)
    pub port: Option<u16>,
    /// Path to identity file for authentication
    pub identity_file: Option<String>,
    /// Additional SSH options (e.g., "StrictHostKeyChecking=no")
    pub ssh_options: Vec<String>,
    /// Remote user name
    pub user: Option<String>,
}

impl SshConfig {
    /// Convert SSH configuration to command-line arguments.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::remote_shell::SshConfig;
    ///
    /// let config = SshConfig {
    ///     port: Some(2222),
    ///     identity_file: Some("/path/to/key".to_string()),
    ///     ssh_options: vec!["StrictHostKeyChecking=no".to_string()],
    ///     user: Some("myuser".to_string()),
    /// };
    ///
    /// let args = config.to_args();
    /// assert!(args.contains(&"-p".to_string()));
    /// assert!(args.contains(&"2222".to_string()));
    /// assert!(args.contains(&"-i".to_string()));
    /// assert!(args.contains(&"/path/to/key".to_string()));
    /// assert!(args.contains(&"-o".to_string()));
    /// assert!(args.contains(&"StrictHostKeyChecking=no".to_string()));
    /// assert!(args.contains(&"-l".to_string()));
    /// assert!(args.contains(&"myuser".to_string()));
    /// ```
    pub fn to_args(&self) -> Vec<String> {
        let mut args = Vec::new();

        if let Some(port) = self.port {
            args.push("-p".to_string());
            args.push(port.to_string());
        }

        if let Some(ref identity_file) = self.identity_file {
            args.push("-i".to_string());
            args.push(identity_file.clone());
        }

        for option in &self.ssh_options {
            args.push("-o".to_string());
            args.push(option.clone());
        }

        if let Some(ref user) = self.user {
            args.push("-l".to_string());
            args.push(user.clone());
        }

        args
    }
}

/// Parse a shell command string into program and arguments.
///
/// Handles quoted strings and whitespace properly. Supports both single and
/// double quotes, with proper escaping.
///
/// # Examples
///
/// ```
/// use core::remote_shell::parse_shell_command;
///
/// let parts = parse_shell_command("ssh -p 2222");
/// assert_eq!(parts, vec!["ssh", "-p", "2222"]);
///
/// let parts = parse_shell_command("ssh -o 'User=myuser'");
/// assert_eq!(parts, vec!["ssh", "-o", "User=myuser"]);
/// ```
pub fn parse_shell_command(command: &str) -> Vec<String> {
    let command = command.trim();
    if command.is_empty() {
        return Vec::new();
    }

    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped = false;
    let mut chars = command.chars().peekable();

    while let Some(ch) = chars.next() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        match ch {
            '\\' if in_single_quote => {
                // In single quotes, backslash is literal
                current.push(ch);
            }
            '\\' => {
                // Outside quotes or in double quotes, backslash escapes next char
                escaped = true;
            }
            '\'' if !in_double_quote => {
                in_single_quote = !in_single_quote;
            }
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
            }
            c if c.is_whitespace() && !in_single_quote && !in_double_quote => {
                if !current.is_empty() {
                    parts.push(current.clone());
                    current.clear();
                }
            }
            _ => {
                current.push(ch);
            }
        }
    }

    if !current.is_empty() {
        parts.push(current);
    }

    parts
}

/// Parse an SSH-style URI into user, host, and path components.
///
/// Supports both SSH and rsync daemon URI formats:
/// - `user@host:path` - SSH remote path
/// - `host:path` - SSH remote path without user
/// - `host::module/path` - rsync daemon syntax
///
/// # Examples
///
/// ```
/// use core::remote_shell::parse_ssh_uri;
///
/// assert_eq!(
///     parse_ssh_uri("user@example.com:/path/to/file"),
///     Some((Some("user"), "example.com", "/path/to/file"))
/// );
///
/// assert_eq!(
///     parse_ssh_uri("example.com:/path/to/file"),
///     Some((None, "example.com", "/path/to/file"))
/// );
///
/// assert_eq!(
///     parse_ssh_uri("example.com::module/path"),
///     Some((None, "example.com", "::module/path"))
/// );
/// ```
pub fn parse_ssh_uri(uri: &str) -> Option<(Option<&str>, &str, &str)> {
    // Check for rsync daemon syntax (host::module)
    if let Some(double_colon_pos) = uri.find("::") {
        let host_part = &uri[..double_colon_pos];
        let path_part = &uri[double_colon_pos..];

        // Parse user@host if present
        if let Some(at_pos) = host_part.find('@') {
            let user = &host_part[..at_pos];
            let host = &host_part[at_pos + 1..];
            return Some((Some(user), host, path_part));
        } else {
            return Some((None, host_part, path_part));
        }
    }

    // Check for SSH syntax (user@host:path or host:path)
    let colon_pos = uri.find(':')?;
    let host_part = &uri[..colon_pos];
    let path_part = &uri[colon_pos + 1..];

    // Parse user@host if present
    if let Some(at_pos) = host_part.find('@') {
        let user = &host_part[..at_pos];
        let host = &host_part[at_pos + 1..];
        Some((Some(user), host, path_part))
    } else {
        Some((None, host_part, path_part))
    }
}

/// Check if a shell argument needs quoting.
///
/// Returns true if the argument contains characters that need shell quoting
/// to be interpreted correctly.
///
/// # Examples
///
/// ```
/// use core::remote_shell::needs_quoting;
///
/// assert!(!needs_quoting("simple"));
/// assert!(needs_quoting("has space"));
/// assert!(needs_quoting("has$dollar"));
/// assert!(needs_quoting("has'quote"));
/// ```
pub fn needs_quoting(arg: &str) -> bool {
    if arg.is_empty() {
        return true;
    }

    // Characters that require quoting in shell arguments
    const SPECIAL_CHARS: &[char] = &[
        ' ', '\t', '\n', '\'', '"', '\\', '$', '`', '!', '*', '?', '[', ']', '(', ')', '{', '}',
        '<', '>', '|', '&', ';', '#', '~',
    ];

    arg.chars().any(|c| SPECIAL_CHARS.contains(&c))
}

/// Quote a shell argument if necessary.
///
/// Returns a shell-safe quoted string if the argument contains special
/// characters, otherwise returns the argument unchanged.
///
/// # Examples
///
/// ```
/// use core::remote_shell::quote_shell_arg;
///
/// assert_eq!(quote_shell_arg("simple"), "simple");
/// assert_eq!(quote_shell_arg("has space"), "'has space'");
/// assert_eq!(quote_shell_arg("has'quote"), "'has'\\''quote'");
/// ```
pub fn quote_shell_arg(arg: &str) -> Cow<'_, str> {
    if !needs_quoting(arg) {
        return Cow::Borrowed(arg);
    }

    // Use single quotes and escape any single quotes in the string
    // by ending the quote, adding an escaped quote, and starting a new quote
    let quoted = arg.replace('\'', r"'\''");
    Cow::Owned(format!("'{}'", quoted))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_remote_shell() {
        let shell = RemoteShell::default();
        assert_eq!(shell.program(), "ssh");
        assert_eq!(shell.args(), &[] as &[String]);
    }

    #[test]
    fn test_simple_command_parsing() {
        let shell = RemoteShell::new("ssh");
        assert_eq!(shell.program(), "ssh");
        assert_eq!(shell.args(), &[] as &[String]);
    }

    #[test]
    fn test_command_with_arguments() {
        let shell = RemoteShell::new("ssh -p 2222 -v");
        assert_eq!(shell.program(), "ssh");
        assert_eq!(shell.args(), &["-p", "2222", "-v"]);
    }

    #[test]
    fn test_command_with_quoted_arguments() {
        let shell = RemoteShell::new("ssh -o 'User=myuser' -o 'Port=2222'");
        assert_eq!(shell.program(), "ssh");
        assert_eq!(shell.args(), &["-o", "User=myuser", "-o", "Port=2222"]);
    }

    #[test]
    fn test_command_with_double_quoted_arguments() {
        let shell = RemoteShell::new("ssh -o \"StrictHostKeyChecking=no\"");
        assert_eq!(shell.program(), "ssh");
        assert_eq!(shell.args(), &["-o", "StrictHostKeyChecking=no"]);
    }

    #[test]
    fn test_command_with_escaped_quotes() {
        // In single quotes, backslashes are literal (not escape chars)
        let shell = RemoteShell::new("ssh -o 'Host\\'s'");
        assert_eq!(shell.program(), "ssh");
        // This will actually be "Host\s" because backslash is literal in single quotes
        assert_eq!(shell.args(), &["-o", "Host\\s"]);

        // To get an actual single quote, need to end the quote, add escaped quote, start new quote
        // The pattern is: 'text'\''more' which becomes text'more
        let shell = RemoteShell::new("ssh -o 'Host'\\''s'");
        assert_eq!(shell.program(), "ssh");
        assert_eq!(shell.args(), &["-o", "Host's"]);
    }

    #[test]
    fn test_empty_command_defaults() {
        let shell = RemoteShell::new("");
        assert_eq!(shell.program(), "ssh");
        assert_eq!(shell.args(), &[] as &[String]);

        let shell = RemoteShell::new("   ");
        assert_eq!(shell.program(), "ssh");
        assert_eq!(shell.args(), &[] as &[String]);
    }

    #[test]
    fn test_build_command() {
        let shell = RemoteShell::new("ssh");
        let cmd = shell.build_command("example.com", "rsync", &["--server", "--sender", "."]);

        assert_eq!(cmd.len(), 6);
        assert_eq!(cmd[0], "ssh");
        assert_eq!(cmd[1], "example.com");
        assert_eq!(cmd[2], "rsync");
        assert_eq!(cmd[3], "--server");
        assert_eq!(cmd[4], "--sender");
        assert_eq!(cmd[5], ".");
    }

    #[test]
    fn test_build_command_with_shell_args() {
        let shell = RemoteShell::new("ssh -p 2222 -o StrictHostKeyChecking=no");
        let cmd = shell.build_command("example.com", "rsync", &["--server"]);

        assert_eq!(cmd[0], "ssh");
        assert_eq!(cmd[1], "-p");
        assert_eq!(cmd[2], "2222");
        assert_eq!(cmd[3], "-o");
        assert_eq!(cmd[4], "StrictHostKeyChecking=no");
        assert_eq!(cmd[5], "example.com");
        assert_eq!(cmd[6], "rsync");
        assert_eq!(cmd[7], "--server");
    }

    #[test]
    fn test_parse_ssh_uri_with_user() {
        let result = parse_ssh_uri("user@example.com:/path/to/file");
        assert_eq!(result, Some((Some("user"), "example.com", "/path/to/file")));
    }

    #[test]
    fn test_parse_ssh_uri_without_user() {
        let result = parse_ssh_uri("example.com:/path/to/file");
        assert_eq!(result, Some((None, "example.com", "/path/to/file")));
    }

    #[test]
    fn test_parse_ssh_uri_rsync_daemon() {
        let result = parse_ssh_uri("example.com::module/path");
        assert_eq!(result, Some((None, "example.com", "::module/path")));
    }

    #[test]
    fn test_parse_ssh_uri_rsync_daemon_with_user() {
        let result = parse_ssh_uri("user@example.com::module/path");
        assert_eq!(result, Some((Some("user"), "example.com", "::module/path")));
    }

    #[test]
    fn test_parse_ssh_uri_local_path() {
        let result = parse_ssh_uri("/local/path");
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_ssh_uri_relative_path() {
        let result = parse_ssh_uri("relative/path");
        assert_eq!(result, None);
    }

    #[test]
    fn test_needs_quoting_simple() {
        assert!(!needs_quoting("simple"));
        assert!(!needs_quoting("simple_file.txt"));
        assert!(!needs_quoting("file123"));
    }

    #[test]
    fn test_needs_quoting_spaces() {
        assert!(needs_quoting("has space"));
        assert!(needs_quoting("multiple  spaces"));
    }

    #[test]
    fn test_needs_quoting_special_chars() {
        assert!(needs_quoting("has$dollar"));
        assert!(needs_quoting("has'quote"));
        assert!(needs_quoting("has\"quote"));
        assert!(needs_quoting("has\\backslash"));
        assert!(needs_quoting("has`backtick"));
        assert!(needs_quoting("has!exclaim"));
        assert!(needs_quoting("has*asterisk"));
        assert!(needs_quoting("has?question"));
        assert!(needs_quoting("has[bracket"));
        assert!(needs_quoting("has(paren"));
        assert!(needs_quoting("has{brace"));
        assert!(needs_quoting("has<angle"));
        assert!(needs_quoting("has|pipe"));
        assert!(needs_quoting("has&ampersand"));
        assert!(needs_quoting("has;semicolon"));
        assert!(needs_quoting("has#hash"));
        assert!(needs_quoting("has~tilde"));
    }

    #[test]
    fn test_needs_quoting_empty() {
        assert!(needs_quoting(""));
    }

    #[test]
    fn test_quote_shell_arg_simple() {
        assert_eq!(quote_shell_arg("simple"), "simple");
    }

    #[test]
    fn test_quote_shell_arg_with_space() {
        assert_eq!(quote_shell_arg("has space"), "'has space'");
    }

    #[test]
    fn test_quote_shell_arg_with_single_quote() {
        assert_eq!(quote_shell_arg("has'quote"), "'has'\\''quote'");
    }

    #[test]
    fn test_quote_shell_arg_with_multiple_single_quotes() {
        assert_eq!(quote_shell_arg("it's won't"), "'it'\\''s won'\\''t'");
    }

    #[test]
    fn test_ssh_config_to_args_empty() {
        let config = SshConfig::default();
        let args = config.to_args();
        assert_eq!(args, Vec::<String>::new());
    }

    #[test]
    fn test_ssh_config_to_args_port() {
        let config = SshConfig {
            port: Some(2222),
            ..Default::default()
        };
        let args = config.to_args();
        assert_eq!(args, vec!["-p", "2222"]);
    }

    #[test]
    fn test_ssh_config_to_args_identity_file() {
        let config = SshConfig {
            identity_file: Some("/path/to/key".to_string()),
            ..Default::default()
        };
        let args = config.to_args();
        assert_eq!(args, vec!["-i", "/path/to/key"]);
    }

    #[test]
    fn test_ssh_config_to_args_ssh_options() {
        let config = SshConfig {
            ssh_options: vec![
                "StrictHostKeyChecking=no".to_string(),
                "UserKnownHostsFile=/dev/null".to_string(),
            ],
            ..Default::default()
        };
        let args = config.to_args();
        assert_eq!(
            args,
            vec!["-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null"]
        );
    }

    #[test]
    fn test_ssh_config_to_args_user() {
        let config = SshConfig {
            user: Some("myuser".to_string()),
            ..Default::default()
        };
        let args = config.to_args();
        assert_eq!(args, vec!["-l", "myuser"]);
    }

    #[test]
    fn test_ssh_config_to_args_complete() {
        let config = SshConfig {
            port: Some(2222),
            identity_file: Some("/path/to/key".to_string()),
            ssh_options: vec![
                "StrictHostKeyChecking=no".to_string(),
                "Compression=yes".to_string(),
            ],
            user: Some("testuser".to_string()),
        };
        let args = config.to_args();

        assert_eq!(args.len(), 10);
        assert_eq!(args[0], "-p");
        assert_eq!(args[1], "2222");
        assert_eq!(args[2], "-i");
        assert_eq!(args[3], "/path/to/key");
        assert_eq!(args[4], "-o");
        assert_eq!(args[5], "StrictHostKeyChecking=no");
        assert_eq!(args[6], "-o");
        assert_eq!(args[7], "Compression=yes");
        assert_eq!(args[8], "-l");
        assert_eq!(args[9], "testuser");
    }

    #[test]
    fn test_parse_shell_command_with_tabs() {
        let parts = parse_shell_command("ssh\t-p\t2222");
        assert_eq!(parts, vec!["ssh", "-p", "2222"]);
    }

    #[test]
    fn test_parse_shell_command_with_mixed_whitespace() {
        let parts = parse_shell_command("  ssh   -p  2222  ");
        assert_eq!(parts, vec!["ssh", "-p", "2222"]);
    }

    #[test]
    fn test_parse_shell_command_with_escaped_space() {
        let parts = parse_shell_command("ssh -o User\\ Name");
        assert_eq!(parts, vec!["ssh", "-o", "User Name"]);
    }

    #[test]
    fn test_remote_shell_equality() {
        let shell1 = RemoteShell::new("ssh -p 2222");
        let shell2 = RemoteShell::new("ssh -p 2222");
        let shell3 = RemoteShell::new("ssh -p 22");

        assert_eq!(shell1, shell2);
        assert_ne!(shell1, shell3);
    }

    #[test]
    fn test_ssh_config_equality() {
        let config1 = SshConfig {
            port: Some(2222),
            user: Some("test".to_string()),
            ..Default::default()
        };
        let config2 = SshConfig {
            port: Some(2222),
            user: Some("test".to_string()),
            ..Default::default()
        };
        let config3 = SshConfig {
            port: Some(22),
            user: Some("test".to_string()),
            ..Default::default()
        };

        assert_eq!(config1, config2);
        assert_ne!(config1, config3);
    }
}
