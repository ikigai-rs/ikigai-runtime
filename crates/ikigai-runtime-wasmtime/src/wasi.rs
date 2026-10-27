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
/// The directory opened is always the root joined with plain names.
///
/// **No symbolic link between the root and the preopen.** Each component below the jail root,
/// down to the preopen directory itself, is checked with `symlink_metadata`, and a link anywhere
/// refuses the grant, naming the link: ikigai-fs refuses a link anywhere in a path, and a
/// preopen opened through one would serve the link's target (`<jail>/data` → a directory
/// outside the jail) under a grant that names `<jail>/data`. The root itself may be a link; that
/// is the canonical-spelling case above, and a root resolving to `/` is refused at mount. A
/// component that does not exist ends the walk, and the preopen reports it. Links INSIDE the
/// preopened directory are wasmtime's: cap-std refuses one that leaves the preopen.
fn place(host_path: &str, root: &Path) -> Result<(PathBuf, String), String> {
    use ikigai_runtime::grants::guest_path_in_jail;
    let spelled = check_jail_root_path(root)?;
    let (base, guest) = match guest_path_in_jail(host_path, &spelled) {
        Ok(guest) => (root.to_path_buf(), guest),
        Err(refusal) => std::fs::canonicalize(root)
            .ok()
            .filter(|canonical| canonical.as_path() != root)
            .and_then(|canonical| {
                let spelled = check_jail_root_path(&canonical).ok()?;
                let guest = guest_path_in_jail(host_path, &spelled).ok()?;
                Some((canonical, guest))
            })
            .ok_or(refusal)?,
    };
    let relative = Path::new(guest.trim_start_matches('/'));
    if !relative
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(format!(
            "`{host_path}` does not name a path under the jail root"
        ));
    }
    let mut walked = base.clone();
    for name in relative.components() {
        walked.push(name);
        match std::fs::symlink_metadata(&walked) {
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
    Ok((walked, guest))
}

/// Apply a plan's grants to a deterministic builder. Errors name the grant the host could not
/// serve: an `fs` grant with no jail root, one outside the jail root (naming the root), one whose
/// path passes through a symbolic link below the root (naming the link), a preopen that does not
/// exist.
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
        let (dir, guest) = place(&preopen.host_path, root)
            .map_err(|reason| format!("urn:cap:fs grant for `{}`: {reason}", preopen.host_path))?;
        let perms = match preopen.access {
            FsAccess::ReadOnly => FsPerms::ReadOnly,
            FsAccess::ReadWrite => FsPerms::ReadWrite,
        };
        builder.preopened_dir(&dir, &guest, perms).map_err(|e| {
            format!(
                "urn:cap:fs grant for `{}`: preopen {}: {e:#}",
                preopen.host_path,
                dir.display()
            )
        })?;
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
        let rules: Vec<String> = plan.net.ip_rules.clone();
        let implicit = plan.net.implicit_unspecified_bind;
        let open = rules.is_empty() && implicit; // Root: every address.
        builder
            .allow_tcp(true)
            .allow_udp(true)
            .socket_addr_check(move |addr, usage| {
                let ok = open || admits(&rules, implicit, addr, usage);
                Box::pin(async move { ok })
            });
    }
    Ok(())
}

/// Whether a socket use is admitted by the plan's IP rules. An outbound connect needs the
/// implicit unspecified bind (`0.0.0.0:0`, measured, R1 B1); a rule `<ip>` admits any port,
/// `<ip>:<port>` exactly that one.
fn admits(rules: &[String], implicit: bool, addr: SocketAddr, usage: SocketAddrUse) -> bool {
    if implicit
        && matches!(usage, SocketAddrUse::TcpBind | SocketAddrUse::UdpBind)
        && addr.ip().is_unspecified()
        && addr.port() == 0
    {
        return true;
    }
    if !matches!(usage, SocketAddrUse::TcpConnect | SocketAddrUse::UdpSend) {
        return false;
    }
    rules.iter().any(|rule| match rule.rsplit_once(':') {
        Some((ip, port)) if port.bytes().all(|b| b.is_ascii_digit()) => {
            ip == addr.ip().to_string() && port == addr.port().to_string()
        }
        _ => rule == &addr.ip().to_string(),
    })
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

    #[test]
    fn an_ip_rule_admits_exactly_its_address_and_the_implicit_bind() {
        let rules = vec!["10.0.0.5:443".to_string()];
        let addr = |s: &str| s.parse::<SocketAddr>().unwrap();
        assert!(admits(
            &rules,
            true,
            addr("0.0.0.0:0"),
            SocketAddrUse::TcpBind
        ));
        assert!(admits(
            &rules,
            true,
            addr("10.0.0.5:443"),
            SocketAddrUse::TcpConnect
        ));
        assert!(!admits(
            &rules,
            true,
            addr("10.0.0.5:80"),
            SocketAddrUse::TcpConnect
        ));
        assert!(!admits(
            &rules,
            true,
            addr("10.0.0.6:443"),
            SocketAddrUse::TcpConnect
        ));
        // Listening is not connecting: an outbound grant never opens a server socket.
        assert!(!admits(
            &rules,
            true,
            addr("10.0.0.5:443"),
            SocketAddrUse::TcpListen
        ));
    }
}
