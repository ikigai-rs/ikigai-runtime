//! Projection of component exports (R-D4): each exported function becomes an endpoint under
//! `urn:runtime:app:{app}:{interface}:{func}` (or `urn:runtime:app:{app}:{func}` at the root of
//! the world), a Source unless declared a Sink, with ArgSpecs from its parameters and its
//! summary from the WIT docs the binary carries. Read from wasmtime's dynamic types: no code is
//! written per component.

use std::collections::{BTreeMap, BTreeSet};

use ikigai_core::{encode_name, ArgSpec, Description, Error, Verb as CoreVerb};
use ikigai_runtime::{ProjectedEndpoint, Verb};
use wasmtime::component::types::{ComponentFunc, ComponentItem, Type};
use wasmtime::component::{Component, ComponentExportIndex};
use wasmtime::Engine;

use super::values;

/// The exported interface a guest serves its OWN endpoints through; it is mounted by its
/// `describe()`, not projected function by function.
pub(crate) const ENDPOINT_API: &str = "ikigai:runtime/endpoint-api";

/// One projected export.
pub(crate) struct ProjectedFunc {
    pub(crate) endpoint: ProjectedEndpoint,
    pub(crate) index: ComponentExportIndex,
    /// `(argument name, parameter type)` in the function's parameter order. A Sink's one
    /// parameter is read from `content` (the pipeline rule), whatever its WIT name.
    pub(crate) params: Vec<(String, Type)>,
    pub(crate) result: Option<Type>,
    pub(crate) description: Description,
}

/// An export the projection left out, and why (the load report, R-D4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    /// The export's full name (`acme:calc/calc#open`, or a root function's name).
    pub export: String,
    /// Why it was not projected.
    pub reason: String,
}

/// The docs a component's embedded WIT carries, by `(bare interface or None, function)`: the
/// `package-docs` custom section wit-parser defines, read with wit-parser's own decoder (R-D14),
/// so the summary is whatever the guest's toolchain kept. Empty when it kept none.
pub(crate) fn docs_of(bytes: &[u8]) -> BTreeMap<(Option<String>, String), String> {
    use wit_parser::decoding::{decode, DecodedWasm};
    use wit_parser::WorldItem;

    let mut docs = BTreeMap::new();
    let Ok(DecodedWasm::Component(resolve, world)) = decode(bytes) else {
        return docs;
    };
    for (_, item) in &resolve.worlds[world].exports {
        match item {
            WorldItem::Function(f) => {
                if let Some(text) = &f.docs.contents {
                    docs.insert((None, f.name.clone()), text.trim().to_string());
                }
            }
            WorldItem::Interface { id, .. } => {
                let interface = &resolve.interfaces[*id];
                let Some(name) = &interface.name else {
                    continue;
                };
                for (fname, f) in &interface.functions {
                    if let Some(text) = &f.docs.contents {
                        docs.insert((Some(name.clone()), fname.clone()), text.trim().to_string());
                    }
                }
            }
            WorldItem::Type { .. } => {}
        }
    }
    docs
}

/// The bare interface name of an exported instance: `acme:calc/calc@1.0.0` is `calc`, a plain
/// `foo` is `foo`.
pub(crate) fn bare_interface(name: &str) -> &str {
    let unversioned = name.split_once('@').map_or(name, |(n, _)| n);
    unversioned
        .rsplit_once('/')
        .map_or(unversioned, |(_, iface)| iface)
}

/// Project every exported function of `component` mounted as `app`. A function whose signature
/// carries a handle, a stream or a future is skipped and reported; two exports that project to
/// the same name refuse the mount as a `Conflict` naming both (R-D4). `sinks` are the projected
/// URNs the mount declares Sinks.
pub(crate) fn project(
    engine: &Engine,
    component: &Component,
    app: &str,
    sinks: &BTreeSet<String>,
    docs: &BTreeMap<(Option<String>, String), String>,
) -> Result<(Vec<ProjectedFunc>, Vec<Skipped>), Error> {
    let mut projector = Projector {
        funcs: Vec::new(),
        skipped: Vec::new(),
        claimed: BTreeMap::new(),
        sinks,
    };

    let ty = component.component_type();
    for (name, item) in ty.exports(engine) {
        match item.ty {
            ComponentItem::ComponentFunc(func) => {
                let Some(index) = component.get_export_index(None, name) else {
                    continue;
                };
                let endpoint = ProjectedEndpoint::root(app, encode_name(name));
                projector.admit(
                    name.to_string(),
                    endpoint,
                    index,
                    &func,
                    docs.get(&(None, name.to_string())),
                )?;
            }
            ComponentItem::ComponentInstance(instance) => {
                let unversioned = name.split_once('@').map_or(name, |(n, _)| n);
                if unversioned == ENDPOINT_API {
                    continue;
                }
                if unversioned.starts_with("wasi:") {
                    projector.skipped.push(Skipped {
                        export: name.to_string(),
                        reason: "a WASI export is a world's entry point, not a projected function"
                            .into(),
                    });
                    continue;
                }
                let Some(parent) = component.get_export_index(None, name) else {
                    continue;
                };
                let interface = bare_interface(name);
                for (fname, item) in instance.exports(engine) {
                    let ComponentItem::ComponentFunc(func) = item.ty else {
                        continue;
                    };
                    let Some(index) = component.get_export_index(Some(&parent), fname) else {
                        continue;
                    };
                    let endpoint = ProjectedEndpoint::in_interface(
                        app,
                        encode_name(interface),
                        encode_name(fname),
                    );
                    projector.admit(
                        format!("{name}#{fname}"),
                        endpoint,
                        index,
                        &func,
                        docs.get(&(Some(interface.to_string()), fname.to_string())),
                    )?;
                }
            }
            _ => {}
        }
    }
    Ok((projector.funcs, projector.skipped))
}

/// The projection's state while it walks the exports.
struct Projector<'a> {
    funcs: Vec<ProjectedFunc>,
    skipped: Vec<Skipped>,
    /// Projected URN to the export that took it, for the Conflict that names both.
    claimed: BTreeMap<String, String>,
    sinks: &'a BTreeSet<String>,
}

impl Projector<'_> {
    fn admit(
        &mut self,
        export: String,
        mut endpoint: ProjectedEndpoint,
        index: ComponentExportIndex,
        func: &ComponentFunc,
        docs: Option<&String>,
    ) -> Result<(), Error> {
        let (funcs, skipped, claimed, sinks) = (
            &mut self.funcs,
            &mut self.skipped,
            &mut self.claimed,
            self.sinks,
        );
        if func.async_() {
            skipped.push(Skipped {
                export,
                reason: "an async (p3) export is not projected until p3 is stable (R-D17)".into(),
            });
            return Ok(());
        }
        let params: Vec<(String, Type)> = func
            .params()
            .map(|(name, ty)| (name.to_string(), ty))
            .collect();
        let results: Vec<Type> = func.results().collect();
        let reason = params
            .iter()
            .find_map(|(_, ty)| values::unprojectable(ty))
            .or_else(|| results.iter().find_map(values::unprojectable));
        if let Some(reason) = reason {
            skipped.push(Skipped {
                export,
                reason: reason.to_string(),
            });
            return Ok(());
        }
        let urn = endpoint.urn();
        if let Some(first) = claimed.get(&urn) {
            return Err(Error::Conflict(format!(
                "`{first}` and `{export}` both project to {urn}; alias one in the declaration"
            )));
        }
        claimed.insert(urn.clone(), export.clone());

        let sink = sinks.contains(&urn);
        let mut params = params;
        if sink {
            endpoint = endpoint.as_sink();
            // A Sink receives the piped value in `content` (the engine's rule; conformance's
            // PIPELINE check). A function with a parameter named `content` already takes it; a
            // function with exactly one parameter takes it under that name; any other shape cannot
            // be a Sink.
            if !params.iter().any(|(name, _)| name == "content") {
                if params.len() == 1 {
                    params[0].0 = "content".to_string();
                } else {
                    return Err(Error::InvalidArgument {
                        name: "sinks".into(),
                        detail: format!(
                        "{urn} is declared a Sink, but `{export}` has no `content` parameter and \
                         not exactly one parameter to receive it"
                    ),
                    });
                }
            }
        }

        let result = results.into_iter().next();
        let id = urn
            .strip_prefix("urn:runtime:app:")
            .unwrap_or(&urn)
            .replace(':', "-");
        let summary = docs
            .cloned()
            .unwrap_or_else(|| format!("The WebAssembly export `{export}`."));
        let mut description = Description::new(id)
            .title(endpoint.func.clone())
            .summary(summary)
            .verb(match endpoint.verb {
                Verb::Source => CoreVerb::Source,
                Verb::Sink => CoreVerb::Sink,
            })
            .output(result.as_ref().map_or("text/plain", values::media_type_of));
        for (name, ty) in &params {
            let mut spec = ArgSpec::new(name.as_str()).class(values::class_of(ty));
            if matches!(ty, Type::Option(_)) {
                spec = spec.optional();
            }
            if let Some(cases) = values::one_of(ty) {
                spec = spec.one_of(cases);
            }
            description = description.input(spec);
        }
        funcs.push(ProjectedFunc {
            endpoint,
            index,
            params,
            result,
            description,
        });
        Ok(())
    }
}
