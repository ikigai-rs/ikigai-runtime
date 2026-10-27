//! The core-module (ikigai-module ABI v1) guest ABI (R-D6, "J2 absorbed").
//!
//! A guest declares its exports with [`ikigai_module!`](crate::ikigai_module), passing a dispatch
//! function `fn(AbiModuleCall) -> Reply`. The SDK handles the `u64` packing, the guest allocator
//! the host writes through, and the synchronous `host_call` pump. The payload is ikigai-module's
//! session, built from the wire-only mirrors in `ikigai-module-abi` (re-exported as [`abi`]): a
//! guest links the session and nothing of ikigai-core.
//!
//! ## Why a guest replies with [`Reply`], not `AbiModuleReply`
//!
//! serde's derived serializer for `AbiModuleReply` matches all eight variants, so every guest
//! links the encoding of every payload, including the ones only a HOST-side module ever sends.
//! [`Reply`] is the guest's role of the same enum: the variants a guest answers with, each
//! encoded at **its `AbiModuleReply` index** through postcard's own serializer, so the bytes are
//! identical (`tests` below compare every variant against the derived encoding) and nothing of
//! the wire is re-implemented. `HostCall` is the SDK's own business ([`host_call`]).

use crate::abi::{
    AbiCapability, AbiModuleCall, AbiModuleCard, AbiModuleError, AbiModuleManifest, AbiModuleReply,
    AbiRepresentation, AbiRequest, AbiSpaceEntry, AbiVerb,
};
use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use ikigai_runtime::session::pack;
use serde::ser::{Serialize, SerializeStructVariant, Serializer};

/// The name serde gives the enum on the wire. postcard writes only the variant INDEX, so this is
/// documentation; the index is the contract.
const REPLY: &str = "AbiModuleReply";

/// What a guest answers a call with: the guest's role of `AbiModuleReply`, encoded at the same
/// variant indexes (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// The invocation finished with a representation that declares no golden thread.
    /// `AbiModuleReply::Resolved`, index 1.
    Resolved(AbiRepresentation),
    /// The invocation failed with an endpoint error. `AbiModuleReply::Error`, index 2.
    Error(String),
    /// The answer to `Describe`: the guest's bound entries, or `None` when it cannot enumerate
    /// them. `AbiModuleReply::Bindings`, index 3.
    Bindings(Option<Vec<AbiSpaceEntry>>),
    /// The answer to `Manifest`: the guest's bindings and how it rewrites names.
    /// `AbiModuleReply::Manifest`, index 4.
    Manifest(AbiModuleManifest),
    /// The invocation failed, and the failure keeps its type. `AbiModuleReply::ErrorTyped`,
    /// index 5.
    ErrorTyped(AbiModuleError),
    /// The invocation finished with a representation and the golden threads its endpoint
    /// declared. `AbiModuleReply::ResolvedThreaded`, index 6.
    ResolvedThreaded {
        /// The result.
        representation: AbiRepresentation,
        /// The names of the threads the endpoint declared on it.
        threads: Vec<String>,
    },
    /// The answer to `Cards`: each bound entry with its description as JSON text (the guest
    /// holds that text; it links no JSON code). `AbiModuleReply::Cards`, index 7.
    Cards(Vec<AbiModuleCard>),
}

impl Serialize for Reply {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Reply::Resolved(r) => s.serialize_newtype_variant(REPLY, 1, "Resolved", r),
            Reply::Error(m) => s.serialize_newtype_variant(REPLY, 2, "Error", m),
            Reply::Bindings(b) => s.serialize_newtype_variant(REPLY, 3, "Bindings", b),
            Reply::Manifest(m) => s.serialize_newtype_variant(REPLY, 4, "Manifest", m),
            Reply::ErrorTyped(e) => s.serialize_newtype_variant(REPLY, 5, "ErrorTyped", e),
            Reply::ResolvedThreaded {
                representation,
                threads,
            } => {
                let mut v = s.serialize_struct_variant(REPLY, 6, "ResolvedThreaded", 2)?;
                v.serialize_field("representation", representation)?;
                v.serialize_field("threads", threads)?;
                v.end()
            }
            Reply::Cards(c) => s.serialize_newtype_variant(REPLY, 7, "Cards", c),
        }
    }
}

impl From<Reply> for AbiModuleReply {
    fn from(reply: Reply) -> Self {
        match reply {
            Reply::Resolved(r) => AbiModuleReply::Resolved(r),
            Reply::Error(m) => AbiModuleReply::Error(m),
            Reply::Bindings(b) => AbiModuleReply::Bindings(b),
            Reply::Manifest(m) => AbiModuleReply::Manifest(m),
            Reply::ErrorTyped(e) => AbiModuleReply::ErrorTyped(e),
            Reply::ResolvedThreaded {
                representation,
                threads,
            } => AbiModuleReply::ResolvedThreaded {
                representation,
                threads,
            },
            Reply::Cards(c) => AbiModuleReply::Cards(c),
        }
    }
}

/// A `HostCall` as the guest writes it: `AbiModuleReply::HostCall`, index 0, borrowed.
struct HostCall<'a> {
    request: &'a AbiRequest,
    capability: &'a AbiCapability,
}

impl Serialize for HostCall<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut v = s.serialize_struct_variant(REPLY, 0, "HostCall", 2)?;
        v.serialize_field("request", self.request)?;
        v.serialize_field("capability", self.capability)?;
        v.end()
    }
}

/// Encode a `HostCall` as the session carries it (`AbiModuleReply::HostCall`, index 0).
pub fn encode_host_call(request: &AbiRequest, capability: &AbiCapability) -> Vec<u8> {
    postcard::to_allocvec(&HostCall {
        request,
        capability,
    })
    .unwrap_or_default()
}

/// Allocate `len` bytes the host will write through (`ikigai_alloc`). The buffer has capacity
/// exactly `len`, so [`dealloc`] with the same `len` frees it soundly.
pub fn alloc(len: usize) -> *mut u8 {
    let mut v = vec![0u8; len];
    let ptr = v.as_mut_ptr();
    core::mem::forget(v);
    ptr
}

/// Free a buffer allocated by [`alloc`] (or handed back by the host as a `host_call` result).
///
/// # Safety
/// `ptr`/`len` must name a buffer this allocator produced and not yet freed.
pub unsafe fn dealloc(ptr: *mut u8, len: usize) {
    drop(Vec::from_raw_parts(ptr, len, len));
}

/// Take ownership of bytes at `ptr`/`len` (reading and freeing them in one move).
///
/// # Safety
/// `ptr`/`len` must name a buffer this allocator produced and not yet freed.
pub unsafe fn take_bytes(ptr: *mut u8, len: usize) -> Vec<u8> {
    Vec::from_raw_parts(ptr, len, len)
}

/// Hand `bytes` to the host and return `(ptr, len)`. The buffer is exact-sized, so the host frees
/// it with the same `len`.
pub fn leak_parts(bytes: Vec<u8>) -> (u32, u32) {
    let boxed = bytes.into_boxed_slice();
    let len = boxed.len();
    let ptr = Box::into_raw(boxed) as *mut u8;
    (ptr as u32, len as u32)
}

/// Hand `bytes` to the host as the ABI's packed `u64` return.
pub fn leak_bytes(bytes: Vec<u8>) -> u64 {
    let (ptr, len) = leak_parts(bytes);
    pack(ptr, len)
}

/// Encode a reply as the session carries it.
pub fn encode_reply(reply: &Reply) -> Vec<u8> {
    postcard::to_allocvec(reply).unwrap_or_default()
}

/// Decode a call, dispatch it, and encode the reply. Pure and testable without a wasm target —
/// [`run_invoke`] is only the pointer glue over this. A call a guest should never receive (a
/// `HostResult` or `HostError` outside a `host_call`) is answered as an endpoint error, and so
/// is one it cannot decode.
pub fn process(call_bytes: &[u8], dispatch: fn(AbiModuleCall) -> Reply) -> Vec<u8> {
    let reply = match AbiModuleCall::decode(call_bytes) {
        Ok(AbiModuleCall::HostResult(_) | AbiModuleCall::HostError(_)) => {
            Reply::Error("guest received a host answer outside a host_call".to_string())
        }
        Ok(call) => dispatch(call),
        Err(e) => Reply::Error(alloc::format!("decode: {e}")),
    };
    encode_reply(&reply)
}

/// The body of the `ikigai_invoke` export: read the call the host wrote, dispatch it, and hand
/// the reply back as the packed `u64`.
///
/// # Safety
/// `ptr`/`len` must name a buffer the host wrote with [`alloc`].
pub unsafe fn run_invoke(ptr: u32, len: u32, dispatch: fn(AbiModuleCall) -> Reply) -> u64 {
    let call_bytes = take_bytes(ptr as *mut u8, len as usize);
    leak_bytes(process(&call_bytes, dispatch))
}

/// The one import the ABI declares: `"ikigai" "host_call"(ptr, len) -> u64`. Only present on
/// wasm32; a native build of the SDK (for linting and docs) has no host to call.
#[cfg(target_arch = "wasm32")]
mod imported {
    #[link(wasm_import_module = "ikigai")]
    extern "C" {
        pub fn host_call(ptr: u32, len: u32) -> u64;
    }
}

/// Make a request of the kernel through the host, mid-session, claiming `capability`.
/// Synchronous: `host_call` returns when the host has resolved — a pending future inside the
/// guest is a design error, not a wait.
///
/// ★ The capability is a CLAIM: the host holds the call to the capability of the session this
/// guest was invoked under, so claiming more than that gains nothing (ledger #83). Pass the
/// session's own capability (from the `Invoke`), or a narrower one.
///
/// A failure keeps its type: a host that answers `HostResult(Err(text))` (an endpoint error)
/// arrives as [`AbiModuleError::Endpoint`].
#[cfg(target_arch = "wasm32")]
pub fn host_call(
    request: &AbiRequest,
    capability: &AbiCapability,
) -> Result<AbiRepresentation, AbiModuleError> {
    let (ptr, len) = leak_parts(encode_host_call(request, capability));
    // SAFETY: the buffer is live for the duration of the call; the host reads it and returns a
    // result buffer it allocated through our `ikigai_alloc`.
    let packed = unsafe { imported::host_call(ptr, len) };
    unsafe { dealloc(ptr as *mut u8, len as usize) };
    let (rptr, rlen) = ikigai_runtime::session::unpack(packed);
    let answer = unsafe { take_bytes(rptr as *mut u8, rlen as usize) };
    answer_from(&answer)
}

/// What the host's answer to a `host_call` means: the representation, or the failure with its
/// type.
pub fn answer_from(answer: &[u8]) -> Result<AbiRepresentation, AbiModuleError> {
    match AbiModuleCall::decode(answer) {
        Ok(AbiModuleCall::HostResult(Ok(representation))) => Ok(representation),
        Ok(AbiModuleCall::HostResult(Err(message))) => Err(AbiModuleError::Endpoint(message)),
        Ok(AbiModuleCall::HostError(error)) => Err(error),
        Ok(_) => Err(AbiModuleError::Endpoint(
            "host answered a host_call with something other than a result".to_string(),
        )),
        Err(e) => Err(AbiModuleError::Endpoint(e.to_string())),
    }
}

/// `Source` a resource through the host (see [`host_call`]).
#[cfg(target_arch = "wasm32")]
pub fn host_source(
    iri: &str,
    capability: &AbiCapability,
) -> Result<AbiRepresentation, AbiModuleError> {
    host_call(&AbiRequest::new(AbiVerb::Source, iri), capability)
}

/// The request a `Source` of `iri` makes (for a native test of what [`host_call`] would send).
pub fn source_request(iri: &str) -> AbiRequest {
    AbiRequest::new(AbiVerb::Source, iri)
}

/// Declare a core module's ABI exports, wiring `ikigai_invoke` to a dispatch function
/// `fn(AbiModuleCall) -> Reply`.
///
/// ```ignore
/// use ikigai_runtime_guest::abi::{AbiModuleCall, AbiRepresentation};
/// use ikigai_runtime_guest::core_module::Reply;
/// fn dispatch(call: AbiModuleCall) -> Reply {
///     Reply::Resolved(AbiRepresentation::new("text/plain", b"hi".to_vec()))
/// }
/// ikigai_runtime_guest::ikigai_module!(dispatch);
/// ```
#[macro_export]
macro_rules! ikigai_module {
    ($dispatch:path) => {
        #[no_mangle]
        pub extern "C" fn ikigai_alloc(len: u32) -> u32 {
            $crate::core_module::alloc(len as usize) as u32
        }

        #[no_mangle]
        pub extern "C" fn ikigai_free(ptr: u32, len: u32) {
            // SAFETY: the host passes a (ptr, len) this module's `ikigai_alloc` produced.
            unsafe { $crate::core_module::dealloc(ptr as *mut u8, len as usize) }
        }

        #[no_mangle]
        pub extern "C" fn ikigai_invoke(ptr: u32, len: u32) -> u64 {
            // SAFETY: the host wrote the call into a buffer from this module's `ikigai_alloc`.
            unsafe { $crate::core_module::run_invoke(ptr, len, $dispatch) }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::{AbiExpiry, AbiModuleRewrite, AbiReprType, AbiSpaceEntry, AbiVerb as V};

    fn representation() -> AbiRepresentation {
        AbiRepresentation {
            repr_type: AbiReprType {
                media_type: "text/plain".into(),
                params: [("charset".to_string(), "utf-8".to_string())].into(),
            },
            bytes: b"hello, world".to_vec(),
            expiry: AbiExpiry::At(42),
        }
    }

    fn entry() -> AbiSpaceEntry {
        AbiSpaceEntry {
            pattern: "urn:m:hello".into(),
            endpoint: "hello".into(),
            origin: None,
        }
    }

    /// Every variant of [`Reply`] encodes to exactly the bytes the derived `AbiModuleReply`
    /// encoder writes for the same value: the role split changes what a guest links, never
    /// what it sends.
    #[test]
    fn every_reply_encodes_as_the_derived_abi_reply_does() {
        let replies = vec![
            Reply::Resolved(representation()),
            Reply::Error("no such endpoint: urn:m:nope".into()),
            Reply::Bindings(None),
            Reply::Bindings(Some(vec![entry()])),
            Reply::Manifest(AbiModuleManifest {
                entries: Some(vec![entry()]),
                rewrite: AbiModuleRewrite::None,
            }),
            Reply::ErrorTyped(AbiModuleError::InvalidArgument {
                name: "x".into(),
                detail: "bad".into(),
            }),
            Reply::ResolvedThreaded {
                representation: representation(),
                threads: vec!["urn:file:notes.txt".into()],
            },
            Reply::Cards(vec![AbiModuleCard {
                entry: entry(),
                description: r#"{"id":"hello"}"#.into(),
            }]),
        ];
        for reply in replies {
            let ours = encode_reply(&reply);
            let derived = AbiModuleReply::from(reply.clone()).encode().unwrap();
            assert_eq!(ours, derived, "{reply:?}");
        }
    }

    #[test]
    fn a_host_call_encodes_as_the_derived_abi_host_call_does() {
        let request = AbiRequest::new(V::Meta, "urn:host:name");
        let capability = AbiCapability::scoped(["urn:cap:host:read:name"]);
        let ours = encode_host_call(&request, &capability);
        let derived = AbiModuleReply::HostCall {
            request,
            capability,
        }
        .encode()
        .unwrap();
        assert_eq!(ours, derived);
    }

    fn echo(call: AbiModuleCall) -> Reply {
        match call {
            AbiModuleCall::Invoke { request, .. } => Reply::Resolved(AbiRepresentation::new(
                "text/plain",
                request.target.into_bytes(),
            )),
            AbiModuleCall::Describe => Reply::Bindings(None),
            _ => Reply::Error("unexpected".into()),
        }
    }

    #[test]
    fn process_decodes_dispatches_and_encodes() {
        let call = AbiModuleCall::Invoke {
            request: source_request("urn:jail:echo"),
            capability: AbiCapability::root(),
        };
        let out = process(&call.encode().unwrap(), echo);
        let reply = AbiModuleReply::decode(&out).unwrap();
        assert_eq!(
            reply,
            AbiModuleReply::Resolved(AbiRepresentation::new(
                "text/plain",
                b"urn:jail:echo".to_vec()
            ))
        );
    }

    #[test]
    fn process_reports_a_decode_failure_as_an_error_reply() {
        let out = process(b"\xff\xff not postcard", echo);
        assert!(matches!(
            AbiModuleReply::decode(&out).unwrap(),
            AbiModuleReply::Error(_)
        ));
    }

    #[test]
    fn a_host_answer_outside_a_host_call_is_refused() {
        let call = AbiModuleCall::HostResult(Err("stray".into()));
        let out = process(&call.encode().unwrap(), echo);
        assert!(matches!(
            AbiModuleReply::decode(&out).unwrap(),
            AbiModuleReply::Error(_)
        ));
    }

    #[test]
    fn a_typed_host_failure_keeps_its_type() {
        let denied = AbiModuleCall::HostError(AbiModuleError::Denied("urn:host:name".into()));
        assert_eq!(
            answer_from(&denied.encode().unwrap()),
            Err(AbiModuleError::Denied("urn:host:name".into()))
        );
        let endpoint = AbiModuleCall::HostResult(Err("boom".into()));
        assert_eq!(
            answer_from(&endpoint.encode().unwrap()),
            Err(AbiModuleError::Endpoint("boom".into()))
        );
    }

    #[test]
    fn alloc_then_take_round_trips_bytes() {
        let len = 5usize;
        let ptr = alloc(len);
        // SAFETY: ptr/len name the buffer we just allocated.
        unsafe {
            core::ptr::copy_nonoverlapping(b"hello".as_ptr(), ptr, len);
            assert_eq!(take_bytes(ptr, len), b"hello");
        }
    }
}
