#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ModuleDefinition {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) comment: Option<String>,
    pub(crate) hosts_allow: Vec<HostPattern>,
    pub(crate) hosts_deny: Vec<HostPattern>,
    pub(crate) auth_users: Vec<String>,
    pub(crate) secrets_file: Option<PathBuf>,
    pub(crate) bandwidth_limit: Option<NonZeroU64>,
    pub(crate) bandwidth_limit_specified: bool,
    pub(crate) bandwidth_burst: Option<NonZeroU64>,
    pub(crate) bandwidth_burst_specified: bool,
    pub(crate) bandwidth_limit_configured: bool,
    pub(crate) refuse_options: Vec<String>,
    pub(crate) read_only: bool,
    pub(crate) write_only: bool,
    pub(crate) numeric_ids: bool,
    pub(crate) uid: Option<u32>,
    pub(crate) gid: Option<u32>,
    pub(crate) timeout: Option<NonZeroU64>,
    pub(crate) listable: bool,
    pub(crate) use_chroot: bool,
    pub(crate) max_connections: Option<NonZeroU32>,
    pub(crate) incoming_chmod: Option<String>,
    pub(crate) outgoing_chmod: Option<String>,
}

impl ModuleDefinition {
    pub(crate) fn permits(&self, addr: IpAddr, hostname: Option<&str>) -> bool {
        if !self.hosts_allow.is_empty()
            && !self
                .hosts_allow
                .iter()
                .any(|pattern| pattern.matches(addr, hostname))
        {
            return false;
        }

        if self
            .hosts_deny
            .iter()
            .any(|pattern| pattern.matches(addr, hostname))
        {
            return false;
        }

        true
    }

    fn requires_hostname_lookup(&self) -> bool {
        self.hosts_allow
            .iter()
            .chain(self.hosts_deny.iter())
            .any(HostPattern::requires_hostname)
    }

    fn requires_authentication(&self) -> bool {
        !self.auth_users.is_empty()
    }

    pub(crate) fn max_connections(&self) -> Option<NonZeroU32> {
        self.max_connections
    }

    pub(crate) fn bandwidth_limit(&self) -> Option<NonZeroU64> {
        self.bandwidth_limit
    }

    pub(crate) fn bandwidth_limit_specified(&self) -> bool {
        self.bandwidth_limit_specified
    }

    pub(crate) fn bandwidth_burst(&self) -> Option<NonZeroU64> {
        self.bandwidth_burst
    }

    pub(crate) fn bandwidth_burst_specified(&self) -> bool {
        self.bandwidth_burst_specified
    }

    fn bandwidth_limit_configured(&self) -> bool {
        self.bandwidth_limit_configured
    }

    fn inherit_refuse_options(&mut self, options: &[String]) {
        if self.refuse_options.is_empty() {
            self.refuse_options = options.to_vec();
        }
    }

    pub(super) fn inherit_incoming_chmod(&mut self, chmod: Option<&str>) {
        if self.incoming_chmod.is_none() {
            self.incoming_chmod = chmod.map(str::to_string);
        }
    }

    pub(super) fn inherit_outgoing_chmod(&mut self, chmod: Option<&str>) {
        if self.outgoing_chmod.is_none() {
            self.outgoing_chmod = chmod.map(str::to_string);
        }
    }
}

#[cfg(test)]
#[allow(dead_code)]
impl ModuleDefinition {
    pub(super) fn auth_users(&self) -> &[String] {
        &self.auth_users
    }

    pub(super) fn secrets_file(&self) -> Option<&Path> {
        self.secrets_file.as_deref()
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn refused_options(&self) -> &[String] {
        &self.refuse_options
    }

    pub(super) fn read_only(&self) -> bool {
        self.read_only
    }

    pub(super) fn write_only(&self) -> bool {
        self.write_only
    }

    pub(super) fn numeric_ids(&self) -> bool {
        self.numeric_ids
    }

    pub(super) fn uid(&self) -> Option<u32> {
        self.uid
    }

    pub(super) fn gid(&self) -> Option<u32> {
        self.gid
    }

    pub(super) fn timeout(&self) -> Option<NonZeroU64> {
        self.timeout
    }

    pub(super) fn listable(&self) -> bool {
        self.listable
    }

    pub(super) fn use_chroot(&self) -> bool {
        self.use_chroot
    }

    pub(super) fn incoming_chmod(&self) -> Option<&str> {
        self.incoming_chmod.as_deref()
    }

    pub(super) fn outgoing_chmod(&self) -> Option<&str> {
        self.outgoing_chmod.as_deref()
    }
}

pub(crate) struct ModuleRuntime {
    pub(crate) definition: ModuleDefinition,
    pub(crate) active_connections: AtomicU32,
    pub(crate) connection_limiter: Option<Arc<ConnectionLimiter>>,
}

#[derive(Debug)]
pub(crate) enum ModuleConnectionError {
    Limit(NonZeroU32),
    Io(io::Error),
}

impl ModuleConnectionError {
    fn io(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<io::Error> for ModuleConnectionError {
    fn from(error: io::Error) -> Self {
        ModuleConnectionError::Io(error)
    }
}

impl ModuleRuntime {
    fn new(
        definition: ModuleDefinition,
        connection_limiter: Option<Arc<ConnectionLimiter>>,
    ) -> Self {
        Self {
            definition,
            active_connections: AtomicU32::new(0),
            connection_limiter,
        }
    }

    fn try_acquire_connection(&self) -> Result<ModuleConnectionGuard<'_>, ModuleConnectionError> {
        if let Some(limit) = self.definition.max_connections() {
            if let Some(limiter) = &self.connection_limiter {
                match limiter.acquire(&self.definition.name, limit) {
                    Ok(lock_guard) => {
                        self.acquire_local_slot(limit)?;
                        return Ok(ModuleConnectionGuard::limited(self, Some(lock_guard)));
                    }
                    Err(error) => return Err(error),
                }
            }

            self.acquire_local_slot(limit)?;
            Ok(ModuleConnectionGuard::limited(self, None))
        } else {
            Ok(ModuleConnectionGuard::unlimited())
        }
    }

    fn acquire_local_slot(&self, limit: NonZeroU32) -> Result<(), ModuleConnectionError> {
        let limit_value = limit.get();
        let mut current = self.active_connections.load(Ordering::Acquire);
        loop {
            if current >= limit_value {
                return Err(ModuleConnectionError::Limit(limit));
            }

            match self.active_connections.compare_exchange(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(updated) => current = updated,
            }
        }
    }

    fn release(&self) {
        if self.definition.max_connections().is_some() {
            self.active_connections.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

pub(crate) struct ConnectionLimiter {
    path: PathBuf,
}

impl ConnectionLimiter {
    pub(crate) fn open(path: PathBuf) -> Result<Self, DaemonError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|error| lock_file_error(&path, error))?;
            }

        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| lock_file_error(&path, error))?;

        drop(file);

        Ok(Self { path })
    }

    pub(crate) fn acquire(
        self: &Arc<Self>,
        module: &str,
        limit: NonZeroU32,
    ) -> Result<ConnectionLockGuard, ModuleConnectionError> {
        let mut file = self.open_file().map_err(ModuleConnectionError::io)?;
        file.lock_exclusive().map_err(ModuleConnectionError::io)?;

        let result = self.increment_count(&mut file, module, limit);
        drop(file);

        result.map(|_| ConnectionLockGuard {
            limiter: Arc::clone(self),
            module: module.to_owned(),
        })
    }

    fn decrement(&self, module: &str) -> io::Result<()> {
        let mut file = self.open_file()?;
        file.lock_exclusive()?;
        let result = self.decrement_count(&mut file, module);
        drop(file);
        result
    }

    fn open_file(&self) -> io::Result<File> {
        OpenOptions::new().read(true).write(true).open(&self.path)
    }

    fn increment_count(
        &self,
        file: &mut File,
        module: &str,
        limit: NonZeroU32,
    ) -> Result<(), ModuleConnectionError> {
        let mut counts = self.read_counts(file)?;
        let current = counts.get(module).copied().unwrap_or(0);
        if current >= limit.get() {
            return Err(ModuleConnectionError::Limit(limit));
        }

        counts.insert(module.to_owned(), current.saturating_add(1));
        self.write_counts(file, &counts)
            .map_err(ModuleConnectionError::io)
    }

    fn decrement_count(&self, file: &mut File, module: &str) -> io::Result<()> {
        let mut counts = self.read_counts(file)?;
        if let Some(entry) = counts.get_mut(module) {
            if *entry > 1 {
                *entry -= 1;
            } else {
                counts.remove(module);
            }
        }

        self.write_counts(file, &counts)
    }

    fn read_counts(&self, file: &mut File) -> io::Result<BTreeMap<String, u32>> {
        file.seek(SeekFrom::Start(0))?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;

        let mut counts = BTreeMap::new();
        for line in contents.lines() {
            let mut parts = line.split_whitespace();
            if let (Some(name), Some(value)) = (parts.next(), parts.next())
                && let Ok(parsed) = value.parse::<u32>() {
                    counts.insert(name.to_owned(), parsed);
                }
        }

        Ok(counts)
    }

    fn write_counts(&self, file: &mut File, counts: &BTreeMap<String, u32>) -> io::Result<()> {
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        for (module, value) in counts {
            if *value > 0 {
                writeln!(file, "{module} {value}")?;
            }
        }
        file.flush()
    }
}

pub(crate) struct ConnectionLockGuard {
    limiter: Arc<ConnectionLimiter>,
    module: String,
}

impl Drop for ConnectionLockGuard {
    fn drop(&mut self) {
        let _ = self.limiter.decrement(&self.module);
    }
}

impl From<ModuleDefinition> for ModuleRuntime {
    fn from(definition: ModuleDefinition) -> Self {
        Self::new(definition, None)
    }
}

impl std::ops::Deref for ModuleRuntime {
    type Target = ModuleDefinition;

    fn deref(&self) -> &Self::Target {
        &self.definition
    }
}

struct ModuleConnectionGuard<'a> {
    module: Option<&'a ModuleRuntime>,
    lock_guard: Option<ConnectionLockGuard>,
}

impl<'a> ModuleConnectionGuard<'a> {
    fn limited(module: &'a ModuleRuntime, lock_guard: Option<ConnectionLockGuard>) -> Self {
        Self {
            module: Some(module),
            lock_guard,
        }
    }

    const fn unlimited() -> Self {
        Self {
            module: None,
            lock_guard: None,
        }
    }
}

impl<'a> Drop for ModuleConnectionGuard<'a> {
    fn drop(&mut self) {
        if let Some(module) = self.module.take() {
            module.release();
        }

        self.lock_guard.take();
    }
}

pub(crate) fn module_peer_hostname<'a>(
    module: &ModuleDefinition,
    cache: &'a mut Option<Option<String>>,
    peer_ip: IpAddr,
    allow_lookup: bool,
) -> Option<&'a str> {
    if !allow_lookup || !module.requires_hostname_lookup() {
        return None;
    }

    if cache.is_none() {
        *cache = Some(resolve_peer_hostname(peer_ip));
    }

    cache.as_ref().and_then(|value| value.as_deref())
}

fn resolve_peer_hostname(peer_ip: IpAddr) -> Option<String> {
    #[cfg(test)]
    if let Some(mapped) = TEST_HOSTNAME_OVERRIDES.with(|map| map.borrow().get(&peer_ip).cloned()) {
        return mapped.map(normalize_hostname_owned);
    }

    lookup_addr(&peer_ip).ok().map(normalize_hostname_owned)
}

fn normalize_hostname_owned(mut name: String) -> String {
    if name.ends_with('.') {
        name.pop();
    }
    name.make_ascii_lowercase();
    name
}

#[cfg(test)]
thread_local! {
    pub(super) static TEST_HOSTNAME_OVERRIDES: RefCell<HashMap<IpAddr, Option<String>>> =
        RefCell::new(HashMap::new());
}

#[cfg(test)]
thread_local! {
    pub(super) static TEST_CONFIG_CANDIDATES: RefCell<Option<Vec<PathBuf>>> =
        const { RefCell::new(Some(Vec::new())) };
}

#[cfg(test)]
thread_local! {
    pub(crate) static TEST_SECRETS_CANDIDATES: RefCell<Option<Vec<PathBuf>>> =
        const { RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    pub(crate) static TEST_SECRETS_ENV: RefCell<Option<TestSecretsEnvOverride>> =
        const { RefCell::new(None) };
}

#[cfg(test)]
#[derive(Clone, Debug, Default)]
pub(crate) struct TestSecretsEnvOverride {
    pub(crate) branded: Option<OsString>,
    pub(crate) legacy: Option<OsString>,
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn set_test_hostname_override(addr: IpAddr, hostname: Option<&str>) {
    TEST_HOSTNAME_OVERRIDES.with(|map| {
        map.borrow_mut()
            .insert(addr, hostname.map(|value| value.to_owned()));
    });
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn clear_test_hostname_overrides() {
    TEST_HOSTNAME_OVERRIDES.with(|map| map.borrow_mut().clear());
}

#[cfg(test)]
mod module_state_tests {
    use super::*;

    // Tests for ModuleDefinition

    #[test]
    fn module_definition_default() {
        let def = ModuleDefinition::default();
        assert!(def.name.is_empty());
        assert!(def.path.as_os_str().is_empty());
        assert!(def.comment.is_none());
        assert!(def.hosts_allow.is_empty());
        assert!(def.hosts_deny.is_empty());
        assert!(def.auth_users.is_empty());
        assert!(!def.read_only);
        assert!(!def.write_only);
        assert!(!def.listable);
    }

    #[test]
    fn module_definition_permits_all_when_no_rules() {
        let def = ModuleDefinition::default();
        let addr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        assert!(def.permits(addr, None));
        assert!(def.permits(addr, Some("example.com")));
    }

    #[test]
    fn module_definition_permits_respects_hosts_allow() {
        let def = ModuleDefinition {
            hosts_allow: vec![HostPattern::Ipv4 {
                network: Ipv4Addr::new(192, 168, 0, 0),
                prefix: 16,
            }],
            ..Default::default()
        };
        let allowed = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        let denied = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        assert!(def.permits(allowed, None));
        assert!(!def.permits(denied, None));
    }

    #[test]
    fn module_definition_permits_respects_hosts_deny() {
        let def = ModuleDefinition {
            hosts_deny: vec![HostPattern::Ipv4 {
                network: Ipv4Addr::new(10, 0, 0, 0),
                prefix: 8,
            }],
            ..Default::default()
        };
        let allowed = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        let denied = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        assert!(def.permits(allowed, None));
        assert!(!def.permits(denied, None));
    }

    #[test]
    fn module_definition_deny_takes_precedence_over_allow() {
        let def = ModuleDefinition {
            hosts_allow: vec![HostPattern::Any],
            hosts_deny: vec![HostPattern::Ipv4 {
                network: Ipv4Addr::new(10, 0, 0, 0),
                prefix: 8,
            }],
            ..Default::default()
        };
        let denied = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        assert!(!def.permits(denied, None));
    }

    #[test]
    fn module_definition_requires_hostname_lookup_when_hostname_pattern() {
        let def = ModuleDefinition {
            hosts_allow: vec![HostPattern::Hostname(HostnamePattern {
                kind: HostnamePatternKind::Suffix("example.com".to_owned()),
            })],
            ..Default::default()
        };
        assert!(def.requires_hostname_lookup());
    }

    #[test]
    fn module_definition_no_hostname_lookup_for_ip_patterns() {
        let def = ModuleDefinition {
            hosts_allow: vec![HostPattern::Ipv4 {
                network: Ipv4Addr::new(192, 168, 0, 0),
                prefix: 16,
            }],
            ..Default::default()
        };
        assert!(!def.requires_hostname_lookup());
    }

    #[test]
    fn module_definition_requires_authentication_when_auth_users_set() {
        let def = ModuleDefinition {
            auth_users: vec!["alice".to_owned()],
            ..Default::default()
        };
        assert!(def.requires_authentication());
    }

    #[test]
    fn module_definition_no_authentication_when_no_auth_users() {
        let def = ModuleDefinition::default();
        assert!(!def.requires_authentication());
    }

    #[test]
    fn module_definition_inherit_refuse_options() {
        let mut def = ModuleDefinition::default();
        let options = vec!["delete".to_owned(), "delete-after".to_owned()];
        def.inherit_refuse_options(&options);
        assert_eq!(def.refuse_options, options);
    }

    #[test]
    fn module_definition_inherit_refuse_options_preserves_existing() {
        let mut def = ModuleDefinition {
            refuse_options: vec!["hardlinks".to_owned()],
            ..Default::default()
        };
        let options = vec!["delete".to_owned()];
        def.inherit_refuse_options(&options);
        assert_eq!(def.refuse_options, vec!["hardlinks".to_owned()]);
    }

    #[test]
    fn module_definition_inherit_chmod() {
        let mut def = ModuleDefinition::default();
        def.inherit_incoming_chmod(Some("Dg+s,ug+w"));
        def.inherit_outgoing_chmod(Some("Fo-w,+X"));
        assert_eq!(def.incoming_chmod.as_deref(), Some("Dg+s,ug+w"));
        assert_eq!(def.outgoing_chmod.as_deref(), Some("Fo-w,+X"));
    }

    #[test]
    fn module_definition_inherit_chmod_preserves_existing() {
        let mut def = ModuleDefinition {
            incoming_chmod: Some("existing".to_owned()),
            outgoing_chmod: Some("existing".to_owned()),
            ..Default::default()
        };
        def.inherit_incoming_chmod(Some("new"));
        def.inherit_outgoing_chmod(Some("new"));
        assert_eq!(def.incoming_chmod.as_deref(), Some("existing"));
        assert_eq!(def.outgoing_chmod.as_deref(), Some("existing"));
    }

    #[test]
    fn module_definition_bandwidth_accessors() {
        let def = ModuleDefinition {
            bandwidth_limit: NonZeroU64::new(1000),
            bandwidth_limit_specified: true,
            bandwidth_burst: NonZeroU64::new(2000),
            bandwidth_burst_specified: true,
            bandwidth_limit_configured: true,
            ..Default::default()
        };
        assert_eq!(def.bandwidth_limit(), NonZeroU64::new(1000));
        assert!(def.bandwidth_limit_specified());
        assert_eq!(def.bandwidth_burst(), NonZeroU64::new(2000));
        assert!(def.bandwidth_burst_specified());
        assert!(def.bandwidth_limit_configured());
    }

    #[test]
    fn module_definition_max_connections() {
        let def = ModuleDefinition {
            max_connections: NonZeroU32::new(10),
            ..Default::default()
        };
        assert_eq!(def.max_connections(), NonZeroU32::new(10));
    }

    // Tests for ModuleRuntime

    #[test]
    fn module_runtime_from_definition() {
        let def = ModuleDefinition {
            name: "test".to_owned(),
            path: PathBuf::from("/test"),
            ..Default::default()
        };
        let runtime: ModuleRuntime = def.into();
        assert_eq!(runtime.definition.name, "test");
    }

    #[test]
    fn module_runtime_deref_to_definition() {
        let def = ModuleDefinition {
            name: "deref_test".to_owned(),
            ..Default::default()
        };
        let runtime: ModuleRuntime = def.into();
        assert_eq!(runtime.name, "deref_test");
    }

    #[test]
    fn module_runtime_requires_authentication() {
        let def = ModuleDefinition {
            auth_users: vec!["user".to_owned()],
            ..Default::default()
        };
        let runtime: ModuleRuntime = def.into();
        assert!(runtime.requires_authentication());
    }

    // Tests for ModuleConnectionError

    #[test]
    fn module_connection_error_io() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "test");
        let err = ModuleConnectionError::io(io_err);
        match err {
            ModuleConnectionError::Io(_) => (),
            ModuleConnectionError::Limit(_) => panic!("Expected Io variant"),
        }
    }

    #[test]
    fn module_connection_error_from_io() {
        let io_err = io::Error::new(io::ErrorKind::PermissionDenied, "test");
        let err: ModuleConnectionError = io_err.into();
        match err {
            ModuleConnectionError::Io(_) => (),
            ModuleConnectionError::Limit(_) => panic!("Expected Io variant"),
        }
    }

    #[test]
    fn module_connection_error_debug() {
        let limit = NonZeroU32::new(5).unwrap();
        let err = ModuleConnectionError::Limit(limit);
        let debug = format!("{err:?}");
        assert!(debug.contains("Limit"));
    }

    // Tests for ModuleConnectionGuard

    #[test]
    fn module_connection_guard_unlimited() {
        let guard = ModuleConnectionGuard::unlimited();
        assert!(guard.module.is_none());
        assert!(guard.lock_guard.is_none());
    }

    // Tests for normalize_hostname_owned

    #[test]
    fn normalize_hostname_removes_trailing_dot() {
        let result = normalize_hostname_owned("example.com.".to_owned());
        assert_eq!(result, "example.com");
    }

    #[test]
    fn normalize_hostname_lowercases() {
        let result = normalize_hostname_owned("EXAMPLE.COM".to_owned());
        assert_eq!(result, "example.com");
    }

    #[test]
    fn normalize_hostname_combined() {
        let result = normalize_hostname_owned("Example.COM.".to_owned());
        assert_eq!(result, "example.com");
    }

    // Tests for module_peer_hostname

    #[test]
    fn module_peer_hostname_returns_none_when_lookup_disabled() {
        let def = ModuleDefinition {
            hosts_allow: vec![HostPattern::Hostname(HostnamePattern {
                kind: HostnamePatternKind::Suffix("example.com".to_owned()),
            })],
            ..Default::default()
        };
        let mut cache = None;
        let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let result = module_peer_hostname(&def, &mut cache, addr, false);
        assert!(result.is_none());
    }

    #[test]
    fn module_peer_hostname_returns_none_when_no_hostname_patterns() {
        let def = ModuleDefinition {
            hosts_allow: vec![HostPattern::Any],
            ..Default::default()
        };
        let mut cache = None;
        let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let result = module_peer_hostname(&def, &mut cache, addr, true);
        assert!(result.is_none());
    }

    #[test]
    fn module_peer_hostname_uses_cache() {
        let def = ModuleDefinition {
            hosts_allow: vec![HostPattern::Hostname(HostnamePattern {
                kind: HostnamePatternKind::Suffix("example.com".to_owned()),
            })],
            ..Default::default()
        };
        // Pre-populate cache
        let mut cache = Some(Some("cached.example.com".to_owned()));
        let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let result = module_peer_hostname(&def, &mut cache, addr, true);
        assert_eq!(result, Some("cached.example.com"));
    }
}

