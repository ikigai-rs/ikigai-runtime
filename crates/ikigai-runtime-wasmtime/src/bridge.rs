//! The host-call bridge: a guest running INSIDE an invocation has its kernel requests served by
//! that invocation, so each one is a tracked dependency of the answer (golden thread, trace,
//! capability), exactly as a linked endpoint's `inv.issue` would be.
//!
//! A wasmtime `Store` must own `'static` data, and an `Invocation` (or ikigai-module's borrowed
//! `&dyn Issuer`) is borrowed, so the store cannot hold it. The store holds a [`BridgeResolver`]
//! instead, which sends each request down a channel; the caller runs the guest and [`serve`]
//! side by side (`futures::join!`), and `serve` answers each request through the borrowed
//! issuer. When the guest finishes, its store is dropped, the channel closes, and `serve` returns.

use std::future::Future;
use std::sync::Arc;

use futures::channel::{mpsc, oneshot};
use futures::StreamExt;
use ikigai_core::{Capability, Error, Representation, Request};
use ikigai_runtime::{HostFuture, HostResolver};

/// One request a guest made, waiting for its answer.
pub(crate) struct Pending {
    request: Request,
    capability: Capability,
    reply: oneshot::Sender<Result<Representation, Error>>,
}

/// The store's end of the bridge.
pub(crate) struct BridgeResolver {
    tx: mpsc::UnboundedSender<Pending>,
}

impl HostResolver for BridgeResolver {
    fn resolve<'a>(&'a self, request: Request, capability: &'a Capability) -> HostFuture<'a> {
        let (reply, answer) = oneshot::channel();
        let sent = self.tx.unbounded_send(Pending {
            request,
            capability: capability.clone(),
            reply,
        });
        Box::pin(async move {
            if sent.is_err() {
                return Err(Error::Unavailable(
                    "the invocation serving this guest's kernel calls has ended".into(),
                ));
            }
            answer.await.unwrap_or_else(|_| {
                Err(Error::Unavailable(
                    "the invocation serving this guest's kernel calls dropped the request".into(),
                ))
            })
        })
    }
}

/// A new bridge: the resolver for the store, and the receiver [`serve`] drains.
pub(crate) fn bridge() -> (Arc<BridgeResolver>, mpsc::UnboundedReceiver<Pending>) {
    let (tx, rx) = mpsc::unbounded();
    (Arc::new(BridgeResolver { tx }), rx)
}

/// Answer every request the guest sends through `issue`, until the guest's store is dropped.
/// Requests are served one at a time, in order: the guest is blocked on each one anyway.
pub(crate) async fn serve<F, Fut>(mut rx: mpsc::UnboundedReceiver<Pending>, mut issue: F)
where
    F: FnMut(Request, Capability) -> Fut,
    Fut: Future<Output = Result<Representation, Error>>,
{
    while let Some(pending) = rx.next().await {
        let answer = issue(pending.request, pending.capability).await;
        let _ = pending.reply.send(answer);
    }
}
