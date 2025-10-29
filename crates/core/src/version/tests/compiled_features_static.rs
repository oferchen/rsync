use super::*;

#[test]
fn compiled_features_static_matches_dynamic_collection() {
    let static_view = compiled_features_static();
    let collected = compiled_features();

    assert_eq!(static_view.as_slice(), collected.as_slice());
    assert_eq!(static_view.len(), collected.len());
    assert_eq!(static_view.is_empty(), collected.is_empty());
    assert_eq!(static_view.bitmap(), COMPILED_FEATURE_BITMAP);

    for feature in CompiledFeature::ALL {
        assert_eq!(static_view.contains(feature), feature.is_enabled());
    }
}

#[test]
fn compiled_features_static_iterator_preserves_ordering() {
    let static_view = compiled_features_static();
    let from_iter: Vec<_> = static_view.iter().collect();

    assert_eq!(from_iter.as_slice(), static_view.as_slice());

    let mut iter = static_view.iter();
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

    assert_eq!(reconstructed.as_slice(), static_view.as_slice());
}
