//! The host side of the `ikigai:runtime` WIT (R-D2): the `kernel` import, served through the
//! kernel under the session's capability, and the typed `endpoint-api` export.
//!
//! The WIT is `wit/runtime.wit`, a copy of `ikigai-runtime-wit`'s (a published crate cannot read
//! outside its own directory, and `bindgen!` reads the file at compile time); `ikigai-runtime-wit`
//! tests that every copy is byte-identical to the canonical one.

use std::sync::Arc;

use ikigai_core::{ArgRef, Capability, Error, Iri, Request, Verb};
use ikigai_runtime::HostResolver;
use wasmtime::component::HasData;

wasmtime::component::bindgen!({
    path: "wit",
    world: "ikigai:runtime/endpoint",
    imports: { default: async | trappable },
    exports: { default: async },
});

pub(crate) use exports::ikigai::runtime::endpoint_api;
pub(crate) use ikigai::runtime::kernel;

/// What the `kernel` import needs: the kernel seam and the capability every call runs under.
/// The guest names no capability at all; the host chose this one (the session's, clamped).
pub(crate) struct KernelCtx {
    pub(crate) host: Arc<dyn HostResolver>,
    pub(crate) capability: Capability,
}

/// The `HasData` marker the generated `add_to_linker` projects the store's [`KernelCtx`] through.
pub(crate) struct KernelData;

impl HasData for KernelData {
    type Data<'a> = &'a mut KernelCtx;
}

impl KernelCtx {
    async fn call(
        &mut self,
        verb: Verb,
        iri: String,
        args: Vec<kernel::Arg>,
    ) -> Result<ikigai_core::Representation, kernel::Error> {
        let target = Iri::parse(iri.as_str())
            .map_err(|e| kernel::Error::InvalidArgument(format!("iri `{iri}`: {e}")))?;
        let mut request = Request::new(verb, target);
        for arg in args {
            if request.args.contains_key(&arg.name) {
                return Err(kernel::Error::InvalidArgument(format!(
                    "argument `{}` given twice; an argument takes one value, so name it once",
                    arg.name
                )));
            }
            request.args.insert(arg.name, ArgRef::Inline(arg.value));
        }
        let host = Arc::clone(&self.host);
        let capability = self.capability.clone();
        host.resolve(request, &capability)
            .await
            .map_err(|e| wit_error(&e))
    }
}

/// A kernel representation as the WIT's `representation` record: the media type in its
/// canonical form (parameters included) and the bytes.
pub(crate) fn wit_representation(repr: ikigai_core::Representation) -> kernel::Representation {
    kernel::Representation {
        media_type: repr.repr_type.canonical(),
        bytes: repr.bytes,
    }
}

/// A kernel error as the WIT's `error` variant. The WIT has no case for `missing-argument`,
/// `unavailable`, `retry-after` or `depth-exceeded` (reported to the hub): a missing argument
/// is an invalid argument naming it, a transient unavailability rides `timeout` (the WIT's one
/// transient case), and the others are `endpoint`, carrying the full message.
pub(crate) fn wit_error(error: &Error) -> kernel::Error {
    match error {
        Error::Unresolved(iri) => kernel::Error::Unresolved(iri.as_str().to_string()),
        Error::NotFound(m) => kernel::Error::NotFound(m.clone()),
        Error::Denied(m) => kernel::Error::Denied(m.clone()),
        Error::InvalidArgument { name, detail } => {
            kernel::Error::InvalidArgument(format!("{name}: {detail}"))
        }
        Error::MissingArgument(name) => {
            kernel::Error::InvalidArgument(format!("{name}: missing required argument"))
        }
        Error::Conflict(m) => kernel::Error::Conflict(m.clone()),
        Error::Timeout(m) | Error::Unavailable(m) => kernel::Error::Timeout(m.clone()),
        Error::OverBudget { detail } => kernel::Error::OverBudget(detail.clone()),
        Error::Endpoint(m) => kernel::Error::Endpoint(m.clone()),
        other => kernel::Error::Endpoint(other.to_string()),
    }
}

/// A guest's WIT `error` as a kernel error. A guest's `invalid-argument` carries no argument
/// name, so it is reported under the name `request`.
pub(crate) fn core_error(error: kernel::Error) -> Error {
    match error {
        kernel::Error::NotFound(m) => Error::NotFound(m),
        kernel::Error::Unresolved(m) => match Iri::parse(m.as_str()) {
            Ok(iri) => Error::Unresolved(iri),
            Err(_) => Error::NotFound(m),
        },
        kernel::Error::Denied(m) => Error::Denied(m),
        kernel::Error::InvalidArgument(detail) => Error::InvalidArgument {
            name: "request".to_string(),
            detail,
        },
        kernel::Error::Conflict(m) => Error::Conflict(m),
        kernel::Error::Timeout(m) => Error::Timeout(m),
        kernel::Error::OverBudget(detail) => Error::OverBudget { detail },
        kernel::Error::Endpoint(m) => Error::Endpoint(m),
    }
}

impl kernel::Host for KernelCtx {
    async fn source(
        &mut self,
        iri: String,
        args: Vec<kernel::Arg>,
    ) -> wasmtime::Result<Result<kernel::Representation, kernel::Error>> {
        Ok(self
            .call(Verb::Source, iri, args)
            .await
            .map(wit_representation))
    }

    async fn sink(
        &mut self,
        iri: String,
        args: Vec<kernel::Arg>,
    ) -> wasmtime::Result<Result<kernel::Representation, kernel::Error>> {
        Ok(self
            .call(Verb::Sink, iri, args)
            .await
            .map(wit_representation))
    }

    async fn exists(
        &mut self,
        iri: String,
        args: Vec<kernel::Arg>,
    ) -> wasmtime::Result<Result<bool, kernel::Error>> {
        Ok(self
            .call(Verb::Exists, iri, args)
            .await
            .map(|repr| repr.bytes == b"true"))
    }

    async fn delete(
        &mut self,
        iri: String,
        args: Vec<kernel::Arg>,
    ) -> wasmtime::Result<Result<kernel::Representation, kernel::Error>> {
        Ok(self
            .call(Verb::Delete, iri, args)
            .await
            .map(wit_representation))
    }

    async fn meta(
        &mut self,
        iri: String,
        args: Vec<kernel::Arg>,
    ) -> wasmtime::Result<Result<kernel::Representation, kernel::Error>> {
        Ok(self
            .call(Verb::Meta, iri, args)
            .await
            .map(wit_representation))
    }
}
