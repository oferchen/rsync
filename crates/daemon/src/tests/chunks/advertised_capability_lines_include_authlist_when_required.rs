#[test]
fn advertised_capability_lines_include_authlist_when_required() {
    let mut definition = base_module("secure");
    definition.auth_users.push(AuthUser::new(String::from("alice")));
    definition.secrets_file = Some(PathBuf::from("secrets.txt"));
    let module = ModuleRuntime::from(definition);

    assert_eq!(
        advertised_capability_lines(&[module]),
        vec![String::from("modules authlist")]
    );
}

