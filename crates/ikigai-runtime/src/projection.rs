//! How a module's exports project to ikigai names and verbs (R-D4).
//!
//! Each export becomes a derived resource under `urn:runtime:app:{app}:{interface}:{func}`
//! (open question 3 = (a)) — **not** `urn:wasm:…`. A function at the root of a world (outside any
//! interface) is `urn:runtime:app:{app}:{func}`. Every export projects as a **Source** by
//! default (a call as a read); a function that mutates is declared a [`Verb::Sink`] in the bundle
//! or declaration, and as a Sink must declare `content` (the pipeline rule conformance enforces).
//!
//! The full WIT value → XSD / JSON type map (R-D4's table) belongs to the component tier (R3b);
//! R3 establishes the naming and verb only, which the core-module tier already uses (a numeric
//! export projects under the same scheme).

/// The default verb of a projected export (R-D4, open question 5 = (a)).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Verb {
    /// A read. The default for every export: WIT has no effect annotation, so purity is never
    /// guessed.
    #[default]
    Source,
    /// A write, declared in the bundle or declaration. Must declare `content`.
    Sink,
}

/// A projected endpoint's identity: the mount name, an optional interface, and the function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectedEndpoint {
    /// The mount name the host or declaration chose.
    pub app: String,
    /// The bare WIT interface name (`handler`, not `wasi:http/handler@0.2.0`), or `None` for a
    /// function exported at the root of the world.
    pub interface: Option<String>,
    /// The function name.
    pub func: String,
    pub verb: Verb,
}

impl ProjectedEndpoint {
    /// A root-of-world Source export: `urn:runtime:app:{app}:{func}`.
    pub fn root(app: impl Into<String>, func: impl Into<String>) -> Self {
        ProjectedEndpoint {
            app: app.into(),
            interface: None,
            func: func.into(),
            verb: Verb::Source,
        }
    }

    /// An interface export: `urn:runtime:app:{app}:{interface}:{func}`.
    pub fn in_interface(
        app: impl Into<String>,
        interface: impl Into<String>,
        func: impl Into<String>,
    ) -> Self {
        ProjectedEndpoint {
            app: app.into(),
            interface: Some(interface.into()),
            func: func.into(),
            verb: Verb::Source,
        }
    }

    /// Declare this export a Sink (it mutates).
    pub fn as_sink(mut self) -> Self {
        self.verb = Verb::Sink;
        self
    }

    /// The projected URN. WIT identifiers are kebab-case ASCII and already legal path segments;
    /// the host still runs each segment through core's `encode_name` before resolving (not done
    /// in this pure identity).
    pub fn urn(&self) -> String {
        match &self.interface {
            Some(interface) => format!("urn:runtime:app:{}:{}:{}", self.app, interface, self.func),
            None => format!("urn:runtime:app:{}:{}", self.app, self.func),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_root_export_projects_without_an_interface_segment() {
        let e = ProjectedEndpoint::root("probe", "run");
        assert_eq!(e.urn(), "urn:runtime:app:probe:run");
        assert_eq!(e.verb, Verb::Source);
    }

    #[test]
    fn an_interface_export_carries_the_bare_interface_name() {
        let e = ProjectedEndpoint::in_interface("geo", "lookup", "find");
        assert_eq!(e.urn(), "urn:runtime:app:geo:lookup:find");
    }

    #[test]
    fn a_sink_is_declared_never_guessed() {
        let e = ProjectedEndpoint::root("notes", "save").as_sink();
        assert_eq!(e.verb, Verb::Sink);
    }
}
