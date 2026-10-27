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

/// Read-only or read-write preopen (R-D3). A WASI preopen always reads (wasmtime's `ReadWrite`
/// includes read), so both need an `fs:read` grant on the path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsAccess {
    /// `fs:read`.
    ReadOnly,
    /// `fs:read` + `fs:write` + `fs:delete`: WASI has no write-without-delete, and no
    /// write-without-read.
    ReadWrite,
}

/// One preopened directory in the plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsPreopen {
    /// The HOST directory the grant names, exactly as ikigai-fs reads the same string: one grant,
    /// one directory, whether it reaches the file through WASI or through `urn:file:*`. The engine
    /// serves it only when it is the jail root or under it, at the guest path
    /// [`guest_path_in_jail`] gives.
    pub host_path: String,
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

/// Check the spelling of an `fs` grant's path before it names a preopen (R-D3). The path is a
/// HOST path, as in ikigai-fs, so it must be absolute, and no segment may be empty, `.` or `..`.
/// It is REFUSED, never normalized: a grant that needs normalizing is not the grant the host meant
/// to give, and `..` against a symlink resolves differently on disk than on paper. This checks
/// the spelling only; whether the path is inside the jail is [`guest_path_in_jail`]'s question.
///
/// ```
/// use ikigai_runtime::grants::check_fs_path;
///
/// assert!(check_fs_path("/srv/jail/data").is_ok());
/// assert!(check_fs_path("/").is_ok()); // well spelled; the jail placement refuses it
/// for bad in ["data", "", "/../outside", "/data/..", "/data/./x", "/data//x", "/data/"] {
///     assert!(check_fs_path(bad).is_err(), "{bad:?} must be refused");
/// }
/// ```
pub fn check_fs_path(path: &str) -> Result<(), String> {
    let Some(rest) = path.strip_prefix('/') else {
        return Err(format!(
            "the path `{path}` is not absolute; an fs grant names a host path"
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

/// Check a jail root, as the host spells it with plain segments (`/srv/jail`): it must pass
/// [`check_fs_path`], and it must not be `/`. A jail root of `/` would preopen the whole disk
/// behind any grant, which is not a jail; a host refuses it when it is configured.
///
/// ```
/// use ikigai_runtime::grants::check_jail_root;
///
/// assert!(check_jail_root("/srv/jail").is_ok());
/// assert!(check_jail_root("/").is_err());
/// assert!(check_jail_root("srv/jail").is_err());
/// ```
pub fn check_jail_root(root: &str) -> Result<(), String> {
    check_fs_path(root).map_err(|reason| format!("the jail root: {reason}"))?;
    if root == "/" {
        return Err(
            "a jail root of `/` would serve the whole disk, which is not a jail; it is refused"
                .to_string(),
        );
    }
    Ok(())
}

/// Place an `fs` grant's HOST path in the jail, answering the GUEST path its preopen appears at.
///
/// A grant names a host path (one grant, one directory, as in ikigai-fs). It is served only when
/// it is the jail root or under it, compared segment by segment (so `/srv/jailx` is not under
/// `/srv/jail`); anything else is REFUSED, naming the path and the jail root, never skipped. `/`
/// is no special case: it is outside every jail root, because [`check_jail_root`] refuses `/`.
/// An ancestor of the jail root is refused too, where ikigai-fs would read it as covering the
/// whole jail: WASI would then be asked to preopen more than the grant's directory, so the host
/// refuses rather than guess.
///
/// The guest sees the part of the host path under the jail root (`/ws` for `<jail>/ws`, `/` for
/// the jail root itself), so a component's paths do not depend on where a host keeps its jail,
/// and two hosts with different jail roots run one component unchanged.
///
/// ```
/// use ikigai_runtime::grants::guest_path_in_jail;
///
/// assert_eq!(guest_path_in_jail("/srv/jail/ws", "/srv/jail").unwrap(), "/ws");
/// assert_eq!(guest_path_in_jail("/srv/jail/ws/in", "/srv/jail").unwrap(), "/ws/in");
/// assert_eq!(guest_path_in_jail("/srv/jail", "/srv/jail").unwrap(), "/");
/// for outside in ["/ws", "/", "/srv", "/srv/jailx", "/srv/jail/../x"] {
///     assert!(guest_path_in_jail(outside, "/srv/jail").is_err(), "{outside:?}");
/// }
/// // A jail root of `/` is refused, so `/` is never served.
/// assert!(guest_path_in_jail("/", "/").is_err());
/// ```
pub fn guest_path_in_jail(host_path: &str, jail_root: &str) -> Result<String, String> {
    check_jail_root(jail_root)?;
    check_fs_path(host_path)?;
    let root: Vec<&str> = segments(jail_root).collect();
    let path: Vec<&str> = segments(host_path).collect();
    match path.strip_prefix(root.as_slice()) {
        Some(rest) => Ok(format!("/{}", rest.join("/"))),
        None => Err(format!(
            "the host path `{host_path}` is outside the jail root `{jail_root}`; an fs grant \
             names a host path, and only the jail root or a path under it is served"
        )),
    }
}

/// The segments of a path [`check_fs_path`] accepted (none for `/`).
fn segments(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|s| !s.is_empty())
}

/// Whether `path` is `base` or under it, segment by segment.
fn at_or_under(path: &str, base: &str) -> bool {
    let mut path = segments(path);
    segments(base).all(|b| path.next() == Some(b))
}

/// Derive the WASI plan from the grants a caller holds. **Pure** — the engine fills the values
/// (env values, the injected clock, the CSPRNG); this function makes only the decision.
///
/// Two `fs` refusals, each naming the grant, never a silent skip:
/// - a path that is not a plain absolute host path ([`check_fs_path`]);
/// - an exclusion (`urn:cap:fs:read:-/ws/secret`) at or under a path the plan preopens, when the
///   preopen's access covers the exclusion's verb. A WASI preopen cannot express "all of `/ws`
///   except `secret`", and serving the preopen would widen the capability, so the plan is refused
///   naming both scopes. An exclusion under no preopen changes nothing here (it still binds
///   `urn:file:*`); one whose path cannot be placed (relative, or with an empty, `.` or `..`
///   segment) is refused while the plan preopens anything it could cover.
///
/// Whether a preopen's host path is inside the jail is the engine's check, against its jail
/// root ([`guest_path_in_jail`]).
pub fn wasi_plan(grants: &Grants) -> Result<WasiPlan, PlanError> {
    let scopes = match grants {
        Grants::Root => return Ok(WasiPlan::everything()),
        Grants::Scoped(scopes) => scopes,
    };

    let mut plan = WasiPlan::denied();

    // Collect fs verbs per path. A preopen always reads, so only a path with `read` gets one:
    // ReadOnly with `read` alone, ReadWrite with `read` + `write` + `delete`. A path without
    // `read` gets no preopen whatever else it holds, and its writes go through the kernel.
    let mut fs_read: BTreeSet<&str> = BTreeSet::new();
    let mut fs_write: BTreeSet<&str> = BTreeSet::new();
    let mut fs_delete: BTreeSet<&str> = BTreeSet::new();
    // fs exclusions as (the scope, its verb, its path after the `-`), checked against the
    // preopens once those are known.
    let mut fs_denies: Vec<(&String, &str, &str)> = Vec::new();

    for scope in scopes {
        let Some(rest) = scope.strip_prefix("urn:cap:") else {
            continue;
        };
        // Deny-shaped exclusions never grant (ikigai_core::is_deny_scope is the shared test).
        // An fs exclusion is kept, because a preopen it falls under cannot honor it.
        if ikigai_core::is_deny_scope(scope) {
            if let Some((verb, path)) = rest
                .strip_prefix("fs:")
                .and_then(|rule| rule.split_once(':'))
                .and_then(|(verb, path)| Some((verb, path.strip_prefix('-')?)))
            {
                if matches!(verb, "read" | "write" | "delete") {
                    fs_denies.push((scope, verb, path));
                }
            }
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

    for &path in &fs_read {
        let access = if fs_write.contains(path) && fs_delete.contains(path) {
            FsAccess::ReadWrite
        } else {
            FsAccess::ReadOnly
        };
        plan.fs.push(FsPreopen {
            host_path: path.to_string(),
            access,
        });
    }

    // An exclusion a preopen would serve anyway: refuse, never widen. A read-only preopen
    // serves reads; a read-write one serves every fs verb.
    for (deny, verb, path) in fs_denies {
        let covers = |p: &FsPreopen| verb == "read" || p.access == FsAccess::ReadWrite;
        let mut covering = plan.fs.iter().filter(|p| covers(p)).peekable();
        if covering.peek().is_none() {
            continue;
        }
        if let Err(reason) = check_fs_path(path) {
            return Err(PlanError {
                scope: deny.clone(),
                reason: format!(
                    "the exclusion cannot be placed against this run's preopens ({reason}); \
                     it is refused rather than dropped"
                ),
            });
        }
        if let Some(preopen) = covering.find(|p| at_or_under(path, &p.host_path)) {
            let held: Vec<String> = [
                ("read", &fs_read),
                ("write", &fs_write),
                ("delete", &fs_delete),
            ]
            .into_iter()
            .filter(|(_, set)| set.contains(preopen.host_path.as_str()))
            .map(|(v, _)| format!("urn:cap:fs:{v}:{}", preopen.host_path))
            .collect();
            return Err(PlanError {
                scope: deny.clone(),
                reason: format!(
                    "it falls at or under the preopen of `{}` (from {}), and a WASI preopen \
                     cannot exclude a path inside it; the plan is refused rather than widened",
                    preopen.host_path,
                    held.join(" + ")
                ),
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
                host_path: "/data".into(),
                access: FsAccess::ReadOnly
            }]
        );
    }

    #[test]
    fn row_fs_read_write_delete_is_readwrite_and_read_with_part_of_it_is_readonly() {
        let rw = plan_of(&[
            "urn:cap:fs:read:/out",
            "urn:cap:fs:write:/out",
            "urn:cap:fs:delete:/out",
        ]);
        assert_eq!(
            rw.fs,
            vec![FsPreopen {
                host_path: "/out".into(),
                access: FsAccess::ReadWrite
            }]
        );
        // read + write without delete: WASI has no write-without-delete, so read-only.
        let ro = plan_of(&["urn:cap:fs:read:/out", "urn:cap:fs:write:/out"]);
        assert_eq!(ro.fs[0].access, FsAccess::ReadOnly);
    }

    /// A WASI preopen always reads (wasmtime's `ReadWrite` includes read), so a path with no
    /// `read` grant gets no preopen at all, and its writes go through the kernel. At 1a0ee9a3
    /// `write` + `delete` alone was a ReadWrite preopen, and the guest read through it.
    #[test]
    fn row_fs_without_read_gets_no_preopen_whatever_else_it_holds() {
        for scopes in [
            &["urn:cap:fs:write:/out"][..],
            &["urn:cap:fs:delete:/out"],
            &["urn:cap:fs:write:/out", "urn:cap:fs:delete:/out"],
        ] {
            assert!(plan_of(scopes).fs.is_empty(), "{scopes:?}");
        }
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

    /// Claim 2 of the review at fe22b361: the plan used to drop every exclusion, so
    /// `read:/ws` + `read:-/ws/secret` preopened all of `/ws`, `secret` included.
    #[test]
    fn an_fs_exclusion_at_or_under_a_preopen_refuses_the_plan_naming_both_scopes() {
        for deny in ["urn:cap:fs:read:-/ws/secret", "urn:cap:fs:read:-/ws"] {
            let err = wasi_plan(&Grants::from_scopes(["urn:cap:fs:read:/ws", deny])).unwrap_err();
            assert_eq!(err.scope, deny);
            assert!(err.reason.contains("urn:cap:fs:read:/ws"), "{err}");
        }
        // A read-write preopen serves every verb, so any verb's exclusion under it refuses.
        let rw = [
            "urn:cap:fs:read:/ws",
            "urn:cap:fs:write:/ws",
            "urn:cap:fs:delete:/ws",
        ];
        for deny in [
            "urn:cap:fs:read:-/ws/secret",
            "urn:cap:fs:write:-/ws/secret",
            "urn:cap:fs:delete:-/ws/secret",
        ] {
            let scopes = rw.iter().copied().chain([deny]);
            let err = wasi_plan(&Grants::from_scopes(scopes)).unwrap_err();
            assert_eq!(err.scope, deny);
            assert!(
                err.reason
                    .contains("urn:cap:fs:read:/ws + urn:cap:fs:write:/ws + urn:cap:fs:delete:/ws"),
                "{err}"
            );
        }
    }

    #[test]
    fn an_fs_exclusion_no_preopen_could_serve_changes_nothing() {
        let expected = plan_of(&["urn:cap:fs:read:/ws"]);
        for deny in [
            "urn:cap:fs:read:-/elsewhere/secret",
            // An ancestor: the deeper allow wins in ikigai-fs, so the preopen is what it grants.
            "urn:cap:fs:read:-/",
            // `/wsx` is not under `/ws`, segment by segment.
            "urn:cap:fs:read:-/wsx",
            // A read-only preopen cannot write or delete, so those exclusions are already held.
            "urn:cap:fs:write:-/ws/secret",
            "urn:cap:fs:delete:-/ws/secret",
        ] {
            assert_eq!(plan_of(&["urn:cap:fs:read:/ws", deny]), expected, "{deny}");
        }
        // With no preopen at all, even an unplaceable exclusion is nothing to the plan.
        assert!(plan_of(&["urn:cap:fs:read:-secret"]).fs.is_empty());
    }

    #[test]
    fn an_fs_exclusion_that_cannot_be_placed_beside_a_preopen_is_refused() {
        for deny in [
            "urn:cap:fs:read:-secret",
            "urn:cap:fs:read:-/ws/../ws/secret",
        ] {
            let err = wasi_plan(&Grants::from_scopes(["urn:cap:fs:read:/ws", deny])).unwrap_err();
            assert_eq!(err.scope, deny);
        }
    }

    /// Claim 1 of the review at fe22b361: a grant used to name `<jail root>/<path>` here and the
    /// host path `<path>` in ikigai-fs. Now it names the host path in both, and only a path at
    /// or under the jail root is served.
    #[test]
    fn a_grant_names_a_host_path_and_is_served_only_inside_the_jail() {
        assert_eq!(guest_path_in_jail("/tmp/j/x", "/tmp/j").unwrap(), "/x");
        assert_eq!(guest_path_in_jail("/tmp/j", "/tmp/j").unwrap(), "/");
        for outside in ["/x", "/", "/tmp", "/tmp/jx", "/tmp/k/x"] {
            let err = guest_path_in_jail(outside, "/tmp/j").unwrap_err();
            assert!(err.contains(outside) && err.contains("/tmp/j"), "{err}");
        }
        assert!(guest_path_in_jail("/", "/").is_err());
        assert!(guest_path_in_jail("/x", "/").is_err());
    }

    #[test]
    fn row_fs_a_path_that_is_not_plain_and_absolute_is_refused_naming_the_grant() {
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
