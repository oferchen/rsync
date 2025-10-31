use std::collections::VecDeque;
use std::ffi::OsString;

pub(crate) fn locate_filter_arguments(args: &[OsString]) -> (Vec<usize>, Vec<usize>) {
    let mut filter_indices = Vec::new();
    let mut rsync_filter_indices = Vec::new();
    let mut after_double_dash = false;
    let mut expect_filter_value = false;

    for (index, arg) in args.iter().enumerate().skip(1) {
        if after_double_dash {
            continue;
        }

        if expect_filter_value {
            expect_filter_value = false;
            continue;
        }

        if arg == "--" {
            after_double_dash = true;
            continue;
        }

        if arg == "--filter" {
            filter_indices.push(index);
            expect_filter_value = true;
            continue;
        }

        let value = arg.to_string_lossy();

        if value.starts_with("--filter=") {
            filter_indices.push(index);
            continue;
        }

        if value.starts_with('-') && !value.starts_with("--") && value.len() > 1 {
            for ch in value[1..].chars() {
                if ch == 'F' {
                    rsync_filter_indices.push(index);
                }
            }
        }
    }

    (filter_indices, rsync_filter_indices)
}

pub(crate) fn collect_filter_arguments(
    filters: &[OsString],
    filter_indices: &[usize],
    rsync_filter_indices: &[usize],
) -> Vec<OsString> {
    if rsync_filter_indices.is_empty() {
        return filters.to_vec();
    }

    let mut raw_queue: VecDeque<(usize, &OsString)> =
        filter_indices.iter().copied().zip(filters.iter()).collect();
    let mut alias_queue: VecDeque<(usize, usize)> = rsync_filter_indices
        .iter()
        .copied()
        .enumerate()
        .map(|(occurrence, position)| (position, occurrence))
        .collect();
    let mut merged = Vec::with_capacity(raw_queue.len() + alias_queue.len() * 2);

    while !raw_queue.is_empty() || !alias_queue.is_empty() {
        match (raw_queue.front(), alias_queue.front()) {
            (Some((raw_index, _)), Some((alias_index, _))) => {
                if alias_index <= raw_index {
                    let (_, occurrence) = alias_queue.pop_front().unwrap();
                    push_rsync_filter_shortcut(&mut merged, occurrence);
                } else {
                    let (_, value) = raw_queue.pop_front().unwrap();
                    merged.push(value.clone());
                }
            }
            (Some(_), None) => {
                let (_, value) = raw_queue.pop_front().unwrap();
                merged.push(value.clone());
            }
            (None, Some(_)) => {
                let (_, occurrence) = alias_queue.pop_front().unwrap();
                push_rsync_filter_shortcut(&mut merged, occurrence);
            }
            (None, None) => break,
        }
    }

    merged
}

fn push_rsync_filter_shortcut(target: &mut Vec<OsString>, occurrence: usize) {
    if occurrence == 0 {
        target.push(OsString::from("dir-merge /.rsync-filter"));
        target.push(OsString::from("exclude .rsync-filter"));
    } else {
        target.push(OsString::from("dir-merge .rsync-filter"));
    }
}
