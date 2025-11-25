mod fallback_error_tests {
    use super::fallback::fallback_error;

    #[test]
    fn fallback_error_tracks_call_site() {
        let call_site_line = line!() + 1;
        let error = fallback_error("call-site");
        let source = error.message().source().expect("source location missing");

        assert_eq!(source.path(), file!());
        assert_eq!(source.line(), call_site_line);
    }
}
