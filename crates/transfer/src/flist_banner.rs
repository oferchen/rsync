//! The client-facing banner a network client prints around its file list.
//!
//! upstream: flist.c:2761-2764 (send_file_list) and flist.c:3119-3122
//! (recv_file_list) pick exactly one of two banners:
//!
//! ```c
//! if (show_filelist_progress)
//!         start_filelist_progress("building file list");
//! else if (inc_recurse && INFO_GTE(FLIST, 1) && !am_server)
//!         rprintf(FCLIENT, "sending incremental file list\n");
//! ```
//!
//! with `show_filelist_progress = INFO_GTE(FLIST, 1) && xfer_dirs && !am_server
//! && !inc_recurse` (flist.c:172). `inc_recurse` is the negotiated
//! `CF_INC_RECURSE` bit (compat.c:757), not the `--recursive` flag.

use std::io::{self, Write};

use logging::{InfoFlag, finfo_suppressed, info_gte};

/// Which banner a client prints for its file list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FlistBanner {
    /// No banner: a server role, `--info=flist0`, `--quiet`, or neither
    /// incremental recursion nor `xfer_dirs`.
    None,
    /// `<kind> ... done` around the whole list - upstream
    /// `start_filelist_progress()` / `finish_filelist_progress()`.
    Progress,
    /// `<verb> incremental file list` before the first list.
    Incremental,
}

/// The transfer side that prints the banner, which fixes its wording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FlistSide {
    /// The sender: `building file list` / `sending incremental file list`.
    Sender,
    /// The receiver: `receiving file list` / `receiving incremental file list`.
    Receiver,
}

impl FlistBanner {
    /// Selects the banner from upstream's two gates.
    ///
    /// `xfer_dirs` is `recurse || dirs || list_only` (options.c:2326-2329).
    /// Both banners are FINFO output, which rwrite() drops under `--quiet`
    /// (log.c:344-345); `start_filelist_progress()` returns early there too
    /// (flist.c:177).
    pub(crate) fn select(client_mode: bool, inc_recurse: bool, xfer_dirs: bool) -> Self {
        if !client_mode || !info_gte(InfoFlag::Flist, 1) || finfo_suppressed() {
            Self::None
        } else if inc_recurse {
            Self::Incremental
        } else if xfer_dirs {
            Self::Progress
        } else {
            Self::None
        }
    }

    /// Writes the part printed before the list.
    ///
    /// upstream: flist.c:179 - `"%s ... "` with no newline for the progress
    /// banner; flist.c:2764 / flist.c:3122 for the incremental one.
    pub(crate) fn start(self, side: FlistSide, out: &mut dyn Write) -> io::Result<()> {
        match (self, side) {
            (Self::None, _) => return Ok(()),
            (Self::Progress, FlistSide::Sender) => out.write_all(b"building file list ... ")?,
            (Self::Progress, FlistSide::Receiver) => out.write_all(b"receiving file list ... ")?,
            (Self::Incremental, FlistSide::Sender) => {
                out.write_all(b"sending incremental file list\n")?;
            }
            (Self::Incremental, FlistSide::Receiver) => {
                out.write_all(b"receiving incremental file list\n")?;
            }
        }
        out.flush()
    }

    /// Writes the part printed once the `count`-entry list is complete.
    ///
    /// upstream: flist.c:200-210 finish_filelist_progress() - `done` at FLIST
    /// level 1; at level 2 the `%d file%sto consider` total, preceded by the
    /// ` %d files...\r` ticks that flist.c:194-197 maybe_emit_filelist_progress()
    /// writes for every hundredth entry. The sender ticks before adding each
    /// entry (flist.c:2091, counts 0, 100, ...), the receiver after
    /// (flist.c:3253, counts 100, 200, ...). The first tick starts on a fresh
    /// line because `start` left `output_needs_newline` set (log.c:375-378).
    pub(crate) fn finish(
        self,
        side: FlistSide,
        count: usize,
        out: &mut dyn Write,
    ) -> io::Result<()> {
        if self != Self::Progress {
            return Ok(());
        }
        if !info_gte(InfoFlag::Flist, 2) {
            out.write_all(b"done\n")?;
            return out.flush();
        }
        let ticks = (0..=count).step_by(100).filter(|&tick| match side {
            FlistSide::Sender => tick < count,
            FlistSide::Receiver => tick > 0,
        });
        for (index, tick) in ticks.enumerate() {
            if index == 0 {
                out.write_all(b"\n")?;
            }
            write!(out, " {tick} files...\r")?;
        }
        let plural = if count == 1 { " " } else { "s " };
        writeln!(out, "{count} file{plural}to consider")?;
        out.flush()
    }
}

/// Runs `emit` against the client stream the banner belongs on: stderr under
/// `--msgs2stderr` (log.c:253), stdout otherwise.
pub(crate) fn with_client_stream(
    msgs_to_stderr: bool,
    emit: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> io::Result<()> {
    if msgs_to_stderr {
        emit(&mut io::stderr().lock())
    } else {
        emit(&mut io::stdout().lock())
    }
}

#[cfg(test)]
mod tests {
    use super::{FlistBanner, FlistSide};
    use logging::VerbosityConfig;

    fn rendered_count(banner: FlistBanner, side: FlistSide, count: usize) -> String {
        let mut out = Vec::new();
        banner.start(side, &mut out).unwrap();
        banner.finish(side, count, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn rendered(banner: FlistBanner, side: FlistSide) -> String {
        logging::init(VerbosityConfig::from_verbose_level(1));
        rendered_count(banner, side, 2)
    }

    /// The incremental banner follows the negotiated CF_INC_RECURSE bit, not
    /// `--recursive`: a recursive transfer without it gets the progress banner.
    #[test]
    fn negotiated_inc_recurse_selects_the_incremental_banner() {
        logging::init(VerbosityConfig::from_verbose_level(1));
        assert_eq!(
            FlistBanner::select(true, true, true),
            FlistBanner::Incremental
        );
        assert_eq!(
            FlistBanner::select(true, false, true),
            FlistBanner::Progress
        );
        assert_eq!(FlistBanner::select(true, false, false), FlistBanner::None);
    }

    /// upstream gates both banners on `!am_server` and `INFO_GTE(FLIST, 1)`.
    #[test]
    fn server_role_and_flist0_print_nothing() {
        logging::init(VerbosityConfig::from_verbose_level(1));
        assert_eq!(FlistBanner::select(false, true, true), FlistBanner::None);
        assert_eq!(FlistBanner::select(false, false, true), FlistBanner::None);
        logging::init(VerbosityConfig::from_verbose_level(0));
        assert_eq!(FlistBanner::select(true, true, true), FlistBanner::None);
        assert_eq!(FlistBanner::select(true, false, true), FlistBanner::None);
    }

    /// Both banners are FINFO lines, which rwrite() drops under `--quiet`.
    #[test]
    fn quiet_prints_nothing() {
        logging::init(VerbosityConfig::from_verbose_level(1));
        logging::set_quiet(true);
        let selected = FlistBanner::select(true, false, true);
        logging::set_quiet(false);
        assert_eq!(selected, FlistBanner::None);
    }

    /// The exact bytes upstream writes for each side.
    #[test]
    fn banner_text_matches_upstream() {
        assert_eq!(
            rendered(FlistBanner::Progress, FlistSide::Sender),
            "building file list ... done\n"
        );
        assert_eq!(
            rendered(FlistBanner::Progress, FlistSide::Receiver),
            "receiving file list ... done\n"
        );
        assert_eq!(
            rendered(FlistBanner::Incremental, FlistSide::Sender),
            "sending incremental file list\n"
        );
        assert_eq!(
            rendered(FlistBanner::Incremental, FlistSide::Receiver),
            "receiving incremental file list\n"
        );
        assert_eq!(rendered(FlistBanner::None, FlistSide::Sender), "");
    }

    /// At FLIST level 2 (`-P`, `-vv`) the progress banner ends with the
    /// entry count and carries a tick for every hundredth entry - from 0 on
    /// the sender, from 100 on the receiver (flist.c:2091 vs flist.c:3253).
    #[test]
    fn flist2_reports_the_count_with_upstream_ticks() {
        let mut config = VerbosityConfig::from_verbose_level(1);
        config.info.flist = 2;
        logging::init(config);
        assert_eq!(
            rendered_count(FlistBanner::Progress, FlistSide::Sender, 10),
            "building file list ... \n 0 files...\r10 files to consider\n"
        );
        assert_eq!(
            rendered_count(FlistBanner::Progress, FlistSide::Sender, 201),
            "building file list ... \n 0 files...\r 100 files...\r 200 files...\r201 files to consider\n"
        );
        assert_eq!(
            rendered_count(FlistBanner::Progress, FlistSide::Receiver, 10),
            "receiving file list ... 10 files to consider\n"
        );
        assert_eq!(
            rendered_count(FlistBanner::Progress, FlistSide::Receiver, 200),
            "receiving file list ... \n 100 files...\r 200 files...\r200 files to consider\n"
        );
        assert_eq!(
            rendered_count(FlistBanner::Progress, FlistSide::Receiver, 1),
            "receiving file list ... 1 file to consider\n"
        );
    }
}
