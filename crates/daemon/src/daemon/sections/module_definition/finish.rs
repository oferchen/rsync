// Validation and finalization of `ModuleDefinitionBuilder`.
//
// Converts accumulated builder state into a validated `ModuleDefinition`,
// applying defaults for unset fields and enforcing cross-field constraints
// (e.g., `auth users` requires `secrets file`).

/// Reports whether a configured module `path` is already absolute in upstream's
/// sense, so it must be served verbatim rather than resolved against the
/// current directory.
///
/// upstream: `normalize_path()` (util1.c:1405-1426) branches on the LEADING
/// BYTE - `if (*path != '/')` - and only then joins onto `curr_dir`. That is a
/// property of how the operator spelled the path in rsyncd.conf, not of the
/// host platform.
///
/// ⚠ Deliberately [`Path::has_root`], NOT [`Path::is_absolute`]. On Windows
/// `is_absolute()` additionally demands a drive prefix, so it reports `false`
/// for `/srv/docs` - the ordinary spelling in every rsyncd.conf - and the
/// caller would then join an already-absolute module path onto the current
/// directory, yielding `D:\srv\docs`. `has_root()` is exactly upstream's test
/// on Unix and keeps a rooted POSIX path verbatim on Windows, while still
/// classifying a genuinely relative `data/docs` as relative.
fn module_path_is_absolute(path: &Path) -> bool {
    path.has_root()
}

/// Built-in `dont compress` suffix list a daemon module inherits when neither
/// the module nor the global section sets the directive.
///
/// Copied verbatim from upstream's generated `default-dont-compress.h`
/// (`#define DEFAULT_DONT_COMPRESS`), which loadparm.c:46 includes and installs
/// as the default value of the `dont compress` parameter (`lp_dont_compress`).
/// The list itself is authored in `rsync.1.md` and extracted by
/// `define-from-md.awk`. Only a bare `*` collapses the whole compression stream
/// (token.c:206-211); the per-suffix lookup in `set_compression` is compiled
/// out (`#if 0`, token.c:227), so this default list is a config-fidelity value
/// with no per-file wire effect - a daemon still compresses a `.gz` exactly as
/// upstream 3.4.4 does.
const DEFAULT_DONT_COMPRESS: &str = "*.3g2 *.3gp *.7z *.aac *.ace *.apk *.avi *.bz2 *.deb \
*.dmg *.ear *.f4v *.flac *.flv *.gpg *.gz *.iso *.jar *.jpeg *.jpg *.lrz *.lz *.lz4 *.lzma \
*.lzo *.m1a *.m1v *.m2a *.m2ts *.m2v *.m4a *.m4b *.m4p *.m4r *.m4v *.mka *.mkv *.mov *.mp1 \
*.mp2 *.mp3 *.mp4 *.mpa *.mpeg *.mpg *.mpv *.mts *.odb *.odf *.odg *.odi *.odm *.odp *.ods \
*.odt *.oga *.ogg *.ogm *.ogv *.ogx *.opus *.otg *.oth *.otp *.ots *.ott *.oxt *.png *.qt \
*.rar *.rpm *.rz *.rzip *.spx *.squashfs *.sxc *.sxd *.sxg *.sxm *.sxw *.sz *.tbz *.tbz2 \
*.tgz *.tlz *.ts *.txz *.tzo *.vob *.war *.webm *.webp *.xz *.z *.zip *.zst";

impl ModuleDefinitionBuilder {
    /// Converts the accumulated builder state into a validated `ModuleDefinition`.
    ///
    /// Parameters not explicitly set on the module inherit from `defaults`,
    /// which captures P_LOCAL directives from the global section of rsyncd.conf.
    ///
    /// upstream: loadparm.c - `init_section()` copies the current global
    /// defaults (`Vars.l`) into each new module section. Per-module directives
    /// then override specific fields.
    fn finish(
        self,
        config_path: &Path,
        default_secrets: Option<&Path>,
        default_incoming_chmod: Option<&str>,
        default_outgoing_chmod: Option<&str>,
        default_use_chroot: Option<bool>,
        defaults: &GlobalModuleDefaults,
    ) -> Result<ModuleDefinition, DaemonError> {
        let path = self.path.ok_or_else(|| {
            config_parse_error(
                config_path,
                self.declaration_line,
                format!(
                    "module '{}' is missing required 'path' directive",
                    self.name
                ),
            )
        })?;

        let use_chroot = self.use_chroot.or(default_use_chroot).unwrap_or(true);
        // upstream: clientserver.c:883 - `use_chroot < 0` means unset. Track
        // explicitness so a runtime chroot() failure can fall back to
        // no-chroot only when the operator did not demand it.
        let use_chroot_explicit = self.use_chroot.is_some() || default_use_chroot.is_some();

        // A relative module `path` is RESOLVED against the daemon's current
        // directory, never refused.
        //
        // upstream: `normalize_path` (util1.c:1405-1426) opens with
        // `if (*path != '/') { /* Make path absolute. */ ... }`, joining the
        // value onto `curr_dir` before cleaning it. `rsync_module()` routes
        // every module path through it on BOTH arms - the chroot arm at
        // clientserver.c:898-916 and the no-chroot arm at :916-918 - so neither
        // arm can observe a relative path by the time it chroots or serves.
        // There is no absolute-path requirement anywhere in upstream's config
        // parser; refusing here rejected a configuration upstream serves.
        //
        // Resolution runs on every platform and is not gated on `use chroot`,
        // because upstream normalizes both arms identically. Doing it here also
        // preserves the invariant the rest of the daemon relies on - that a
        // `ModuleDefinition::path` is absolute - rather than merely dropping a
        // check and letting a relative path travel further.
        //
        // The bare root `/` is already absolute and is preserved verbatim:
        // upstream loadparm.c (P_PATH) keeps a single-slash module path as-is,
        // and clientserver.c serves from it both with and without chroot (the
        // chroot("/") is a no-op). See the upstream daemon-path-root-read
        // scenario.
        let path = if module_path_is_absolute(&path) {
            path
        } else {
            let current_dir = std::env::current_dir().map_err(|err| {
                config_parse_error(
                    config_path,
                    self.declaration_line,
                    format!(
                        "module '{}' has a relative 'path' and the current directory \
                         could not be read: {err}",
                        self.name
                    ),
                )
            })?;
            // upstream `normalize_path` does not stop at the join: it feeds the
            // result through `clean_fname(path, CFN_COLLAPSE_DOT_DOT_DIRS |
            // CFN_DROP_TRAILING_DOT_DIR)` (util1.c:1420), so `path = ./data`
            // becomes `<cwd>/data`, not `<cwd>/./data`. Joining alone would
            // leave the `.` in every derived path the module reports.
            filters::collapse_dot_dot_dirs(&current_dir.join(path))
        };

        if self.auth_users.as_ref().is_some_and(Vec::is_empty) {
            return Err(config_parse_error(
                config_path,
                self.declaration_line,
                format!(
                    "'auth users' directive in module '{}' must list at least one user",
                    self.name
                ),
            ));
        }

        // upstream: daemon-parm.h:262 - `auth users` is P_LOCAL. A module with
        // no explicit list inherits the global-section default, so a global
        // `auth users` forces authentication on every module that lacks its own
        // (authenticate.c:228 auth_server reads lp_auth_users). Mirrors
        // hosts_allow below.
        let auth_users = self
            .auth_users
            .or_else(|| defaults.auth_users.clone())
            .unwrap_or_default();
        // upstream: daemon-parm.h:273 - `auth digest` is P_LOCAL, so a value in
        // the global section is the default for every module that does not set
        // its own, exactly like `auth users` above.
        let auth_digest = self.auth_digest.or_else(|| defaults.auth_digest.clone());
        let secrets_file = if auth_users.is_empty() {
            self.secrets_file
        } else if let Some(path) = self.secrets_file {
            Some(path)
        } else if let Some(default) = default_secrets {
            Some(default.to_path_buf())
        } else {
            return Err(config_parse_error(
                config_path,
                self.declaration_line,
                format!(
                    "module '{}' specifies 'auth users' but is missing the required 'secrets file' directive",
                    self.name
                ),
            ));
        };

        // upstream: loadparm.c - exclude/include/filter are STRING parameters.
        // In the global section they set defaults; per-module directives override
        // (not append to) the defaults. If the module has its own exclude rules,
        // use those; otherwise inherit the global defaults.
        let exclude = if self.exclude.is_empty() {
            defaults.exclude.clone()
        } else {
            self.exclude
        };
        let include = if self.include.is_empty() {
            defaults.include.clone()
        } else {
            self.include
        };
        let filter = if self.filter.is_empty() {
            defaults.filter.clone()
        } else {
            self.filter
        };

        // upstream: loadparm.c - hosts_allow/hosts_deny are STRING P_LOCAL
        // parameters with global defaults.
        let hosts_allow = self
            .hosts_allow
            .or_else(|| defaults.hosts_allow.clone())
            .unwrap_or_default();
        let hosts_deny = self
            .hosts_deny
            .or_else(|| defaults.hosts_deny.clone())
            .unwrap_or_default();

        Ok(ModuleDefinition {
            name: self.name,
            path,
            comment: self.comment.or_else(|| defaults.comment.clone()),
            hosts_allow,
            hosts_deny,
            auth_users,
            auth_digest,
            secrets_file,
            refuse_options: self.refuse_options.unwrap_or_default(),
            read_only: self.read_only.or(defaults.read_only).unwrap_or(true),
            write_only: self.write_only.or(defaults.write_only).unwrap_or(false),
            // upstream: clientserver.c:1201-1204 - `numeric ids` is a BOOL3
            // tri-state. Preserve the unset third state (`None`) so a chrooted
            // module with the directive unset can still be forced to numeric
            // ids at session setup; collapsing to `false` here loses that.
            numeric_ids: self.numeric_ids.or(defaults.numeric_ids),
            // upstream: clientserver.c:781,790 read the per-module `lp_uid`/
            // `lp_gid`, which inherit the global-section default when the module
            // sets no explicit value (daemon-parm.txt marks both P_LOCAL).
            uid: self.uid.or(defaults.uid),
            gid: self.gid.or_else(|| defaults.gid.clone()),
            timeout: self.timeout.or(defaults.timeout).unwrap_or(None),
            listable: self.listable.or(defaults.listable).unwrap_or(true),
            use_chroot,
            use_chroot_explicit,
            max_connections: self
                .max_connections
                .or(defaults.max_connections)
                .unwrap_or(MaxConnections::Unlimited),
            incoming_chmod: self
                .incoming_chmod
                .unwrap_or_else(|| default_incoming_chmod.map(str::to_string)),
            outgoing_chmod: self
                .outgoing_chmod
                .unwrap_or_else(|| default_outgoing_chmod.map(str::to_string)),
            fake_super: self.fake_super.or(defaults.fake_super).unwrap_or(false),
            insecure_links: self
                .insecure_links
                .or(defaults.insecure_links)
                .unwrap_or(false),
            munge_symlinks: self
                .munge_symlinks
                .or(defaults.munge_symlinks)
                .unwrap_or(None),
            max_verbosity: self.max_verbosity.or(defaults.max_verbosity).unwrap_or(1),
            ignore_errors: self
                .ignore_errors
                .or(defaults.ignore_errors)
                .unwrap_or(false),
            ignore_nonreadable: self
                .ignore_nonreadable
                .or(defaults.ignore_nonreadable)
                .unwrap_or(false),
            transfer_logging: self
                .transfer_logging
                .or(defaults.transfer_logging)
                .unwrap_or(false),
            log_format: self.log_format.unwrap_or_else(|| {
                defaults
                    .log_format
                    .clone()
                    .or_else(|| Some("%o %h [%a] %m (%u) %f %l".to_owned()))
            }),
            log_file: self.log_file.or_else(|| defaults.log_file.clone()),
            // upstream: loadparm.c:46 seeds `dont compress` with the built-in
            // DEFAULT_DONT_COMPRESS list when neither the module nor the global
            // section sets it, so `lp_dont_compress(module_id)` is never empty.
            dont_compress: self
                .dont_compress
                .unwrap_or_else(|| defaults.dont_compress.clone())
                .or_else(|| Some(DEFAULT_DONT_COMPRESS.to_owned())),
            early_exec: self
                .early_exec
                .unwrap_or_else(|| defaults.early_exec.clone()),
            pre_xfer_exec: self
                .pre_xfer_exec
                .unwrap_or_else(|| defaults.pre_xfer_exec.clone()),
            post_xfer_exec: self
                .post_xfer_exec
                .unwrap_or_else(|| defaults.post_xfer_exec.clone()),
            name_converter: self
                .name_converter
                .unwrap_or_else(|| defaults.name_converter.clone()),
            temp_dir: self.temp_dir.unwrap_or_else(|| defaults.temp_dir.clone()),
            charset: self.charset.unwrap_or_else(|| defaults.charset.clone()),
            forward_lookup: self
                .forward_lookup
                .or(defaults.forward_lookup)
                .unwrap_or(true),
            strict_modes: self.strict_modes.or(defaults.strict_modes).unwrap_or(true),
            exclude_from: self.exclude_from.or_else(|| defaults.exclude_from.clone()),
            include_from: self.include_from.or_else(|| defaults.include_from.clone()),
            open_noatime: self.open_noatime.or(defaults.open_noatime).unwrap_or(false),
            // upstream: daemon-parm.h:78 default True; module value overrides the
            // global-section default (defaults.reverse_lookup), else built-in True.
            reverse_lookup: self
                .reverse_lookup
                .or(defaults.reverse_lookup)
                .unwrap_or(true),
            // upstream: daemon-parm.h:46 - a module `lock file` overrides the
            // daemon-wide lock file; `None` inherits it at connection setup.
            lock_file: self.lock_file,
            // upstream: loadparm.c syslog_tag/syslog_facility are P_LOCAL, so a
            // global-section value seeds every module's default (init_section
            // copy). A module directive overrides it; otherwise the module
            // inherits the global-section value, else the built-in default.
            syslog_tag: self.syslog_tag.or_else(|| defaults.syslog_tag.clone()),
            syslog_facility: self
                .syslog_facility
                .or_else(|| defaults.syslog_facility.clone()),
            filter,
            exclude,
            include,
        })
    }
}
