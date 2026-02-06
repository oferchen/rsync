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

        if arg == "--filter" || arg == "-f" {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    #[test]
    fn locate_filter_arguments_empty() {
        let args: Vec<OsString> = vec![os("rsync")];
        let (filter_indices, rsync_filter_indices) = locate_filter_arguments(&args);
        assert!(filter_indices.is_empty());
        assert!(rsync_filter_indices.is_empty());
    }

    #[test]
    fn locate_filter_arguments_finds_filter() {
        let args = vec![os("rsync"), os("--filter"), os("- *.bak")];
        let (filter_indices, rsync_filter_indices) = locate_filter_arguments(&args);
        assert_eq!(filter_indices, vec![1]);
        assert!(rsync_filter_indices.is_empty());
    }

    #[test]
    fn locate_filter_arguments_finds_filter_equals() {
        let args = vec![os("rsync"), os("--filter=- *.bak")];
        let (filter_indices, _) = locate_filter_arguments(&args);
        assert_eq!(filter_indices, vec![1]);
    }

    #[test]
    fn locate_filter_arguments_finds_short_f() {
        let args = vec![os("rsync"), os("-avF")];
        let (filter_indices, rsync_filter_indices) = locate_filter_arguments(&args);
        assert!(filter_indices.is_empty());
        assert_eq!(rsync_filter_indices, vec![1]);
    }

    #[test]
    fn locate_filter_arguments_multiple_f() {
        let args = vec![os("rsync"), os("-F"), os("-F")];
        let (_, rsync_filter_indices) = locate_filter_arguments(&args);
        assert_eq!(rsync_filter_indices, vec![1, 2]);
    }

    #[test]
    fn locate_filter_arguments_stops_at_double_dash() {
        let args = vec![os("rsync"), os("--"), os("--filter"), os("pattern")];
        let (filter_indices, _) = locate_filter_arguments(&args);
        assert!(filter_indices.is_empty());
    }

    #[test]
    fn locate_filter_arguments_mixed() {
        let args = vec![
            os("rsync"),
            os("-avF"),
            os("--filter"),
            os("- *.tmp"),
            os("-F"),
        ];
        let (filter_indices, rsync_filter_indices) = locate_filter_arguments(&args);
        assert_eq!(filter_indices, vec![2]);
        assert_eq!(rsync_filter_indices, vec![1, 4]);
    }

    #[test]
    fn collect_filter_arguments_no_aliases() {
        let filters = vec![os("- *.bak"), os("+ *.txt")];
        let filter_indices = vec![1, 2];
        let rsync_filter_indices: Vec<usize> = vec![];
        let result = collect_filter_arguments(&filters, &filter_indices, &rsync_filter_indices);
        assert_eq!(result, filters);
    }

    #[test]
    fn collect_filter_arguments_first_f_alias() {
        let filters: Vec<OsString> = vec![];
        let filter_indices: Vec<usize> = vec![];
        let rsync_filter_indices = vec![1];
        let result = collect_filter_arguments(&filters, &filter_indices, &rsync_filter_indices);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], os("dir-merge /.rsync-filter"));
        assert_eq!(result[1], os("exclude .rsync-filter"));
    }

    #[test]
    fn collect_filter_arguments_second_f_alias() {
        let filters: Vec<OsString> = vec![];
        let filter_indices: Vec<usize> = vec![];
        let rsync_filter_indices = vec![1, 2];
        let result = collect_filter_arguments(&filters, &filter_indices, &rsync_filter_indices);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], os("dir-merge /.rsync-filter"));
        assert_eq!(result[1], os("exclude .rsync-filter"));
        assert_eq!(result[2], os("dir-merge .rsync-filter"));
    }

    #[test]
    fn collect_filter_arguments_interleaved() {
        let filters = vec![os("- *.bak")];
        let filter_indices = vec![3];
        let rsync_filter_indices = vec![1];
        let result = collect_filter_arguments(&filters, &filter_indices, &rsync_filter_indices);
        // -F at position 1 comes before --filter at position 3
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], os("dir-merge /.rsync-filter"));
        assert_eq!(result[1], os("exclude .rsync-filter"));
        assert_eq!(result[2], os("- *.bak"));
    }

    #[test]
    fn push_rsync_filter_shortcut_first() {
        let mut target = Vec::new();
        push_rsync_filter_shortcut(&mut target, 0);
        assert_eq!(target.len(), 2);
        assert_eq!(target[0], os("dir-merge /.rsync-filter"));
        assert_eq!(target[1], os("exclude .rsync-filter"));
    }

    #[test]
    fn push_rsync_filter_shortcut_subsequent() {
        let mut target = Vec::new();
        push_rsync_filter_shortcut(&mut target, 1);
        assert_eq!(target.len(), 1);
        assert_eq!(target[0], os("dir-merge .rsync-filter"));
    }
}
