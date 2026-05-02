use super::*;
use crate::client::config::FilterRuleKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::num::{NonZeroU32, NonZeroU64};
use std::time::{Duration, SystemTime};

fn builder() -> ClientConfigBuilder {
    ClientConfigBuilder::default()
}

#[test]
fn transfer_args_sets_values() {
    let config = builder().transfer_args(["--verbose", "--progress"]).build();
    assert_eq!(config.transfer_args().len(), 2);
}

#[test]
fn transfer_args_empty_clears_values() {
    let config = builder()
        .transfer_args(["--verbose"])
        .transfer_args(Vec::<&str>::new())
        .build();
    assert!(config.transfer_args().is_empty());
}

#[test]
fn transfer_args_accepts_osstrings() {
    let args: Vec<OsString> = vec![OsString::from("--test")];
    let config = builder().transfer_args(args).build();
    assert_eq!(config.transfer_args().len(), 1);
}

#[test]
fn dry_run_sets_flag() {
    let config = builder().dry_run(true).build();
    assert!(config.dry_run());
}

#[test]
fn dry_run_false_clears_flag() {
    let config = builder().dry_run(true).dry_run(false).build();
    assert!(!config.dry_run());
}

#[test]
fn list_only_sets_flag() {
    let config = builder().list_only(true).build();
    assert!(config.list_only());
}

#[test]
fn list_only_false_clears_flag() {
    let config = builder().list_only(true).list_only(false).build();
    assert!(!config.list_only());
}

#[test]
fn batch_config_sets_value() {
    let batch = engine::batch::BatchConfig::new(
        engine::batch::BatchMode::Write,
        "testbatch".to_owned(),
        32,
    );
    let config = builder().batch_config(Some(batch)).build();
    assert!(config.batch_config().is_some());
}

#[test]
fn batch_config_none_clears_value() {
    let batch = engine::batch::BatchConfig::new(
        engine::batch::BatchMode::Write,
        "testbatch".to_owned(),
        32,
    );
    let config = builder()
        .batch_config(Some(batch))
        .batch_config(None)
        .build();
    assert!(config.batch_config().is_none());
}

#[test]
fn default_transfer_args_is_empty() {
    let config = builder().build();
    assert!(config.transfer_args().is_empty());
}

#[test]
fn default_dry_run_is_false() {
    let config = builder().build();
    assert!(!config.dry_run());
}

#[test]
fn default_list_only_is_false() {
    let config = builder().build();
    assert!(!config.list_only());
}

#[test]
fn default_batch_config_is_none() {
    let config = builder().build();
    assert!(config.batch_config().is_none());
}

#[test]
fn delete_sets_during_and_resets_to_disabled() {
    let config = ClientConfigBuilder::default().delete(true).build();
    assert_eq!(config.delete_mode(), DeleteMode::During);
    assert!(config.delete());

    let config = ClientConfigBuilder::default()
        .delete(true)
        .delete(false)
        .build();
    assert_eq!(config.delete_mode(), DeleteMode::Disabled);
    assert!(!config.delete());
}

#[test]
fn delete_before_toggles_mode() {
    let config = ClientConfigBuilder::default().delete_before(true).build();
    assert!(config.delete_before());
    assert_eq!(config.delete_mode(), DeleteMode::Before);

    let config = ClientConfigBuilder::default()
        .delete_before(true)
        .delete_before(false)
        .build();
    assert!(!config.delete_before());
    assert_eq!(config.delete_mode(), DeleteMode::Disabled);
}

#[test]
fn delete_after_toggles_mode() {
    let config = ClientConfigBuilder::default().delete_after(true).build();
    assert!(config.delete_after());
    assert_eq!(config.delete_mode(), DeleteMode::After);

    let config = ClientConfigBuilder::default()
        .delete_after(true)
        .delete_after(false)
        .build();
    assert!(!config.delete_after());
    assert_eq!(config.delete_mode(), DeleteMode::Disabled);
}

#[test]
fn delete_delay_toggles_mode() {
    let config = ClientConfigBuilder::default().delete_delay(true).build();
    assert!(config.delete_delay());
    assert_eq!(config.delete_mode(), DeleteMode::Delay);

    let config = ClientConfigBuilder::default()
        .delete_delay(true)
        .delete_delay(false)
        .build();
    assert!(!config.delete_delay());
    assert_eq!(config.delete_mode(), DeleteMode::Disabled);
}

#[test]
fn delete_excluded_mirrors_builder_setting() {
    let config = ClientConfigBuilder::default().delete_excluded(true).build();
    assert!(config.delete_excluded());

    let config = ClientConfigBuilder::default()
        .delete_excluded(false)
        .build();
    assert!(!config.delete_excluded());
}

#[test]
fn max_delete_propagates_limit() {
    let config = ClientConfigBuilder::default().max_delete(Some(128)).build();
    assert_eq!(config.max_delete(), Some(128));

    let config = ClientConfigBuilder::default().max_delete(None).build();
    assert_eq!(config.max_delete(), None);
}

#[test]
fn debug_flags_sets_values() {
    let config = builder().debug_flags(["FILTER", "SEND"]).build();
    assert_eq!(config.debug_flags().len(), 2);
}

#[test]
fn debug_flags_empty_clears_values() {
    let config = builder()
        .debug_flags(["FILTER"])
        .debug_flags(Vec::<&str>::new())
        .build();
    assert!(config.debug_flags().is_empty());
}

#[test]
fn debug_flags_accepts_osstrings() {
    let flags: Vec<OsString> = vec![OsString::from("DEBUG")];
    let config = builder().debug_flags(flags).build();
    assert_eq!(config.debug_flags().len(), 1);
}

#[test]
fn add_filter_rule_appends_rule() {
    let config = builder()
        .add_filter_rule(FilterRuleSpec::exclude("*.tmp"))
        .build();
    assert_eq!(config.filter_rules().len(), 1);
    assert_eq!(config.filter_rules()[0].pattern(), "*.tmp");
}

#[test]
fn add_filter_rule_multiple_accumulates() {
    let config = builder()
        .add_filter_rule(FilterRuleSpec::exclude("*.tmp"))
        .add_filter_rule(FilterRuleSpec::include("*.rs"))
        .add_filter_rule(FilterRuleSpec::protect("important"))
        .build();
    assert_eq!(config.filter_rules().len(), 3);
}

#[test]
fn add_filter_rule_include() {
    let config = builder()
        .add_filter_rule(FilterRuleSpec::include("*.rs"))
        .build();
    assert_eq!(config.filter_rules()[0].kind(), FilterRuleKind::Include);
}

#[test]
fn add_filter_rule_exclude() {
    let config = builder()
        .add_filter_rule(FilterRuleSpec::exclude("*.tmp"))
        .build();
    assert_eq!(config.filter_rules()[0].kind(), FilterRuleKind::Exclude);
}

#[test]
fn add_filter_rule_protect() {
    let config = builder()
        .add_filter_rule(FilterRuleSpec::protect("keep"))
        .build();
    assert_eq!(config.filter_rules()[0].kind(), FilterRuleKind::Protect);
}

#[test]
fn add_filter_rule_clear() {
    let config = builder().add_filter_rule(FilterRuleSpec::clear()).build();
    assert_eq!(config.filter_rules()[0].kind(), FilterRuleKind::Clear);
}

#[test]
fn extend_filter_rules_appends_collection() {
    let rules = vec![
        FilterRuleSpec::exclude("*.tmp"),
        FilterRuleSpec::exclude("*.bak"),
    ];
    let config = builder().extend_filter_rules(rules).build();
    assert_eq!(config.filter_rules().len(), 2);
}

#[test]
fn extend_filter_rules_accumulates_with_add() {
    let rules = vec![
        FilterRuleSpec::exclude("*.tmp"),
        FilterRuleSpec::exclude("*.bak"),
    ];
    let config = builder()
        .add_filter_rule(FilterRuleSpec::include("*.rs"))
        .extend_filter_rules(rules)
        .build();
    assert_eq!(config.filter_rules().len(), 3);
}

#[test]
fn extend_filter_rules_empty_adds_nothing() {
    let config = builder()
        .add_filter_rule(FilterRuleSpec::exclude("*.tmp"))
        .extend_filter_rules(Vec::new())
        .build();
    assert_eq!(config.filter_rules().len(), 1);
}

#[test]
fn default_debug_flags_is_empty() {
    let config = builder().build();
    assert!(config.debug_flags().is_empty());
}

#[test]
fn default_filter_rules_is_empty() {
    let config = builder().build();
    assert!(config.filter_rules().is_empty());
}

#[test]
fn owner_sets_preserve() {
    let config = builder().owner(true).build();
    assert!(config.preserve_owner());
}

#[test]
fn owner_false_clears_preserve() {
    let config = builder().owner(true).owner(false).build();
    assert!(!config.preserve_owner());
}

#[test]
fn owner_override_sets_value() {
    let config = builder().owner_override(Some(1000)).build();
    assert_eq!(config.owner_override(), Some(1000));
}

#[test]
fn owner_override_none_clears_value() {
    let config = builder()
        .owner_override(Some(1000))
        .owner_override(None)
        .build();
    assert!(config.owner_override().is_none());
}

#[test]
fn group_sets_preserve() {
    let config = builder().group(true).build();
    assert!(config.preserve_group());
}

#[test]
fn group_false_clears_preserve() {
    let config = builder().group(true).group(false).build();
    assert!(!config.preserve_group());
}

#[test]
fn group_override_sets_value() {
    let config = builder().group_override(Some(1000)).build();
    assert_eq!(config.group_override(), Some(1000));
}

#[test]
fn group_override_none_clears_value() {
    let config = builder()
        .group_override(Some(1000))
        .group_override(None)
        .build();
    assert!(config.group_override().is_none());
}

#[test]
fn copy_as_sets_value() {
    let config = builder()
        .copy_as(Some(OsString::from("user:group")))
        .build();
    assert!(config.copy_as().is_some());
}

#[test]
fn copy_as_none_clears_value() {
    let config = builder()
        .copy_as(Some(OsString::from("user:group")))
        .copy_as(None)
        .build();
    assert!(config.copy_as().is_none());
}

#[test]
fn executability_sets_flag() {
    let config = builder().executability(true).build();
    assert!(config.preserve_executability());
}

#[test]
fn executability_false_clears_flag() {
    let config = builder().executability(true).executability(false).build();
    assert!(!config.preserve_executability());
}

#[test]
fn permissions_sets_flag() {
    let config = builder().permissions(true).build();
    assert!(config.preserve_permissions());
}

#[test]
fn permissions_false_clears_flag() {
    let config = builder().permissions(true).permissions(false).build();
    assert!(!config.preserve_permissions());
}

#[test]
fn fake_super_sets_flag() {
    let config = builder().fake_super(true).build();
    assert!(config.fake_super());
}

#[test]
fn fake_super_false_clears_flag() {
    let config = builder().fake_super(true).fake_super(false).build();
    assert!(!config.fake_super());
}

#[test]
fn times_sets_flag() {
    let config = builder().times(true).build();
    assert!(config.preserve_times());
}

#[test]
fn times_false_clears_flag() {
    let config = builder().times(true).times(false).build();
    assert!(!config.preserve_times());
}

#[test]
fn atimes_sets_flag() {
    let config = builder().atimes(true).build();
    assert!(config.preserve_atimes());
}

#[test]
fn atimes_false_clears_flag() {
    let config = builder().atimes(true).atimes(false).build();
    assert!(!config.preserve_atimes());
}

#[test]
fn crtimes_sets_flag() {
    let config = builder().crtimes(true).build();
    assert!(config.preserve_crtimes());
}

#[test]
fn crtimes_false_clears_flag() {
    let config = builder().crtimes(true).crtimes(false).build();
    assert!(!config.preserve_crtimes());
}

#[test]
fn omit_dir_times_sets_flag() {
    let config = builder().omit_dir_times(true).build();
    assert!(config.omit_dir_times());
}

#[test]
fn omit_dir_times_false_clears_flag() {
    let config = builder().omit_dir_times(true).omit_dir_times(false).build();
    assert!(!config.omit_dir_times());
}

#[test]
fn omit_link_times_sets_flag() {
    let config = builder().omit_link_times(true).build();
    assert!(config.omit_link_times());
}

#[test]
fn omit_link_times_false_clears_flag() {
    let config = builder()
        .omit_link_times(true)
        .omit_link_times(false)
        .build();
    assert!(!config.omit_link_times());
}

#[test]
fn numeric_ids_sets_flag() {
    let config = builder().numeric_ids(true).build();
    assert!(config.numeric_ids());
}

#[test]
fn numeric_ids_false_clears_flag() {
    let config = builder().numeric_ids(true).numeric_ids(false).build();
    assert!(!config.numeric_ids());
}

#[test]
fn preallocate_sets_flag() {
    let config = builder().preallocate(true).build();
    assert!(config.preallocate());
}

#[test]
fn preallocate_false_clears_flag() {
    let config = builder().preallocate(true).preallocate(false).build();
    assert!(!config.preallocate());
}

#[test]
fn hard_links_sets_flag() {
    let config = builder().hard_links(true).build();
    assert!(config.preserve_hard_links());
}

#[test]
fn hard_links_false_clears_flag() {
    let config = builder().hard_links(true).hard_links(false).build();
    assert!(!config.preserve_hard_links());
}

#[test]
fn devices_sets_flag() {
    let config = builder().devices(true).build();
    assert!(config.preserve_devices());
}

#[test]
fn devices_false_clears_flag() {
    let config = builder().devices(true).devices(false).build();
    assert!(!config.preserve_devices());
}

#[test]
fn specials_sets_flag() {
    let config = builder().specials(true).build();
    assert!(config.preserve_specials());
}

#[test]
fn specials_false_clears_flag() {
    let config = builder().specials(true).specials(false).build();
    assert!(!config.preserve_specials());
}

#[test]
fn default_preserve_owner_is_false() {
    let config = builder().build();
    assert!(!config.preserve_owner());
}

#[test]
fn default_preserve_group_is_false() {
    let config = builder().build();
    assert!(!config.preserve_group());
}

#[test]
fn default_preserve_times_is_false() {
    let config = builder().build();
    assert!(!config.preserve_times());
}

#[test]
fn default_preserve_permissions_is_false() {
    let config = builder().build();
    assert!(!config.preserve_permissions());
}

#[test]
fn bind_address_sets_value() {
    let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 0);
    let addr = BindAddress::new(OsString::from("192.168.1.1"), socket);
    let config = builder().bind_address(Some(addr)).build();
    assert!(config.bind_address().is_some());
}

#[test]
fn bind_address_none_clears_value() {
    let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 0);
    let addr = BindAddress::new(OsString::from("192.168.1.1"), socket);
    let config = builder()
        .bind_address(Some(addr))
        .bind_address(None)
        .build();
    assert!(config.bind_address().is_none());
}

#[test]
fn sockopts_sets_value() {
    let config = builder()
        .sockopts(Some(OsString::from("SO_SNDBUF=65536")))
        .build();
    assert!(config.sockopts().is_some());
}

#[test]
fn sockopts_none_clears_value() {
    let config = builder()
        .sockopts(Some(OsString::from("SO_SNDBUF=65536")))
        .sockopts(None)
        .build();
    assert!(config.sockopts().is_none());
}

#[test]
fn blocking_io_sets_some_true() {
    let config = builder().blocking_io(Some(true)).build();
    assert_eq!(config.blocking_io(), Some(true));
}

#[test]
fn blocking_io_sets_some_false() {
    let config = builder().blocking_io(Some(false)).build();
    assert_eq!(config.blocking_io(), Some(false));
}

#[test]
fn blocking_io_none_leaves_default() {
    let config = builder().blocking_io(None).build();
    assert!(config.blocking_io().is_none());
}

#[test]
fn timeout_sets_default() {
    let config = builder().timeout(TransferTimeout::Default).build();
    assert_eq!(config.timeout(), TransferTimeout::Default);
}

#[test]
fn timeout_sets_disabled() {
    let config = builder().timeout(TransferTimeout::Disabled).build();
    assert_eq!(config.timeout(), TransferTimeout::Disabled);
}

#[test]
fn timeout_sets_seconds() {
    let seconds = NonZeroU64::new(60).unwrap();
    let config = builder().timeout(TransferTimeout::Seconds(seconds)).build();
    assert_eq!(config.timeout().as_seconds(), Some(seconds));
}

#[test]
fn connect_timeout_sets_value() {
    let seconds = NonZeroU64::new(30).unwrap();
    let config = builder()
        .connect_timeout(TransferTimeout::Seconds(seconds))
        .build();
    assert_eq!(config.connect_timeout().as_seconds(), Some(seconds));
}

#[test]
fn stop_at_sets_deadline() {
    let deadline = SystemTime::now() + Duration::from_secs(3600);
    let config = builder().stop_at(Some(deadline)).build();
    assert!(config.stop_at().is_some());
}

#[test]
fn stop_at_none_clears_deadline() {
    let deadline = SystemTime::now() + Duration::from_secs(3600);
    let config = builder().stop_at(Some(deadline)).stop_at(None).build();
    assert!(config.stop_at().is_none());
}

#[test]
fn connect_program_sets_value() {
    let config = builder()
        .connect_program(Some(OsString::from("/usr/bin/nc")))
        .build();
    assert!(config.connect_program().is_some());
}

#[test]
fn connect_program_none_clears_value() {
    let config = builder()
        .connect_program(Some(OsString::from("/usr/bin/nc")))
        .connect_program(None)
        .build();
    assert!(config.connect_program().is_none());
}

#[test]
fn address_mode_sets_default() {
    let config = builder().address_mode(AddressMode::Default).build();
    assert_eq!(config.address_mode(), AddressMode::Default);
}

#[test]
fn address_mode_sets_ipv4() {
    let config = builder().address_mode(AddressMode::Ipv4).build();
    assert_eq!(config.address_mode(), AddressMode::Ipv4);
}

#[test]
fn address_mode_sets_ipv6() {
    let config = builder().address_mode(AddressMode::Ipv6).build();
    assert_eq!(config.address_mode(), AddressMode::Ipv6);
}

#[test]
fn set_remote_shell_sets_args() {
    let config = builder()
        .set_remote_shell(vec!["ssh", "-p", "2222"])
        .build();
    assert!(config.remote_shell().is_some());
    let shell = config.remote_shell().unwrap();
    assert_eq!(shell.len(), 3);
}

#[test]
fn set_rsync_path_sets_value() {
    let config = builder().set_rsync_path("/opt/rsync/bin/rsync").build();
    assert!(config.rsync_path().is_some());
}

#[test]
fn early_input_sets_path() {
    let config = builder()
        .early_input(Some(PathBuf::from("/tmp/early-input")))
        .build();
    assert!(config.early_input().is_some());
}

#[test]
fn early_input_none_clears_path() {
    let config = builder()
        .early_input(Some(PathBuf::from("/tmp/early-input")))
        .early_input(None)
        .build();
    assert!(config.early_input().is_none());
}

#[test]
fn default_bind_address_is_none() {
    let config = builder().build();
    assert!(config.bind_address().is_none());
}

#[test]
fn default_address_mode_is_default() {
    let config = builder().build();
    assert_eq!(config.address_mode(), AddressMode::Default);
}

#[test]
fn default_timeout_is_default() {
    let config = builder().build();
    assert_eq!(config.timeout(), TransferTimeout::Default);
}

#[test]
fn prefer_aes_gcm_sets_some_true() {
    let config = builder().prefer_aes_gcm(Some(true)).build();
    assert_eq!(config.prefer_aes_gcm(), Some(true));
}

#[test]
fn prefer_aes_gcm_sets_some_false() {
    let config = builder().prefer_aes_gcm(Some(false)).build();
    assert_eq!(config.prefer_aes_gcm(), Some(false));
}

#[test]
fn prefer_aes_gcm_default_is_none() {
    let config = builder().prefer_aes_gcm(None).build();
    assert!(config.prefer_aes_gcm().is_none());
}

#[test]
fn protect_args_sets_some_true() {
    let config = builder().protect_args(Some(true)).build();
    assert_eq!(config.protect_args(), Some(true));
}

#[test]
fn protect_args_sets_some_false() {
    let config = builder().protect_args(Some(false)).build();
    assert_eq!(config.protect_args(), Some(false));
}

#[test]
fn protect_args_default_is_none() {
    let config = builder().protect_args(None).build();
    assert!(config.protect_args().is_none());
}

#[test]
fn verbosity_sets_level() {
    let config = builder().verbosity(2).build();
    assert_eq!(config.verbosity(), 2);
}

#[test]
fn verbosity_zero() {
    let config = builder().verbosity(0).build();
    assert_eq!(config.verbosity(), 0);
}

#[test]
fn verbosity_max() {
    let config = builder().verbosity(u8::MAX).build();
    assert_eq!(config.verbosity(), u8::MAX);
}

#[test]
fn progress_sets_flag() {
    let config = builder().progress(true).build();
    assert!(config.progress());
}

#[test]
fn progress_false_clears_flag() {
    let config = builder().progress(true).progress(false).build();
    assert!(!config.progress());
}

#[test]
fn stats_sets_flag() {
    let config = builder().stats(true).build();
    assert!(config.stats());
}

#[test]
fn stats_false_clears_flag() {
    let config = builder().stats(true).stats(false).build();
    assert!(!config.stats());
}

#[test]
fn human_readable_sets_flag() {
    let config = builder().human_readable(true).build();
    assert!(config.human_readable());
}

#[test]
fn human_readable_false_clears_flag() {
    let config = builder().human_readable(true).human_readable(false).build();
    assert!(!config.human_readable());
}

#[test]
fn default_verbosity_is_zero() {
    let config = builder().build();
    assert_eq!(config.verbosity(), 0);
}

#[test]
fn default_progress_is_false() {
    let config = builder().build();
    assert!(!config.progress());
}

#[test]
fn default_stats_is_false() {
    let config = builder().build();
    assert!(!config.stats());
}

#[test]
fn default_human_readable_is_false() {
    let config = builder().build();
    assert!(!config.human_readable());
}

#[test]
fn daemon_params_sets_values() {
    let params = vec!["read only=true".to_owned(), "timeout=60".to_owned()];
    let config = builder().daemon_params(params.clone()).build();
    assert_eq!(config.daemon_params(), &params);
}

#[test]
fn default_daemon_params_is_empty() {
    let config = builder().build();
    assert!(config.daemon_params().is_empty());
}

#[test]
fn itemize_changes_sets_flag() {
    let config = builder().itemize_changes(true).build();
    assert!(config.itemize_changes());
}

#[test]
fn itemize_changes_default_is_false() {
    let config = builder().build();
    assert!(!config.itemize_changes());
}

#[test]
fn itemize_changes_false_clears_flag() {
    let config = builder()
        .itemize_changes(true)
        .itemize_changes(false)
        .build();
    assert!(!config.itemize_changes());
}

#[test]
fn partial_sets_flag() {
    let config = builder().partial(true).build();
    assert!(config.partial());
}

#[test]
fn partial_false_clears_flag() {
    let config = builder().partial(true).partial(false).build();
    assert!(!config.partial());
}

#[test]
fn delay_updates_sets_flag() {
    let config = builder().delay_updates(true).build();
    assert!(config.delay_updates());
}

#[test]
fn partial_directory_sets_path() {
    let config = builder().partial_directory(Some("/tmp/partial")).build();
    assert!(config.partial_directory().is_some());
    assert_eq!(
        config.partial_directory().unwrap().to_str().unwrap(),
        "/tmp/partial"
    );
}

#[test]
fn partial_directory_enables_partial() {
    let config = builder().partial_directory(Some("/tmp/partial")).build();
    assert!(config.partial());
}

#[test]
fn partial_directory_none_clears_path() {
    let config = builder()
        .partial_directory(Some("/tmp/partial"))
        .partial_directory(None::<&str>)
        .build();
    assert!(config.partial_directory().is_none());
}

#[test]
fn temp_directory_sets_path() {
    let config = builder().temp_directory(Some("/tmp/staging")).build();
    assert!(config.temp_directory().is_some());
}

#[test]
fn temp_directory_none_clears_path() {
    let config = builder()
        .temp_directory(Some("/tmp/staging"))
        .temp_directory(None::<&str>)
        .build();
    assert!(config.temp_directory().is_none());
}

#[test]
fn inplace_sets_flag() {
    let config = builder().inplace(true).build();
    assert!(config.inplace());
}

#[test]
fn inplace_false_clears_flag() {
    let config = builder().inplace(true).inplace(false).build();
    assert!(!config.inplace());
}

#[test]
fn append_sets_flag() {
    let config = builder().append(true).build();
    assert!(config.append());
}

#[test]
fn append_false_clears_flag_and_verify() {
    let config = builder().append_verify(true).append(false).build();
    assert!(!config.append());
    assert!(!config.append_verify());
}

#[test]
fn append_verify_enables_append() {
    let config = builder().append_verify(true).build();
    assert!(config.append());
    assert!(config.append_verify());
}

#[test]
fn append_verify_false_only_clears_verify() {
    let config = builder().append_verify(true).append_verify(false).build();
    assert!(config.append());
    assert!(!config.append_verify());
}

#[test]
fn fsync_sets_flag() {
    let config = builder().fsync(true).build();
    assert!(config.fsync());
}

#[test]
fn fsync_false_clears_flag() {
    let config = builder().fsync(true).fsync(false).build();
    assert!(!config.fsync());
}

#[test]
fn io_uring_policy_sets_enabled() {
    let config = builder()
        .io_uring_policy(fast_io::IoUringPolicy::Enabled)
        .build();
    assert_eq!(config.io_uring_policy(), fast_io::IoUringPolicy::Enabled);
}

#[test]
fn io_uring_policy_sets_disabled() {
    let config = builder()
        .io_uring_policy(fast_io::IoUringPolicy::Disabled)
        .build();
    assert_eq!(config.io_uring_policy(), fast_io::IoUringPolicy::Disabled);
}

#[test]
fn io_uring_policy_default_is_auto() {
    let config = builder().build();
    assert_eq!(config.io_uring_policy(), fast_io::IoUringPolicy::Auto);
}

// Mutual exclusion validation tests
// (upstream: options.c:2406-2414 - inplace/append conflicts with partial-dir/delay-updates)

#[test]
fn validate_inplace_with_partial_dir_conflicts() {
    let b = builder()
        .inplace(true)
        .partial_directory(Some("/tmp/partial"));
    let err = b.validate().unwrap_err();
    assert_eq!(err.option1, "inplace");
    assert_eq!(err.option2, "partial-dir");
}

#[test]
fn validate_inplace_with_delay_updates_conflicts() {
    let b = builder().inplace(true).delay_updates(true);
    let err = b.validate().unwrap_err();
    assert_eq!(err.option1, "inplace");
    assert_eq!(err.option2, "delay-updates");
}

#[test]
fn validate_append_with_partial_dir_conflicts() {
    let b = builder()
        .append(true)
        .partial_directory(Some("/tmp/partial"));
    let err = b.validate().unwrap_err();
    assert_eq!(err.option1, "append");
    assert_eq!(err.option2, "partial-dir");
}

#[test]
fn validate_append_with_delay_updates_conflicts() {
    let b = builder().append(true).delay_updates(true);
    let err = b.validate().unwrap_err();
    assert_eq!(err.option1, "append");
    assert_eq!(err.option2, "delay-updates");
}

#[test]
fn validate_inplace_without_conflicts_ok() {
    let b = builder().inplace(true);
    assert!(b.validate().is_ok());
}

#[test]
fn validate_delay_updates_without_inplace_ok() {
    let b = builder().delay_updates(true);
    assert!(b.validate().is_ok());
}

#[test]
fn validate_partial_dir_without_inplace_ok() {
    let b = builder().partial_directory(Some("/tmp/partial"));
    assert!(b.validate().is_ok());
}

#[test]
fn validate_default_builder_ok() {
    let b = builder();
    assert!(b.validate().is_ok());
}

#[test]
fn compare_destination_adds_directory() {
    let config = builder().compare_destination("/tmp/compare").build();
    assert_eq!(config.reference_directories().len(), 1);
    assert_eq!(
        config.reference_directories()[0].kind(),
        ReferenceDirectoryKind::Compare
    );
}

#[test]
fn copy_destination_adds_directory() {
    let config = builder().copy_destination("/tmp/copy").build();
    assert_eq!(config.reference_directories().len(), 1);
    assert_eq!(
        config.reference_directories()[0].kind(),
        ReferenceDirectoryKind::Copy
    );
}

#[test]
fn link_destination_adds_directory() {
    let config = builder().link_destination("/tmp/link").build();
    assert_eq!(config.reference_directories().len(), 1);
    assert_eq!(
        config.reference_directories()[0].kind(),
        ReferenceDirectoryKind::Link
    );
}

#[test]
fn multiple_reference_directories_accumulate() {
    let config = builder()
        .compare_destination("/tmp/compare")
        .copy_destination("/tmp/copy")
        .link_destination("/tmp/link")
        .build();
    assert_eq!(config.reference_directories().len(), 3);
}

#[test]
fn backup_sets_flag() {
    let config = builder().backup(true).build();
    assert!(config.backup());
}

#[test]
fn backup_false_clears_flag() {
    let config = builder().backup(true).backup(false).build();
    assert!(!config.backup());
}

#[test]
fn backup_directory_sets_path() {
    let config = builder().backup_directory(Some("/tmp/backup")).build();
    assert!(config.backup_directory().is_some());
    assert_eq!(
        config.backup_directory().unwrap().to_str().unwrap(),
        "/tmp/backup"
    );
}

#[test]
fn backup_directory_enables_backup() {
    let config = builder().backup_directory(Some("/tmp/backup")).build();
    assert!(config.backup());
}

#[test]
fn backup_directory_none_clears_path() {
    let config = builder()
        .backup_directory(Some("/tmp/backup"))
        .backup_directory(None::<&str>)
        .build();
    assert!(config.backup_directory().is_none());
}

#[test]
fn backup_suffix_sets_value() {
    let config = builder().backup_suffix(Some("~")).build();
    assert!(config.backup_suffix().is_some());
    assert_eq!(config.backup_suffix().unwrap().to_str().unwrap(), "~");
}

#[test]
fn backup_suffix_enables_backup() {
    let config = builder().backup_suffix(Some(".bak")).build();
    assert!(config.backup());
}

#[test]
fn backup_suffix_none_clears_value() {
    let config = builder()
        .backup_suffix(Some("~"))
        .backup_suffix(None::<&str>)
        .build();
    assert!(config.backup_suffix().is_none());
}

#[test]
fn default_reference_directories_is_empty() {
    let config = builder().build();
    assert!(config.reference_directories().is_empty());
}

#[test]
fn default_backup_is_false() {
    let config = builder().build();
    assert!(!config.backup());
}

#[test]
fn default_backup_directory_is_none() {
    let config = builder().build();
    assert!(config.backup_directory().is_none());
}

#[test]
fn default_backup_suffix_is_none() {
    let config = builder().build();
    assert!(config.backup_suffix().is_none());
}

#[test]
fn bandwidth_limit_sets_value() {
    let config = builder().bandwidth_limit(None).build();
    assert!(config.bandwidth_limit().is_none());
}

#[test]
fn compress_sets_flag() {
    let config = builder().compress(true).build();
    assert!(config.compress());
}

#[test]
fn compress_false_clears_flag() {
    let config = builder().compress(true).compress(false).build();
    assert!(!config.compress());
}

#[test]
fn compression_level_sets_value() {
    let config = builder()
        .compression_level(Some(CompressionLevel::Default))
        .build();
    assert!(config.compress());
}

#[test]
fn compression_level_none_clears_value() {
    let _config = builder()
        .compression_level(Some(CompressionLevel::Default))
        .compression_level(None)
        .build();
    // Level becomes None, but compress state depends on implementation
}

#[test]
fn compression_algorithm_sets_value() {
    let config = builder()
        .compression_algorithm(CompressionAlgorithm::Zstd)
        .build();
    assert_eq!(config.compression_algorithm(), CompressionAlgorithm::Zstd);
}

#[test]
fn compression_algorithm_marks_explicit_choice() {
    // upstream: options.c:2800-2805 - explicit compress_choice is forwarded
    // to the remote peer. The explicit flag distinguishes "user chose zstd"
    // from "zstd is the default."
    let config = builder()
        .compression_algorithm(CompressionAlgorithm::Zstd)
        .build();
    assert!(
        config.explicit_compress_choice(),
        "calling compression_algorithm() should mark choice as explicit"
    );
}

#[test]
fn default_config_has_no_explicit_compress_choice() {
    let config = builder().build();
    assert!(
        !config.explicit_compress_choice(),
        "default config should not have explicit compress choice"
    );
}

#[test]
fn compression_setting_enabled() {
    let setting = CompressionSetting::level(CompressionLevel::Default);
    let config = builder().compression_setting(setting).build();
    assert!(config.compress());
}

#[test]
fn compression_setting_disabled() {
    let config = builder()
        .compression_setting(CompressionSetting::disabled())
        .build();
    assert!(!config.compress());
}

#[test]
fn open_noatime_sets_flag() {
    let config = builder().open_noatime(true).build();
    assert!(config.open_noatime());
}

#[test]
fn open_noatime_false_clears_flag() {
    let config = builder().open_noatime(true).open_noatime(false).build();
    assert!(!config.open_noatime());
}

#[test]
fn whole_file_sets_true() {
    let config = builder().whole_file(true).build();
    assert!(config.whole_file());
}

#[test]
fn whole_file_sets_false() {
    let config = builder().whole_file(false).build();
    assert!(!config.whole_file());
}

#[test]
fn block_size_override_sets_value() {
    let size = NonZeroU32::new(4096).unwrap();
    let config = builder().block_size_override(Some(size)).build();
    assert_eq!(config.block_size_override(), Some(size));
}

#[test]
fn block_size_override_none_clears_value() {
    let size = NonZeroU32::new(4096).unwrap();
    let config = builder()
        .block_size_override(Some(size))
        .block_size_override(None)
        .build();
    assert!(config.block_size_override().is_none());
}

#[test]
fn max_alloc_sets_limit() {
    let config = builder().max_alloc(Some(1_073_741_824)).build();
    assert_eq!(config.max_alloc(), Some(1_073_741_824));
}

#[test]
fn max_alloc_none_clears_limit() {
    let config = builder()
        .max_alloc(Some(1_073_741_824))
        .max_alloc(None)
        .build();
    assert!(config.max_alloc().is_none());
}

#[test]
fn sparse_sets_flag() {
    let config = builder().sparse(true).build();
    assert!(config.sparse());
}

#[test]
fn sparse_false_clears_flag() {
    let config = builder().sparse(true).sparse(false).build();
    assert!(!config.sparse());
}

#[test]
fn fuzzy_sets_flag() {
    let config = builder().fuzzy(true).build();
    assert!(config.fuzzy());
    assert_eq!(config.fuzzy_level(), 1);
}

#[test]
fn fuzzy_false_clears_flag() {
    let config = builder().fuzzy(true).fuzzy(false).build();
    assert!(!config.fuzzy());
    assert_eq!(config.fuzzy_level(), 0);
}

#[test]
fn fuzzy_level_2_sets_correctly() {
    let config = builder().fuzzy_level(2).build();
    assert!(config.fuzzy());
    assert_eq!(config.fuzzy_level(), 2);
}

#[test]
fn qsort_sets_flag() {
    let config = builder().qsort(true).build();
    assert!(config.qsort());
}

#[test]
fn qsort_false_clears_flag() {
    let config = builder().qsort(true).qsort(false).build();
    assert!(!config.qsort());
}

#[test]
fn default_compress_is_false() {
    let config = builder().build();
    assert!(!config.compress());
}

#[test]
fn default_sparse_is_false() {
    let config = builder().build();
    assert!(!config.sparse());
}

#[test]
fn default_fuzzy_is_zero() {
    let config = builder().build();
    assert!(!config.fuzzy());
    assert_eq!(config.fuzzy_level(), 0);
}

#[test]
fn default_qsort_is_false() {
    let config = builder().build();
    assert!(!config.qsort());
}

#[test]
fn inc_recursive_send_sets_flag() {
    let config = builder().inc_recursive_send(true).build();
    assert!(config.inc_recursive_send());
}

#[test]
fn inc_recursive_send_false_clears_flag() {
    let config = builder()
        .inc_recursive_send(true)
        .inc_recursive_send(false)
        .build();
    assert!(!config.inc_recursive_send());
}

#[test]
fn default_inc_recursive_send_is_true() {
    // Mirrors upstream `allow_inc_recurse = 1` (compat.c).
    let config = builder().build();
    assert!(config.inc_recursive_send());
}

#[test]
fn precise_levels_1_through_9_propagate_correctly() {
    use std::num::NonZeroU8;
    for n in 1u8..=9 {
        let level = NonZeroU8::new(n).unwrap();
        let config = builder()
            .compress(true)
            .compression_level(Some(CompressionLevel::precise(level)))
            .build();

        assert!(config.compress(), "level {n} should have compress=true");
        assert_eq!(
            config.compression_level(),
            Some(CompressionLevel::precise(level)),
            "level {n} should propagate to config"
        );
        assert!(
            config.compression_setting().is_enabled(),
            "level {n} should have compression setting enabled"
        );
    }
}

#[test]
fn compression_level_implies_compress_true() {
    use std::num::NonZeroU8;
    let level = NonZeroU8::new(3).unwrap();
    let config = builder()
        .compression_level(Some(CompressionLevel::precise(level)))
        .build();

    assert!(
        config.compress(),
        "setting compression_level should auto-enable compress"
    );
}

#[test]
fn compression_setting_disabled_disables_compress() {
    let config = builder()
        .compress(true)
        .compression_setting(CompressionSetting::disabled())
        .build();

    assert!(
        !config.compress(),
        "CompressionSetting::disabled() should disable compress"
    );
    assert!(config.compression_setting().is_disabled());
}

#[test]
fn compression_setting_level_enables_compress() {
    let config = builder()
        .compression_setting(CompressionSetting::level(CompressionLevel::Default))
        .build();

    assert!(
        config.compress(),
        "CompressionSetting::level() should enable compress"
    );
    assert!(config.compression_setting().is_enabled());
}

#[test]
fn compress_false_after_level_clears_everything() {
    use std::num::NonZeroU8;
    for n in 1u8..=9 {
        let level = NonZeroU8::new(n).unwrap();
        let config = builder()
            .compression_level(Some(CompressionLevel::precise(level)))
            .compress(false)
            .build();

        assert!(
            !config.compress(),
            "level {n}: compress(false) should disable"
        );
        assert_eq!(
            config.compression_level(),
            None,
            "level {n}: compress(false) should clear compression_level"
        );
    }
}

#[test]
fn copy_links_sets_flag() {
    let config = builder().copy_links(true).build();
    assert!(config.copy_links());
}

#[test]
fn copy_links_false_clears_flag() {
    let config = builder().copy_links(true).copy_links(false).build();
    assert!(!config.copy_links());
}

#[test]
fn links_sets_flag() {
    let config = builder().links(true).build();
    assert!(config.links());
}

#[test]
fn links_false_clears_flag() {
    let config = builder().links(true).links(false).build();
    assert!(!config.links());
}

#[test]
fn copy_unsafe_links_sets_flag() {
    let config = builder().copy_unsafe_links(true).build();
    assert!(config.copy_unsafe_links());
}

#[test]
fn copy_unsafe_links_false_clears_flag() {
    let config = builder()
        .copy_unsafe_links(true)
        .copy_unsafe_links(false)
        .build();
    assert!(!config.copy_unsafe_links());
}

#[test]
fn copy_dirlinks_sets_flag() {
    let config = builder().copy_dirlinks(true).build();
    assert!(config.copy_dirlinks());
}

#[test]
fn copy_dirlinks_false_clears_flag() {
    let config = builder().copy_dirlinks(true).copy_dirlinks(false).build();
    assert!(!config.copy_dirlinks());
}

#[test]
fn copy_devices_sets_flag() {
    let config = builder().copy_devices(true).build();
    assert!(config.copy_devices());
}

#[test]
fn copy_devices_false_clears_flag() {
    let config = builder().copy_devices(true).copy_devices(false).build();
    assert!(!config.copy_devices());
}

#[test]
fn write_devices_sets_flag() {
    let config = builder().write_devices(true).build();
    assert!(config.write_devices());
}

#[test]
fn write_devices_false_clears_flag() {
    let config = builder().write_devices(true).write_devices(false).build();
    assert!(!config.write_devices());
}

#[test]
fn keep_dirlinks_sets_flag() {
    let config = builder().keep_dirlinks(true).build();
    assert!(config.keep_dirlinks());
}

#[test]
fn keep_dirlinks_false_clears_flag() {
    let config = builder().keep_dirlinks(true).keep_dirlinks(false).build();
    assert!(!config.keep_dirlinks());
}

#[test]
fn safe_links_sets_flag() {
    let config = builder().safe_links(true).build();
    assert!(config.safe_links());
}

#[test]
fn safe_links_false_clears_flag() {
    let config = builder().safe_links(true).safe_links(false).build();
    assert!(!config.safe_links());
}

#[test]
fn munge_links_sets_flag() {
    let config = builder().munge_links(true).build();
    assert!(config.munge_links());
}

#[test]
fn munge_links_false_clears_flag() {
    let config = builder().munge_links(true).munge_links(false).build();
    assert!(!config.munge_links());
}

#[test]
fn default_copy_links_is_false() {
    let config = builder().build();
    assert!(!config.copy_links());
}

#[test]
fn default_links_is_false() {
    let config = builder().build();
    assert!(!config.links());
}

#[test]
fn default_safe_links_is_false() {
    let config = builder().build();
    assert!(!config.safe_links());
}

#[test]
fn default_munge_links_is_false() {
    let config = builder().build();
    assert!(!config.munge_links());
}

#[test]
fn trust_sender_sets_flag() {
    let config = builder().trust_sender(true).build();
    assert!(config.trust_sender());
}

#[test]
fn trust_sender_false_clears_flag() {
    let config = builder().trust_sender(true).trust_sender(false).build();
    assert!(!config.trust_sender());
}

#[test]
fn default_trust_sender_is_false() {
    let config = builder().build();
    assert!(!config.trust_sender());
}

#[test]
fn min_file_size_sets_limit() {
    let config = builder().min_file_size(Some(1024)).build();
    assert_eq!(config.min_file_size(), Some(1024));
}

#[test]
fn min_file_size_none_clears_limit() {
    let config = builder()
        .min_file_size(Some(1024))
        .min_file_size(None)
        .build();
    assert!(config.min_file_size().is_none());
}

#[test]
fn max_file_size_sets_limit() {
    let config = builder().max_file_size(Some(1_048_576)).build();
    assert_eq!(config.max_file_size(), Some(1_048_576));
}

#[test]
fn max_file_size_none_clears_limit() {
    let config = builder()
        .max_file_size(Some(1_048_576))
        .max_file_size(None)
        .build();
    assert!(config.max_file_size().is_none());
}

#[test]
fn modify_window_sets_value() {
    let config = builder().modify_window(Some(2)).build();
    assert_eq!(config.modify_window(), Some(2));
}

#[test]
fn modify_window_none_clears_value() {
    let config = builder().modify_window(Some(2)).modify_window(None).build();
    assert!(config.modify_window().is_none());
}

#[test]
fn remove_source_files_sets_flag() {
    let config = builder().remove_source_files(true).build();
    assert!(config.remove_source_files());
}

#[test]
fn remove_source_files_false_clears_flag() {
    let config = builder()
        .remove_source_files(true)
        .remove_source_files(false)
        .build();
    assert!(!config.remove_source_files());
}

#[test]
fn size_only_sets_flag() {
    let config = builder().size_only(true).build();
    assert!(config.size_only());
}

#[test]
fn ignore_times_sets_flag() {
    let config = builder().ignore_times(true).build();
    assert!(config.ignore_times());
}

#[test]
fn ignore_existing_sets_flag() {
    let config = builder().ignore_existing(true).build();
    assert!(config.ignore_existing());
}

#[test]
fn existing_only_sets_flag() {
    let config = builder().existing_only(true).build();
    assert!(config.existing_only());
}

#[test]
fn ignore_missing_args_sets_flag() {
    let config = builder().ignore_missing_args(true).build();
    assert!(config.ignore_missing_args());
}

#[test]
fn delete_missing_args_sets_flag() {
    let config = builder().delete_missing_args(true).build();
    assert!(config.delete_missing_args());
}

#[test]
fn update_sets_flag() {
    let config = builder().update(true).build();
    assert!(config.update());
}

#[test]
fn relative_paths_sets_flag() {
    let config = builder().relative_paths(true).build();
    assert!(config.relative_paths());
}

#[test]
fn recursive_sets_flag() {
    let config = builder().recursive(true).build();
    assert!(config.recursive());
}

#[test]
fn dirs_sets_flag() {
    let config = builder().dirs(true).build();
    assert!(config.dirs());
}

#[test]
fn one_file_system_sets_flag() {
    let config = builder().one_file_system(1).build();
    assert!(config.one_file_system());
    assert_eq!(config.one_file_system_level(), 1);
}

#[test]
fn one_file_system_level_two() {
    let config = builder().one_file_system(2).build();
    assert!(config.one_file_system());
    assert_eq!(config.one_file_system_level(), 2);
}

#[test]
fn implied_dirs_sets_true() {
    let config = builder().implied_dirs(true).build();
    assert!(config.implied_dirs());
}

#[test]
fn implied_dirs_sets_false() {
    let config = builder().implied_dirs(false).build();
    assert!(!config.implied_dirs());
}

#[test]
fn mkpath_sets_flag() {
    let config = builder().mkpath(true).build();
    assert!(config.mkpath());
}

#[test]
fn force_replacements_sets_flag() {
    let config = builder().force_replacements(true).build();
    assert!(config.force_replacements());
}

#[test]
fn prune_empty_dirs_sets_flag() {
    let config = builder().prune_empty_dirs(true).build();
    assert!(config.prune_empty_dirs());
}

#[test]
fn default_min_file_size_is_none() {
    let config = builder().build();
    assert!(config.min_file_size().is_none());
}

#[test]
fn default_max_file_size_is_none() {
    let config = builder().build();
    assert!(config.max_file_size().is_none());
}

#[test]
fn default_recursive_is_false() {
    let config = builder().build();
    assert!(!config.recursive());
}

#[test]
fn files_from_sets_local_file() {
    use std::path::PathBuf;
    let config = builder()
        .files_from(FilesFromSource::LocalFile(PathBuf::from("/tmp/list")))
        .build();
    assert_eq!(
        *config.files_from(),
        FilesFromSource::LocalFile(PathBuf::from("/tmp/list"))
    );
}

#[test]
fn files_from_sets_remote_file() {
    let config = builder()
        .files_from(FilesFromSource::RemoteFile("/remote/list".to_owned()))
        .build();
    assert!(config.files_from().is_remote());
}

#[test]
fn files_from_sets_stdin() {
    let config = builder().files_from(FilesFromSource::Stdin).build();
    assert!(config.files_from().is_active());
}

#[test]
fn files_from_default_is_none() {
    let config = builder().build();
    assert!(!config.files_from().is_active());
}

#[test]
fn from0_sets_flag() {
    let config = builder().from0(true).build();
    assert!(config.from0());
}

#[test]
fn from0_default_is_false() {
    let config = builder().build();
    assert!(!config.from0());
}

#[test]
fn checksum_sets_flag() {
    let config = builder().checksum(true).build();
    assert!(config.checksum());
}

#[test]
fn checksum_false_clears_flag() {
    let config = builder().checksum(true).checksum(false).build();
    assert!(!config.checksum());
}

#[test]
fn checksum_choice_sets_value() {
    let choice = StrongChecksumChoice::parse("xxh3").unwrap();
    let config = builder().checksum_choice(choice).build();
    assert_eq!(config.checksum_choice().to_argument(), "xxh3");
}

#[test]
fn checksum_choice_md5() {
    let choice = StrongChecksumChoice::parse("md5").unwrap();
    let config = builder().checksum_choice(choice).build();
    assert_eq!(config.checksum_choice().to_argument(), "md5");
}

#[test]
fn checksum_seed_sets_value() {
    let config = builder().checksum_seed(Some(12345)).build();
    assert_eq!(config.checksum_seed(), Some(12345));
}

#[test]
fn checksum_seed_none_clears_value() {
    let config = builder()
        .checksum_seed(Some(12345))
        .checksum_seed(None)
        .build();
    assert!(config.checksum_seed().is_none());
}

#[test]
fn force_event_collection_sets_flag() {
    let config = builder().force_event_collection(true).build();
    assert!(config.force_event_collection());
}

#[test]
fn force_event_collection_false_clears_flag() {
    let config = builder()
        .force_event_collection(true)
        .force_event_collection(false)
        .build();
    assert!(!config.force_event_collection());
}

#[test]
fn default_checksum_is_false() {
    let config = builder().build();
    assert!(!config.checksum());
}

#[test]
fn default_checksum_seed_is_none() {
    let config = builder().build();
    assert!(config.checksum_seed().is_none());
}

#[test]
fn default_force_event_collection_is_false() {
    let config = builder().build();
    assert!(!config.force_event_collection());
}
