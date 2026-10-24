//! The ikigai-module **ABI v1** session (R-D6, "J2 absorbed"): the postcard payload that crosses
//! the `ikigai_invoke` / `host_call` boundary, and the `u64` packing the ABI returns.
//!
//! ABI v1, pinned here and by the doctest below:
//! - the guest exports `ikigai_alloc(len) -> u32`, `ikigai_free(ptr, len)` and
//!   `ikigai_invoke(ptr, len) -> u64`;
//! - `ikigai_invoke` and `host_call` return one packed `u64` of `ptr << 32 | len` (see [`pack`]
//!   and [`unpack`]) — one value, no multi-value return and no out-pointer, so a Java, Python or
//!   JavaScript host reads it with no helper;
//! - the one import is `"ikigai" "host_call"(ptr, len) -> u64`;
//! - the payload is a postcard-encoded [`ModuleCall`] (host → guest) or [`ModuleReply`]
//!   (guest → host). Variants are only ever appended, so indexes stay stable.
//!
//! **Buffer ownership:** the host writes a call into guest memory it obtained with
//! `ikigai_alloc`, and frees it with `ikigai_free`; a buffer the guest returns (from
//! `ikigai_invoke` or a `host_call` reply the host allocated) is the host's to read and free. The
//! guest drives the session synchronously: `host_call` returns when the host has resolved, so a
//! pending future inside the guest is a design error, not a wait.
//!
//! This mirrors `ikigai-module` 0.4.0's `ModuleCall`/`ModuleReply` shape. Whether the runtime
//! owns these types or shares `ikigai-module`'s (and whether they are byte-compatible across the
//! migration) is a decision for the hub (R-D20, item 1); R3 keeps them here so host, guest SDK
//! and fixtures share one definition and the tier is self-contained.
//!
//! ```
//! use ikigai_runtime::session::{pack, unpack, ModuleCall};
//! use ikigai_runtime::Capability;
//!
//! // The packing the ABI returns: ptr in the high 32 bits, len in the low 32.
//! let packed = pack(0x1234, 0x5678);
//! assert_eq!(packed, 0x0000_1234_0000_5678);
//! assert_eq!(unpack(packed), (0x1234, 0x5678));
//!
//! // One Invoke round-trips through the same postcard the host and guest use.
//! let call = ModuleCall::Invoke {
//!     endpoint: "urn:jail:hello".to_string(),
//!     body: b"hi".to_vec(),
//!     capability: Capability::root(),
//! };
//! let bytes = postcard::to_allocvec(&call).unwrap();
//! let back: ModuleCall = postcard::from_bytes(&bytes).unwrap();
//! assert_eq!(call, back);
//! ```

use ikigai_core::Capability;
use serde::{Deserialize, Serialize};

/// Host → guest: what the host asks the module to do, or the answer to a `host_call`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ModuleCall {
    /// Invoke an endpoint the module serves, with a by-value body, under `capability`.
    Invoke {
        endpoint: String,
        body: Vec<u8>,
        capability: Capability,
    },
    /// The host's answer to a [`ModuleReply::HostCall`] — the resolved bytes, or an error message
    /// (the typed error is dropped at this boundary, a known limitation, PENDING §1).
    HostResult(Result<Vec<u8>, String>),
    /// Ask the module to describe its endpoints (the contract JSON).
    Describe,
}

/// Guest → host: the module's reply, or a request back to the kernel mid-session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ModuleReply {
    /// The invocation resolved to these bytes.
    Resolved(Vec<u8>),
    /// The module is asking the host to resolve a Source request, carrying the capability it
    /// claims; the host clamps that to the session's (never widening) before resolving.
    HostCall { iri: String, capability: Capability },
    /// The module's `describe()` JSON, answering a [`ModuleCall::Describe`].
    Describe(String),
    /// The module failed. The string is the message; the type is not carried here.
    Error(String),
}

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

    #[test]
    fn reply_variants_round_trip_through_postcard() {
        let replies = [
            ModuleReply::Resolved(b"ok".to_vec()),
            ModuleReply::HostCall {
                iri: "urn:host:name".into(),
                capability: Capability::root(),
            },
            ModuleReply::Describe("{}".into()),
            ModuleReply::Error("boom".into()),
        ];
        for r in replies {
            let bytes = postcard::to_allocvec(&r).unwrap();
            let back: ModuleReply = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(r, back);
        }
    }
}
