//! The WIT package parses and its worlds hold the interfaces the decision record names (R-D16).

use ikigai_runtime_wit::RUNTIME_WIT;
use wit_parser::{Resolve, WorldItem};

fn load() -> Resolve {
    let mut resolve = Resolve::new();
    resolve
        .push_str("runtime.wit", RUNTIME_WIT)
        .expect("the ikigai:runtime WIT package must parse");
    resolve
}

#[test]
fn the_package_parses() {
    let _ = load();
}

#[test]
fn it_defines_the_kernel_and_endpoint_interfaces() {
    let resolve = load();
    let names: Vec<&str> = resolve
        .interfaces
        .iter()
        .filter_map(|(_, i)| i.name.as_deref())
        .collect();
    assert!(names.contains(&"kernel"), "interfaces were {names:?}");
    // `endpoint-api`, not `endpoint`: WIT forbids a world and interface sharing a name, and the
    // two world names are kept (see the WIT).
    assert!(names.contains(&"endpoint-api"), "interfaces were {names:?}");
}

#[test]
fn it_defines_the_endpoint_and_app_worlds() {
    let resolve = load();
    let worlds: Vec<&str> = resolve
        .worlds
        .iter()
        .map(|(_, w)| w.name.as_str())
        .collect();
    assert!(worlds.contains(&"endpoint"), "worlds were {worlds:?}");
    assert!(worlds.contains(&"app"), "worlds were {worlds:?}");
}

#[test]
fn the_endpoint_world_imports_kernel_and_exports_endpoint() {
    let resolve = load();
    let world = resolve
        .worlds
        .iter()
        .find(|(_, w)| w.name == "endpoint")
        .map(|(_, w)| w)
        .expect("an `endpoint` world");

    let interface_name = |item: &WorldItem| -> Option<String> {
        match item {
            WorldItem::Interface { id, .. } => resolve.interfaces[*id].name.clone(),
            _ => None,
        }
    };
    let imports: Vec<String> = world.imports.values().filter_map(interface_name).collect();
    let exports: Vec<String> = world.exports.values().filter_map(interface_name).collect();

    assert!(
        imports.contains(&"kernel".to_string()),
        "imports were {imports:?}"
    );
    assert!(
        exports.contains(&"endpoint-api".to_string()),
        "exports were {exports:?}"
    );
}
