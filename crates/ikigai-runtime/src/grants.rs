//! The **grant → WASI plan** (R-D3): a pure function from the capabilities a caller holds to the
//! WASI mechanisms a run is allowed, deny-by-default. Nothing is ever inherited from the host
//! process; the clock and randomness are always injected (deterministic unless a grant says
//! otherwise). The engine turns a [`WasiPlan`] into an actual WASI context; keeping the decision
//! pure means it is tested as a table, one case per row.
//!
//! The new one-segment capability families this introduces (R-D3, open question 1): `urn:cap:env:<NAME>`,
//! `urn:cap:clock:wall`, `urn:cap:random:secure`. They are one segment each, not one coarse
//! `urn:cap:runtime:*` key, because the same authorities are needed by a native app or an
//! ordinary endpoint, and a trailing wildcard is a presence test that would bundle all three
//! behind one grant.

use ikigai_core::Capability;
use std::collections::BTreeSet;

/// The capabilities a caller holds, as the plan reads them.
#[derive(Clone, Debug)]
pub enum Grants {
    /// Full authority (a resource owner's root). Every WASI mechanism is unlocked — the native,
    /// trusted case (R-D9); it never arises in the jail.
    Root,
    /// Exactly these `urn:cap:` scopes.
    Scoped(BTreeSet<String>),
}

impl Grants {
    /// The grants a [`Capability`] carries. `Capability::Root` has no scope set and becomes
    /// [`Grants::Root`].
    pub fn from_capability(cap: &Capability) -> Grants {
        match cap.scopes() {
            None => Grants::Root,
            Some(scopes) => Grants::Scoped(scopes.clone()),
        }
    }

    /// Build from an iterator of scope strings (for tests and hosts that compose scopes directly).
    pub fn from_scopes<I, S>(scopes: I) -> Grants
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Grants::Scoped(scopes.into_iter().map(Into::into).collect())
    }
}

/// Read-only or read-write preopen (R-D3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsAccess {
    ReadOnly,
    ReadWrite,
}

/// One preopened directory in the plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsPreopen {
    /// The path as the guest sees it (also the deepest grant's root).
    pub guest_path: String,
    pub access: FsAccess,
}

/// Whether the guest sees the injected kernel wall clock, or a fixed one (R-D3). A run holding
/// `clock:wall` is uncacheable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockAccess {
    /// No `clock:wall`: a fixed, deterministic clock.
    Fixed,
    /// `urn:cap:clock:wall`: the kernel's injected `Clock` (never the OS clock).
    Wall,
}

/// Whether the guest sees a secure CSPRNG or a deterministic seeded one (R-D3). A run holding
/// `random:secure` is uncacheable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RandomAccess {
    /// No `random:secure`: a deterministic RNG seeded from the artifact and input digests.
    Seeded,
    /// `urn:cap:random:secure`: the host CSPRNG.
    Secure,
}

/// The network access a run is granted (R-D3).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetAccess {
    /// `<ip>:<port>` literals admitted at the socket layer (raw TCP/UDP).
    pub ip_rules: Vec<String>,
    /// Host or glob rules routed to `urn:http:*` through `wasi:http` only — a socket check sees
    /// an IP, never a host name, so these cannot be enforced at a socket.
    pub http_hosts: Vec<String>,
    /// The implicit unspecified bind (`0.0.0.0:0`), which every outbound connect needs
    /// (measured, R1 B1). Only set when at least one IP rule is present.
    pub implicit_unspecified_bind: bool,
}

/// The WASI context a run is allowed, derived purely from its grants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasiPlan {
    pub fs: Vec<FsPreopen>,
    /// Environment variable NAMES to set; the VALUES come from the host's config home, never
    /// `inherit_env`.
    pub env_names: Vec<String>,
    pub net: NetAccess,
    pub clock: ClockAccess,
    pub random: RandomAccess,
    /// stdout/stderr are always captured and bounded (never `inherit_stdio`): stdout is authority
    /// too, and past its bound the capture is refused, not truncated.
    pub stdio_captured: bool,
    /// DNS (`allow_ip_name_lookup`) is always off.
    pub dns: bool,
    /// WASI has no spawn; there is no spawn family and nothing enforces one, so it is always off.
    pub spawn: bool,
}

impl WasiPlan {
    /// The deny-by-default context: nothing but a fixed clock, a seeded RNG and captured stdio.
    fn denied() -> WasiPlan {
        WasiPlan {
            fs: Vec::new(),
            env_names: Vec::new(),
            net: NetAccess::default(),
            clock: ClockAccess::Fixed,
            random: RandomAccess::Seeded,
            stdio_captured: true,
            dns: false,
            spawn: false,
        }
    }

    /// Every WASI mechanism unlocked (the `Root` / native case).
    fn everything() -> WasiPlan {
        WasiPlan {
            fs: Vec::new(),
            env_names: Vec::new(),
            net: NetAccess {
                ip_rules: Vec::new(),
                http_hosts: Vec::new(),
                implicit_unspecified_bind: true,
            },
            clock: ClockAccess::Wall,
            random: RandomAccess::Secure,
            stdio_captured: true,
            // Even Root gets no DNS or spawn here: neither is a WASI mechanism this host turns on.
            dns: false,
            spawn: false,
        }
    }
}

/// A grant the plan refuses to serve, named with the reason. A refusal, never a silent skip: a
/// grant the host cannot honor safely fails the call that holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanError {
    /// The grant as the caller holds it (`urn:cap:fs:read:/../outside`).
    pub scope: String,
    /// Why it is refused.
    pub reason: String,
}

impl core::fmt::Display for PlanError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}: {}", self.scope, self.reason)
    }
}

impl std::error::Error for PlanError {}

/// Check an `fs` grant's path before it names a preopen (R-D3). A preopen is served from
/// `<jail root>/<path>`, so the path must stay inside the jail root by its own spelling: it must
/// be absolute, and no segment may be empty, `.` or `..`. It is REFUSED, never normalized: a
/// grant that needs normalizing is not the grant the host meant to give, and `..` against a
/// symlink resolves differently on disk than on paper. `/` alone is the jail root itself.
///
/// ```
/// use ikigai_runtime::grants::check_fs_path;
///
/// assert!(check_fs_path("/data").is_ok());
/// assert!(check_fs_path("/data/in").is_ok());
/// assert!(check_fs_path("/").is_ok());
/// for bad in ["data", "", "/../outside", "/data/..", "/data/./x", "/data//x", "/data/"] {
///     assert!(check_fs_path(bad).is_err(), "{bad:?} must be refused");
/// }
/// ```
pub fn check_fs_path(path: &str) -> Result<(), String> {
    let Some(rest) = path.strip_prefix('/') else {
        return Err(format!(
            "the path `{path}` is not absolute; an fs grant names a path under the jail root"
        ));
    };
    if rest.is_empty() {
        return Ok(());
    }
    for segment in rest.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(format!(
                "the path `{path}` has a `{segment}` segment; an fs grant path must not climb, \
                 repeat or skip a segment, and it is refused rather than normalized"
            ));
        }
    }
    Ok(())
}

/// Derive the WASI plan from the grants a caller holds. **Pure** — the engine fills the values
/// (env values, the injected clock, the CSPRNG); this function makes only the decision. An `fs`
/// grant whose path could leave the jail root is refused ([`check_fs_path`]), naming the grant.
pub fn wasi_plan(grants: &Grants) -> Result<WasiPlan, PlanError> {
    let scopes = match grants {
        Grants::Root => return Ok(WasiPlan::everything()),
        Grants::Scoped(scopes) => scopes,
    };

    let mut plan = WasiPlan::denied();

    // Collect fs verbs per path so a path with both `write` and `delete` becomes ReadWrite, a
    // path with `read` becomes ReadOnly, and a write-only path gets no preopen (its writes go
    // through the kernel, since WASI has no write-without-delete).
    let mut fs_read: BTreeSet<&str> = BTreeSet::new();
    let mut fs_write: BTreeSet<&str> = BTreeSet::new();
    let mut fs_delete: BTreeSet<&str> = BTreeSet::new();

    for scope in scopes {
        // Deny-shaped exclusions narrow a capability; the plan is a positive derivation and
        // ignores them (ikigai_core::is_deny_scope is the shared test).
        let Some(rest) = scope.strip_prefix("urn:cap:") else {
            continue;
        };
        if ikigai_core::is_deny_scope(scope) {
            continue;
        }
        let (family, rule) = match rest.split_once(':') {
            Some((f, r)) => (f, r),
            None => (rest, ""),
        };
        match family {
            "fs" => {
                let (set, path) = match rule.split_once(':') {
                    Some(("read", path)) => (&mut fs_read, path),
                    Some(("write", path)) => (&mut fs_write, path),
                    Some(("delete", path)) => (&mut fs_delete, path),
                    _ => continue,
                };
                check_fs_path(path).map_err(|reason| PlanError {
                    scope: scope.clone(),
                    reason,
                })?;
                set.insert(path);
            }
            "net" => {
                if is_ip_literal_rule(rule) {
                    plan.net.ip_rules.push(rule.to_string());
                } else {
                    plan.net.http_hosts.push(rule.to_string());
                }
            }
            "env" => {
                if !rule.is_empty() {
                    plan.env_names.push(rule.to_string());
                }
            }
            "clock" if rule == "wall" => plan.clock = ClockAccess::Wall,
            "random" if rule == "secure" => plan.random = RandomAccess::Secure,
            _ => {}
        }
    }

    let mut paths: BTreeSet<&str> = BTreeSet::new();
    paths.extend(fs_read.iter().copied());
    paths.extend(fs_write.iter().copied());
    for path in paths {
        let can_read = fs_read.contains(path);
        let can_write = fs_write.contains(path) && fs_delete.contains(path);
        let access = match (can_read, can_write) {
            (_, true) => Some(FsAccess::ReadWrite),
            (true, false) => Some(FsAccess::ReadOnly),
            // write-only (no delete, no read): no WASI preopen; writes go through the kernel.
            (false, false) => None,
        };
        if let Some(access) = access {
            plan.fs.push(FsPreopen {
                guest_path: path.to_string(),
                access,
            });
        }
    }

    plan.net.implicit_unspecified_bind = !plan.net.ip_rules.is_empty();
    Ok(plan)
}

/// Whether a `net` rule names an IP literal (served at the socket) rather than a host or glob
/// (served through `wasi:http`). R3 recognizes an `a.b.c.d[:port]` IPv4 literal; IPv6 literals
/// are a later refinement.
fn is_ip_literal_rule(rule: &str) -> bool {
    let host = rule.rsplit_once(':').map_or(rule, |(h, _)| h);
    let octets: Vec<&str> = host.split('.').collect();
    octets.len() == 4
        && octets.iter().all(|o| {
            !o.is_empty() && o.bytes().all(|b| b.is_ascii_digit()) && o.parse::<u8>().is_ok()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_of(scopes: &[&str]) -> WasiPlan {
        wasi_plan(&Grants::from_scopes(scopes.iter().copied())).expect("a servable plan")
    }

    #[test]
    fn row_default_denies_everything_but_the_fixed_clock_seeded_rng_and_captured_stdio() {
        let p = plan_of(&[]);
        assert!(p.fs.is_empty());
        assert!(p.env_names.is_empty());
        assert_eq!(p.net, NetAccess::default());
        assert_eq!(p.clock, ClockAccess::Fixed);
        assert_eq!(p.random, RandomAccess::Seeded);
        assert!(p.stdio_captured);
        assert!(!p.dns);
        assert!(!p.spawn);
    }

    #[test]
    fn row_fs_read_is_a_readonly_preopen() {
        let p = plan_of(&["urn:cap:fs:read:/data"]);
        assert_eq!(
            p.fs,
            vec![FsPreopen {
                guest_path: "/data".into(),
                access: FsAccess::ReadOnly
            }]
        );
    }

    #[test]
    fn row_fs_write_plus_delete_is_readwrite_but_write_alone_is_not() {
        let rw = plan_of(&["urn:cap:fs:write:/out", "urn:cap:fs:delete:/out"]);
        assert_eq!(
            rw.fs,
            vec![FsPreopen {
                guest_path: "/out".into(),
                access: FsAccess::ReadWrite
            }]
        );
        // write without delete: no WASI preopen (writes go through the kernel).
        let wonly = plan_of(&["urn:cap:fs:write:/out"]);
        assert!(wonly.fs.is_empty());
    }

    #[test]
    fn row_net_ip_literal_is_a_socket_rule_with_the_implicit_bind() {
        let p = plan_of(&["urn:cap:net:10.0.0.5:443"]);
        assert_eq!(p.net.ip_rules, vec!["10.0.0.5:443".to_string()]);
        assert!(p.net.http_hosts.is_empty());
        assert!(p.net.implicit_unspecified_bind);
    }

    #[test]
    fn row_net_host_or_glob_goes_to_wasi_http_only() {
        let p = plan_of(&["urn:cap:net:api.example.com"]);
        assert_eq!(p.net.http_hosts, vec!["api.example.com".to_string()]);
        assert!(p.net.ip_rules.is_empty());
        // No raw socket means no implicit bind.
        assert!(!p.net.implicit_unspecified_bind);
    }

    #[test]
    fn row_env_sets_the_named_variable() {
        let p = plan_of(&["urn:cap:env:HOME"]);
        assert_eq!(p.env_names, vec!["HOME".to_string()]);
    }

    #[test]
    fn row_clock_wall_injects_the_kernel_clock() {
        assert_eq!(plan_of(&["urn:cap:clock:wall"]).clock, ClockAccess::Wall);
    }

    #[test]
    fn row_random_secure_uses_the_host_csprng() {
        assert_eq!(
            plan_of(&["urn:cap:random:secure"]).random,
            RandomAccess::Secure
        );
    }

    #[test]
    fn row_root_unlocks_everything() {
        let p = wasi_plan(&Grants::Root).unwrap();
        assert_eq!(p.clock, ClockAccess::Wall);
        assert_eq!(p.random, RandomAccess::Secure);
        assert!(p.net.implicit_unspecified_bind);
    }

    #[test]
    fn deny_shaped_scopes_do_not_grant() {
        // `urn:cap:env:-SECRET` is an exclusion, never a grant.
        let p = plan_of(&["urn:cap:env:-SECRET"]);
        assert!(p.env_names.is_empty());
    }

    #[test]
    fn dns_and_spawn_are_never_enabled() {
        let p = plan_of(&[
            "urn:cap:net:10.0.0.5:443",
            "urn:cap:net:api.example.com",
            "urn:cap:clock:wall",
        ]);
        assert!(!p.dns);
        assert!(!p.spawn);
    }

    #[test]
    fn row_fs_a_path_that_could_leave_the_jail_root_is_refused_naming_the_grant() {
        for scope in [
            "urn:cap:fs:read:/../outside",
            "urn:cap:fs:read:/data/../../outside",
            "urn:cap:fs:write:/data/./x",
            "urn:cap:fs:delete:/data//x",
            "urn:cap:fs:read:relative",
            "urn:cap:fs:read:",
        ] {
            let err = wasi_plan(&Grants::from_scopes([scope])).unwrap_err();
            assert_eq!(err.scope, scope);
            assert!(err.to_string().starts_with(scope), "{err}");
        }
        // One bad grant refuses the whole plan; it is never skipped beside a good one.
        assert!(wasi_plan(&Grants::from_scopes([
            "urn:cap:fs:read:/data",
            "urn:cap:fs:read:/../outside",
        ]))
        .is_err());
    }
}
