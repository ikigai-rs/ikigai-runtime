//! A mounted component as a core [`Space`]: "the runtime mints; core arranges" (R-D7, R-D8). Each
//! projected export and each endpoint an `endpoint-api` component serves is an [`Endpoint`] under
//! the mount's own `urn:runtime:app:{app}:` prefix, and nowhere else.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures::join;
use ikigai_core::{
    encode_name, Bindings, Capability, Description, Endpoint, Error, Invocation, ReprType,
    Representation, Request, Resolution, Resolved, Result, Scope, Space, SpaceEntry,
};
use ikigai_runtime::{Deadline, Limits};
use wasmtime::component::types::Type;
use wasmtime::component::Val;

use super::bindings::{core_error, endpoint_api, kernel};
use super::engine::{report, LoadReport, MountedApp, Ran};
use super::project::{ProjectedFunc, Skipped};
use super::values;
use crate::bridge::{bridge, serve};

/// A mounted component: a core `Space` whose endpoints are its projected exports (and, for an
/// `endpoint-api` component, the endpoints its `describe()` names), all under
/// `urn:runtime:app:{app}:`. Arrange it with a core declaration or a `Fallback` like any space.
pub struct ComponentSpace {
    app: Arc<MountedApp>,
    endpoints: BTreeMap<String, Arc<dyn Endpoint>>,
    report: LoadReport,
}

impl ComponentSpace {
    pub(crate) fn new(
        app: Arc<MountedApp>,
        funcs: Vec<ProjectedFunc>,
        served: Vec<Description>,
        skipped: Vec<Skipped>,
    ) -> Result<Self> {
        // `mount` checked the name is one segment.
        let name = app.app();
        let mut served_by_urn: BTreeMap<String, Description> = BTreeMap::new();
        for description in served {
            let urn = format!("urn:runtime:app:{name}:{}", encode_name(&description.id));
            if served_by_urn.insert(urn.clone(), description).is_some() {
                return Err(Error::Conflict(format!(
                    "{name}: describe() names {urn} twice"
                )));
            }
        }
        let report = report(&funcs, &served_by_urn, skipped);
        let mut endpoints: BTreeMap<String, Arc<dyn Endpoint>> = BTreeMap::new();
        for func in funcs {
            let urn = func.endpoint.urn();
            endpoints.insert(
                urn,
                Arc::new(ExportEndpoint {
                    app: Arc::clone(&app),
                    func: Arc::new(func),
                }),
            );
        }
        for (urn, description) in served_by_urn {
            if endpoints.contains_key(&urn) {
                return Err(Error::Conflict(format!(
                    "{urn} is both a projected export and an endpoint describe() serves"
                )));
            }
            endpoints.insert(
                urn,
                Arc::new(ApiEndpoint {
                    app: Arc::clone(&app),
                    description,
                }),
            );
        }
        Ok(ComponentSpace {
            app,
            endpoints,
            report,
        })
    }

    /// The mount name.
    pub fn app(&self) -> &str {
        self.app.app()
    }

    /// Every URN this mount answers.
    pub fn urns(&self) -> Vec<String> {
        self.endpoints.keys().cloned().collect()
    }

    /// What was projected, served and skipped, with the reasons (R-D4's load report).
    pub fn report(&self) -> &LoadReport {
        &self.report
    }
}

impl Space for ComponentSpace {
    fn resolve(&self, request: &Request, _scope: &Scope) -> Resolution {
        match self.endpoints.get(request.target.as_str()) {
            Some(endpoint) => Resolution::Hit(Resolved::new(Arc::clone(endpoint), Bindings::new())),
            None => Resolution::Miss,
        }
    }

    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        Some(
            self.endpoints
                .iter()
                .map(|(urn, endpoint)| SpaceEntry::new(urn.clone(), endpoint.name().to_string()))
                .collect(),
        )
    }
}

/// The caller's own time limit: what remains of the issue context's `not_after`, as a host
/// deadline (its exhaustion is a `Timeout`, R-D10).
fn caller_limits(inv: &Invocation<'_>) -> Limits {
    match inv.time_left() {
        Some(left) => Limits::deadline(Deadline::host(left)),
        None => Limits::unbounded(),
    }
}

/// Issue a guest's kernel request through the invocation running it, under `capability` (the
/// session's: the mount ceiling clamped by the caller's), so it is a tracked dependency of the
/// answer.
async fn issue_under(
    inv: &Invocation<'_>,
    request: Request,
    capability: Capability,
) -> Result<Representation> {
    match capability.scopes() {
        None => inv.issue(request).await,
        Some(scopes) => inv.issue_attenuated(request, scopes.iter().cloned()).await,
    }
}

/// An argument's bytes, however it was supplied (inline, by reference, by content id).
async fn arg_bytes(inv: &Invocation<'_>, name: &str) -> Result<Vec<u8>> {
    let body = inv.content_body(name).await?;
    Ok(inv.collect(body).await?.to_vec())
}

/// Put what a call cost and printed on the trace (R-D10: fuel consumed is a note; R-D3: stdout
/// and stderr go to the trace, never to the host's own stdio).
fn note(inv: &Invocation<'_>, ran: &Ran) {
    if let Some(fuel) = ran.consumed.fuel {
        inv.trace_note("runtime.fuel", fuel.to_string());
    }
    for (key, bytes) in [
        ("runtime.stdout", &ran.stdout),
        ("runtime.stderr", &ran.stderr),
    ] {
        if !bytes.is_empty() {
            const SHOWN: usize = 1024;
            let shown = String::from_utf8_lossy(&bytes[..bytes.len().min(SHOWN)]);
            let more = if bytes.len() > SHOWN {
                format!(" … ({} bytes in all)", bytes.len())
            } else {
                String::new()
            };
            inv.trace_note(key, format!("{shown}{more}"));
        }
    }
}

/// A projected export (R-D4).
struct ExportEndpoint {
    app: Arc<MountedApp>,
    func: Arc<ProjectedFunc>,
}

#[async_trait]
impl Endpoint for ExportEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let mut params = Vec::with_capacity(self.func.params.len());
        for (name, ty) in &self.func.params {
            if inv.request.args.contains_key(name) {
                let bytes = arg_bytes(inv, name).await?;
                params.push(values::val_from_bytes(ty, &bytes, name)?);
            } else if matches!(ty, Type::Option(_)) {
                params.push(Val::Option(None));
            } else {
                return Err(Error::MissingArgument(name.clone()));
            }
        }
        let (resolver, requests) = bridge();
        let session = self
            .app
            .session(inv.capability, inv.request, caller_limits(inv), resolver);
        let cacheable = self.app.cacheable(&session);
        // The session (and with it the bridge's last sender) moves into the call and is dropped
        // when the call ends, which ends the loop serving the guest's kernel requests.
        let (ran, ()) = join!(
            self.app.call_export(&self.func, params, session),
            serve(requests, |request, capability| issue_under(
                inv, request, capability
            ))
        );
        let (value, ran) = ran?;
        note(inv, &ran);
        let urn = self.func.endpoint.urn();
        let representation = values::val_to_representation(self.func.result.as_ref(), value, &urn)?;
        Ok(if cacheable {
            representation.cacheable()
        } else {
            representation
        })
    }

    fn name(&self) -> &str {
        &self.func.description.id
    }

    fn describe(&self) -> Description {
        self.func.description.clone()
    }
}

/// An endpoint an `endpoint-api` component serves, by the id its `describe()` gave it.
struct ApiEndpoint {
    app: Arc<MountedApp>,
    description: Description,
}

#[async_trait]
impl Endpoint for ApiEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let mut args = Vec::with_capacity(inv.request.args.len());
        for name in inv.request.args.keys() {
            args.push(kernel::Arg {
                name: name.clone(),
                value: arg_bytes(inv, name).await?,
            });
        }
        let request = endpoint_api::Request {
            endpoint: self.description.id.clone(),
            args,
        };
        let (resolver, requests) = bridge();
        let session = self
            .app
            .session(inv.capability, inv.request, caller_limits(inv), resolver);
        let cacheable = self.app.cacheable(&session);
        let (ran, ()) = join!(
            self.app.call_api(request, session),
            serve(requests, |request, capability| issue_under(
                inv, request, capability
            ))
        );
        let (answer, ran) = ran?;
        note(inv, &ran);
        let representation = answer.map_err(core_error)?;
        // The base media type: core has no media-type parser, and ikigai-http keeps only the
        // base type of a Content-Type for the same reason (reported to the hub).
        let media = representation
            .media_type
            .split(';')
            .next()
            .unwrap_or("application/octet-stream")
            .trim()
            .to_string();
        let representation = Representation::new(ReprType::new(media), representation.bytes);
        Ok(if cacheable {
            representation.cacheable()
        } else {
            representation
        })
    }

    fn name(&self) -> &str {
        &self.description.id
    }

    fn describe(&self) -> Description {
        self.description.clone()
    }
}
