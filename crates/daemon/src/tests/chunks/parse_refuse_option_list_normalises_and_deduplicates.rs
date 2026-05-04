#[test]
fn parse_refuse_option_list_normalises_and_deduplicates() {
    let options = parse_refuse_option_list("delete, ICONV ,compress, delete")
        .expect("parse refuse option list");
    assert_eq!(options, ["delete", "iconv", "compress"]);

    let err = parse_refuse_option_list("   ,").expect_err("blank option list rejected");
    assert_eq!(err, "must specify at least one option");
}

