use super::*;

#[test]
fn compiled_features_display_reflects_active_features() {
    let display = compiled_features_display();
    assert_eq!(display.features(), compiled_features().as_slice());
    assert_eq!(display.is_empty(), compiled_features().is_empty());
}

#[test]
fn compiled_features_display_formats_space_separated_list() {
    let display = CompiledFeaturesDisplay::new(vec![
        CompiledFeature::Acl,
        CompiledFeature::Xattr,
        CompiledFeature::Iconv,
    ]);

    assert_eq!(display.to_string(), "ACLs xattrs iconv");
}

#[test]
fn compiled_features_display_into_iter_exposes_features() {
    let mut display = CompiledFeaturesDisplay::new(vec![
        CompiledFeature::Acl,
        CompiledFeature::Xattr,
        CompiledFeature::Iconv,
    ]);

    let from_ref: Vec<_> = (&display).into_iter().copied().collect();
    assert_eq!(from_ref, display.features());

    let from_mut: Vec<_> = (&mut display).into_iter().map(|feature| *feature).collect();
    assert_eq!(from_mut, display.features());

    let owned: Vec<_> = display.clone().into_iter().collect();
    assert_eq!(owned, display.features());
}

#[test]
fn compiled_features_display_handles_empty_list() {
    let display = CompiledFeaturesDisplay::new(Vec::new());

    assert!(display.is_empty());
    assert!(display.to_string().is_empty());
}

#[test]
fn compiled_features_display_len_and_iter_match_features() {
    let display = CompiledFeaturesDisplay::new(vec![CompiledFeature::Acl, CompiledFeature::Xattr]);

    assert_eq!(display.len(), display.features().len());
    let collected: Vec<_> = display.iter().copied().collect();
    assert_eq!(collected, display.features());

    let empty = CompiledFeaturesDisplay::new(Vec::new());
    assert_eq!(empty.len(), 0);
    assert!(empty.iter().next().is_none());
}

#[test]
fn compiled_features_display_collect_from_iterator() {
    let display: CompiledFeaturesDisplay = [CompiledFeature::Acl, CompiledFeature::Iconv]
        .into_iter()
        .collect();

    assert_eq!(
        display.features(),
        &[CompiledFeature::Acl, CompiledFeature::Iconv]
    );
}

#[test]
fn compiled_features_display_extend_supports_owned_and_borrowed() {
    let mut display = CompiledFeaturesDisplay::new(vec![CompiledFeature::Acl]);
    display.extend([CompiledFeature::Xattr]);

    assert_eq!(
        display.features(),
        &[CompiledFeature::Acl, CompiledFeature::Xattr]
    );

    let borrowed = [CompiledFeature::Zstd, CompiledFeature::SdNotify];
    display.extend(borrowed.iter());

    assert_eq!(
        display.features(),
        &[
            CompiledFeature::Acl,
            CompiledFeature::Xattr,
            CompiledFeature::Zstd,
            CompiledFeature::SdNotify,
        ]
    );
}

#[test]
fn compiled_features_display_retain_filters_in_place() {
    let mut display = CompiledFeaturesDisplay::new(vec![
        CompiledFeature::Acl,
        CompiledFeature::Xattr,
        CompiledFeature::Iconv,
    ]);

    display.retain(|feature| !matches!(feature, CompiledFeature::Xattr));

    assert_eq!(
        display.features(),
        &[CompiledFeature::Acl, CompiledFeature::Iconv]
    );
}

#[test]
fn compiled_features_display_retain_can_drop_all_features() {
    let mut display = CompiledFeaturesDisplay::new(vec![CompiledFeature::Acl]);
    display.retain(|_| false);

    assert!(display.is_empty());
    assert!(display.features().is_empty());
}
