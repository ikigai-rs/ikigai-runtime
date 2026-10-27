//! The ikigai-module **ABI v1** session (R-D6, "J2 absorbed"): the postcard payload that crosses
//! the `ikigai_invoke` / `host_call` boundary, and the `u64` packing the ABI returns.
//!
//! ABI v1, pinned here and by the doctest below:
//! - the guest exports [`EXPORT_ALLOC`]`(len) -> u32`, [`EXPORT_FREE`]`(ptr, len)` and
//!   [`EXPORT_INVOKE`]`(ptr, len) -> u64`, and its linear memory as [`EXPORT_MEMORY`];
//! - `ikigai_invoke` and `host_call` return one packed `u64` of `ptr << 32 | len` (see [`pack`]
//!   and [`unpack`]): one value, no multi-value return and no out-pointer, so a Java, Python or
//!   JavaScript host reads it with no helper;
//! - the one import is [`IMPORT_MODULE`] [`IMPORT_HOST_CALL`]`(ptr, len) -> u64`;
//! - the payload is **ikigai-module's session**, byte for byte: a postcard-encoded `ModuleCall`
//!   (host to guest) or `ModuleReply` (guest to host). A guest builds it from the wire-only
//!   mirrors in [`ikigai_module_abi`] (re-exported as [`abi`]), which carry no ikigai-core
//!   types; a host keeps ikigai-module's typed session and converts at the boundary (R-D6 as
//!   amended 2026-10-27, option (b)). Variants are only ever appended, so indexes stay stable.
//!
//! **Buffer ownership:** the host writes a call into guest memory it obtained with
//! `ikigai_alloc`, and frees it with `ikigai_free`; a buffer the guest returns (from
//! `ikigai_invoke` or a `host_call` reply the host allocated) is the host's to read and free. The
//! guest drives the session synchronously: `host_call` returns when the host has resolved, so a
//! pending future inside the guest is a design error, not a wait.
//!
//! ★ **A guest's capability is a claim, never authority.** A `HostCall` carries the capability the
//! guest writes into it; the host maps every host call to the capability of the session the guest
//! was invoked under (ledger #83), so a guest can narrow its authority and never widen it.
//!
//! ```
//! use ikigai_runtime::session::{abi, pack, unpack};
//!
//! // The packing the ABI returns: ptr in the high 32 bits, len in the low 32.
//! let packed = pack(0x1234, 0x5678);
//! assert_eq!(packed, 0x0000_1234_0000_5678);
//! assert_eq!(unpack(packed), (0x1234, 0x5678));
//!
//! // One Invoke, as the session carries it (ikigai-module's bytes exactly).
//! let call = abi::AbiModuleCall::Invoke {
//!     request: abi::AbiRequest::new(abi::AbiVerb::Source, "urn:jail:hello"),
//!     capability: abi::AbiCapability::root(),
//! };
//! let bytes = call.encode().unwrap();
//! assert_eq!(bytes[0], 0, "Invoke is variant 0");
//! assert_eq!(abi::AbiModuleCall::decode(&bytes).unwrap(), call);
//! ```

/// The wire-only session types, re-exported so a host or guest names one definition.
pub use ikigai_module_abi as abi;
pub use ikigai_module_abi::{
    AbiCapability, AbiModuleCall, AbiModuleError, AbiModuleReply, AbiRepresentation, AbiRequest,
    AbiVerb,
};

/// The guest export the host allocates call buffers through: `ikigai_alloc(len) -> u32`.
pub const EXPORT_ALLOC: &str = "ikigai_alloc";
/// The guest export the host frees buffers through: `ikigai_free(ptr, len)`.
pub const EXPORT_FREE: &str = "ikigai_free";
/// The guest export that runs one session: `ikigai_invoke(ptr, len) -> u64` (packed).
pub const EXPORT_INVOKE: &str = "ikigai_invoke";
/// The guest's exported linear memory.
pub const EXPORT_MEMORY: &str = "memory";
/// The import module of the ABI's one import.
pub const IMPORT_MODULE: &str = "ikigai";
/// The ABI's one import: `"ikigai" "host_call"(ptr, len) -> u64` (packed).
pub const IMPORT_HOST_CALL: &str = "host_call";

/// Pack a pointer and length into the ABI's single `u64` return (`ptr << 32 | len`).
#[inline]
pub fn pack(ptr: u32, len: u32) -> u64 {
    (u64::from(ptr) << 32) | u64::from(len)
}

/// Unpack the ABI's `u64` return into `(ptr, len)`.
#[inline]
pub fn unpack(packed: u64) -> (u32, u32) {
    ((packed >> 32) as u32, (packed & 0xFFFF_FFFF) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_unpack_round_trips() {
        for (ptr, len) in [
            (0u32, 0u32),
            (1, 2),
            (0xFFFF_FFFF, 0),
            (0, 0xFFFF_FFFF),
            (0xDEAD_BEEF, 0x0BAD_F00D),
        ] {
            assert_eq!(unpack(pack(ptr, len)), (ptr, len));
        }
    }
}
