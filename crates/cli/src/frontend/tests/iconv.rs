use super::*;
use crate::frontend::execution::resolve_iconv_setting;
use std::ffi::OsStr;

#[test]
fn resolve_iconv_setting_parses_explicit_spec() {
    let setting =
        resolve_iconv_setting(Some(OsStr::new("utf8,iso88591")), false).expect("parse iconv spec");
    assert_eq!(
        setting,
        core::client::IconvSetting::Explicit {
            local: "utf8".to_string(),
            remote: Some("iso88591".to_string()),
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
