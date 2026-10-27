//! A jailed core module mounted into a kernel through ikigai-module's own [`ModuleTransport`]:
//! the same `ModuleSpace` that mounts a module in-process or over a socket mounts it in the
//! jail. One module, servable both ways (R-D6 as amended); the mount's floor, card handling,
//! rewrite declaration and dependency recording are ikigai-module's, not a second copy here.

use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{Capability, Error, Issuer, Representation, Request, Result};
use ikigai_module::{ModuleCall, ModuleCard, ModuleManifest, ModuleReply, ModuleTransport};
use ikigai_runtime::{Call, CoreEngine, Limits, NoHost, Output};

use crate::bridge::{bridge, serve};
use crate::core_tier::{Compiled, WasmtimeCoreEngine};

/// A [`ModuleTransport`] over a jailed ABI v1 core module: each `invoke` is one session in a
/// fresh `Store`, under `limits`, with every `host_call` answered through the host's
/// [`Issuer`] (for a `ModuleSpace` mount, the invocation's own `inv.issue`, so each guest read is
/// a dependency of the answer).
///
/// ```ignore
/// let engine = Arc::new(WasmtimeCoreEngine::new(Limits::fuel(1_000_000_000))?);
/// let module = Arc::new(engine.compile(&wasm)?);
/// let transport = Arc::new(JailTransport::new(engine, module, Limits::fuel(50_000_000)));
/// let space = ModuleSpace::connect(["urn:jail:"], transport, ModuleFloor::public()).await?;
/// ```
pub struct JailTransport {
    engine: Arc<WasmtimeCoreEngine>,
    module: Arc<Compiled>,
    limits: Limits,
}

impl JailTransport {
    /// Serve `module` on `engine`, each session under `limits` (met with the engine's ceiling).
    pub fn new(engine: Arc<WasmtimeCoreEngine>, module: Arc<Compiled>, limits: Limits) -> Self {
        JailTransport {
            engine,
            module,
            limits,
        }
    }

    /// Run one host-authored call that needs no kernel (a mount-time `Manifest` or `Cards`),
    /// and decode the final reply.
    async fn ask(&self, call: &ModuleCall) -> Result<ModuleReply> {
        let encoded = encode(call)?;
        let invoked = self
            .engine
            .invoke(
                &self.module,
                Call::Session {
                    encoded_call: &encoded,
                },
                &self.limits,
                Arc::new(NoHost),
            )
            .await?;
        decode_reply(invoked.output)
    }
}

#[async_trait]
impl ModuleTransport for JailTransport {
    async fn invoke(
        &self,
        request: Request,
        capability: &Capability,
        host: &dyn Issuer,
    ) -> Result<Representation> {
        let encoded = encode(&ModuleCall::Invoke {
            request,
            capability: capability.clone(),
        })?;
        let (resolver, requests) = bridge();
        let run = self.engine.invoke(
            &self.module,
            Call::Session {
                encoded_call: &encoded,
            },
            &self.limits,
            resolver,
        );
        // The guest and the host loop run side by side; the guest's store (and with it the
        // bridge's sender) is dropped when the run finishes, which ends the loop.
        let (ran, ()) = futures::join!(
            run,
            serve(requests, |request, capability| async move {
                host.issue(request, &capability).await
            })
        );
        outcome_from(decode_reply(ran?.output)?)
    }

    async fn manifest(&self) -> Result<ModuleManifest> {
        match self.ask(&ModuleCall::Manifest).await? {
            ModuleReply::Manifest(manifest) => Ok(manifest),
            // A guest that does not answer `Manifest` cannot say how it names things: that is
            // `Unknown`, never "does not rewrite" (ikigai-module's rule for an old peer).
            _ => Ok(ModuleManifest::unknown(None)),
        }
    }

    async fn cards(&self) -> Result<Vec<ModuleCard>> {
        match self.ask(&ModuleCall::Cards).await? {
            ModuleReply::Cards(cards) => Ok(cards),
            // A guest that serves no cards has none: the host's own cards stand.
            _ => Ok(Vec::new()),
        }
    }
}

fn encode(call: &ModuleCall) -> Result<Vec<u8>> {
    postcard::to_allocvec(call).map_err(|e| Error::Endpoint(format!("encode ModuleCall: {e}")))
}

/// Decode a session's final reply into ikigai-module's typed reply. The bytes are the guest's,
/// so a target that is not an IRI fails here, typed, exactly as it would over a socket.
fn decode_reply(output: Output) -> Result<ModuleReply> {
    let Output::Session(bytes) = output else {
        return Err(Error::Endpoint("expected a session output".into()));
    };
    postcard::from_bytes::<ModuleReply>(&bytes)
        .map_err(|e| Error::Endpoint(format!("undecodable ModuleReply from the guest: {e}")))
}

/// What a module's final reply means to the host: ikigai-module's private `outcome_from`,
/// restated because the crate does not export it (reported to the hub with `host_answer`).
fn outcome_from(reply: ModuleReply) -> Result<Representation> {
    match reply {
        ModuleReply::Resolved(representation) => Ok(representation),
        ModuleReply::ResolvedThreaded {
            representation,
            threads,
        } => Ok(threads
            .into_iter()
            .fold(representation, |representation, thread| {
                representation.depends_on(thread)
            })),
        ModuleReply::Error(message) => Err(Error::Endpoint(message)),
        ModuleReply::ErrorTyped(error) => Err(error.into()),
        ModuleReply::HostCall { .. } => Err(Error::Endpoint(
            "module made a HostCall where a final reply was expected".to_string(),
        )),
        ModuleReply::Bindings(_) | ModuleReply::Manifest(_) | ModuleReply::Cards(_) => Err(
            Error::Endpoint("module answered Invoke with a description".to_string()),
        ),
    }
}
