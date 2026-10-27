//! The component engine: compile and cache components, mount one as an app, and run one call in
//! a fresh `Store` with the WASI its world and grants allow.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ikigai_core::{Capability, Description, Error, Request, Verb};
use ikigai_runtime::{
    wasi_plan, Artifact, ArtifactDigest, CompiledKey, Consumed, EngineCompatHash, EngineError,
    Grants, HostResolver, Limits, WasiPlan,
};
use wasmtime::component::types::ComponentItem;
use wasmtime::component::{
    Component, ComponentExportIndex, InstancePre, Linker, ResourceTable, Val,
};
use wasmtime::{Engine, ResourceLimiter, Store};
use wasmtime_wasi::p2::pipe::MemoryOutputPipe;
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::{WasiHttpCtx, WasiHttpCtxView, WasiHttpView};

use super::bindings::{endpoint_api, kernel, KernelCtx, KernelData};
use super::http::KernelHttp;
use super::imports::{unresolved, unversioned, ImportBindings};
use super::project::{docs_of, project, ProjectedFunc, Skipped, ENDPOINT_API};
use super::space::ComponentSpace;
use super::values;
use crate::budget::{
    apply_budget, check_metering, consumed_of, engine_for, trap_to_error, EpochTicker, HostRefusal,
    MemLimiter, Metering,
};
use crate::wasi::{apply_plan, deterministic_builder, seed_of, AppHost, STDIO_CAPACITY};

/// Which `ikigai:runtime` world a component runs in (R-D2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum World {
    /// No WASI authority: whatever WASI the component imports is linked against the empty,
    /// deterministic context (no files, env, args, sockets or DNS; stdio captured; a fixed clock;
    /// a seeded RNG), whatever the caller holds. Its exports are cacheable: its WASI is
    /// deterministic and every kernel call it makes is a tracked dependency (R-D4).
    #[default]
    Endpoint,
    /// The WASI a grant unlocks, built per call from the effective grants through the
    /// grant → WASI plan (R-D3). Uncacheable unless the mount declares it pure, and never
    /// cacheable while it holds `clock:wall`, `random:secure` or a network grant.
    App,
}

/// How one component is mounted: everything the host decides and the guest cannot.
#[derive(Clone)]
pub struct AppConfig {
    /// The mount name, `{app}` in `urn:runtime:app:{app}:…`.
    pub app: String,
    /// The world the component runs in.
    pub world: World,
    /// The app's grant ceiling (from the config home, never from the guest): every call runs under
    /// `ceiling ∩ caller`, for its kernel calls and, in the `app` world, its WASI.
    pub ceiling: Capability,
    /// What only the host knows about the `app` world's WASI: the jail root, env values, clock.
    pub host: AppHost,
    /// The configured import bindings (R-D5).
    pub bindings: ImportBindings,
    /// The app's own budget, met with the engine ceiling and the caller's grants (R-D10).
    pub limits: Limits,
    /// Projected URNs declared Sinks (WIT has no effect annotation; purity is never guessed).
    pub sinks: BTreeSet<String>,
    /// The bundle's declaration that an `app`-world export is a pure function of its inputs,
    /// so cacheable when it holds no clock, randomness or network grant. Never inferred.
    pub pure: bool,
}

impl AppConfig {
    /// A mount of `app` in `world`, with no authority, no bindings and no budget of its own.
    pub fn new(app: impl Into<String>, world: World) -> Self {
        AppConfig {
            app: app.into(),
            world,
            ceiling: Capability::root().attenuate(std::iter::empty::<String>()),
            host: AppHost::default(),
            bindings: ImportBindings::new(),
            limits: Limits::unbounded(),
            sinks: BTreeSet::new(),
            pure: false,
        }
    }

    /// Builder: the grant ceiling.
    pub fn ceiling(mut self, ceiling: Capability) -> Self {
        self.ceiling = ceiling;
        self
    }

    /// Builder: the host's half of the `app` world's WASI.
    pub fn host(mut self, host: AppHost) -> Self {
        self.host = host;
        self
    }

    /// Builder: the import bindings.
    pub fn bindings(mut self, bindings: ImportBindings) -> Self {
        self.bindings = bindings;
        self
    }

    /// Builder: the app's budget.
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Builder: declare a projected URN a Sink.
    pub fn sink(mut self, urn: impl Into<String>) -> Self {
        self.sinks.insert(urn.into());
        self
    }

    /// Builder: declare the app pure (see [`AppConfig::pure`]).
    pub fn pure(mut self) -> Self {
        self.pure = true;
        self
    }
}

/// The wasmtime component engine (R-D1): compiles, caches and runs components. Cheap to clone;
/// every mount shares its compiled-form cache.
#[derive(Clone)]
pub struct ComponentEngine {
    inner: Arc<EngineInner>,
}

pub(crate) struct EngineInner {
    pub(crate) engine: Engine,
    ceiling: Limits,
    metering: Metering,
    _ticker: EpochTicker,
    /// The compiled-form cache, keyed by artifact digest and the engine-compatibility hash
    /// (R-D7). In memory; a digest key never goes stale, so it only needs eviction.
    compiled: Mutex<HashMap<CompiledKey, Component>>,
}

impl ComponentEngine {
    /// Build the engine with a host ceiling, metering by fuel and epoch. An unbounded ceiling is
    /// refused: there is no silent unlimited default (R-D10).
    pub fn new(ceiling: Limits) -> Result<Self, EngineError> {
        Self::with_metering(ceiling, Metering::FuelAndEpoch)
    }

    /// Build the engine with an explicit [`Metering`].
    pub fn with_metering(ceiling: Limits, metering: Metering) -> Result<Self, EngineError> {
        let engine = engine_for(&ceiling, metering)?;
        let ticker = EpochTicker::start(&engine);
        Ok(ComponentEngine {
            inner: Arc::new(EngineInner {
                engine,
                ceiling,
                metering,
                _ticker: ticker,
                compiled: Mutex::new(HashMap::new()),
            }),
        })
    }

    /// The engine-compatibility hash compiled forms are keyed by (R-D7).
    pub fn compat_hash(&self) -> EngineCompatHash {
        crate::compat_hash(&self.inner.engine)
    }

    /// Compile an artifact, or take it from the cache by digest and compatibility hash. A
    /// precompiled form is never deserialized from anywhere but this cache (R-D7).
    pub fn compile(&self, artifact: &Artifact) -> Result<Component, EngineError> {
        let key = artifact.compiled_key(&self.compat_hash());
        if let Some(component) = self
            .inner
            .compiled
            .lock()
            .expect("the compiled cache")
            .get(&key)
        {
            return Ok(component.clone());
        }
        // Compiled outside the lock, so one large compile does not stall every other mount; two
        // racing compiles of one digest produce the same form, and the second insert is a no-op
        // in effect.
        let component = Component::new(&self.inner.engine, artifact.bytes())
            .map_err(|e| EngineError::Compile(format!("{e:#}")))?;
        self.inner
            .compiled
            .lock()
            .expect("the compiled cache")
            .entry(key)
            .or_insert_with(|| component.clone());
        Ok(component)
    }

    /// How many compiled forms the cache holds.
    pub fn cached(&self) -> usize {
        self.inner
            .compiled
            .lock()
            .expect("the compiled cache")
            .len()
    }

    /// Mount `artifact` as `config.app`: check its imports (every one bound, R-D5), project its
    /// exports (R-D4), and, if it exports `ikigai:runtime/endpoint-api`, ask its `describe()`
    /// for the endpoints it serves. The result is a core `Space` answering only under
    /// `urn:runtime:app:{app}:`.
    pub async fn mount(
        &self,
        artifact: &Artifact,
        config: AppConfig,
    ) -> Result<ComponentSpace, Error> {
        let name = config.app.as_str();
        if name.is_empty() || name.contains(':') || ikigai_core::encode_name(name) != name {
            return Err(Error::InvalidArgument {
                name: "app".into(),
                detail: format!(
                    "`{name}` is not one name segment; a mount answers under urn:runtime:app:{{app}}:"
                ),
            });
        }
        let component = self.compile(artifact).map_err(|e| Error::InvalidArgument {
            name: "artifact".into(),
            detail: e.to_string(),
        })?;
        let engine = &self.inner.engine;
        let imports = classify_imports(engine, &component, &config.bindings)?;
        let docs = docs_of(artifact.bytes());
        let (funcs, skipped) = project(engine, &component, &config.app, &config.sinks, &docs)?;
        let api = component
            .component_type()
            .exports(engine)
            .find(|(name, _)| unversioned(name) == ENDPOINT_API)
            .and_then(|(name, _)| {
                let parent = component.get_export_index(None, name)?;
                Some(ApiIndices {
                    describe: component.get_export_index(Some(&parent), "describe")?,
                    invoke: component.get_export_index(Some(&parent), "invoke")?,
                })
            });
        let app = Arc::new(MountedApp {
            engine: Arc::clone(&self.inner),
            component,
            digest: artifact.digest().clone(),
            config,
            imports,
            api,
            pres: Mutex::new(HashMap::new()),
        });
        let served = match &app.api {
            Some(_) => app.describe_api().await?,
            None => Vec::new(),
        };
        ComponentSpace::new(app, funcs, served, skipped)
    }
}

/// The export indices of `ikigai:runtime/endpoint-api`.
#[derive(Clone)]
struct ApiIndices {
    describe: ComponentExportIndex,
    invoke: ComponentExportIndex,
}

/// The imports of a component, sorted by who serves them.
pub(crate) struct Imports {
    /// `wasi:sockets/*` instances and their functions: linked only when a grant opens a socket.
    sockets: Vec<(String, Vec<String>)>,
    /// `wasi:http/*` instances and their functions: linked only when a grant names a host.
    http: Vec<(String, Vec<String>)>,
    /// Configured bindings: instance name and its functions.
    bound: Vec<(String, Vec<String>)>,
}

/// The WASI p2 interfaces the runtime links (the `wasi:cli/imports` world, wasmtime-wasi 50);
/// any other `wasi:` import is unbound and refuses the mount.
const LINKED_WASI: &[&str] = &[
    "wasi:io/",
    "wasi:clocks/",
    "wasi:random/",
    "wasi:cli/",
    "wasi:filesystem/",
    "wasi:sockets/",
    "wasi:http/",
];

fn classify_imports(
    engine: &Engine,
    component: &Component,
    bindings: &ImportBindings,
) -> Result<Imports, Error> {
    let mut imports = Imports {
        sockets: Vec::new(),
        http: Vec::new(),
        bound: Vec::new(),
    };
    for (name, item) in component.component_type().imports(engine) {
        let item = item.ty;
        let functions = |item: &ComponentItem| -> Vec<String> {
            match item {
                ComponentItem::ComponentInstance(instance) => instance
                    .exports(engine)
                    .filter(|(_, item)| matches!(item.ty, ComponentItem::ComponentFunc(_)))
                    .map(|(name, _)| name.to_string())
                    .collect(),
                _ => Vec::new(),
            }
        };
        let base = unversioned(name);
        if base == "ikigai:runtime/kernel" {
            continue;
        }
        if base.starts_with("wasi:") {
            if !LINKED_WASI.iter().any(|prefix| base.starts_with(prefix)) {
                return Err(unresolved(name));
            }
            if base.starts_with("wasi:sockets/") {
                imports.sockets.push((name.to_string(), functions(&item)));
            } else if base.starts_with("wasi:http/") {
                imports.http.push((name.to_string(), functions(&item)));
            }
            continue;
        }
        // Anything else is the app's own import: every function needs a binding, or the mount
        // is refused naming the first one without (R-D5: never silently stubbed).
        let ComponentItem::ComponentInstance(_) = &item else {
            return Err(unresolved(name));
        };
        if !bindings.binds_instance(name) {
            return Err(unresolved(name));
        }
        let funcs = functions(&item);
        if let Some(missing) = funcs.iter().find(|f| bindings.get(name, f).is_none()) {
            return Err(unresolved(&format!("{name}#{missing}")));
        }
        imports.bound.push((name.to_string(), funcs));
    }
    Ok(imports)
}

/// Which grant-gated WASI a call's linker carries. One `InstancePre` is cached per shape.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct LinkShape {
    sockets: bool,
    http: bool,
}

/// One mounted component.
pub(crate) struct MountedApp {
    engine: Arc<EngineInner>,
    component: Component,
    digest: ArtifactDigest,
    pub(crate) config: AppConfig,
    imports: Imports,
    api: Option<ApiIndices>,
    /// `InstancePre` per link shape (R-D7: compile once, instantiate per call).
    pres: Mutex<HashMap<LinkShape, InstancePre<HostState>>>,
}

/// One call's authority, budget and kernel seam.
pub(crate) struct Session {
    /// The capability the call runs under: the mount's ceiling clamped by the caller's.
    pub(crate) capability: Capability,
    plan: WasiPlan,
    shape: LinkShape,
    pub(crate) limits: Limits,
    host: Arc<dyn HostResolver>,
    /// Bytes the deterministic seed is drawn from (the request's identity).
    seed_input: Vec<u8>,
}

/// What a call produced besides its value: its cost and its captured stdio.
#[derive(Debug, Default)]
pub(crate) struct Ran {
    pub(crate) consumed: Consumed,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

impl MountedApp {
    /// The session a call by `caller` (with `request`) runs in. `host` serves its kernel calls.
    pub(crate) fn session(
        &self,
        caller: &Capability,
        request: &Request,
        caller_limits: Limits,
        host: Arc<dyn HostResolver>,
    ) -> Result<Session, Error> {
        let capability = self.config.ceiling.clamp(caller);
        // A grant the plan refuses (an fs path that could leave the jail root) fails the call,
        // naming the grant: never a silent skip.
        let refused =
            |e: ikigai_runtime::PlanError| Error::Denied(format!("{}: {e}", self.config.app));
        let (plan, shape) = match self.config.world {
            World::Endpoint => (
                wasi_plan(&Grants::from_scopes(std::iter::empty::<String>())).map_err(refused)?,
                LinkShape {
                    sockets: false,
                    http: false,
                },
            ),
            World::App => {
                let grants = Grants::from_capability(&capability);
                let plan = wasi_plan(&grants).map_err(refused)?;
                let root = matches!(grants, Grants::Root);
                let shape = LinkShape {
                    sockets: root || !plan.net.ip_rules.is_empty(),
                    http: root || !plan.net.http_hosts.is_empty(),
                };
                (plan, shape)
            }
        };
        let limits = self
            .config
            .limits
            .meet(&Limits::from_grants(&Grants::from_capability(caller)))
            .meet(&caller_limits);
        Ok(Session {
            capability,
            plan,
            shape,
            limits,
            host,
            seed_input: postcard::to_allocvec(request).unwrap_or_default(),
        })
    }

    /// Whether this call's answer may be cached (R-D4: by world, never by guess).
    pub(crate) fn cacheable(&self, session: &Session) -> bool {
        match self.config.world {
            World::Endpoint => true,
            World::App => {
                use ikigai_runtime::{ClockAccess, RandomAccess};
                self.config.pure
                    && session.plan.clock == ClockAccess::Fixed
                    && session.plan.random == RandomAccess::Seeded
                    && !session.shape.sockets
                    && !session.shape.http
            }
        }
    }

    /// The `InstancePre` for a link shape, built on first use.
    fn pre(&self, shape: LinkShape) -> Result<InstancePre<HostState>, Error> {
        let mut pres = self.pres.lock().expect("the InstancePre cache");
        if let Some(pre) = pres.get(&shape) {
            return Ok(pre.clone());
        }
        let linker = self.linker(shape)?;
        let pre = linker
            .instantiate_pre(&self.component)
            .map_err(|e| Error::Endpoint(format!("linking {}: {e:#}", self.config.app)))?;
        pres.insert(shape, pre.clone());
        Ok(pre)
    }

    /// The linker for a shape: all of WASI p2 against the call's context, the `kernel` import,
    /// the configured bindings, and `wasi:http` routed to the kernel — then every socket or HTTP
    /// function the shape does not grant SHADOWED by a refusal, so the import is linked (a std
    /// guest that never calls it still instantiates) and every call of it fails at the import
    /// with a typed `Denied` (R-D3: an ungranted interface has no implementation).
    fn linker(&self, shape: LinkShape) -> Result<Linker<HostState>, Error> {
        let link = |e: wasmtime::Error| Error::Endpoint(format!("linking: {e:#}"));
        let mut linker: Linker<HostState> = Linker::new(&self.engine.engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker).map_err(link)?;
        wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker).map_err(link)?;
        kernel::add_to_linker::<HostState, KernelData>(&mut linker, |s: &mut HostState| {
            &mut s.kernel
        })
        .map_err(link)?;
        for (instance, functions) in &self.imports.bound {
            let mut inst = linker.instance(instance).map_err(link)?;
            for function in functions {
                let binding = self
                    .config
                    .bindings
                    .get(instance, function)
                    .cloned()
                    .ok_or_else(|| unresolved(&format!("{instance}#{function}")))?;
                inst.func_new_async(function, move |store, ty, params, results| {
                    let binding = binding.clone();
                    Box::new(async move {
                        let kernel = &store.data().kernel;
                        let host = Arc::clone(&kernel.host);
                        let capability = kernel.capability.clone();
                        let mut request = Request::new(binding.verb, binding.target.clone());
                        for ((name, _), value) in ty.params().zip(params) {
                            request = request.with_arg(
                                name,
                                ikigai_core::ArgRef::Inline(values::val_to_arg_bytes(value)),
                            );
                        }
                        let answer = host.resolve(request, &capability).await;
                        let result_ty = ty.results().next();
                        match (answer, result_ty) {
                            (Ok(repr), Some(rty)) => {
                                results[0] = values::representation_to_val(&rty, &repr)
                                    .map_err(|e| wasmtime::Error::new(HostRefusal(e)))?;
                                Ok(())
                            }
                            (Ok(_), None) => Ok(()),
                            // A `result<_, string>` takes the refusal as its `err`; any other
                            // shape cannot hold it, so the call fails with its type intact.
                            (Err(e), Some(wasmtime::component::types::Type::Result(r)))
                                if matches!(
                                    r.err(),
                                    Some(wasmtime::component::types::Type::String)
                                ) =>
                            {
                                results[0] =
                                    Val::Result(Err(Some(Box::new(Val::String(e.to_string())))));
                                Ok(())
                            }
                            (Err(e), _) => Err(wasmtime::Error::new(HostRefusal(e))),
                        }
                    })
                })
                .map_err(link)?;
            }
        }
        linker.allow_shadowing(true);
        let mut deny = |groups: &[(String, Vec<String>)], why: &'static str| {
            for (instance, functions) in groups {
                let mut inst = linker.instance(instance).map_err(link)?;
                for function in functions {
                    let name = format!("{instance}#{function}");
                    inst.func_new(function, move |_, _, _, _| {
                        Err(wasmtime::Error::new(HostRefusal(Error::Denied(format!(
                            "{name}: {why}"
                        )))))
                    })
                    .map_err(link)?;
                }
            }
            Ok::<(), Error>(())
        };
        if !shape.sockets {
            deny(
                &self.imports.sockets,
                "no grant opens a socket (urn:cap:net:<ip>[:<port>] in the app world)",
            )?;
        }
        if !shape.http {
            deny(
                &self.imports.http,
                "no grant names a host (urn:cap:net:<host> in the app world)",
            )?;
        }
        Ok(linker)
    }

    /// A fresh store for one call: the WASI context from the session's plan, the kernel seam,
    /// the budget.
    fn store(&self, session: &Session) -> Result<(Store<HostState>, Pipes), Error> {
        let mut builder = deterministic_builder(&seed_of(
            self.digest.as_str().as_bytes(),
            &session.seed_input,
        ));
        let stdout = MemoryOutputPipe::new(STDIO_CAPACITY);
        let stderr = MemoryOutputPipe::new(STDIO_CAPACITY);
        builder.stdout(stdout.clone()).stderr(stderr.clone());
        if self.config.world == World::App {
            apply_plan(&mut builder, &session.plan, &self.config.host)
                .map_err(|detail| Error::Denied(format!("{}: {detail}", self.config.app)))?;
        }
        let state = HostState {
            wasi: builder.build(),
            table: ResourceTable::new(),
            http: WasiHttpCtx::new(),
            hooks: KernelHttp {
                host: Arc::clone(&session.host),
                capability: session.capability.clone(),
            },
            kernel: KernelCtx {
                host: Arc::clone(&session.host),
                capability: session.capability.clone(),
            },
            limiter: MemLimiter::new(session.limits.memory_bytes),
        };
        let mut store = Store::new(&self.engine.engine, state);
        store.limiter(|s| &mut s.limiter as &mut dyn ResourceLimiter);
        Ok((store, Pipes { stdout, stderr }))
    }

    fn effective(&self, session: &Session) -> Result<Limits, Error> {
        check_metering(&session.limits, self.engine.metering)?;
        Ok(self.engine.ceiling.meet(&session.limits))
    }

    /// Run one projected export with `params` (already converted), returning its result value.
    pub(crate) async fn call_export(
        &self,
        func: &ProjectedFunc,
        params: Vec<Val>,
        session: Session,
    ) -> Result<(Option<Val>, Ran), Error> {
        let limits = self.effective(&session)?;
        let pre = self.pre(session.shape)?;
        let (mut store, pipes) = self.store(&session)?;
        // The store holds the session's kernel seam now; drop the session's own handle so the
        // seam closes when the store does.
        drop(session);
        apply_budget(&mut store, &limits, self.engine.metering);
        let instance = pre
            .instantiate_async(&mut store)
            .await
            .map_err(|e| trap_to_error(e, &limits, store.data().limiter.denied))?;
        let f = instance
            .get_func(&mut store, func.index)
            .ok_or_else(|| Error::NotFound(format!("export {}", func.endpoint.urn())))?;
        let mut results = vec![Val::Bool(false); func.result.iter().count()];
        let before = store.get_fuel().ok();
        let started = Instant::now();
        f.call_async(&mut store, &params, &mut results)
            .await
            .map_err(|e| trap_to_error(e, &limits, store.data().limiter.denied))?;
        let mut consumed = consumed_of(&store, before);
        consumed.wall = Some(started.elapsed());
        drop(store);
        Ok((results.into_iter().next(), pipes.ran(consumed)))
    }

    /// Ask an `endpoint-api` component what it serves: its `describe()`, run once at mount with
    /// no authority (the endpoint world's context and a kernel that refuses), parsed as core
    /// `Description` JSON: one object or an array of them. Each `id` is mounted at
    /// `urn:runtime:app:{app}:{id}`.
    async fn describe_api(&self) -> Result<Vec<Description>, Error> {
        let api = self
            .api
            .clone()
            .expect("called for an endpoint-api component");
        let nobody = Capability::root().attenuate(std::iter::empty::<String>());
        let request = Request::new(
            Verb::Meta,
            ikigai_core::Iri::parse(format!("urn:runtime:app:{}", self.config.app)).map_err(
                |e| Error::InvalidArgument {
                    name: "app".into(),
                    detail: e.to_string(),
                },
            )?,
        );
        let mut session = self.session(
            &nobody,
            &request,
            Limits::unbounded(),
            Arc::new(ikigai_runtime::NoHost),
        )?;
        session.shape = LinkShape {
            sockets: false,
            http: false,
        };
        let limits = self.effective(&session)?;
        let pre = self.pre(session.shape)?;
        let (mut store, _pipes) = self.store(&session)?;
        apply_budget(&mut store, &limits, self.engine.metering);
        let instance = pre
            .instantiate_async(&mut store)
            .await
            .map_err(|e| trap_to_error(e, &limits, store.data().limiter.denied))?;
        let describe = instance
            .get_typed_func::<(), (String,)>(&mut store, &api.describe)
            .map_err(|e| Error::Endpoint(format!("endpoint-api.describe: {e:#}")))?;
        let (json,) = describe
            .call_async(&mut store, ())
            .await
            .map_err(|e| trap_to_error(e, &limits, store.data().limiter.denied))?;
        parse_descriptions(&json).map_err(|e| Error::InvalidArgument {
            name: "describe".into(),
            detail: format!(
                "{}: describe() is not core Description JSON: {e}",
                self.config.app
            ),
        })
    }

    /// Run `endpoint-api.invoke` for one served endpoint.
    pub(crate) async fn call_api(
        &self,
        request: endpoint_api::Request,
        session: Session,
    ) -> Result<(Result<kernel::Representation, kernel::Error>, Ran), Error> {
        let api = self
            .api
            .clone()
            .expect("called for an endpoint-api component");
        let limits = self.effective(&session)?;
        let pre = self.pre(session.shape)?;
        let (mut store, pipes) = self.store(&session)?;
        drop(session);
        apply_budget(&mut store, &limits, self.engine.metering);
        let instance = pre
            .instantiate_async(&mut store)
            .await
            .map_err(|e| trap_to_error(e, &limits, store.data().limiter.denied))?;
        let invoke = instance
            .get_typed_func::<(endpoint_api::Request,), (Result<kernel::Representation, kernel::Error>,)>(
                &mut store,
                &api.invoke,
            )
            .map_err(|e| Error::Endpoint(format!("endpoint-api.invoke: {e:#}")))?;
        let before = store.get_fuel().ok();
        let started = Instant::now();
        let (answer,) = invoke
            .call_async(&mut store, (request,))
            .await
            .map_err(|e| trap_to_error(e, &limits, store.data().limiter.denied))?;
        let mut consumed = consumed_of(&store, before);
        consumed.wall = Some(started.elapsed());
        drop(store);
        Ok((answer, pipes.ran(consumed)))
    }

    /// The projected-and-skipped report's app name.
    pub(crate) fn app(&self) -> &str {
        &self.config.app
    }
}

fn parse_descriptions(json: &str) -> Result<Vec<Description>, serde_json::Error> {
    match serde_json::from_str::<Vec<Description>>(json) {
        Ok(many) => Ok(many),
        Err(_) => serde_json::from_str::<Description>(json).map(|one| vec![one]),
    }
}

/// The captured stdio of one call.
struct Pipes {
    stdout: MemoryOutputPipe,
    stderr: MemoryOutputPipe,
}

impl Pipes {
    fn ran(self, consumed: Consumed) -> Ran {
        Ran {
            consumed,
            stdout: self.stdout.contents().to_vec(),
            stderr: self.stderr.contents().to_vec(),
        }
    }
}

/// The store data of one component call.
pub(crate) struct HostState {
    wasi: WasiCtx,
    table: ResourceTable,
    http: WasiHttpCtx,
    hooks: KernelHttp,
    kernel: KernelCtx,
    limiter: MemLimiter,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl WasiHttpView for HostState {
    fn http(&mut self) -> WasiHttpCtxView<'_> {
        WasiHttpCtxView {
            hooks: &mut self.hooks,
            table: &mut self.table,
            ctx: &mut self.http,
        }
    }
}

/// The load report of a mount: what was projected, what was served through `endpoint-api`, and
/// what was skipped with its reason (R-D4).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoadReport {
    /// The projected URNs.
    pub projected: Vec<String>,
    /// The URNs an `endpoint-api` component serves through its own `describe()`.
    pub served: Vec<String>,
    /// Exports left out, with the reason.
    pub skipped: Vec<Skipped>,
}

pub(crate) fn report(
    funcs: &[ProjectedFunc],
    served: &BTreeMap<String, Description>,
    skipped: Vec<Skipped>,
) -> LoadReport {
    LoadReport {
        projected: funcs.iter().map(|f| f.endpoint.urn()).collect(),
        served: served.keys().cloned().collect(),
        skipped,
    }
}
