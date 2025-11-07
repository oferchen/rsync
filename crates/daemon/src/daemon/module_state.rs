#[derive(Clone, Debug, Eq, PartialEq)]
struct ModuleDefinition {
    name: String,
    path: PathBuf,
    comment: Option<String>,
    hosts_allow: Vec<HostPattern>,
    hosts_deny: Vec<HostPattern>,
    auth_users: Vec<String>,
    secrets_file: Option<PathBuf>,
    bandwidth_limit: Option<NonZeroU64>,
    bandwidth_limit_specified: bool,
    bandwidth_burst: Option<NonZeroU64>,
    bandwidth_burst_specified: bool,
    bandwidth_limit_configured: bool,
    refuse_options: Vec<String>,
    read_only: bool,
    write_only: bool,
    numeric_ids: bool,
    uid: Option<u32>,
    gid: Option<u32>,
    timeout: Option<NonZeroU64>,
    listable: bool,
    use_chroot: bool,
    max_connections: Option<NonZeroU32>,
    incoming_chmod: Option<String>,
    outgoing_chmod: Option<String>,
}

impl ModuleDefinition {
    fn permits(&self, addr: IpAddr, hostname: Option<&str>) -> bool {
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

    fn max_connections(&self) -> Option<NonZeroU32> {
        self.max_connections
    }

    fn bandwidth_limit(&self) -> Option<NonZeroU64> {
        self.bandwidth_limit
    }

    fn bandwidth_limit_specified(&self) -> bool {
        self.bandwidth_limit_specified
    }

    fn bandwidth_burst(&self) -> Option<NonZeroU64> {
        self.bandwidth_burst
    }

    fn bandwidth_burst_specified(&self) -> bool {
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

struct ModuleRuntime {
    definition: ModuleDefinition,
    active_connections: AtomicU32,
    connection_limiter: Option<Arc<ConnectionLimiter>>,
}

#[derive(Debug)]
enum ModuleConnectionError {
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

struct ConnectionLimiter {
    path: PathBuf,
}

impl ConnectionLimiter {
    fn open(path: PathBuf) -> Result<Self, DaemonError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|error| lock_file_error(&path, error))?;
            }
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

    fn acquire(
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
            module: module.to_string(),
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

        counts.insert(module.to_string(), current.saturating_add(1));
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
            if let (Some(name), Some(value)) = (parts.next(), parts.next()) {
                if let Ok(parsed) = value.parse::<u32>() {
                    counts.insert(name.to_string(), parsed);
                }
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

struct ConnectionLockGuard {
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

fn module_peer_hostname<'a>(
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
    pub(super) static TEST_SECRETS_CANDIDATES: RefCell<Option<Vec<PathBuf>>> =
        const { RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    pub(super) static TEST_SECRETS_ENV: RefCell<Option<TestSecretsEnvOverride>> =
        const { RefCell::new(None) };
}

#[cfg(test)]
#[derive(Clone, Debug, Default)]
pub(super) struct TestSecretsEnvOverride {
    pub(crate) branded: Option<OsString>,
    pub(crate) legacy: Option<OsString>,
}

#[cfg(test)]
#[allow(dead_code)]
fn set_test_hostname_override(addr: IpAddr, hostname: Option<&str>) {
    TEST_HOSTNAME_OVERRIDES.with(|map| {
        map.borrow_mut()
            .insert(addr, hostname.map(|value| value.to_string()));
    });
}

#[cfg(test)]
#[allow(dead_code)]
fn clear_test_hostname_overrides() {
    TEST_HOSTNAME_OVERRIDES.with(|map| map.borrow_mut().clear());
}

