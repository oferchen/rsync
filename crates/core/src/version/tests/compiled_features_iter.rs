use super::*;

#[test]
fn compiled_features_iter_matches_collected_set() {
    let via_iter: Vec<_> = compiled_features_iter().collect();
    assert_eq!(via_iter, compiled_features());
}

#[test]
fn compiled_features_iter_rev_matches_reverse_order() {
    let forward: Vec<_> = compiled_features_iter().collect();
    let mut expected = forward;
    expected.reverse();

    let backward: Vec<_> = compiled_features_iter().rev().collect();
    assert_eq!(backward, expected);
}

#[test]
fn compiled_features_iter_is_fused_and_updates_len() {
    let mut iter = compiled_features_iter();
    let (lower, upper) = iter.size_hint();
    assert_eq!(Some(lower), upper);
    let expected = compiled_features();
    assert_eq!(lower, expected.len());
    assert_eq!(iter.len(), expected.len());
    assert_eq!(iter.len(), lower);

    while iter.next().is_some() {
        let (lower, upper) = iter.size_hint();
        assert_eq!(Some(lower), upper);
        assert_eq!(iter.len(), lower);
    }

    assert_eq!(iter.next(), None);
    assert_eq!(iter.next(), None);
    assert_eq!(iter.len(), 0);

    let mut rev_iter = compiled_features_iter();
    while rev_iter.next_back().is_some() {
        let (lower, upper) = rev_iter.size_hint();
        assert_eq!(Some(lower), upper);
        assert_eq!(rev_iter.len(), lower);
    }

    assert_eq!(rev_iter.next_back(), None);
    assert_eq!(rev_iter.len(), 0);
}

#[test]
fn compiled_features_iter_next_back_matches_reverse_collection() {
    let mut iter = compiled_features_iter();
    let mut reversed = Vec::new();

    while let Some(feature) = iter.next_back() {
        reversed.push(feature);
    }

    let expected: Vec<_> = compiled_features().into_iter().rev().collect();
    assert_eq!(reversed, expected);
}

#[test]
fn compiled_features_iter_supports_mixed_direction_iteration() {
    let expected = compiled_features();
    let mut iter = compiled_features_iter();

    let front = iter.next();
    let back = iter.next_back();
    let mut remainder: Vec<_> = iter.collect();

    let mut reconstructed = Vec::new();
    if let Some(feature) = front {
        reconstructed.push(feature);
    }

    reconstructed.append(&mut remainder);

    if let Some(feature) = back {
        reconstructed.push(feature);
    }

    assert_eq!(reconstructed, expected);
}
