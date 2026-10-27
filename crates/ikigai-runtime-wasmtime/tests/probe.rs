//! R1's probe table as tests (R-D18, R-D2, R-D3): a Rust std guest (`comp-probe`) tries every
//! ambient authority, one export per authority. The `endpoint` world grants NO WASI authority
//! whatever the caller holds; the `app` world grants exactly the row of each grant held, and
//! every default denial holds with none. Behind the `jail-fixtures` feature.
//!
//! | authority | endpoint world, and app with no grant | app world, the grant alone |
//! |---|---|---|
//! | args, env | `[]`, absent | `urn:cap:env:HOME`: exactly that value, from the HOST |
//! | read `/`, read or write `/data` | no preopen | `fs:read:/data` reads, cannot write; `fs:write`+`fs:delete` writes |
//! | TCP connect | refused at the import | `net:<ip>:<port>`: that address and no other |
//! | DNS | refused at the import | still refused (never granted) |
//! | spawn | `Unsupported` (no WASI interface) | `Unsupported` |
//! | wall clock | fixed at the epoch | `clock:wall`: the KERNEL's clock |
//! | randomness | seeded: the same bytes every call | `random:secure`: the host CSPRNG |
//! | stdout | captured, never the host's | captured |
//! | `wasi:http` | refused at the import | `net:<host>`: through the kernel's `urn:http:get` |
#![cfg(feature = "jail-fixtures")]

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;

use std::sync::Mutex;

use ikigai_core::{
    ArgRef, Description, EndpointSpace, Exact, Fallback, FixedClock, FnEndpoint, Invocation, Iri,
    Kernel, MetaRenderer, ReprType, Representation, Request, Space, TraceEvent, Tracer, Verb,
};
use ikigai_runtime::{Artifact, Capability, Error, Limits};
use ikigai_runtime_wasmtime::component::{AppConfig, ComponentEngine, World};
use ikigai_runtime_wasmtime::wasi::AppHost;

const PROBE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/comp_probe.wasm"));

struct PlainRenderer;

impl MetaRenderer for PlainRenderer {
    fn render(&self, d: &Description, _t: &ReprType) -> ikigai_core::Result<Representation> {
        Ok(Representation::new(
            ReprType::new("text/plain"),
            d.id.clone().into_bytes(),
        ))
    }
}

/// A stand-in for ikigai-http's `urn:http:get`, applying the same ACL shape: the caller must
/// hold `urn:cap:net:<the URL's host>`.
fn http_space() -> EndpointSpace {
    EndpointSpace::new().bind(
        Exact::new("urn:http:get"),
        FnEndpoint::new("http-get", |inv: &Invocation<'_>| {
            let url = inv.inline_str("url")?.to_string();
            let host = url
                .split("://")
                .nth(1)
                .and_then(|rest| rest.split('/').next())
                .unwrap_or_default();
            if !inv.capability.allows(&format!("urn:cap:net:{host}")) {
                return Err(Error::Denied(format!("urn:cap:net:{host}")));
            }
            Ok(Representation::new(
                ReprType::new("text/plain"),
                format!("hello from {url}").into_bytes(),
            ))
        }),
    )
}

/// A jail root holding `data/x.txt`, unique to the test.
fn jail(test: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "ikigai-runtime-probe-{}-{test}",
        std::process::id()
    ));
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("x.txt"), "inside the jail").unwrap();
    root
}

struct Probe {
    kernel: Kernel,
}

impl Probe {
    async fn mount(world: World, host: AppHost) -> Probe {
        let engine = ComponentEngine::new(Limits::fuel(u64::MAX)).unwrap();
        let space = engine
            .mount(
                &Artifact::from_bytes(PROBE.to_vec()),
                AppConfig::new("probe", world)
                    .ceiling(Capability::root())
                    .host(host)
                    .limits(Limits::fuel(500_000_000)),
            )
            .await
            .expect("mount the probe");
        let root: Arc<dyn Space> = Arc::new(Fallback::new(vec![
            Arc::new(http_space()) as Arc<dyn Space>,
            Arc::new(space) as Arc<dyn Space>,
        ]));
        Probe {
            kernel: Kernel::with_meta_renderer(root, Arc::new(PlainRenderer)),
        }
    }

    async fn try_(
        &self,
        func: &str,
        args: &[(&str, &str)],
        grants: &[&str],
    ) -> Result<String, Error> {
        let mut request = Request::new(
            Verb::Source,
            Iri::parse(format!("urn:runtime:app:probe:probe:{func}")).unwrap(),
        );
        for (name, value) in args {
            request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
        }
        let capability = Capability::root().attenuate(grants.iter().map(|g| g.to_string()));
        self.kernel
            .issue(request, &capability)
            .await
            .map(|repr| String::from_utf8(repr.bytes).unwrap())
    }

    async fn run(&self, func: &str, args: &[(&str, &str)], grants: &[&str]) -> String {
        self.try_(func, args, grants)
            .await
            .unwrap_or_else(|e| panic!("{func}: {e:?}"))
    }
}

fn denied_at_the_import(result: Result<String, Error>, what: &str) {
    match result {
        Err(Error::Denied(message)) => {
            assert!(message.contains(what), "names {what}: {message}")
        }
        other => panic!("expected a denial at the {what} import, got {other:?}"),
    }
}

/// Every default denial, for a caller holding the given grants: the `endpoint` world ignores
/// them all, and the `app` world with none must look the same.
async fn assert_every_default_denial(probe: &Probe, grants: &[&str]) {
    assert_eq!(probe.run("args", &[], grants).await, "ok: []");
    assert!(probe
        .run("env", &[("name", "HOME")], grants)
        .await
        .starts_with("err:"));
    assert!(probe
        .run("read-root", &[], grants)
        .await
        .starts_with("err:"));
    assert!(probe
        .run("read-data", &[], grants)
        .await
        .starts_with("err:"));
    assert!(probe
        .run("write-data", &[("text", "x")], grants)
        .await
        .starts_with("err:"));
    denied_at_the_import(
        probe.try_("tcp", &[("addr", "127.0.0.1:9")], grants).await,
        "wasi:sockets",
    );
    denied_at_the_import(
        probe.try_("dns", &[("host", "example.com")], grants).await,
        "wasi:sockets",
    );
    assert_eq!(probe.run("spawn", &[], grants).await, "err: Unsupported");
    assert_eq!(probe.run("clock", &[], grants).await, "ok: 0");
    let a = probe.run("random", &[], grants).await;
    let b = probe.run("random", &[], grants).await;
    assert_eq!(
        a, b,
        "seeded randomness: the same call answers the same bytes"
    );
    assert_eq!(
        probe.run("print", &[("text", "hi")], grants).await,
        "ok: printed"
    );
    denied_at_the_import(
        probe
            .try_("http-get", &[("url", "https://example.com/x")], grants)
            .await,
        "wasi:http",
    );
}

#[tokio::test]
async fn the_endpoint_world_grants_no_wasi_authority_even_to_root() {
    let probe = Probe::mount(World::Endpoint, AppHost::default()).await;
    // A caller holding every grant in the table still gets none of it in the endpoint world.
    let everything = [
        "urn:cap:env:HOME",
        "urn:cap:fs:read:/data",
        "urn:cap:fs:write:/data",
        "urn:cap:fs:delete:/data",
        "urn:cap:net:127.0.0.1:9",
        "urn:cap:net:example.com",
        "urn:cap:clock:wall",
        "urn:cap:random:secure",
    ];
    assert_every_default_denial(&probe, &everything).await;
}

#[tokio::test]
async fn the_app_world_with_no_grant_denies_every_row() {
    let probe = Probe::mount(World::App, AppHost::default()).await;
    assert_every_default_denial(&probe, &[]).await;
}

#[tokio::test]
async fn env_grants_exactly_the_named_value_from_the_host() {
    let host = AppHost {
        env: BTreeMap::from([
            ("HOME".to_string(), "/jail".to_string()),
            ("SECRET".to_string(), "never".to_string()),
        ]),
        ..AppHost::default()
    };
    let probe = Probe::mount(World::App, host).await;
    let grants = ["urn:cap:env:HOME"];
    assert_eq!(
        probe.run("env", &[("name", "HOME")], &grants).await,
        "ok: /jail"
    );
    // A value the host holds but no grant names never reaches the guest.
    assert!(probe
        .run("env", &[("name", "SECRET")], &grants)
        .await
        .starts_with("err:"));
}

#[tokio::test]
async fn fs_read_preopens_read_only_and_write_needs_write_and_delete() {
    let root = jail("fs");
    let probe = Probe::mount(
        World::App,
        AppHost {
            fs_root: Some(root.clone()),
            ..AppHost::default()
        },
    )
    .await;
    let read = ["urn:cap:fs:read:/data"];
    assert_eq!(
        probe.run("read-data", &[], &read).await,
        "ok: \"inside the jail\""
    );
    assert!(probe.run("read-root", &[], &read).await.starts_with("err:"));
    assert!(
        probe
            .run("write-data", &[("text", "x")], &read)
            .await
            .starts_with("err:"),
        "a read grant cannot write"
    );
    let write = ["urn:cap:fs:write:/data", "urn:cap:fs:delete:/data"];
    assert_eq!(
        probe
            .run("write-data", &[("text", "written")], &write)
            .await,
        "ok: ()"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("data").join("out.txt")).unwrap(),
        "written"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn an_ip_grant_opens_exactly_that_address() {
    // A listener the test keeps open for its whole life (bound here, never released early).
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let probe = Probe::mount(World::App, AppHost::default()).await;
    let grant = format!("urn:cap:net:{addr}");
    assert_eq!(
        probe.run("tcp", &[("addr", &addr)], &[&grant]).await,
        "ok: ()"
    );
    // Another address under the same grant is refused at the socket check.
    let other = format!("127.0.0.1:{}", listener.local_addr().unwrap().port() ^ 1);
    assert!(probe
        .run("tcp", &[("addr", &other)], &[&grant])
        .await
        .starts_with("err:"));
    // DNS is never granted, even beside an IP grant.
    assert!(probe
        .run("dns", &[("host", "example.com")], &[&grant])
        .await
        .starts_with("err:"));
    drop(listener);
}

#[tokio::test]
async fn clock_wall_injects_the_kernels_clock() {
    let probe = Probe::mount(
        World::App,
        AppHost {
            clock: Arc::new(FixedClock::at(1_700_000_000_000)),
            ..AppHost::default()
        },
    )
    .await;
    assert_eq!(
        probe.run("clock", &[], &["urn:cap:clock:wall"]).await,
        "ok: 1700000000"
    );
}

#[tokio::test]
async fn random_secure_is_the_host_csprng() {
    let probe = Probe::mount(World::App, AppHost::default()).await;
    let grants = ["urn:cap:random:secure"];
    let a = probe.run("random", &[], &grants).await;
    let b = probe.run("random", &[], &grants).await;
    assert_ne!(a, b, "two secure draws differ");
}

#[tokio::test]
async fn a_host_grant_reaches_the_web_only_through_the_kernel() {
    let probe = Probe::mount(World::App, AppHost::default()).await;
    let grants = ["urn:cap:net:example.com"];
    assert_eq!(
        probe
            .run("http-get", &[("url", "https://example.com/x")], &grants)
            .await,
        "ok: 200 hello from https://example.com/x"
    );
    // A host the grant does not name: wasi:http is linked (some host is granted), and the
    // kernel's ACL refuses it as `HTTP-request-denied`.
    assert!(probe
        .run("http-get", &[("url", "https://elsewhere.test/x")], &grants)
        .await
        .starts_with("err:"));
}

#[tokio::test]
async fn spawn_is_unsupported_even_for_root() {
    let probe = Probe::mount(World::App, AppHost::default()).await;
    let root = Capability::root();
    let request = Request::new(
        Verb::Source,
        Iri::parse("urn:runtime:app:probe:probe:spawn").unwrap(),
    );
    let answer = probe.kernel.issue(request, &root).await.unwrap();
    assert_eq!(answer.bytes, b"err: Unsupported");
}

/// A tracer that keeps every event.
#[derive(Default)]
struct Kept(Mutex<Vec<TraceEvent>>);

impl Tracer for Kept {
    fn record(&self, event: TraceEvent) {
        self.0.lock().unwrap().push(event);
    }
}

#[tokio::test]
async fn stdout_is_captured_to_the_trace_never_the_hosts_and_fuel_is_noted() {
    let probe = Probe::mount(World::Endpoint, AppHost::default()).await;
    let kept = Arc::new(Kept::default());
    probe.kernel.set_tracer(kept.clone());
    assert_eq!(
        probe.run("print", &[("text", "to the trace")], &[]).await,
        "ok: printed"
    );
    let events = kept.0.lock().unwrap();
    let event = events
        .iter()
        .find(|e| e.target == "urn:runtime:app:probe:probe:print")
        .expect("the call was traced");
    let note = |key: &str| {
        event
            .notes
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(note("runtime.stdout").as_deref(), Some("to the trace\n"));
    let fuel: u64 = note("runtime.fuel").expect("fuel noted").parse().unwrap();
    assert!(fuel > 0);
}

/// A grant whose path climbs out of the jail root must never preopen outside it. The ceiling is
/// ROOT, so core's `attenuate` passes the caller's scope through as written: the plan is the
/// only place that can refuse it.
#[tokio::test]
async fn a_grant_path_that_climbs_out_of_the_jail_root_is_refused() {
    let base = std::env::temp_dir().join(format!(
        "ikigai-runtime-probe-{}-escape",
        std::process::id()
    ));
    let root = base.join("jail");
    let outside = base.join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "outside the jail").unwrap();
    let probe = Probe::mount(
        World::App,
        AppHost {
            fs_root: Some(root.clone()),
            ..AppHost::default()
        },
    )
    .await;
    let answer = probe
        .try_(
            "read-path",
            &[("path", "/../outside/secret.txt")],
            &["urn:cap:fs:read:/../outside"],
        )
        .await;
    let _ = std::fs::remove_dir_all(&base);
    match answer {
        Err(Error::Denied(message)) => {
            assert!(
                message.contains("/../outside"),
                "names the grant: {message}"
            )
        }
        other => panic!("the jail root must hold; got {other:?}"),
    }
}
