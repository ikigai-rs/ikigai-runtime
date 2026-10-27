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
use std::path::PathBuf;
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
    /// The jail root: a granted `fs` path `<p>` is preopened from `<fs_root>/<p>` and appears to
    /// the guest as `<p>`. `None` refuses every `fs` grant at instantiation (a grant the host
    /// cannot serve is an error, never silently dropped).
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

/// Apply a plan's grants to a deterministic builder. Errors name the grant the host could not
/// serve (an `fs` grant with no jail root, a preopen that does not exist).
pub fn apply_plan(
    builder: &mut WasiCtxBuilder,
    plan: &WasiPlan,
    host: &AppHost,
) -> Result<(), String> {
    for preopen in &plan.fs {
        let Some(root) = &host.fs_root else {
            return Err(format!(
                "urn:cap:fs grant for `{}`: this host has no jail root to serve it from",
                preopen.guest_path
            ));
        };
        let host_path = root.join(preopen.guest_path.trim_start_matches('/'));
        let perms = match preopen.access {
            FsAccess::ReadOnly => FsPerms::ReadOnly,
            FsAccess::ReadWrite => FsPerms::ReadWrite,
        };
        builder
            .preopened_dir(&host_path, &preopen.guest_path, perms)
            .map_err(|e| {
                format!(
                    "urn:cap:fs grant for `{}`: preopen {}: {e:#}",
                    preopen.guest_path,
                    host_path.display()
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
