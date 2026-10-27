//! Import binding by configuration (R-D5): every import a component makes, other than the
//! runtime's own `kernel` and the WASI the grants decide, is bound to a kernel request, one line
//! per function:
//!
//! ```text
//! acme:geo/lookup.find  ->  source urn:geo:find  (parameters by name)
//! ```
//!
//! Each bound call is issued through the invocation's context under the caller's clamped
//! capability, so it is traced, golden-threaded and denied exactly as any other request. **An
//! import with no binding refuses the mount** with `Unresolved`, naming it: it is never stubbed
//! with a default.

use std::collections::BTreeMap;
use std::fmt;

use ikigai_core::{encode_name, Error, Iri, Verb};

/// One bound import function: the verb and the IRI its calls are issued to. The function's
/// parameters become the request's named arguments, by their WIT names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportBinding {
    /// The verb the call is issued with.
    pub verb: Verb,
    /// The resource the call is issued to.
    pub target: Iri,
}

/// The import bindings of one mount: `interface.function` (the interface without its
/// `@version`) to the kernel request it is issued as.
#[derive(Clone, Debug, Default)]
pub struct ImportBindings {
    bound: BTreeMap<(String, String), ImportBinding>,
}

impl ImportBindings {
    /// No bindings: every import other than `kernel` and WASI refuses the mount.
    pub fn new() -> Self {
        ImportBindings::default()
    }

    /// Bind `import` (`<namespace>:<package>/<interface>.<function>`, the version left out) to a
    /// `verb` on `target`. Refuses a malformed import name or target, and a second binding for
    /// the same function (a binding names one request; nothing is silently replaced).
    ///
    /// ```
    /// use ikigai_core::Verb;
    /// use ikigai_runtime_wasmtime::component::ImportBindings;
    ///
    /// let bindings = ImportBindings::new()
    ///     .bind("acme:geo/lookup.find", Verb::Source, "urn:geo:find")
    ///     .unwrap();
    /// assert!(bindings.get("acme:geo/lookup@1.0.0", "find").is_some());
    /// assert!(ImportBindings::new().bind("no-function-here", Verb::Source, "urn:x").is_err());
    /// ```
    pub fn bind(mut self, import: &str, verb: Verb, target: &str) -> Result<Self, Error> {
        let (interface, function) = import.rsplit_once('.').ok_or_else(|| {
            invalid(format!(
                "`{import}` names no function: write `<interface>.<function>`"
            ))
        })?;
        if interface.is_empty() || function.is_empty() || interface.contains('@') {
            return Err(invalid(format!(
                "`{import}`: write `<namespace>:<package>/<interface>.<function>`, without a version"
            )));
        }
        let target = Iri::parse(target)
            .map_err(|e| invalid(format!("`{import}` -> `{target}`: not an IRI: {e}")))?;
        let key = (interface.to_string(), function.to_string());
        if self.bound.contains_key(&key) {
            return Err(Error::Conflict(format!(
                "`{import}` is bound twice; a binding names one request"
            )));
        }
        self.bound.insert(key, ImportBinding { verb, target });
        Ok(self)
    }

    /// The binding for `function` of the import instance named `instance` (its version, if any,
    /// is ignored: a binding is to the interface, not to one release of it).
    pub fn get(&self, instance: &str, function: &str) -> Option<&ImportBinding> {
        let interface = unversioned(instance);
        self.bound
            .get(&(interface.to_string(), function.to_string()))
    }

    /// Whether anything binds a function of this import instance.
    pub(crate) fn binds_instance(&self, instance: &str) -> bool {
        let interface = unversioned(instance);
        self.bound.keys().any(|(i, _)| i == interface)
    }
}

impl fmt::Display for ImportBindings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for ((interface, function), binding) in &self.bound {
            writeln!(
                f,
                "{interface}.{function}  ->  {} {}",
                verb_word(binding.verb),
                binding.target.as_str()
            )?;
        }
        Ok(())
    }
}

fn verb_word(verb: Verb) -> &'static str {
    match verb {
        Verb::Source => "source",
        Verb::Sink => "sink",
        Verb::Exists => "exists",
        Verb::Delete => "delete",
        Verb::Meta => "meta",
    }
}

/// An import instance's name without its `@version`.
pub(crate) fn unversioned(instance: &str) -> &str {
    instance.split_once('@').map_or(instance, |(name, _)| name)
}

fn invalid(detail: String) -> Error {
    Error::InvalidArgument {
        name: "import".to_string(),
        detail,
    }
}

/// The `Unresolved` an unbound import refuses with: the import's own name when it is an IRI
/// (`acme:geo/lookup@1.0.0` is one), else a runtime name for it.
pub(crate) fn unresolved(import: &str) -> Error {
    match Iri::parse(import) {
        Ok(iri) => Error::Unresolved(iri),
        Err(_) => Error::Unresolved(
            Iri::parse(format!("urn:runtime:import:{}", encode_name(import)))
                .unwrap_or_else(|_| Iri::parse("urn:runtime:import").expect("a valid IRI")),
        ),
    }
}
