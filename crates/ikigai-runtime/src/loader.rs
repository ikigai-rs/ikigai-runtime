//! Loading: an artifact is a content-addressed **atom**, and compiled forms are cached by the
//! artifact digest and an engine-compatibility hash (R-D7).
//!
//! `urn:runtime:artifact:{digest}` is write-once: a Sink whose bytes do not hash to `{digest}` is
//! InvalidArgument. Being content-addressed, its answer is cacheable forever. A compiled form
//! (`.cwasm`) is keyed by [`CompiledKey`] and **deserialized only from the host's own cache** —
//! deserializing a precompiled file is `unsafe` because it is native code, so the runtime never
//! deserializes one it did not produce (R-D7). The in-memory compiled form is a host cache keyed
//! by the artifact digest, which can never go stale because the key is a content digest.

use std::fmt;

/// A tagged content digest (the "tag, don't multihash" decision), e.g. `blake3:<hex>`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArtifactDigest(String);

impl ArtifactDigest {
    /// The tagged digest of some bytes. R3 uses blake3 (already in the graph through ikigai-core).
    pub fn of(bytes: &[u8]) -> Self {
        ArtifactDigest(format!("blake3:{}", blake3::hash(bytes).to_hex()))
    }

    /// Wrap an already-formed tagged digest string (for example read from a bundle).
    pub fn from_tagged(tagged: impl Into<String>) -> Self {
        ArtifactDigest(tagged.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ArtifactDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An engine's precompile-compatibility hash (wasmtime exposes it as
/// `Engine::precompile_compatibility_hash`). A compiled form is reused only under an identical
/// one; otherwise the host recompiles from the signed artifact.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EngineCompatHash(String);

impl EngineCompatHash {
    pub fn new(hash: impl Into<String>) -> Self {
        EngineCompatHash(hash.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The cache key for a compiled form: the artifact digest and the engine-compatibility hash.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CompiledKey {
    pub digest: ArtifactDigest,
    pub engine: EngineCompatHash,
}

impl CompiledKey {
    pub fn new(digest: ArtifactDigest, engine: EngineCompatHash) -> Self {
        CompiledKey { digest, engine }
    }
}

/// A content-addressed artifact: bytes that hash to their name. Write-once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artifact {
    digest: ArtifactDigest,
    bytes: Vec<u8>,
}

/// Why a load or Sink was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum LoadError {
    /// The bytes do not hash to the claimed digest (an InvalidArgument at the kernel seam).
    DigestMismatch {
        claimed: ArtifactDigest,
        actual: ArtifactDigest,
    },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::DigestMismatch { claimed, actual } => {
                write!(
                    f,
                    "artifact bytes hash to {actual}, not the claimed {claimed}"
                )
            }
        }
    }
}

impl std::error::Error for LoadError {}

impl Artifact {
    /// Create an artifact from bytes, naming it by their digest.
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        let bytes = bytes.into();
        let digest = ArtifactDigest::of(&bytes);
        Artifact { digest, bytes }
    }

    /// Store bytes under a claimed digest (the write-once Sink). Refuses when they do not hash to
    /// it (R-D7).
    pub fn sink(claimed: ArtifactDigest, bytes: impl Into<Vec<u8>>) -> Result<Self, LoadError> {
        let bytes = bytes.into();
        let actual = ArtifactDigest::of(&bytes);
        if actual != claimed {
            return Err(LoadError::DigestMismatch { claimed, actual });
        }
        Ok(Artifact {
            digest: claimed,
            bytes,
        })
    }

    pub fn digest(&self) -> &ArtifactDigest {
        &self.digest
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The compiled-form cache key for this artifact under a given engine.
    pub fn compiled_key(&self, engine: &EngineCompatHash) -> CompiledKey {
        CompiledKey::new(self.digest.clone(), engine.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_artifact_is_named_by_its_digest() {
        let a = Artifact::from_bytes(b"\0asm\x01\0\0\0".to_vec());
        assert!(a.digest().as_str().starts_with("blake3:"));
        assert_eq!(a.digest(), &ArtifactDigest::of(a.bytes()));
    }

    #[test]
    fn a_sink_refuses_bytes_that_do_not_hash_to_the_claimed_digest() {
        let good = ArtifactDigest::of(b"the real bytes");
        assert!(Artifact::sink(good.clone(), b"the real bytes".to_vec()).is_ok());
        let err = Artifact::sink(good, b"other bytes".to_vec()).unwrap_err();
        assert!(matches!(err, LoadError::DigestMismatch { .. }));
    }

    #[test]
    fn the_compiled_key_combines_digest_and_engine() {
        let a = Artifact::from_bytes(b"m".to_vec());
        let k = a.compiled_key(&EngineCompatHash::new("wasmtime-50-aarch64"));
        assert_eq!(k.digest, *a.digest());
        assert_eq!(k.engine.as_str(), "wasmtime-50-aarch64");
    }
}
