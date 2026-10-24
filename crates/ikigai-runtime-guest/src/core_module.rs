//! The core-module (ikigai-module ABI v1) guest ABI (R-D6, "J2 absorbed").
//!
//! A guest declares its exports with [`ikigai_module!`](crate::ikigai_module), passing a dispatch
//! function. The SDK
//! handles the `u64` packing, the guest allocator the host writes through, and the synchronous
//! `host_call` pump. The payload is the postcard [`ModuleCall`]/[`ModuleReply`] session defined in
//! `ikigai-runtime` (so host, guest and fixtures share one definition).

use ikigai_runtime::session::{pack, ModuleCall, ModuleReply};

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

/// Decode a `ModuleCall`, dispatch it, and encode the `ModuleReply`. Pure and testable without a
/// wasm target — [`run_invoke`] is only the pointer glue over this.
pub fn process(call_bytes: &[u8], dispatch: fn(ModuleCall) -> ModuleReply) -> Vec<u8> {
    let reply = match postcard::from_bytes::<ModuleCall>(call_bytes) {
        Ok(call) => dispatch(call),
        Err(e) => ModuleReply::Error(format!("decode: {e}")),
    };
    postcard::to_allocvec(&reply).unwrap_or_default()
}

/// The body of the `ikigai_invoke` export: read the call the host wrote, dispatch it, and hand
/// the reply back as the packed `u64`.
///
/// # Safety
/// `ptr`/`len` must name a buffer the host wrote with [`alloc`].
pub unsafe fn run_invoke(ptr: u32, len: u32, dispatch: fn(ModuleCall) -> ModuleReply) -> u64 {
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

/// Source a resource through the host, mid-session, under `capability` (the host clamps it to the
/// session's). Synchronous: `host_call` returns when the host has resolved — a pending future
/// inside the guest is a design error, not a wait.
#[cfg(target_arch = "wasm32")]
pub fn host_source(iri: &str, capability: &ikigai_runtime::Capability) -> Result<Vec<u8>, String> {
    let reply = ModuleReply::HostCall {
        iri: iri.to_string(),
        capability: capability.clone(),
    };
    let request = postcard::to_allocvec(&reply).map_err(|e| e.to_string())?;
    let (ptr, len) = leak_parts(request);
    // SAFETY: the buffer is live for the duration of the call; the host reads it and returns a
    // result buffer it allocated through our `ikigai_alloc`.
    let packed = unsafe { imported::host_call(ptr, len) };
    unsafe { dealloc(ptr as *mut u8, len as usize) };
    let (rptr, rlen) = ikigai_runtime::session::unpack(packed);
    let result = unsafe { take_bytes(rptr as *mut u8, rlen as usize) };
    match postcard::from_bytes::<ModuleCall>(&result).map_err(|e| e.to_string())? {
        ModuleCall::HostResult(r) => r,
        other => Err(format!("host returned {other:?}, expected HostResult")),
    }
}

/// Declare a core module's ABI exports, wiring `ikigai_invoke` to a dispatch function
/// `fn(ModuleCall) -> ModuleReply`.
///
/// ```ignore
/// use ikigai_runtime::session::{ModuleCall, ModuleReply};
/// fn dispatch(call: ModuleCall) -> ModuleReply { ModuleReply::Resolved(b"hi".to_vec()) }
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

    fn echo(call: ModuleCall) -> ModuleReply {
        match call {
            ModuleCall::Invoke { body, .. } => ModuleReply::Resolved(body),
            ModuleCall::Describe => ModuleReply::Describe("{}".into()),
            ModuleCall::HostResult(_) => ModuleReply::Error("unexpected HostResult".into()),
        }
    }

    #[test]
    fn process_decodes_dispatches_and_encodes() {
        let call = ModuleCall::Invoke {
            endpoint: "urn:jail:echo".into(),
            body: b"payload".to_vec(),
            capability: ikigai_runtime::Capability::root(),
        };
        let encoded = postcard::to_allocvec(&call).unwrap();
        let out = process(&encoded, echo);
        let reply: ModuleReply = postcard::from_bytes(&out).unwrap();
        assert_eq!(reply, ModuleReply::Resolved(b"payload".to_vec()));
    }

    #[test]
    fn process_reports_a_decode_failure_as_an_error_reply() {
        let out = process(b"\xff\xff not postcard", echo);
        let reply: ModuleReply = postcard::from_bytes(&out).unwrap();
        assert!(matches!(reply, ModuleReply::Error(_)));
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
