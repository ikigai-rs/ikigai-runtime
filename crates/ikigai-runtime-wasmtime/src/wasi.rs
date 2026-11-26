//! The WASI context a run gets, built from a [`WasiPlan`] (R-D3).
//!
//! Nothing is ever inherited from the host process. Without a grant the guest sees a **fixed**
//! clock and a **seeded** RNG (seeded from the artifact and input digests), captured and bounded
//! stdio, no args, no environment, no preopens, no sockets and no DNS. Each grant then unlocks
//! exactly its row of the table: a preopen per `fs` grant, an env value per `env` grant (the VALUE
//! from the host, never `inherit_env`), the kernel's injected [`Clock`] for
//! `clock:wall`, the host CSPRNG for `random:secure`, and an exact socket rule (plus the implicit
//! unspecified bind every connect needs) per IP-literal `net` grant.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ikigai_core::Clock;
use ikigai_runtime::grants::NetAccess;
use ikigai_runtime::{ClockAccess, FsAccess, RandomAccess, WasiPlan};
use rand::SeedableRng;
use wasmtime_wasi::sockets::SocketAddrUse;
use wasmtime_wasi::{FsPerms, HostMonotonicClock, HostWallClock, WasiCtxBuilder};

/// The bound on captured stdout and stderr, each. Past it a write is refused (the pipe traps),
/// never truncated (R-D3: stdout is authority too).
pub const STDIO_CAPACITY: usize = 1 << 20;

/// A 32-byte seed for a run's deterministic randomness: the digest of the artifact and of the
/// input, so the same call on the same artifact sees the same numbers on every machine.
pub fn seed_of(artifact: &[u8], input: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ikigai-runtime.wasi.seed.v1");
    hasher.update(&(artifact.len() as u64).to_le_bytes());
    hasher.update(artifact);
    hasher.update(input);
    *hasher.finalize().as_bytes()
}

/// The fixed clock a guest sees without `urn:cap:clock:wall`: the Unix epoch, forever, and a
/// monotonic clock that never advances.
#[derive(Clone, Copy, Debug, Default)]
pub struct FixedClock;

impl HostWallClock for FixedClock {
    fn resolution(&self) -> Duration {
        Duration::from_millis(1)
    }

    fn now(&self) -> Duration {
        Duration::ZERO
    }
}

impl HostMonotonicClock for FixedClock {
    fn resolution(&self) -> u64 {
        1_000_000
    }

    fn now(&self) -> u64 {
        0
    }
}

/// The kernel's injected [`Clock`] as WASI's wall and monotonic clocks (`urn:cap:clock:wall`):
/// never the OS clock, so a book's story clock or a test clock reaches the guest too.
#[derive(Clone)]
pub struct KernelClock(pub Arc<dyn Clock>);

impl KernelClock {
    fn millis(&self) -> u64 {
        self.0.now().as_millis()
    }
}

impl HostWallClock for KernelClock {
    fn resolution(&self) -> Duration {
        Duration::from_millis(1)
    }

    fn now(&self) -> Duration {
        Duration::from_millis(self.millis())
    }
}

impl HostMonotonicClock for KernelClock {
    fn resolution(&self) -> u64 {
        1_000_000
    }

    fn now(&self) -> u64 {
        self.millis().saturating_mul(1_000_000)
    }
}

/// The deny-by-default builder: a fixed clock, a seeded RNG, no network of any kind, no DNS, and
/// stdio left closed or discarded until the caller captures it. The starting point of every
/// WASI context the runtime builds.
pub fn deterministic_builder(seed: &[u8; 32]) -> WasiCtxBuilder {
    let mut builder = WasiCtxBuilder::new();
    builder
        .wall_clock(FixedClock)
        .monotonic_clock(FixedClock)
        .secure_random(rand::rngs::StdRng::from_seed(*seed))
        .insecure_random(rand::rngs::StdRng::from_seed(derive(seed, b"insecure")))
        .insecure_random_seed(u128::from_le_bytes(
            derive(seed, b"insecure-seed")[..16]
                .try_into()
                .expect("16 bytes"),
        ))
        .allow_tcp(false)
        .allow_udp(false)
        .allow_ip_name_lookup(false)
        .socket_addr_check(|_, _| Box::pin(async { false }));
    builder
}

fn derive(seed: &[u8; 32], label: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_keyed(seed);
    hasher.update(label);
    *hasher.finalize().as_bytes()
}

/// What only the HOST knows about an `app` world's WASI (R-D3): where a granted path lives on
/// disk, the VALUE of a granted environment variable (from the config home, never the process),
/// and the kernel's clock. A grant decides WHETHER; this says WHAT.
#[derive(Clone)]
pub struct AppHost {
    /// The jail root. An `fs` grant names a HOST path, as in ikigai-fs; it is preopened only
    /// when it is this root or under it, and appears to the guest as its part under the root
    /// (`<fs_root>/data` is `/data` in the guest). A grant outside the root is refused, naming
    /// both. The root must be absolute with plain segments and must not be `/`
    /// ([`check_jail_root_path`]; a mount refuses it). `None` refuses every `fs` grant at
    /// instantiation (a grant the host cannot serve is an error, never silently dropped).
    pub fs_root: Option<PathBuf>,
    /// Environment values for granted names. A granted name the host has no value for is set to
    /// nothing (the guest sees it absent), never read from the process.
    pub env: BTreeMap<String, String>,
    /// The kernel's injected clock, used only when the run holds `urn:cap:clock:wall`.
    pub clock: Arc<dyn Clock>,
}

impl Default for AppHost {
    fn default() -> Self {
        AppHost {
            fs_root: None,
            env: BTreeMap::new(),
            clock: Arc::new(ikigai_core::SystemClock),
        }
    }
}

/// The jail root as plain segments (`/srv/jail`, from `/srv/jail/` or `/srv/./jail`), checked by
/// [`check_jail_root`](ikigai_runtime::grants::check_jail_root): absolute, no `..`, valid UTF-8,
/// and not `/`. A mount refuses a root this refuses, so a misconfigured host fails when it is
/// configured, not on its first call; [`apply_plan`] checks again, since the field is public.
pub fn check_jail_root_path(root: &Path) -> Result<String, String> {
    let shown = root.display();
    let mut spelled = String::new();
    let mut absolute = false;
    for component in root.components() {
        match component {
            Component::RootDir => absolute = true,
            Component::CurDir => {}
            Component::Normal(name) => {
                let name = name
                    .to_str()
                    .ok_or_else(|| format!("the jail root `{shown}` is not valid UTF-8"))?;
                spelled.push('/');
                spelled.push_str(name);
            }
            Component::ParentDir | Component::Prefix(_) => {
                return Err(format!(
                    "the jail root `{shown}` must be an absolute path with plain segments"
                ))
            }
        }
    }
    if !absolute {
        return Err(format!("the jail root `{shown}` is not absolute"));
    }
    if spelled.is_empty() {
        spelled.push('/');
    }
    ikigai_runtime::grants::check_jail_root(&spelled)
        .map_err(|reason| format!("`{shown}`: {reason}"))?;
    Ok(spelled)
}

/// Where a granted host path is served from and seen at: `(the directory to open, the guest
/// path)`. The grant is placed against the jail root as configured and, failing that, as the
/// filesystem spells it (every link resolved: `/private/var/…` for `/var/…` on macOS), the two
/// spellings ikigai-fs places a scope against, so one grant string names one directory in both.
/// The directory answered is always the root joined with the grant's plain names below it, the
/// granted directory itself, whether or not it exists yet.
///
/// **No symbolic link between the root and the preopen.** Each existing component below the jail
/// root, down to the preopen directory itself, is checked with `symlink_metadata`, and a link
/// anywhere refuses the grant, naming the link: ikigai-fs refuses a link anywhere in a path, and a
/// preopen opened through one would serve the link's target (`<jail>/data` → a directory
/// outside the jail) under a grant that names `<jail>/data`. The root itself may be a link; that
/// is the canonical-spelling case above, and a root resolving to `/` is refused at mount. This
/// check names the link early; [`open_no_follow`] is what holds the preopen to it, since it opens
/// each component without following a link. Links INSIDE the preopened directory are wasmtime's:
/// cap-std refuses one that leaves the preopen.
fn place(host_path: &str, root: &Path) -> Result<(PathBuf, String), String> {
    let (base, guest) = place_base(host_path, root)?;
    let relative = Path::new(guest.trim_start_matches('/'));
    let mut walked = base.clone();
    for name in relative.components() {
        walked.push(name);
        // ambient: fs: reads only the link bit of each component between the host-configured jail root and a granted preopen, to refuse a grant that passes through a symbolic link
        #[allow(clippy::disallowed_methods)]
        let meta = std::fs::symlink_metadata(&walked);
        match meta {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!(
                    "`{}` is a symbolic link; an fs grant path must not pass through one, \
                     since the preopen would serve the link's target",
                    walked.display()
                ))
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    Ok((base.join(relative), guest))
}

/// The root spelling a grant places against (as configured, else as the filesystem spells it)
/// and the guest path, whose names below `/` are plain.
fn place_base(host_path: &str, root: &Path) -> Result<(PathBuf, String), String> {
    use ikigai_runtime::grants::guest_path_in_jail;
    let spelled = check_jail_root_path(root)?;
    let (base, guest) = match guest_path_in_jail(host_path, &spelled) {
        Ok(guest) => (root.to_path_buf(), guest),
        Err(refusal) => canonical_root(root)
            .filter(|canonical| canonical.as_path() != root)
            .and_then(|canonical| {
                let spelled = check_jail_root_path(&canonical).ok()?;
                let guest = guest_path_in_jail(host_path, &spelled).ok()?;
                Some((canonical, guest))
            })
            .ok_or(refusal)?,
    };
    if !Path::new(guest.trim_start_matches('/'))
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(format!(
            "`{host_path}` does not name a path under the jail root"
        ));
    }
    Ok((base, guest))
}

/// The jail root as the filesystem spells it, every link resolved.
fn canonical_root(root: &Path) -> Option<PathBuf> {
    // ambient: fs: resolves the host-configured jail root (never the grant path) through its links, so a scope spelled the way the filesystem spells the root still places
    #[allow(clippy::disallowed_methods)]
    let canonical = std::fs::canonicalize(root);
    canonical.ok()
}

// The placement of an `fs` exclusion below is a COPY of ikigai-fs 0.6.0's (`Roots`, `Rule::map`,
// `Rule::map_resolved`, `Jail::on_disk_names`, `caseless_eq`), because ikigai-fs keeps it
// private and this runtime cannot depend on an unpublished change to it. It belongs in ikigai-fs,
// beside the ACL it serves, as one public function this copy is then replaced by. Until then any
// change to how ikigai-fs places a deny must be made here too.

/// The names, below the jail root, an `fs` exclusion binds, as ikigai-fs maps it: its path placed
/// against the root as mounted and as the filesystem spells it, and again with its existing
/// prefix resolved through the host's links (`current/secret` binds `releases/v2/secret`),
/// against the canonical root. An exclusion naming the root or an ancestor of it answers `[]`
/// only when it names the root itself; an ancestor covers the jail less specifically than any
/// grant inside it, so it binds no preopen.
fn deny_names_in_jail(deny: &str, root: &Path) -> Vec<Vec<String>> {
    let segments = |path: &str| -> Vec<String> {
        path.split('/')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    };
    let mut roots: Vec<Vec<String>> = Vec::new();
    if let Ok(spelled) = check_jail_root_path(root) {
        roots.push(segments(&spelled));
    }
    let canonical = canonical_root(root)
        .and_then(|c| check_jail_root_path(&c).ok())
        .map(|c| segments(&c));
    if let Some(canonical) = &canonical {
        if !roots.contains(canonical) {
            roots.push(canonical.clone());
        }
    }
    let scope = segments(deny);
    let mut out: Vec<Vec<String>> = roots
        .iter()
        .filter_map(|r| scope.strip_prefix(r.as_slice()).map(<[String]>::to_vec))
        .collect();
    if let (Some(canonical), Some(resolved)) = (&canonical, resolve_host_path(Path::new(deny))) {
        if let Ok(resolved) = check_jail_root_path(&resolved).or_else(|_| {
            // The resolved path may be `/` itself, which is no jail root but is a path.
            (resolved == Path::new("/"))
                .then(|| "/".to_string())
                .ok_or(())
        }) {
            if let Some(rest) = segments(&resolved).strip_prefix(canonical.as_slice()) {
                let rest = rest.to_vec();
                if !out.contains(&rest) {
                    out.push(rest);
                }
            }
        }
    }
    out
}

/// A host path with its longest existing prefix resolved through every link, and the rest
/// appended as written (ikigai-fs's `resolve_host_path`).
fn resolve_host_path(path: &Path) -> Option<PathBuf> {
    let mut existing = path.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        // ambient: fs: resolves an fs EXCLUSION's path through the host's links (never to open it), so an exclusion spelled through a link binds the directory it leads to
        #[allow(clippy::disallowed_methods)]
        let resolved = std::fs::canonicalize(&existing);
        if let Ok(mut resolved) = resolved {
            resolved.extend(rest.iter().rev());
            return Some(resolved);
        }
        rest.push(existing.file_name()?.to_os_string());
        if !existing.pop() {
            return None;
        }
    }
}

/// Names below `root` in their on-disk spelling, as far as they exist without passing a link;
/// past that, as written. Answers the names and how many are on disk (ikigai-fs's
/// `on_disk_names`): on a volume that folds case, `WS` is the directory listed as `ws`.
fn on_disk_names(root: &Path, names: &[String]) -> (Vec<String>, usize) {
    let mut out: Vec<String> = Vec::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirEntryExt, MetadataExt};
        let mut dir = root.to_path_buf();
        for name in names {
            let path = dir.join(name);
            // ambient: fs: reads the inode and link bit of an fs EXCLUSION's components below the host-configured jail root, to learn their on-disk names; nothing is opened for the guest
            #[allow(clippy::disallowed_methods)]
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                break;
            };
            if meta.file_type().is_symlink() {
                break;
            }
            // ambient: fs: lists the directory holding an exclusion's component to find the entry with its inode, its on-disk name
            #[allow(clippy::disallowed_methods)]
            let Ok(entries) = std::fs::read_dir(&dir) else {
                break;
            };
            let real = entries
                .flatten()
                .find(|e| e.ino() == meta.ino())
                .and_then(|e| e.file_name().into_string().ok());
            let Some(real) = real else { break };
            dir.push(&real);
            out.push(real);
        }
    }
    #[cfg(not(unix))]
    let _ = root;
    let on_disk = out.len();
    out.extend(names[on_disk..].iter().cloned());
    (out, on_disk)
}

/// Whether a volume that folds case could open `a` and `b` as one name. ikigai-fs also folds
/// Unicode normalization; this copy folds case only, so it binds a little less widely there.
fn caseless_eq(a: &str, b: &str) -> bool {
    a == b || a.to_lowercase() == b.to_lowercase()
}

/// The first exclusion a preopen at `preopen` (names below the jail root) would serve, with the
/// names it binds: one whose mapped names are the preopen's or under them, compared by on-disk
/// name where the exclusion exists and without case where it does not, as ikigai-fs compares.
fn deny_inside<'a>(
    plan: &'a WasiPlan,
    access: FsAccess,
    preopen: &[String],
    root: &Path,
) -> Option<(&'a ikigai_runtime::grants::FsDeny, String)> {
    let (preopen, _) = on_disk_names(root, preopen);
    plan.fs_denies
        .iter()
        .filter(|deny| deny.bound_by(access))
        .find_map(|deny| {
            deny_names_in_jail(&deny.host_path, root)
                .into_iter()
                .find_map(|names| {
                    let (names, on_disk) = on_disk_names(root, &names);
                    let inside = names.len() >= preopen.len()
                        && names.iter().zip(&preopen).enumerate().all(|(i, (d, p))| {
                            if i < on_disk {
                                d == p
                            } else {
                                caseless_eq(d, p)
                            }
                        });
                    inside.then(|| format!("/{}", names.join("/")))
                })
                .map(|names| (deny, names))
        })
}

/// Open `relative` (plain names) below `base` one component at a time from a handle on `base`,
/// never following a link: a link anywhere below the root refuses the open, so the directory
/// opened is the one [`place`] checked even if a link is swapped in afterwards. Answers the
/// handle and a path naming exactly that open directory, for wasmtime's path-taking preopen.
#[cfg(unix)]
fn open_no_follow(base: &Path, relative: &Path) -> Result<(std::os::fd::OwnedFd, PathBuf), String> {
    use rustix::fs::{Mode, OFlags};
    use std::os::fd::AsRawFd;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    // ambient: fs: the preopen's anchor, a handle on the host-configured jail root (its own links followed: host configuration, not a grant); every grant name is then opened relative to it
    #[allow(clippy::disallowed_methods)]
    let mut dir = rustix::fs::open(base, flags, Mode::empty())
        .map_err(|e| format!("the jail root `{}` cannot be opened ({e})", base.display()))?;
    let mut walked = base.to_path_buf();
    for name in relative.components() {
        walked.push(name);
        let next = rustix::fs::openat(
            &dir,
            name.as_os_str(),
            flags | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|e| {
            if e == rustix::io::Errno::LOOP || e == rustix::io::Errno::NOTDIR {
                format!(
                    "`{}` is a symbolic link or not a directory; an fs grant path must not \
                         pass through a link, since the preopen would serve the link's target",
                    walked.display()
                )
            } else {
                format!("`{}` cannot be opened ({e})", walked.display())
            }
        })?;
        dir = next;
    }
    // wasmtime-wasi 50 preopens only by path. On Linux `/proc/self/fd/<n>` is that path: opening
    // it opens the directory the handle holds, never a name looked up again, so a link swapped
    // in after this walk is not seen. macOS's `/dev/fd/<n>` does not open as a directory, so
    // there the checked path itself is answered, and the window between this walk and
    // wasmtime's own open stays open until wasmtime-wasi takes an opened directory.
    #[cfg(target_os = "linux")]
    let path = PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd()));
    #[cfg(not(target_os = "linux"))]
    let path = {
        let _ = dir.as_raw_fd();
        walked
    };
    Ok((dir, path))
}

/// Apply a plan's grants to a deterministic builder. Errors name the grant the host could not
/// serve: an `fs` grant with no jail root, one outside the jail root (naming the root), one whose
/// path passes through a symbolic link below the root (naming the link), one whose preopen would
/// serve a path an `fs` exclusion names however it is spelled (naming the exclusion), a preopen
/// that does not exist.
pub fn apply_plan(
    builder: &mut WasiCtxBuilder,
    plan: &WasiPlan,
    host: &AppHost,
) -> Result<(), String> {
    for preopen in &plan.fs {
        let Some(root) = &host.fs_root else {
            return Err(format!(
                "urn:cap:fs grant for `{}`: this host has no jail root to serve it from",
                preopen.host_path
            ));
        };
        let refuse =
            |reason: String| format!("urn:cap:fs grant for `{}`: {reason}", preopen.host_path);
        let (dir, guest) = place(&preopen.host_path, root).map_err(refuse)?;
        let names: Vec<String> = guest
            .split('/')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        if let Some((deny, bound)) = deny_inside(plan, preopen.access, &names, root) {
            return Err(refuse(format!(
                "the exclusion {} binds `{bound}` below the jail root, at or under this \
                 preopen (`{guest}`), and a WASI preopen cannot exclude a path inside it; the \
                 grant is refused rather than widened",
                deny.scope
            )));
        }
        let perms = match preopen.access {
            FsAccess::ReadOnly => FsPerms::ReadOnly,
            FsAccess::ReadWrite => FsPerms::ReadWrite,
        };
        let shown = dir.display();
        #[cfg(unix)]
        {
            let (base, _) = place_base(&preopen.host_path, root).map_err(refuse)?;
            let (handle, opened) = open_no_follow(&base, Path::new(guest.trim_start_matches('/')))
                .map_err(|reason| refuse(format!("preopen {shown}: {reason}")))?;
            builder
                .preopened_dir(&opened, &guest, perms)
                .map_err(|e| refuse(format!("preopen {shown}: {e:#}")))?;
            drop(handle);
        }
        #[cfg(not(unix))]
        builder
            .preopened_dir(&dir, &guest, perms)
            .map_err(|e| refuse(format!("preopen {shown}: {e:#}")))?;
    }
    for name in &plan.env_names {
        if let Some(value) = host.env.get(name) {
            builder.env(name, value);
        }
    }
    if plan.clock == ClockAccess::Wall {
        let clock = KernelClock(Arc::clone(&host.clock));
        builder.wall_clock(clock.clone()).monotonic_clock(clock);
    }
    if plan.random == RandomAccess::Secure {
        builder.secure_random(rand::rngs::StdRng::from_rng(&mut rand::rng()));
    }
    if !plan.net.ip_rules.is_empty() || plan.net.implicit_unspecified_bind {
        let open = plan.net.ip_rules.is_empty() && plan.net.implicit_unspecified_bind; // Root.
        let net = Socket::of(&plan.net);
        builder
            .allow_tcp(true)
            .allow_udp(true)
            .socket_addr_check(move |addr, usage| {
                let ok = open || net.admits(addr, usage);
                Box::pin(async move { ok })
            });
    }
    Ok(())
}

/// What a guest's socket uses are answered under: the plan's socket access list as a capability,
/// asked of ikigai-http, so a socket and an HTTP request read one access list one way.
#[derive(Clone, Debug)]
struct Socket {
    access: ikigai_core::Capability,
    implicit_unspecified_bind: bool,
}

impl Socket {
    fn of(net: &NetAccess) -> Socket {
        Socket {
            access: ikigai_core::Capability::scoped(net.socket_scopes.iter().cloned()),
            implicit_unspecified_bind: net.implicit_unspecified_bind,
        }
    }

    /// Whether a socket use is admitted. An outbound connect needs the implicit unspecified bind
    /// (`0.0.0.0:0`, measured, R1 B1); a connect or send is admitted exactly when
    /// `ikigai_http::net_allows_port` admits its address and port under the access list (a
    /// socket has no path, so `/`). Nothing else (listening, any other bind) is admitted.
    fn admits(&self, addr: SocketAddr, usage: SocketAddrUse) -> bool {
        if self.implicit_unspecified_bind
            && matches!(usage, SocketAddrUse::TcpBind | SocketAddrUse::UdpBind)
            && addr.ip().is_unspecified()
            && addr.port() == 0
        {
            return true;
        }
        matches!(usage, SocketAddrUse::TcpConnect | SocketAddrUse::UdpSend)
            && ikigai_http::net_allows_port(
                &self.access,
                &addr.ip().to_string(),
                Some(addr.port()),
                "/",
            )
    }
}

/// Whether the plan's socket rules admit a use (the check [`apply_plan`] installs, short of the
/// root plan's every-address case).
#[cfg(test)]
fn socket_admits(net: &NetAccess, addr: SocketAddr, usage: SocketAddrUse) -> bool {
    Socket::of(net).admits(addr, usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_plan_refuses_a_preopen_outside_the_jail_root_even_if_the_plan_holds_one() {
        // A plan built by hand: `wasi_plan` refuses the first spelling itself, and only the jail
        // placement can refuse the second.
        let plan =
            ikigai_runtime::wasi_plan(&ikigai_runtime::Grants::from_scopes(std::iter::empty::<
                String,
            >()))
            .unwrap();
        let root = std::env::temp_dir().join("ikigai-runtime-apply-plan");
        let spelled = check_jail_root_path(&root).unwrap();
        let host = AppHost {
            fs_root: Some(root.clone()),
            ..AppHost::default()
        };
        for path in [format!("{spelled}/../outside"), "/elsewhere".to_string()] {
            let mut plan = plan.clone();
            plan.fs.push(ikigai_runtime::grants::FsPreopen {
                host_path: path.clone(),
                access: FsAccess::ReadOnly,
            });
            let mut builder = deterministic_builder(&[0; 32]);
            let err = apply_plan(&mut builder, &plan, &host).unwrap_err();
            assert!(err.contains(&path), "{err}");
        }
    }

    #[test]
    fn a_grant_is_placed_under_the_jail_root_and_seen_at_its_part_below_it() {
        let root = Path::new("/srv/jail");
        assert_eq!(
            place("/srv/jail/ws", root).unwrap(),
            (PathBuf::from("/srv/jail/ws"), "/ws".to_string())
        );
        assert_eq!(
            place("/srv/jail", root).unwrap(),
            (PathBuf::from("/srv/jail"), "/".to_string())
        );
        let err = place("/ws", root).unwrap_err();
        assert!(err.contains("/ws") && err.contains("/srv/jail"), "{err}");
    }

    /// ikigai-fs places a scope against the root as mounted AND as the filesystem spells it, so
    /// a grant written against either spelling names one directory in both.
    #[cfg(unix)]
    #[test]
    fn a_grant_spelled_against_the_resolved_jail_root_is_placed_too() {
        let base =
            std::env::temp_dir().join(format!("ikigai-runtime-place-{}", std::process::id()));
        let real = base.join("real");
        let link = base.join("link");
        std::fs::create_dir_all(real.join("ws")).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let resolved = std::fs::canonicalize(&real).unwrap();
        let grant = format!("{}/ws", check_jail_root_path(&resolved).unwrap());
        let placed = place(&grant, &link);
        let outside = place("/elsewhere/ws", &link);
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(placed.unwrap(), (resolved.join("ws"), "/ws".to_string()));
        assert!(outside.is_err());
    }

    /// At 1a0ee9a3 `<jail>/data` as a link to a directory outside the jail was preopened, and the
    /// guest read the link's target under a grant naming `<jail>/data`.
    #[cfg(unix)]
    #[test]
    fn a_link_anywhere_below_the_jail_root_refuses_the_grant_naming_the_link() {
        let base =
            std::env::temp_dir().join(format!("ikigai-runtime-links-{}", std::process::id()));
        let root = base.join("jail");
        let outside = base.join("outside");
        std::fs::create_dir_all(root.join("real").join("sub")).unwrap();
        std::fs::create_dir_all(outside.join("sub")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("leaf")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("mid")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("inner")).unwrap();
        let spelled = check_jail_root_path(&root).unwrap();
        let at = |rel: &str| place(&format!("{spelled}/{rel}"), &root);
        let results = [
            ("leaf", at("leaf")),
            ("mid", at("mid/sub")),
            // A link that stays inside the jail is still a second name for a directory.
            ("inner", at("inner/sub")),
        ];
        let plain = at("real/sub");
        let _ = std::fs::remove_dir_all(&base);
        for (link, result) in results {
            let err = result.unwrap_err();
            assert!(
                err.contains(&format!("{spelled}/{link}`")) && err.contains("symbolic link"),
                "{err}"
            );
        }
        assert_eq!(plain.unwrap().0, root.join("real").join("sub"));
    }

    #[test]
    fn a_jail_root_of_slash_or_a_relative_or_climbing_one_is_refused() {
        for bad in ["/", "srv/jail", "/srv/../jail"] {
            assert!(check_jail_root_path(Path::new(bad)).is_err(), "{bad}");
        }
        assert_eq!(
            check_jail_root_path(Path::new("/srv/./jail/")).unwrap(),
            "/srv/jail"
        );
    }

    #[test]
    fn the_seed_depends_on_artifact_and_input() {
        let a = seed_of(b"artifact", b"in");
        assert_eq!(a, seed_of(b"artifact", b"in"));
        assert_ne!(a, seed_of(b"artifact", b"other"));
        assert_ne!(a, seed_of(b"other", b"in"));
        // The artifact's length is part of the seed, so the boundary cannot shift.
        assert_ne!(seed_of(b"ab", b"c"), seed_of(b"a", b"bc"));
    }

    fn plan_of(scopes: &[&str]) -> WasiPlan {
        ikigai_runtime::wasi_plan(&ikigai_runtime::Grants::from_scopes(scopes.iter().copied()))
            .unwrap()
    }

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn an_ip_rule_admits_exactly_its_address_and_the_implicit_bind() {
        let net = plan_of(&["urn:cap:net:10.0.0.5:443"]).net;
        assert!(socket_admits(
            &net,
            addr("0.0.0.0:0"),
            SocketAddrUse::TcpBind
        ));
        assert!(socket_admits(
            &net,
            addr("10.0.0.5:443"),
            SocketAddrUse::TcpConnect
        ));
        assert!(!socket_admits(
            &net,
            addr("10.0.0.5:80"),
            SocketAddrUse::TcpConnect
        ));
        assert!(!socket_admits(
            &net,
            addr("10.0.0.6:443"),
            SocketAddrUse::TcpConnect
        ));
        // Listening is not connecting: an outbound grant never opens a server socket.
        assert!(!socket_admits(
            &net,
            addr("10.0.0.5:443"),
            SocketAddrUse::TcpListen
        ));
        // A host or glob grant is `wasi:http`'s, never a socket's.
        let net = plan_of(&["urn:cap:net:10.0.0.5:443", "urn:cap:net:*"]).net;
        assert!(!socket_admits(
            &net,
            addr("10.0.0.6:443"),
            SocketAddrUse::TcpConnect
        ));
    }

    /// ikigai-http reads `net:10.0.0.5` + `net:-10.0.0.5:22` as "every port but 22", and an
    /// exclusion that ties an allow (the same address and port) as a refusal.
    #[test]
    fn a_net_exclusion_binds_the_socket_as_it_binds_ikigai_http() {
        for (scopes, port) in [
            (["urn:cap:net:10.0.0.5", "urn:cap:net:-10.0.0.5:22"], 22),
            (
                ["urn:cap:net:10.0.0.5:443", "urn:cap:net:-10.0.0.5:443"],
                443,
            ),
        ] {
            let cap = ikigai_core::Capability::scoped(scopes);
            assert!(
                !ikigai_http::net_allows_port(&cap, "10.0.0.5", Some(port), "/"),
                "ikigai-http refuses 10.0.0.5:{port} under {scopes:?}"
            );
            let plan = ikigai_runtime::wasi_plan(&ikigai_runtime::Grants::from_capability(&cap));
            if let Ok(plan) = plan {
                let at: SocketAddr = format!("10.0.0.5:{port}").parse().unwrap();
                assert!(
                    !socket_admits(&plan.net, at, SocketAddrUse::TcpConnect),
                    "the socket admits {at}, which {scopes:?} excludes; plan.net = {:?}",
                    plan.net
                );
            }
        }
        // The rest of the address is still served.
        let net = plan_of(&["urn:cap:net:10.0.0.5", "urn:cap:net:-10.0.0.5:22"]).net;
        assert!(socket_admits(
            &net,
            addr("10.0.0.5:443"),
            SocketAddrUse::TcpConnect
        ));
        assert!(!socket_admits(
            &net,
            addr("10.0.0.5:22"),
            SocketAddrUse::UdpSend
        ));
    }

    /// A malformed exclusion: the holder meant to exclude something and nobody can tell what,
    /// so ikigai-http refuses every address under it, and so does the socket.
    #[test]
    fn a_malformed_net_exclusion_refuses_every_socket_as_ikigai_http_does() {
        let scopes = ["urn:cap:net:10.0.0.5:443", "urn:cap:net:-10.0.0.5:notaport"];
        let cap = ikigai_core::Capability::scoped(scopes);
        assert!(!ikigai_http::net_allows_port(
            &cap,
            "10.0.0.5",
            Some(443),
            "/"
        ));
        let net = plan_of(&scopes).net;
        assert!(!socket_admits(
            &net,
            addr("10.0.0.5:443"),
            SocketAddrUse::TcpConnect
        ));
    }

    /// A socket carries no path, so an exclusion of a path on an address the run may reach by
    /// socket is refused with a sentence; one on a host name is `wasi:http`'s and changes nothing.
    #[test]
    fn a_path_scoped_net_exclusion_under_an_ip_grant_refuses_the_plan() {
        for deny in ["urn:cap:net:-10.0.0.5/admin", "urn:cap:net:-*/admin"] {
            let err = ikigai_runtime::wasi_plan(&ikigai_runtime::Grants::from_scopes([
                "urn:cap:net:10.0.0.5",
                deny,
            ]))
            .unwrap_err();
            assert_eq!(err.scope, deny);
            assert!(
                err.reason.contains("10.0.0.5") && err.reason.contains("path"),
                "{err}"
            );
        }
        let net = plan_of(&["urn:cap:net:10.0.0.5", "urn:cap:net:-api.example.com/admin"]).net;
        assert!(socket_admits(
            &net,
            addr("10.0.0.5:443"),
            SocketAddrUse::TcpConnect
        ));
    }

    /// Build the plan from `scopes` and apply it for a jail at `root`: Ok when both accept.
    #[cfg(unix)]
    fn serve(scopes: &[String], root: &Path) -> Result<WasiPlan, String> {
        let plan =
            ikigai_runtime::wasi_plan(&ikigai_runtime::Grants::from_scopes(scopes.iter().cloned()))
                .map_err(|e| e.to_string())?;
        let host = AppHost {
            fs_root: Some(root.to_path_buf()),
            ..AppHost::default()
        };
        let mut builder = deterministic_builder(&[0; 32]);
        apply_plan(&mut builder, &plan, &host)?;
        Ok(plan)
    }

    /// An `fs` exclusion spelled against the root's OTHER spelling (the one `place` itself
    /// accepts for a grant) names a directory inside the preopen; ikigai-fs places a deny
    /// against both spellings.
    #[cfg(unix)]
    #[test]
    fn an_fs_exclusion_spelled_against_the_resolved_root_refuses_the_preopen() {
        let base =
            std::env::temp_dir().join(format!("ikigai-runtime-deny-b1-{}", std::process::id()));
        let real = base.join("real");
        let link = base.join("link");
        std::fs::create_dir_all(real.join("ws").join("secret")).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let mounted = check_jail_root_path(&link).unwrap();
        let resolved = check_jail_root_path(&std::fs::canonicalize(&real).unwrap()).unwrap();
        let scopes = [
            format!("urn:cap:fs:read:{mounted}/ws"),
            format!("urn:cap:fs:read:-{resolved}/ws/secret"),
        ];
        let outcome = serve(&scopes, &link);
        // Without the exclusion the same preopen is served.
        let alone = serve(&scopes[..1], &link);
        let _ = std::fs::remove_dir_all(&base);
        let err = outcome.expect_err("the preopen of `ws` serves `secret`, which is excluded");
        assert!(err.contains(&scopes[1]), "{err}");
        assert!(alone.is_ok(), "{alone:?}");
    }

    /// An `fs` exclusion spelled through a link INSIDE the jail (`current/secret` for
    /// `releases/v2/secret`): ikigai-fs resolves a deny through links and binds the file the
    /// link leads to.
    #[cfg(unix)]
    #[test]
    fn an_fs_exclusion_spelled_through_a_link_inside_the_jail_refuses_the_preopen() {
        let base =
            std::env::temp_dir().join(format!("ikigai-runtime-deny-b2-{}", std::process::id()));
        let root = std::fs::canonicalize({
            std::fs::create_dir_all(base.join("jail")).unwrap();
            base.join("jail")
        })
        .unwrap();
        std::fs::create_dir_all(root.join("releases").join("v2").join("secret")).unwrap();
        std::os::unix::fs::symlink(root.join("releases").join("v2"), root.join("current")).unwrap();
        let spelled = check_jail_root_path(&root).unwrap();
        let scopes = [
            format!("urn:cap:fs:read:{spelled}/releases"),
            format!("urn:cap:fs:read:-{spelled}/current/secret"),
        ];
        let outcome = serve(&scopes, &root);
        // The same exclusion beside a preopen it does not reach changes nothing.
        std::fs::create_dir_all(root.join("other")).unwrap();
        let apart = serve(
            &[
                format!("urn:cap:fs:read:{spelled}/other"),
                scopes[1].clone(),
            ],
            &root,
        );
        let _ = std::fs::remove_dir_all(&base);
        let err = outcome.expect_err("the preopen of `releases` serves `releases/v2/secret`");
        assert!(err.contains("/releases/v2/secret"), "{err}");
        assert!(apart.is_ok(), "{apart:?}");
    }

    /// macOS's case-insensitive APFS only: an `fs` exclusion spelled in another case names the
    /// same directory on disk, and ikigai-fs maps a deny to its on-disk names.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_fs_exclusion_spelled_in_another_case_refuses_the_preopen() {
        let base =
            std::env::temp_dir().join(format!("ikigai-runtime-deny-b3-{}", std::process::id()));
        std::fs::create_dir_all(base.join("ws").join("secret")).unwrap();
        let root = std::fs::canonicalize(&base).unwrap();
        let spelled = check_jail_root_path(&root).unwrap();
        let scopes = [
            format!("urn:cap:fs:read:{spelled}/ws"),
            format!("urn:cap:fs:read:-{spelled}/WS/Secret"),
        ];
        let outcome = serve(&scopes, &root);
        let _ = std::fs::remove_dir_all(&base);
        assert!(
            outcome.is_err(),
            "the preopen of `{spelled}/ws` serves `secret`, which `{}` excludes: {outcome:?}",
            scopes[1]
        );
    }

    /// An exclusion naming something not on disk is compared without case, as ikigai-fs does:
    /// the volume's rules for a name that does not exist yet are unknowable.
    #[cfg(unix)]
    #[test]
    fn an_fs_exclusion_of_a_missing_path_is_compared_without_case() {
        let base =
            std::env::temp_dir().join(format!("ikigai-runtime-deny-case-{}", std::process::id()));
        std::fs::create_dir_all(base.join("ws")).unwrap();
        let root = std::fs::canonicalize(&base).unwrap();
        let spelled = check_jail_root_path(&root).unwrap();
        // `ws` exists, so it is compared by its on-disk name; `Later` does not, so any case.
        let outcome = serve(
            &[
                format!("urn:cap:fs:read:{spelled}/ws"),
                format!("urn:cap:fs:read:-{spelled}/ws/Later"),
            ],
            &root,
        );
        let _ = std::fs::remove_dir_all(&base);
        assert!(outcome.is_err(), "{outcome:?}");
    }

    /// The directory `place` answers is the granted one even when a component is missing: never
    /// the path walked so far, an ancestor that holds more than the grant.
    #[test]
    fn place_answers_the_granted_directory_when_a_component_is_missing() {
        let root =
            std::env::temp_dir().join(format!("ikigai-runtime-place-c-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let spelled = check_jail_root_path(&root).unwrap();
        let placed = place(&format!("{spelled}/missing/sub/leaf"), &root);
        let _ = std::fs::remove_dir_all(&root);
        let (dir, guest) = placed.unwrap();
        assert_eq!(guest, "/missing/sub/leaf");
        assert_eq!(
            dir,
            root.join("missing").join("sub").join("leaf"),
            "the directory to open is not the granted one"
        );
    }

    /// A link swapped in AFTER placement (where `place` saw a missing component, or a plain
    /// directory) is not followed: the preopen is opened one component at a time without
    /// following a link, and refuses the link.
    #[cfg(unix)]
    #[test]
    fn a_link_swapped_in_after_placement_is_refused_at_the_open() {
        let base = std::env::temp_dir().join(format!("ikigai-runtime-swap-{}", std::process::id()));
        let root = base.join("jail");
        let outside = base.join("outside");
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::create_dir_all(outside.join("sub")).unwrap();
        let spelled = check_jail_root_path(&root).unwrap();
        // Placed while `data` is a plain directory and `missing` does not exist.
        let placed_data = place(&format!("{spelled}/data"), &root);
        let placed_missing = place(&format!("{spelled}/missing/sub"), &root);
        // Then both become links out of the jail.
        std::fs::remove_dir(root.join("data")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("data")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("missing")).unwrap();
        let opened_data = open_no_follow(&root, Path::new("data"));
        let opened_missing = open_no_follow(&root, Path::new("missing/sub"));
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(placed_data.unwrap().0, root.join("data"));
        assert_eq!(placed_missing.unwrap().0, root.join("missing").join("sub"));
        for opened in [opened_data, opened_missing] {
            let err = opened.unwrap_err();
            assert!(err.contains("symbolic link"), "{err}");
        }
    }

    /// The path handed to wasmtime names the very directory the no-follow walk opened.
    #[cfg(unix)]
    #[test]
    fn a_preopen_is_served_from_the_directory_the_walk_opened() {
        let base = std::env::temp_dir().join(format!("ikigai-runtime-walk-{}", std::process::id()));
        let root = base.join("jail");
        std::fs::create_dir_all(root.join("ws").join("in")).unwrap();
        std::fs::write(root.join("ws").join("in").join("marker"), b"x").unwrap();
        let (handle, path) = open_no_follow(&root, Path::new("ws/in")).unwrap();
        let seen = std::fs::read(path.join("marker"));
        let scopes = [format!(
            "urn:cap:fs:read:{}/ws/in",
            check_jail_root_path(&root).unwrap()
        )];
        let served = serve(&scopes, &root);
        drop(handle);
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(seen.unwrap(), b"x");
        assert!(served.is_ok(), "{served:?}");
    }
}

/// The bound on a J2 session's captured stdout and stderr, each (R-D6 point 2, amended
/// 2026-11-12). Past it a write is DROPPED and counted, never refused: a session's answer is its
/// reply, so a chatty guest loses its log, not its call.
pub const SESSION_STDIO_CAPACITY: usize = 64 * 1024;

/// What a J2 session's guest wrote to stdout and stderr, for a host to log or discard. Never the
/// host's own stdio and never the representation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionStdio {
    /// The first [`SESSION_STDIO_CAPACITY`] bytes written to stdout.
    pub stdout: Vec<u8>,
    /// The first [`SESSION_STDIO_CAPACITY`] bytes written to stderr.
    pub stderr: Vec<u8>,
    /// Bytes written to stdout past the bound, dropped.
    pub stdout_dropped: u64,
    /// Bytes written to stderr past the bound, dropped.
    pub stderr_dropped: u64,
}

/// A bounded output capture that keeps the first `capacity` bytes and counts the rest. Every
/// write succeeds, so the guest never sees the bound.
#[derive(Clone, Debug)]
pub(crate) struct BoundedCapture {
    state: Arc<std::sync::Mutex<(Vec<u8>, u64)>>,
    capacity: usize,
}

impl BoundedCapture {
    pub(crate) fn new(capacity: usize) -> Self {
        BoundedCapture {
            state: Arc::default(),
            capacity,
        }
    }

    fn take(&self, bytes: &[u8]) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let room = self.capacity.saturating_sub(state.0.len());
        let kept = room.min(bytes.len());
        state.0.extend_from_slice(&bytes[..kept]);
        state.1 += (bytes.len() - kept) as u64;
    }

    /// The bytes kept and the count dropped.
    pub(crate) fn contents(&self) -> (Vec<u8>, u64) {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        (state.0.clone(), state.1)
    }
}

impl wasmtime_wasi::cli::IsTerminal for BoundedCapture {
    fn is_terminal(&self) -> bool {
        false
    }
}

impl wasmtime_wasi::cli::StdoutStream for BoundedCapture {
    fn p2_stream(&self) -> Box<dyn wasmtime_wasi::p2::OutputStream> {
        Box::new(self.clone())
    }

    fn async_stream(&self) -> Box<dyn tokio::io::AsyncWrite + Send + Sync> {
        Box::new(self.clone())
    }
}

impl wasmtime_wasi::p2::OutputStream for BoundedCapture {
    fn write(&mut self, bytes: bytes::Bytes) -> wasmtime_wasi::p2::StreamResult<()> {
        self.take(&bytes);
        Ok(())
    }

    fn flush(&mut self) -> wasmtime_wasi::p2::StreamResult<()> {
        Ok(())
    }

    fn check_write(&mut self) -> wasmtime_wasi::p2::StreamResult<usize> {
        // Always ready: past the bound the bytes are dropped, so there is no backpressure.
        Ok(SESSION_STDIO_CAPACITY)
    }
}

#[async_trait::async_trait]
impl wasmtime_wasi::p2::Pollable for BoundedCapture {
    async fn ready(&mut self) {}
}

impl tokio::io::AsyncWrite for BoundedCapture {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.take(buf);
        std::task::Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}
