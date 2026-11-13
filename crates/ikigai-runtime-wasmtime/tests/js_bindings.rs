//! The JavaScript guest bindings for the J2 session (#2414, stage 1): GENERATED, never written
//! by hand.
//!
//! The resource is [`ABI`], a statement of `ikigai-module-abi`'s wire types (variant for
//! variant, field for field, in wire order). [`generate`] derives from it, in JavaScript, a
//! postcard codec for every type and the `ikigai` surface: one function per `AbiVerb` variant,
//! taking a target and named arguments. The output is checked in at
//! `fixtures/js-plugin/src/ikigai.js`, which the `js-plugin` fixture evaluates once at
//! pre-initialization.
//!
//! Two pins hold the resource to the Rust it states:
//! - `the_checked_in_bindings_are_what_the_abi_generates`: the file is exactly the generator's
//!   output, so an edit to the resource regenerates it and nothing else does
//!   (`IKIGAI_WRITE_BINDINGS=1` rewrites it).
//! - The ABI vectors (`fixtures/js-plugin/vectors/module-abi.txt`, ikigai-module 0.6.0's
//!   `tests/vectors/module-abi.txt` verbatim): `the_vendored_vectors_are_what_rust_encodes`
//!   checks them against the `ikigai-module-abi` this crate links, and `tests/js_handler.rs`
//!   decodes and re-encodes every one of them with the generated codec inside QuickJS.
//!
//! A spike fixture behind `js-fixtures`, not a public API.
#![cfg(feature = "js-fixtures")]

use std::fmt::Write as _;
use std::path::PathBuf;

use ikigai_module::abi::{AbiModuleCall, AbiModuleReply};

/// A wire type, as postcard writes it.
#[derive(Clone, Copy)]
enum Ty {
    /// A varint length and UTF-8 bytes.
    Str,
    /// A varint length and bytes.
    Bytes,
    /// A varint.
    U64,
    /// A tag byte (0 none, 1 some) and the value.
    Opt(&'static Ty),
    /// A varint count and the items.
    Seq(&'static Ty),
    /// A varint count and the items, sorted and unique (a `BTreeSet`).
    Set(&'static Ty),
    /// A varint count and the pairs, sorted by key (a `BTreeMap`).
    Map(&'static Ty, &'static Ty),
    /// A variant index (0 `Ok`, 1 `Err`) and the value.
    Result(&'static Ty, &'static Ty),
    /// A type of [`ABI`], by name.
    Named(&'static str),
}

enum Shape {
    Unit,
    Newtype(Ty),
    Fields(&'static [(&'static str, Ty)]),
}

enum Def {
    Struct(&'static str, &'static [(&'static str, Ty)]),
    Enum(&'static str, &'static [(&'static str, Shape)]),
}

use Ty::*;

/// The resource: `ikigai-module-abi` 0.1.0's types. ORDER IS THE WIRE: append only.
const ABI: &[Def] = &[
    Def::Enum(
        "AbiModuleCall",
        &[
            (
                "Invoke",
                Shape::Fields(&[
                    ("request", Named("AbiRequest")),
                    ("capability", Named("AbiCapability")),
                ]),
            ),
            (
                "HostResult",
                Shape::Newtype(Result(&Named("AbiRepresentation"), &Str)),
            ),
            ("Describe", Shape::Unit),
            ("Manifest", Shape::Unit),
            ("HostError", Shape::Newtype(Named("AbiModuleError"))),
            ("Cards", Shape::Unit),
        ],
    ),
    Def::Enum(
        "AbiModuleReply",
        &[
            (
                "HostCall",
                Shape::Fields(&[
                    ("request", Named("AbiRequest")),
                    ("capability", Named("AbiCapability")),
                ]),
            ),
            ("Resolved", Shape::Newtype(Named("AbiRepresentation"))),
            ("Error", Shape::Newtype(Str)),
            (
                "Bindings",
                Shape::Newtype(Opt(&Seq(&Named("AbiSpaceEntry")))),
            ),
            ("Manifest", Shape::Newtype(Named("AbiModuleManifest"))),
            ("ErrorTyped", Shape::Newtype(Named("AbiModuleError"))),
            (
                "ResolvedThreaded",
                Shape::Fields(&[
                    ("representation", Named("AbiRepresentation")),
                    ("threads", Seq(&Str)),
                ]),
            ),
            ("Cards", Shape::Newtype(Seq(&Named("AbiModuleCard")))),
        ],
    ),
    Def::Enum(
        "AbiModuleError",
        &[
            ("Unresolved", Shape::Newtype(Str)),
            ("MissingArgument", Shape::Newtype(Str)),
            (
                "InvalidArgument",
                Shape::Fields(&[("name", Str), ("detail", Str)]),
            ),
            ("Endpoint", Shape::Newtype(Str)),
            ("Denied", Shape::Newtype(Str)),
            ("NotFound", Shape::Newtype(Str)),
            ("Timeout", Shape::Newtype(Str)),
            ("Unavailable", Shape::Newtype(Str)),
        ],
    ),
    Def::Enum(
        "AbiVerb",
        &[
            ("Source", Shape::Unit),
            ("Sink", Shape::Unit),
            ("Exists", Shape::Unit),
            ("Delete", Shape::Unit),
            ("Meta", Shape::Unit),
        ],
    ),
    Def::Enum(
        "AbiArgRef",
        &[
            ("Reference", Shape::Newtype(Str)),
            ("Inline", Shape::Newtype(Bytes)),
            ("Content", Shape::Newtype(Str)),
        ],
    ),
    Def::Struct(
        "AbiRequest",
        &[
            ("verb", Named("AbiVerb")),
            ("target", Str),
            ("args", Map(&Str, &Named("AbiArgRef"))),
        ],
    ),
    Def::Struct("AbiCapability", &[("kind", Named("AbiCapabilityKind"))]),
    Def::Enum(
        "AbiCapabilityKind",
        &[("Root", Shape::Unit), ("Scoped", Shape::Newtype(Set(&Str)))],
    ),
    Def::Struct(
        "AbiReprType",
        &[("media_type", Str), ("params", Map(&Str, &Str))],
    ),
    Def::Enum(
        "AbiExpiry",
        &[
            ("Always", Shape::Unit),
            ("At", Shape::Newtype(U64)),
            ("Never", Shape::Unit),
        ],
    ),
    Def::Struct(
        "AbiRepresentation",
        &[
            ("repr_type", Named("AbiReprType")),
            ("bytes", Bytes),
            ("expiry", Named("AbiExpiry")),
        ],
    ),
    Def::Struct(
        "AbiSpaceEntry",
        &[("pattern", Str), ("endpoint", Str), ("origin", Opt(&Str))],
    ),
    Def::Enum(
        "AbiModuleRewrite",
        &[
            ("None", Shape::Unit),
            ("Table", Shape::Newtype(Str)),
            ("Undeclarable", Shape::Newtype(Str)),
            ("Unknown", Shape::Unit),
        ],
    ),
    Def::Struct(
        "AbiModuleManifest",
        &[
            ("entries", Opt(&Seq(&Named("AbiSpaceEntry")))),
            ("rewrite", Named("AbiModuleRewrite")),
        ],
    ),
    Def::Struct(
        "AbiModuleCard",
        &[("entry", Named("AbiSpaceEntry")), ("description", Str)],
    ),
];

/// A JavaScript expression for the encoder (`E`) or decoder (`D`) of `ty`.
fn expr(side: &str, ty: Ty) -> String {
    match ty {
        Str => format!("{side}.str"),
        Bytes => format!("{side}.bytes"),
        U64 => format!("{side}.u64"),
        Opt(t) => format!("{side}.opt({})", expr(side, *t)),
        Seq(t) => format!("{side}.seq({})", expr(side, *t)),
        Set(t) => format!("{side}.set({})", expr(side, *t)),
        Map(k, v) => format!("{side}.map({}, {})", expr(side, *k), expr(side, *v)),
        Result(o, e) => format!("{side}.result({}, {})", expr(side, *o), expr(side, *e)),
        Named(n) => format!("{}{n}", if side == "E" { "enc" } else { "dec" }),
    }
}

/// The codec's fixed part: postcard's primitives, over a byte array.
const PRELUDE: &str = include_str!("js_bindings/prelude.js");
/// The surface's fixed part: how a verb call crosses `__ikigai_host_call`.
const SURFACE: &str = include_str!("js_bindings/surface.js");

/// Derive the bindings from [`ABI`].
fn generate() -> String {
    let mut js = String::new();
    js.push_str(
        "// GENERATED by crates/ikigai-runtime-wasmtime/tests/js_bindings.rs from its `ABI`\n\
         // (ikigai-module-abi 0.1.0). Do not edit: IKIGAI_WRITE_BINDINGS=1 regenerates it.\n\
         (function () {\n\"use strict\";\n",
    );
    js.push_str(PRELUDE);
    for def in ABI {
        match def {
            Def::Struct(name, fields) => {
                let _ = writeln!(js, "function enc{name}(w, v) {{");
                for (f, ty) in *fields {
                    let _ = writeln!(js, "  {}(w, v.{f});", expr("E", *ty));
                }
                let _ = writeln!(js, "}}\nfunction dec{name}(r) {{\n  const v = {{}};");
                for (f, ty) in *fields {
                    let _ = writeln!(js, "  v.{f} = {}(r);", expr("D", *ty));
                }
                let _ = writeln!(js, "  return v;\n}}");
            }
            Def::Enum(name, variants) => {
                let _ = writeln!(js, "function enc{name}(w, v) {{\n  switch (v.tag) {{");
                for (i, (variant, shape)) in variants.iter().enumerate() {
                    let _ = write!(js, "    case \"{variant}\": E.u64(w, {i});");
                    match shape {
                        Shape::Unit => {}
                        Shape::Newtype(ty) => {
                            let _ = write!(js, " {}(w, v.value);", expr("E", *ty));
                        }
                        Shape::Fields(fields) => {
                            for (f, ty) in *fields {
                                let _ = write!(js, " {}(w, v.{f});", expr("E", *ty));
                            }
                        }
                    }
                    js.push_str(" return;\n");
                }
                let _ = writeln!(
                    js,
                    "  }}\n  throw new TypeError(\"not a {name}: \" + v.tag);\n}}"
                );
                let _ = writeln!(js, "function dec{name}(r) {{\n  switch (D.u64(r)) {{");
                for (i, (variant, shape)) in variants.iter().enumerate() {
                    let _ = write!(js, "    case {i}: ");
                    match shape {
                        Shape::Unit => {
                            let _ = write!(js, "return {{ tag: \"{variant}\" }};");
                        }
                        Shape::Newtype(ty) => {
                            let _ = write!(
                                js,
                                "return {{ tag: \"{variant}\", value: {}(r) }};",
                                expr("D", *ty)
                            );
                        }
                        Shape::Fields(fields) => {
                            let _ = write!(js, "{{ const v = {{ tag: \"{variant}\" }};");
                            for (f, ty) in *fields {
                                let _ = write!(js, " v.{f} = {}(r);", expr("D", *ty));
                            }
                            js.push_str(" return v; }");
                        }
                    }
                    js.push('\n');
                }
                let _ = writeln!(
                    js,
                    "  }}\n  throw new TypeError(\"no such {name} variant\");\n}}"
                );
            }
        }
    }
    // The codec, by type name, for the surface and for the vector check.
    js.push_str("const codec = {\n");
    for def in ABI {
        let (Def::Struct(name, _) | Def::Enum(name, _)) = def;
        let _ = writeln!(
            js,
            "  {name}: {{ encode: (v) => encodeWith(enc{name}, v), decode: (b) => decodeWith(dec{name}, b) }},"
        );
    }
    js.push_str("};\n");
    // The verbs: one function per AbiVerb variant, named for it.
    let Some(Def::Enum(_, verbs)) = ABI.iter().find(|d| matches!(d, Def::Enum("AbiVerb", _)))
    else {
        unreachable!("the ABI states AbiVerb")
    };
    let classes = ABI
        .iter()
        .find_map(|d| match d {
            Def::Enum("AbiModuleError", v) => Some(v),
            _ => None,
        })
        .expect("the ABI states AbiModuleError");
    js.push_str("const verbs = [");
    for (verb, _) in verbs.iter() {
        let _ = write!(js, "\"{verb}\", ");
    }
    js.push_str("];\nconst refusals = [");
    for (class, _) in classes.iter() {
        let _ = write!(js, "\"{class}\", ");
    }
    js.push_str("];\n");
    js.push_str(SURFACE);
    js.push_str("})();\n");
    js
}

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn the_checked_in_bindings_are_what_the_abi_generates() {
    let path = workspace().join("fixtures/js-plugin/src/ikigai.js");
    let generated = generate();
    if std::env::var_os("IKIGAI_WRITE_BINDINGS").is_some() {
        std::fs::write(&path, &generated).unwrap();
    }
    let checked_in = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        checked_in == generated,
        "fixtures/js-plugin/src/ikigai.js is not what the ABI generates; \
         rerun with IKIGAI_WRITE_BINDINGS=1"
    );
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn the_vendored_vectors_are_what_rust_encodes() {
    let vectors =
        std::fs::read_to_string(workspace().join("fixtures/js-plugin/vectors/module-abi.txt"))
            .unwrap();
    let mut seen = 0;
    for line in vectors
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let (name, encoded) = line.split_once(' ').unwrap();
        let bytes = hex(encoded);
        let again = if name.starts_with("ModuleCall.") {
            AbiModuleCall::decode(&bytes).unwrap().encode().unwrap()
        } else {
            AbiModuleReply::decode(&bytes).unwrap().encode().unwrap()
        };
        assert_eq!(again, bytes, "{name}");
        seen += 1;
    }
    // One per variant of both enums.
    assert_eq!(seen, 6 + 8);
}
