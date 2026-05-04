use super::*;
use crate::frontend::execution::resolve_iconv_setting;
use std::ffi::OsStr;

#[cfg(feature = "iconv")]
#[test]
fn resolve_iconv_setting_parses_explicit_spec() {
    let setting =
        resolve_iconv_setting(Some(OsStr::new("utf8,iso88591")), false).expect("parse iconv spec");
    assert_eq!(
        setting,
        core::client::IconvSetting::Explicit {
            local: "utf8".to_owned(),
            remote: Some("iso88591".to_owned()),
        }
    );
}

#[test]
fn resolve_iconv_setting_honours_disable_flag() {
    let setting = resolve_iconv_setting(None, true).expect("disable iconv");
    assert!(setting.is_disabled());
}

#[test]
fn resolve_iconv_setting_rejects_empty_spec() {
    let error =
        resolve_iconv_setting(Some(OsStr::new("   ")), false).expect_err("reject empty iconv spec");
    assert!(error.to_string().contains("iconv value must not be empty"));
}

#[cfg(not(feature = "iconv"))]
#[test]
fn resolve_iconv_setting_rejects_explicit_when_feature_disabled() {
    // Closes #1915 - --iconv must hard-fail rather than silently no-op
    // when the iconv feature is compiled out.
    let error = resolve_iconv_setting(Some(OsStr::new("utf8,iso88591")), false)
        .expect_err("reject iconv spec when feature is disabled");
    assert!(error.to_string().contains("iconv feature"));
}
