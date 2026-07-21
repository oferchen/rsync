//! Trailing-slash source-operand layout parity between oc-rsync and upstream.
//!
//! A trailing slash on the source operand changes the destination layout: `rsync
//! src dst/` copies the directory itself, creating `dst/src/...`, whereas `rsync
//! src/ dst/` copies the *contents* of `src` directly into `dst`. This is one of
//! rsync's most load-bearing conventions, so this check runs both operand forms
//! over every transport and asserts oc-rsync reproduces upstream's destination
//! tree exactly in each, plus a non-vacuous guard that the two forms actually
//! produced different layouts (the named subdirectory is present under the
//! no-slash form and absent under the slash form).
//!
//! It builds the transfer commands directly rather than via `pull_into`, which
//! always appends a trailing slash and so could not exercise the no-slash form.
//!
//! upstream: the trailing-slash convention is implemented by trimming the slash
//! and recording whether it was present (see `flist.c` `send_file_name` /
//! `f_name` handling and the `sanitize_path`/trailing-slash logic in `options.c`);
//! the behavior is what `testsuite/trimslash.test` guards at the primitive level.

use std::path::Path;
use std::process::{Command, Output};

use crate::commands::validate::support;
use crate::commands::validate::transport::{DaemonHandle, Transport};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};
use crate::error::{TaskError, TaskResult};

/// The trailing-slash source-operand layout parity check.
pub struct TrimSlash;

/// Name of the source directory. Under the no-slash form this becomes a
/// subdirectory of the destination; under the slash form it disappears.
const SRC_NAME: &str = "tree";
/// A file inside the source tree; its destination path differs between forms.
const LEAF: &str = "leaf.txt";

/// Preserve metadata; `--numeric-ids` keeps owner comparison host-stable. No
/// path-altering option (`-R`, `-d`) so the trailing slash is the only variable.
const FLAGS: &[&str] = &["-rlptgoD", "--numeric-ids"];

/// One operand form: whether the source operand carries a trailing slash.
#[derive(Clone, Copy)]
enum Form {
    /// `src` (no trailing slash): copies the directory, creating `dst/src/...`.
    NoSlash,
    /// `src/` (trailing slash): copies the contents directly into `dst`.
    Slash,
}

/// Both operand forms, in report order.
const FORMS: [Form; 2] = [Form::NoSlash, Form::Slash];

impl Form {
    /// Stable short label used in the cell name.
    fn label(self) -> &'static str {
        match self {
            Form::NoSlash => "no-slash",
            Form::Slash => "slash",
        }
    }

    /// The trailing-slash suffix appended after the source directory path.
    fn suffix(self) -> &'static str {
        match self {
            Form::NoSlash => "",
            Form::Slash => "/",
        }
    }
}

impl Check for TrimSlash {
    fn name(&self) -> &'static str {
        "trimslash"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("trimslash");
        let src = root.join(SRC_NAME);
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        let mut out = Vec::new();
        for &transport in ctx.transports {
            for form in FORMS {
                out.push(self.cell(ctx, transport, form, &root, &src));
            }
        }
        out
    }
}

impl TrimSlash {
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        form: Form,
        root: &Path,
        src: &Path,
    ) -> CheckOutcome {
        let label = format!("{} {}", transport.label(), form.label());
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), label, "no sshd on localhost:22");
        }

        let daemon = match transport {
            Transport::Daemon => match DaemonHandle::start(ctx.upstream, src, ctx.work) {
                Ok(handle) => Some(handle),
                Err(e) => return CheckOutcome::skip(self.name(), label, format!("daemon: {e}")),
            },
            _ => None,
        };
        let module_url = daemon.as_ref().map(|d| d.module_url());
        let flags: Vec<String> = FLAGS.iter().map(|s| s.to_string()).collect();

        let up_dst = root.join(format!("up-{}-{}", transport.label(), form.label()));
        let oc_dst = root.join(format!("oc-{}-{}", transport.label(), form.label()));

        let up_transport = transport.for_upstream();
        let up_operand = source_operand(up_transport, src, form, module_url.as_deref());
        match transfer(
            ctx.upstream,
            ctx.upstream,
            up_transport,
            &up_operand,
            &up_dst,
            &flags,
        ) {
            Ok(out) if out.status.success() => {}
            other => return skip_or_fail(self.name(), &label, "upstream", other),
        }
        let oc_operand = source_operand(transport, src, form, module_url.as_deref());
        match transfer(
            ctx.oc,
            ctx.upstream,
            transport,
            &oc_operand,
            &oc_dst,
            &flags,
        ) {
            Ok(out) if out.status.success() => {}
            other => return skip_or_fail(self.name(), &label, "oc", other),
        }
        drop(daemon);

        // Non-vacuous: the layout must match the form. The no-slash form nests
        // the tree under its own name (`dst/tree/leaf.txt`); the slash form
        // drops the leaf directly into `dst` (`dst/leaf.txt`, no `tree`). The
        // daemon module already exports the tree's contents, so its "no-slash"
        // operand (the bare module) behaves like the slash form; skip the layout
        // assertion there and rely on the oc-vs-upstream compare.
        if !matches!(transport, Transport::Daemon) {
            if let Err(e) = check_layout(form, &oc_dst, "oc") {
                return CheckOutcome::fail(self.name(), label, e);
            }
            if let Err(e) = check_layout(form, &up_dst, "upstream") {
                return CheckOutcome::fail(self.name(), label, e);
            }
        }

        match support::content_diff(&oc_dst, &up_dst) {
            Some(diff) => CheckOutcome::fail(self.name(), label, diff),
            None => CheckOutcome::pass(self.name(), label),
        }
    }
}

/// Assert `dst`'s layout matches `form`, so the check cannot pass vacuously.
fn check_layout(form: Form, dst: &Path, who: &str) -> Result<(), String> {
    match form {
        Form::NoSlash => {
            if !dst.join(SRC_NAME).join(LEAF).exists() {
                return Err(format!(
                    "{who}: no-slash form did not nest {SRC_NAME}/{LEAF}"
                ));
            }
        }
        Form::Slash => {
            if !dst.join(LEAF).exists() {
                return Err(format!(
                    "{who}: slash form did not place {LEAF} at dest root"
                ));
            }
            if dst.join(SRC_NAME).exists() {
                return Err(format!("{who}: slash form wrongly nested {SRC_NAME}"));
            }
        }
    }
    Ok(())
}

/// Build the source operand for `transport` and `form`. `SRC_NAME` is always the
/// final path component so the no-slash form has a name to nest under; the slash
/// form appends a trailing `/`.
fn source_operand(
    transport: Transport,
    src: &Path,
    form: Form,
    module_url: Option<&str>,
) -> String {
    let suffix = form.suffix();
    match transport {
        Transport::Local => format!("{}{suffix}", src.display()),
        Transport::SshSubprocess => format!("localhost:{}{suffix}", src.display()),
        Transport::Russh => format!("ssh://localhost{}{suffix}", src.display()),
        // The daemon exports the tree as module `m`; the bare module URL already
        // ends in `/m/`, so both forms transfer the module contents.
        Transport::Daemon => module_url.unwrap_or("").to_string(),
    }
}

/// Reset `dst`, build the per-transport command, and run it capturing output.
fn transfer(
    client: &Path,
    upstream: &Path,
    transport: Transport,
    operand: &str,
    dst: &Path,
    flags: &[String],
) -> TaskResult<Output> {
    reset_dir(dst)?;
    let dst_arg = format!("{}/", dst.display());
    let mut cmd = Command::new(client);
    cmd.args(flags);
    match transport {
        Transport::Local | Transport::Daemon => {
            cmd.arg(operand).arg(&dst_arg);
        }
        Transport::SshSubprocess => {
            cmd.arg(format!("--rsync-path={}", upstream.display()))
                .arg("-e")
                .arg("ssh -o BatchMode=yes -o StrictHostKeyChecking=no")
                .arg(operand)
                .arg(&dst_arg);
        }
        Transport::Russh => {
            cmd.arg(format!("--rsync-path={}", upstream.display()))
                .arg(operand)
                .arg(&dst_arg);
        }
    }
    cmd.output()
        .map_err(|e| TaskError::Validation(format!("spawn rsync: {e}")))
}

/// Recreate `dir` as an empty directory.
fn reset_dir(dir: &Path) -> TaskResult<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)
            .map_err(|e| TaskError::Validation(format!("remove {}: {e}", dir.display())))?;
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| TaskError::Validation(format!("create {}: {e}", dir.display())))
}

/// Distinguish a genuine divergence from an unrunnable cell (e.g. ssh refused).
fn skip_or_fail(
    check: &'static str,
    label: &str,
    who: &str,
    result: TaskResult<Output>,
) -> CheckOutcome {
    match result {
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let code = out.status.code().unwrap_or(-1);
            CheckOutcome::fail(
                check,
                label,
                format!("{who} exited {code}: {}", stderr.trim()),
            )
        }
        Err(e) => CheckOutcome::skip(check, label, format!("{who} could not run: {e}")),
    }
}

/// Build the source tree. Idempotent: removes any prior tree first. A single
/// backdated leaf file is enough to observe the layout difference.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(src).map_err(|e| e.to_string())?;
    std::fs::write(src.join(LEAF), b"leaf body\n").map_err(|e| e.to_string())?;
    for entry in support::rel_entries(src) {
        let path = src.join(&entry);
        support::capture(
            "touch",
            &["-h", "-d", "@1614830767", &path.to_string_lossy()],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn slash_form_appends_trailing_slash_only_when_requested() {
        let src = Path::new("/work/trimslash/tree");
        assert_eq!(
            source_operand(Transport::Local, src, Form::NoSlash, None),
            "/work/trimslash/tree"
        );
        assert_eq!(
            source_operand(Transport::Local, src, Form::Slash, None),
            "/work/trimslash/tree/"
        );
    }

    #[test]
    fn network_forms_prefix_the_host_and_preserve_the_suffix() {
        let src = Path::new("/work/trimslash/tree");
        assert_eq!(
            source_operand(Transport::SshSubprocess, src, Form::NoSlash, None),
            "localhost:/work/trimslash/tree"
        );
        assert_eq!(
            source_operand(Transport::Russh, src, Form::Slash, None),
            "ssh://localhost/work/trimslash/tree/"
        );
    }

    #[test]
    fn check_layout_enforces_the_form_specific_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("no-slash");
        std::fs::create_dir_all(nested.join(SRC_NAME)).unwrap();
        std::fs::write(nested.join(SRC_NAME).join(LEAF), b"x").unwrap();
        assert!(check_layout(Form::NoSlash, &nested, "oc").is_ok());
        assert!(check_layout(Form::Slash, &nested, "oc").is_err());

        let flat = tmp.path().join("slash");
        std::fs::create_dir_all(&flat).unwrap();
        std::fs::write(flat.join(LEAF), b"x").unwrap();
        assert!(check_layout(Form::Slash, &flat, "oc").is_ok());
        assert!(check_layout(Form::NoSlash, &flat, "oc").is_err());
    }
}
