//! The order in which upstream prints a local copy's per-entry lines.
//!
//! A local upstream copy runs its generator and sender as separate processes.
//! The generator walks the file lists in order. It prints its own notices
//! (`is uptodate`, the skip notices, deletions) the moment it checks an entry.
//! Every other entry it itemizes to the sender, which prints the name when it
//! handles that request. Those requests wait in the generator's output buffer
//! until the generator blocks: when it has finished a list and the next one
//! has not arrived, and at the end of the walk.
//!
//! Under INC_RECURSE each directory's children form their own list, walked
//! depth-first, and the directory itself is itemized at the start of that
//! list. The sender sends lists ahead while fewer than `MIN_FILECNT_LOOKAHEAD`
//! entries are queued, so a large tree prints its notices and names window by
//! window. Whether a small tree's next list arrives before the generator
//! finishes the current one is a race in upstream; this module takes a list as
//! arrived once the lookahead has sent it.
//!
//! # Upstream Reference
//!
//! - generator.c:2780-2790 - a directory is itemized at the start of its own
//!   list, under the gap NDX below it.
//! - generator.c:2803-2818 - the per-entry walk.
//! - generator.c:2830-2841 - without INC_RECURSE one list; with it, the
//!   generator waits for the next list before going on.
//! - io.c:1958 `wait_for_receiver()` - blocks in `perform_io()`, which flushes
//!   the buffered requests.
//! - flist.c:send_extra_file_list() - one sub-list per directory, depth-first.
//! - sender.c:515-547 - the sub-list lookahead.

use std::collections::HashMap;
use std::path::is_separator;

use protocol::flist::SubListLookahead;

use super::{ClientEvent, ClientEventKind};

/// Returns the indices of `events` in the order upstream prints their lines.
///
/// `events` are a local copy's records, each directory's entries in file-list
/// order. `inc_recurse` says whether the copy recurses incrementally. Without
/// it the whole transfer is one list: every notice comes first, then the
/// names. Hard-link aliases of a leader placed in this run come last, as
/// upstream finishes them after the regular per-file lines (hlink.c:250-251).
#[must_use]
pub fn listing_order(events: &[ClientEvent], inc_recurse: bool) -> Vec<usize> {
    let lists = FileLists::new(events, inc_recurse);
    let mut lookahead = SubListLookahead::new(lists.sizes);
    // upstream: sender.c:515 - the sender fills its lookahead before it reads
    // the first request.
    lookahead.serve(0);
    let mut order = Vec::with_capacity(events.len());
    let mut hardlinks = Vec::new();
    let mut requests: Vec<(usize, usize)> = Vec::new();
    for (list, walked) in lists.walked.iter().enumerate() {
        if list > 0 && lookahead.sent() < list {
            for (request_list, request) in requests.drain(..) {
                order.push(request);
                lookahead.serve(request_list);
            }
        }
        for &index in walked {
            let event = &events[index];
            if event.is_deferred_hardlink() {
                hardlinks.push(index);
            } else if event.is_generator_notice() {
                order.push(index);
            } else {
                requests.push((list, index));
            }
        }
    }
    order.extend(requests.into_iter().map(|(_, request)| request));
    order.extend(hardlinks);
    order
}

/// The incremental lists of a local copy, derived from its entries' paths.
struct FileLists {
    /// The events walked in each list, lists in walk order.
    walked: Vec<Vec<usize>>,
    /// Entry count of each list, in walk order.
    sizes: Vec<usize>,
}

impl FileLists {
    fn new(events: &[ClientEvent], inc_recurse: bool) -> Self {
        if !inc_recurse {
            return Self {
                walked: vec![(0..events.len()).collect()],
                sizes: vec![events.len()],
            };
        }
        // Lists are numbered by first sight here and put in walk order below.
        // List 0 holds the top-level entries; each directory owns the list of
        // its children.
        let mut ids = ListIds::default();
        let mut walked_in = Vec::with_capacity(events.len());
        for event in events {
            // A deletion is no file-list entry. The generator prints it while
            // walking a directory, where the copy recorded it.
            if matches!(event.kind(), ClientEventKind::EntryDeleted) {
                walked_in.push(walked_in.last().copied().unwrap_or(0));
                continue;
            }
            let path = event.relative_path().as_os_str().as_encoded_bytes();
            let member_of = ids.of(parent_of(path));
            ids.sizes[member_of] += 1;
            if is_directory(event) && !is_root(path) {
                let own = ids.of(path);
                ids.subdirs[member_of].push(own);
                walked_in.push(own);
            } else {
                walked_in.push(member_of);
            }
        }
        let rank = depth_first_ranks(&ids.subdirs);
        let mut walked = vec![Vec::new(); ids.sizes.len()];
        for (index, list) in walked_in.into_iter().enumerate() {
            walked[rank[list]].push(index);
        }
        let mut ranked_sizes = vec![0; ids.sizes.len()];
        for (list, size) in ids.sizes.into_iter().enumerate() {
            ranked_sizes[rank[list]] = size;
        }
        Self {
            walked,
            sizes: ranked_sizes,
        }
    }
}

/// The lists of a copy, numbered by first sight, with each one's entry count
/// and the lists of its subdirectories.
struct ListIds<'a> {
    // Keyed by the path's encoded bytes: comparing and hashing them is far
    // cheaper than `Path`'s component-wise equality and hash.
    ids: HashMap<&'a [u8], usize>,
    // Consecutive entries share a parent, so most lookups repeat the last.
    last: Option<(&'a [u8], usize)>,
    sizes: Vec<usize>,
    subdirs: Vec<Vec<usize>>,
}

impl Default for ListIds<'_> {
    fn default() -> Self {
        Self {
            ids: HashMap::new(),
            last: None,
            sizes: vec![0],
            subdirs: vec![Vec::new()],
        }
    }
}

impl<'a> ListIds<'a> {
    /// The list holding `dir`'s children.
    fn of(&mut self, key: &'a [u8]) -> usize {
        if is_root(key) {
            return 0;
        }
        if let Some((last, id)) = self.last
            && last == key
        {
            return id;
        }
        let next = self.sizes.len();
        let id = *self.ids.entry(key).or_insert(next);
        if id == next {
            self.sizes.push(0);
            self.subdirs.push(Vec::new());
        }
        self.last = Some((key, id));
        id
    }
}

/// Each list's position in the depth-first walk from list 0. A list no
/// directory event leads to (its directory was not itself an entry) follows
/// the walk in order of first sight.
fn depth_first_ranks(subdirs: &[Vec<usize>]) -> Vec<usize> {
    const UNRANKED: usize = usize::MAX;
    let mut rank = vec![UNRANKED; subdirs.len()];
    let mut next = 0;
    let mut stack = vec![0];
    while let Some(list) = stack.pop() {
        if rank[list] != UNRANKED {
            continue;
        }
        rank[list] = next;
        next += 1;
        stack.extend(subdirs[list].iter().rev());
    }
    for slot in &mut rank {
        if *slot == UNRANKED {
            *slot = next;
            next += 1;
        }
    }
    rank
}

/// The encoded bytes of `path`'s parent, empty for a top-level name. Every
/// platform separator is ASCII, so a byte scan finds them in any encoding.
fn parent_of(path: &[u8]) -> &[u8] {
    path.iter()
        .rposition(|&byte| byte.is_ascii() && is_separator(char::from(byte)))
        .map_or(&path[..0], |end| &path[..end])
}

/// Whether `path` names the transfer root.
fn is_root(path: &[u8]) -> bool {
    path.is_empty() || path == b"."
}

fn is_directory(event: &ClientEvent) -> bool {
    event.is_directory()
        || event
            .metadata()
            .is_some_and(|metadata| metadata.kind().is_directory())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use engine::local_copy::LocalCopyChangeSet;

    use super::*;
    use crate::client::summary::{ClientEntryKind, ClientEntryMetadata};

    fn file(path: &str, transferred: bool) -> ClientEvent {
        let kind = if transferred {
            ClientEventKind::DataCopied
        } else {
            ClientEventKind::MetadataReused
        };
        ClientEvent::for_test(
            PathBuf::from(path),
            kind,
            transferred,
            Some(ClientEntryMetadata::for_test(ClientEntryKind::File)),
            LocalCopyChangeSet::new(),
        )
    }

    fn dir(path: &str, created: bool) -> ClientEvent {
        ClientEvent::for_test(
            PathBuf::from(path),
            ClientEventKind::DirectoryCreated,
            created,
            Some(ClientEntryMetadata::for_test(ClientEntryKind::Directory)),
            LocalCopyChangeSet::new(),
        )
    }

    /// Renders an order the way the oracle runs were summarised: transferred
    /// names verbatim, each run of `is uptodate` files as `[NU]`, unchanged
    /// directories (which print nothing) dropped.
    fn signature(events: &[ClientEvent], inc_recurse: bool) -> String {
        let mut out = Vec::new();
        let mut uptodate = 0;
        for index in listing_order(events, inc_recurse) {
            let event = &events[index];
            if event.is_uptodate() {
                uptodate += usize::from(!is_directory(event));
                continue;
            }
            if uptodate > 0 {
                out.push(format!("[{uptodate}U]"));
                uptodate = 0;
            }
            let name = event.relative_path().display().to_string();
            out.push(if is_directory(event) {
                name + "/"
            } else {
                name
            });
        }
        if uptodate > 0 {
            out.push(format!("[{uptodate}U]"));
        }
        out.join(" ")
    }

    /// Three directories of 500 files, `f001` and `f075` changed in each,
    /// in the order a local copy records them.
    fn three_dirs_of_500() -> Vec<ClientEvent> {
        let mut events: Vec<ClientEvent> = ["x", "y", "z"].iter().map(|d| dir(d, false)).collect();
        for d in ["x", "y", "z"] {
            for i in 1..=500 {
                events.push(file(&format!("{d}/f{i:03}"), i == 1 || i == 75));
            }
        }
        events
    }

    #[test]
    fn notices_print_before_the_names_their_list_holds_back() {
        // rsync 3.5.1 -r -t -vv on this tree prints both notices before `aaa`
        // on every run: the generator writes them itself while `aaa` waits in
        // its buffer for the end of the list.
        let events = [file("aaa", true), file("bbb", false), file("zzz", false)];
        assert_eq!(signature(&events, true), "[2U] aaa");
    }

    #[test]
    fn a_large_tree_interleaves_window_by_window_like_upstream() {
        // rsync 3.5.1 -r -t -vv, 15 of 15 runs. The sender's lookahead covers
        // x and y but not z, so the generator flushes x's and y's names when
        // it reaches z, and z's at the end. A model without the lookahead
        // prints all 1494 notices first.
        assert_eq!(
            signature(&three_dirs_of_500(), true),
            "[996U] x/f001 x/f075 y/f001 y/f075 [498U] z/f001 z/f075"
        );
    }

    #[test]
    fn without_inc_recurse_every_notice_precedes_every_name() {
        assert_eq!(
            signature(&three_dirs_of_500(), false),
            "[1494U] x/f001 x/f075 y/f001 y/f075 z/f001 z/f075"
        );
    }

    #[test]
    fn a_directory_prints_at_the_start_of_its_own_list_depth_first() {
        // A copy records `b` before `a/x`, but upstream walks `a`'s sub-list,
        // then `a/x`'s, then `b`'s, itemizing each directory as its list starts.
        let events = [
            dir("a", true),
            dir("b", true),
            dir("a/x", true),
            file("a/x/f", true),
            file("b/g", true),
        ];
        assert_eq!(signature(&events, true), "a/ a/x/ a/x/f b/ b/g");
    }

    #[test]
    fn a_deletion_prints_before_names_held_back_ahead_of_it() {
        let deletion = ClientEvent::for_test(
            PathBuf::from("old"),
            ClientEventKind::EntryDeleted,
            false,
            None,
            LocalCopyChangeSet::new(),
        );
        let events = [file("aaa", true), deletion];
        let order = listing_order(&events, true);
        assert_eq!(order, [1, 0], "the generator prints a deletion at once");
    }
}
