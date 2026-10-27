//! The space adapter: a loaded module presented as resolvable endpoints.
//!
//! "The runtime mints; core arranges" (R-D7's second design point): the runtime loads a module,
//! projects its exports and would register them in core's `Registry`, after which a core
//! declaration arranges them — which respects core's rule that a declaration "arranges; it never
//! mints", with no core change. [`LoadedModule`] is that seam's description: the mount name, the
//! calling convention detected from its exports, and the projected endpoints. The engine crate
//! does the minting: `ikigai-runtime-wasmtime`'s `ComponentSpace` (components) and its
//! `JailTransport` under ikigai-module's `ModuleSpace` (core modules) are core `Space`s.

use crate::projection::ProjectedEndpoint;

/// The calling convention a core module was loaded under (R-D6), chosen from its exports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Convention {
    /// A numeric-signature export, invoked directly.
    Numeric,
    /// The ikigai-module ABI v1 (`ikigai_invoke` + `host_call`, a postcard session).
    Session,
    /// A WASI p1 command module (stdin → stdout).
    Command,
}

/// A loaded module as the kernel will see it: a mount name, the convention it runs under, and the
/// endpoints its exports project to (R-D4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedModule {
    pub app: String,
    pub convention: Convention,
    pub endpoints: Vec<ProjectedEndpoint>,
}

impl LoadedModule {
    pub fn new(
        app: impl Into<String>,
        convention: Convention,
        endpoints: Vec<ProjectedEndpoint>,
    ) -> Self {
        LoadedModule {
            app: app.into(),
            convention,
            endpoints,
        }
    }

    /// The projected URNs this module answers under its own `urn:runtime:app:{app}:` prefix. A
    /// loaded module can never answer for `urn:runtime:artifact:*`, `urn:runtime:bundle:*`,
    /// `urn:kernel:*` or `urn:cap:*`: a mounted space answers only names under its own prefix,
    /// so core's `Kernel::check_sealing` passes for any arrangement that seals those
    /// (`ikigai-runtime-wasmtime`'s `component_wat` test checks it).
    pub fn urns(&self) -> Vec<String> {
        self.endpoints.iter().map(ProjectedEndpoint::urn).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_loaded_module_lists_its_projected_urns_under_its_own_prefix() {
        let m = LoadedModule::new(
            "probe",
            Convention::Session,
            vec![
                ProjectedEndpoint::root("probe", "run"),
                ProjectedEndpoint::root("probe", "report"),
            ],
        );
        assert_eq!(
            m.urns(),
            vec!["urn:runtime:app:probe:run", "urn:runtime:app:probe:report"]
        );
        assert!(m
            .urns()
            .iter()
            .all(|u| u.starts_with("urn:runtime:app:probe:")));
    }
}
