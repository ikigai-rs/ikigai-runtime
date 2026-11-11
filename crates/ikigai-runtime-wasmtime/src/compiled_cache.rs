//! The host's own on-disk cache of compiled forms (R-D7), so the first call after a restart
//! deserializes instead of compiling.
//!
//! An entry is `{dir}/{engine}/{digest}.cwasm`, keyed by [`CompiledKey`]: the artifact digest and
//! the engine-compatibility hash. Deserializing a compiled form runs native code, so R-D7's rule
//! holds here:
//!
//! - an entry is written only by this cache, after this host compiled the artifact itself;
//! - it is loaded only if its header names the identical digest and compatibility hash, and its
//!   body hashes to the blake3 digest the header records, so a truncated, altered or renamed file
//!   is never handed to `deserialize`;
//! - anything else (a corrupt entry, a mismatched one, one wasmtime itself refuses) recompiles
//!   from the artifact, overwrites the entry, and says so in the [`CacheOutcome`];
//! - there is no way to load a `.cwasm` from anywhere but this directory: no API takes one, so a
//!   compiled form is never signed, shipped or imported.
//!
//! A write that fails costs only the next restart's compile, so it is reported, never an error.

use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ikigai_runtime::loader::CompiledKey;

/// The first bytes of every entry: this cache's own format, version 1.
const MAGIC: &[u8; 16] = b"ikigai-cwasm-v1\n";

/// A directory the host owns, holding the compiled forms it produced.
#[derive(Clone, Debug)]
pub struct CompiledCache {
    dir: PathBuf,
}

/// Where a compiled form came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The engine's in-memory cache (the component tier only).
    Memory,
    /// This host's on-disk cache: deserialized, not compiled.
    Disk,
    /// Compiled from the artifact.
    Compiled,
}

/// What a compile did, and anything about the on-disk cache worth telling the host: an entry that
/// was rejected and recompiled, or one that could not be written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheOutcome {
    pub origin: Origin,
    /// One plain sentence per event, for example
    /// `the cached compiled form at /var/cache/x.cwasm is corrupt (its body does not match its
    /// hash); compiled again from the artifact`.
    pub notes: Vec<String>,
}

impl CacheOutcome {
    pub(crate) fn new(origin: Origin) -> Self {
        CacheOutcome {
            origin,
            notes: Vec::new(),
        }
    }
}

impl fmt::Display for CacheOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.origin {
            Origin::Memory => "taken from memory",
            Origin::Disk => "loaded from the compiled-form cache",
            Origin::Compiled => "compiled from the artifact",
        })?;
        for note in &self.notes {
            write!(f, "; {note}")?;
        }
        Ok(())
    }
}

/// Why an entry on disk was not used.
enum Reject {
    Missing,
    Unusable(String),
}

impl CompiledCache {
    /// A cache in `dir`, which the host owns. Created on first write (owner-only on Unix).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        CompiledCache { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file an entry for `key` lives in. Both parts are reduced to `[A-Za-z0-9-]`, so a key
    /// can never name a path outside the cache.
    pub fn path_of(&self, key: &CompiledKey) -> PathBuf {
        self.dir
            .join(file_safe(key.engine.as_str()))
            .join(format!("{}.cwasm", file_safe(key.digest.as_str())))
    }

    /// Load the compiled form for `key` with `deserialize`, or compile it with `compile` and write
    /// it. `deserialize` is handed only the body of an entry whose header and hash check out.
    pub(crate) fn load_or_compile<T, E: fmt::Display>(
        &self,
        key: &CompiledKey,
        deserialize: impl FnOnce(&[u8]) -> Result<T, E>,
        compile: impl FnOnce() -> Result<T, ikigai_runtime::EngineError>,
        serialize: impl FnOnce(&T) -> Result<Vec<u8>, E>,
    ) -> Result<(T, CacheOutcome), ikigai_runtime::EngineError> {
        let path = self.path_of(key);
        let mut outcome = CacheOutcome::new(Origin::Compiled);
        match read_entry(&path, key) {
            Ok(body) => match deserialize(&body) {
                Ok(compiled) => return Ok((compiled, CacheOutcome::new(Origin::Disk))),
                Err(e) => outcome.notes.push(format!(
                    "the cached compiled form at {} was refused by the engine ({e}); \
                     compiled again from the artifact",
                    path.display()
                )),
            },
            Err(Reject::Missing) => {}
            Err(Reject::Unusable(why)) => outcome.notes.push(format!(
                "the cached compiled form at {} {why}; compiled again from the artifact",
                path.display()
            )),
        }
        let compiled = compile()?;
        let written = serialize(&compiled)
            .map_err(|e| e.to_string())
            .and_then(|body| write_entry(&path, key, &body).map_err(|e| e.to_string()));
        if let Err(e) = written {
            outcome.notes.push(format!(
                "the compiled form could not be written to {} ({e}); the next restart compiles again",
                path.display()
            ));
        }
        Ok((compiled, outcome))
    }
}

fn file_safe(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn header_of(key: &CompiledKey) -> String {
    format!("{}\n{}\n", key.digest.as_str(), key.engine.as_str())
}

fn read_entry(path: &Path, key: &CompiledKey) -> Result<Vec<u8>, Reject> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Reject::Missing),
        Err(e) => return Err(Reject::Unusable(format!("could not be read ({e})"))),
    };
    let corrupt = |why: &str| Reject::Unusable(format!("is corrupt ({why})"));
    let rest = bytes
        .strip_prefix(MAGIC.as_slice())
        .ok_or_else(|| corrupt("it does not start with this cache's format marker"))?;
    if rest.len() < 4 {
        return Err(corrupt("it ends inside its header"));
    }
    let (len, rest) = rest.split_at(4);
    let len = u32::from_le_bytes(len.try_into().expect("four bytes")) as usize;
    if rest.len() < len + 32 {
        return Err(corrupt("it ends inside its header"));
    }
    let (header, rest) = rest.split_at(len);
    let (hash, body) = rest.split_at(32);
    if header != header_of(key).as_bytes() {
        return Err(Reject::Unusable(format!(
            "was written for another artifact or engine (its header names {:?}, not {:?})",
            String::from_utf8_lossy(header).trim_end(),
            header_of(key).trim_end()
        )));
    }
    if blake3::hash(body).as_bytes() != hash {
        return Err(corrupt("its body does not match its hash"));
    }
    Ok(body.to_vec())
}

fn write_entry(path: &Path, key: &CompiledKey, body: &[u8]) -> std::io::Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().expect("an entry has a directory");
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;
    let header = header_of(key);
    let tmp = dir.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .expect("an entry has a name")
            .to_string_lossy(),
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(MAGIC)?;
        file.write_all(&(header.len() as u32).to_le_bytes())?;
        file.write_all(header.as_bytes())?;
        file.write_all(blake3::hash(body).as_bytes())?;
        file.write_all(body)?;
        file.sync_all()?;
        // Renamed into place, so a reader sees a whole entry or none.
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}
