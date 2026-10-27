//! WIT values ↔ ikigai arguments and representations (R-D4's type map), written once for every
//! component: no component gets its own glue.
//!
//! | WIT | as an argument (bytes in) | as a result (representation out) |
//! |---|---|---|
//! | `bool`, integers, floats | the XSD lexical form (`true`, `-3`, `2.5`); out of range is InvalidArgument | `text/plain`, the same lexical form |
//! | `char`, `string` | UTF-8 text (a `char` is exactly one code point) | `text/plain; charset=utf-8` |
//! | `list<u8>` | the raw bytes | `application/octet-stream` |
//! | `enum` | one of its case names | `text/plain`, the case name |
//! | `option<T>` | an optional argument: absent is `none` | `none` is NotFound; `some(v)` is `v` |
//! | `result<T, E>` | JSON (`{"ok": …}` / `{"err": …}`) | `ok(v)` is `v`; `err(e)` is the refusal as a VALUE, `application/problem+json` |
//! | `record`, `tuple`, `variant`, `list<T>`, `map`, `flags` | JSON | `application/json` |
//!
//! JSON follows the shape wasmtime's `Val` already has: a record is an object, a tuple and a list
//! are arrays, a variant is `{"case": payload}` (or `"case"` with no payload), an enum is its case
//! name, flags are an array of names, an option is `null` or the value, a map is an array of
//! `[key, value]` pairs. Handles (`own`/`borrow`), streams, futures and error contexts are not
//! projected (R-D4: handles cannot outlive a per-call instance; streams are ledger #2247).

use ikigai_core::{Error, ReprType, Representation};
use serde_json::{json, Map as JsonMap, Number, Value as Json};
use wasmtime::component::types::Type;
use wasmtime::component::Val;

/// `xsd:` and `rdf:JSON` datatype IRIs for ArgSpec classes.
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const RDF_JSON: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#JSON";

/// The media type of a JSON-mapped result.
pub(crate) const JSON: &str = "application/json";
/// The media type of a `result`'s `err` returned as a value.
pub(crate) const PROBLEM_JSON: &str = "application/problem+json";
const TEXT: &str = "text/plain; charset=utf-8";
const BYTES: &str = "application/octet-stream";

/// Why a type cannot be projected, or `None` when it can.
pub(crate) fn unprojectable(ty: &Type) -> Option<&'static str> {
    match ty {
        Type::Own(_) | Type::Borrow(_) => {
            Some("a resource handle cannot outlive a per-call instance")
        }
        Type::Future(_) | Type::Stream(_) => {
            Some("streams and futures are not projected yet (ledger #2247)")
        }
        Type::ErrorContext => Some("an error context is not a value"),
        Type::List(l) => unprojectable(&l.ty()),
        Type::FixedLengthList(l) => unprojectable(&l.ty()),
        Type::Map(m) => unprojectable(&m.key()).or_else(|| unprojectable(&m.value())),
        Type::Record(r) => r.fields().find_map(|f| unprojectable(&f.ty)),
        Type::Tuple(t) => t.types().find_map(|t| unprojectable(&t)),
        Type::Variant(v) => v
            .cases()
            .find_map(|c| c.ty.as_ref().and_then(unprojectable)),
        Type::Option(o) => unprojectable(&o.ty()),
        Type::Result(r) => r
            .ok()
            .as_ref()
            .and_then(unprojectable)
            .or_else(|| r.err().as_ref().and_then(unprojectable)),
        _ => None,
    }
}

/// The ArgSpec class of a parameter type (R-D4's table): an XSD datatype for a scalar, `rdf:JSON`
/// for a structure.
pub(crate) fn class_of(ty: &Type) -> String {
    let xsd = |local: &str| format!("{XSD}{local}");
    match ty {
        Type::Bool => xsd("boolean"),
        Type::S8 => xsd("byte"),
        Type::S16 => xsd("short"),
        Type::S32 => xsd("int"),
        Type::S64 => xsd("long"),
        Type::U8 => xsd("unsignedByte"),
        Type::U16 => xsd("unsignedShort"),
        Type::U32 => xsd("unsignedInt"),
        Type::U64 => xsd("unsignedLong"),
        Type::Float32 => xsd("float"),
        Type::Float64 => xsd("double"),
        // Bytes are the type the WIRE carries (ikigai-http's `content` precedent), as is an enum's
        // case name.
        Type::Char | Type::String | Type::Enum(_) => xsd("string"),
        Type::List(l) if matches!(l.ty(), Type::U8) => xsd("string"),
        Type::Option(o) => class_of(&o.ty()),
        _ => RDF_JSON.to_string(),
    }
}

/// The case names of an `enum` (its ArgSpec `one_of`), or `None`.
pub(crate) fn one_of(ty: &Type) -> Option<Vec<String>> {
    match ty {
        Type::Enum(e) => Some(e.names().map(str::to_string).collect()),
        Type::Option(o) => one_of(&o.ty()),
        _ => None,
    }
}

/// The media type a result of `ty` is served as.
pub(crate) fn media_type_of(ty: &Type) -> &'static str {
    match ty {
        Type::Bool
        | Type::S8
        | Type::S16
        | Type::S32
        | Type::S64
        | Type::U8
        | Type::U16
        | Type::U32
        | Type::U64
        | Type::Float32
        | Type::Float64
        | Type::Char
        | Type::String
        | Type::Enum(_) => TEXT,
        Type::List(l) if matches!(l.ty(), Type::U8) => BYTES,
        Type::Option(o) => media_type_of(&o.ty()),
        Type::Result(r) => r.ok().as_ref().map_or(TEXT, media_type_of),
        _ => JSON,
    }
}

fn invalid(name: &str, detail: impl Into<String>) -> Error {
    Error::InvalidArgument {
        name: name.to_string(),
        detail: detail.into(),
    }
}

/// An argument's bytes as a value of `ty` (see the module table). `name` is the parameter, for
/// the refusal.
pub(crate) fn val_from_bytes(ty: &Type, bytes: &[u8], name: &str) -> Result<Val, Error> {
    if let Type::List(l) = ty {
        if matches!(l.ty(), Type::U8) {
            return Ok(Val::List(bytes.iter().map(|b| Val::U8(*b)).collect()));
        }
    }
    if let Type::Option(o) = ty {
        return Ok(Val::Option(Some(Box::new(val_from_bytes(
            &o.ty(),
            bytes,
            name,
        )?))));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid(name, "not UTF-8"))?;
    match ty {
        Type::String => Ok(Val::String(text.to_string())),
        Type::Char => {
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Ok(Val::Char(c)),
                _ => Err(invalid(name, "a char is exactly one code point")),
            }
        }
        Type::Enum(e) => {
            if e.names().any(|n| n == text) {
                Ok(Val::Enum(text.to_string()))
            } else {
                let names: Vec<&str> = e.names().collect();
                Err(invalid(name, format!("`{text}` is not one of {names:?}")))
            }
        }
        Type::Bool
        | Type::S8
        | Type::S16
        | Type::S32
        | Type::S64
        | Type::U8
        | Type::U16
        | Type::U32
        | Type::U64
        | Type::Float32
        | Type::Float64 => scalar_from_text(ty, text.trim(), name),
        _ => {
            let json: Json = serde_json::from_str(text)
                .map_err(|e| invalid(name, format!("not the JSON this type takes: {e}")))?;
            val_from_json(ty, &json, name)
        }
    }
}

fn scalar_from_text(ty: &Type, text: &str, name: &str) -> Result<Val, Error> {
    fn int<T: std::str::FromStr>(text: &str, name: &str, wit: &str) -> Result<T, Error> {
        text.parse::<T>()
            .map_err(|_| invalid(name, format!("`{text}` is not a {wit}")))
    }
    Ok(match ty {
        Type::Bool => match text {
            "true" | "1" => Val::Bool(true),
            "false" | "0" => Val::Bool(false),
            _ => return Err(invalid(name, format!("`{text}` is not an xsd:boolean"))),
        },
        Type::S8 => Val::S8(int(text, name, "s8")?),
        Type::S16 => Val::S16(int(text, name, "s16")?),
        Type::S32 => Val::S32(int(text, name, "s32")?),
        Type::S64 => Val::S64(int(text, name, "s64")?),
        Type::U8 => Val::U8(int(text, name, "u8")?),
        Type::U16 => Val::U16(int(text, name, "u16")?),
        Type::U32 => Val::U32(int(text, name, "u32")?),
        Type::U64 => Val::U64(int(text, name, "u64")?),
        Type::Float32 => Val::Float32(int(text, name, "f32")?),
        Type::Float64 => Val::Float64(int(text, name, "f64")?),
        _ => unreachable!("scalar_from_text is called for scalars only"),
    })
}

/// A JSON value as a value of `ty` (the shape in the module docs).
pub(crate) fn val_from_json(ty: &Type, json: &Json, name: &str) -> Result<Val, Error> {
    let shape = |what: &str| invalid(name, format!("expected {what}, got {json}"));
    match ty {
        Type::Bool => json
            .as_bool()
            .map(Val::Bool)
            .ok_or_else(|| shape("a boolean")),
        Type::S8
        | Type::S16
        | Type::S32
        | Type::S64
        | Type::U8
        | Type::U16
        | Type::U32
        | Type::U64
        | Type::Float32
        | Type::Float64 => match json {
            Json::Number(n) => scalar_from_text(ty, &n.to_string(), name),
            _ => Err(shape("a number")),
        },
        Type::Char | Type::String | Type::Enum(_) => match json {
            Json::String(s) => val_from_bytes(ty, s.as_bytes(), name),
            _ => Err(shape("a string")),
        },
        Type::List(l) => {
            let items = json.as_array().ok_or_else(|| shape("an array"))?;
            let elem = l.ty();
            items
                .iter()
                .map(|item| val_from_json(&elem, item, name))
                .collect::<Result<_, _>>()
                .map(Val::List)
        }
        Type::FixedLengthList(l) => {
            let items = json.as_array().ok_or_else(|| shape("an array"))?;
            let elem = l.ty();
            items
                .iter()
                .map(|item| val_from_json(&elem, item, name))
                .collect::<Result<_, _>>()
                .map(Val::FixedLengthList)
        }
        Type::Map(m) => {
            let pairs = json
                .as_array()
                .ok_or_else(|| shape("an array of [key, value]"))?;
            let (kt, vt) = (m.key(), m.value());
            pairs
                .iter()
                .map(|pair| match pair.as_array().map(Vec::as_slice) {
                    Some([k, v]) => {
                        Ok((val_from_json(&kt, k, name)?, val_from_json(&vt, v, name)?))
                    }
                    _ => Err(shape("a [key, value] pair")),
                })
                .collect::<Result<_, _>>()
                .map(Val::Map)
        }
        Type::Record(r) => {
            let object = json.as_object().ok_or_else(|| shape("an object"))?;
            r.fields()
                .map(|field| {
                    let value = object
                        .get(field.name)
                        .ok_or_else(|| invalid(name, format!("missing field `{}`", field.name)))?;
                    Ok((
                        field.name.to_string(),
                        val_from_json(&field.ty, value, name)?,
                    ))
                })
                .collect::<Result<_, _>>()
                .map(Val::Record)
        }
        Type::Tuple(t) => {
            let items = json.as_array().ok_or_else(|| shape("an array"))?;
            if items.len() != t.types().len() {
                return Err(shape(&format!("{} elements", t.types().len())));
            }
            t.types()
                .zip(items)
                .map(|(ty, item)| val_from_json(&ty, item, name))
                .collect::<Result<_, _>>()
                .map(Val::Tuple)
        }
        Type::Variant(v) => {
            let (case, payload) = case_of(json).ok_or_else(|| shape("{\"case\": payload}"))?;
            let found = v
                .cases()
                .find(|c| c.name == case)
                .ok_or_else(|| invalid(name, format!("no case `{case}`")))?;
            let value = match (&found.ty, payload) {
                (Some(ty), Some(p)) => Some(Box::new(val_from_json(ty, p, name)?)),
                (None, None) | (None, Some(Json::Null)) => None,
                _ => return Err(invalid(name, format!("case `{case}`: wrong payload"))),
            };
            Ok(Val::Variant(case.to_string(), value))
        }
        Type::Option(o) => match json {
            Json::Null => Ok(Val::Option(None)),
            other => Ok(Val::Option(Some(Box::new(val_from_json(
                &o.ty(),
                other,
                name,
            )?)))),
        },
        Type::Result(r) => {
            let (case, payload) =
                case_of(json).ok_or_else(|| shape("{\"ok\": …} or {\"err\": …}"))?;
            let side = |ty: Option<Type>| -> Result<Option<Box<Val>>, Error> {
                match (ty, payload) {
                    (Some(ty), Some(p)) => Ok(Some(Box::new(val_from_json(&ty, p, name)?))),
                    (None, None) | (None, Some(Json::Null)) => Ok(None),
                    _ => Err(invalid(name, format!("`{case}`: wrong payload"))),
                }
            };
            match case {
                "ok" => Ok(Val::Result(Ok(side(r.ok())?))),
                "err" => Ok(Val::Result(Err(side(r.err())?))),
                _ => Err(shape("{\"ok\": …} or {\"err\": …}")),
            }
        }
        Type::Flags(f) => {
            let items = json
                .as_array()
                .ok_or_else(|| shape("an array of flag names"))?;
            let names: Vec<&str> = f.names().collect();
            items
                .iter()
                .map(|item| match item.as_str() {
                    Some(n) if names.contains(&n) => Ok(n.to_string()),
                    _ => Err(invalid(name, format!("{item} is not one of {names:?}"))),
                })
                .collect::<Result<_, _>>()
                .map(Val::Flags)
        }
        Type::Own(_) | Type::Borrow(_) | Type::Future(_) | Type::Stream(_) | Type::ErrorContext => {
            Err(invalid(name, "this type is not projected"))
        }
    }
}

/// `{"case": payload}` or `"case"`.
fn case_of(json: &Json) -> Option<(&str, Option<&Json>)> {
    match json {
        Json::String(case) => Some((case.as_str(), None)),
        Json::Object(object) if object.len() == 1 => {
            let (case, payload) = object.iter().next()?;
            Some((case.as_str(), Some(payload)))
        }
        _ => None,
    }
}

/// A value as JSON (the shape in the module docs).
pub(crate) fn val_to_json(val: &Val) -> Json {
    let float = |f: f64| Number::from_f64(f).map_or_else(|| json!(f.to_string()), Json::Number);
    match val {
        Val::Bool(b) => json!(b),
        Val::S8(v) => json!(v),
        Val::U8(v) => json!(v),
        Val::S16(v) => json!(v),
        Val::U16(v) => json!(v),
        Val::S32(v) => json!(v),
        Val::U32(v) => json!(v),
        Val::S64(v) => json!(v),
        Val::U64(v) => json!(v),
        Val::Float32(v) => float(f64::from(*v)),
        Val::Float64(v) => float(*v),
        Val::Char(c) => json!(c.to_string()),
        Val::String(s) => json!(s),
        Val::List(items) | Val::Tuple(items) | Val::FixedLengthList(items) => {
            Json::Array(items.iter().map(val_to_json).collect())
        }
        Val::Map(pairs) => Json::Array(
            pairs
                .iter()
                .map(|(k, v)| Json::Array(vec![val_to_json(k), val_to_json(v)]))
                .collect(),
        ),
        Val::Record(fields) => Json::Object(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), val_to_json(v)))
                .collect::<JsonMap<_, _>>(),
        ),
        Val::Variant(case, None) => json!(case),
        Val::Variant(case, Some(payload)) => json!({ case.as_str(): val_to_json(payload) }),
        Val::Enum(case) => json!(case),
        Val::Option(None) => Json::Null,
        Val::Option(Some(v)) => val_to_json(v),
        Val::Result(Ok(v)) => json!({ "ok": v.as_deref().map_or(Json::Null, val_to_json) }),
        Val::Result(Err(e)) => json!({ "err": e.as_deref().map_or(Json::Null, val_to_json) }),
        Val::Flags(names) => json!(names),
        Val::Resource(_) | Val::Future(_) | Val::Stream(_) | Val::ErrorContext(_) => Json::Null,
    }
}

/// A value's text form, for a scalar result or a scalar argument of a bound import.
fn scalar_text(val: &Val) -> Option<String> {
    Some(match val {
        Val::Bool(b) => b.to_string(),
        Val::S8(v) => v.to_string(),
        Val::U8(v) => v.to_string(),
        Val::S16(v) => v.to_string(),
        Val::U16(v) => v.to_string(),
        Val::S32(v) => v.to_string(),
        Val::U32(v) => v.to_string(),
        Val::S64(v) => v.to_string(),
        Val::U64(v) => v.to_string(),
        Val::Float32(v) => v.to_string(),
        Val::Float64(v) => v.to_string(),
        Val::Char(c) => c.to_string(),
        Val::String(s) => s.clone(),
        Val::Enum(case) => case.clone(),
        _ => return None,
    })
}

/// A value's bytes as a kernel argument: the inverse of [`val_from_bytes`].
pub(crate) fn val_to_arg_bytes(val: &Val) -> Vec<u8> {
    match val {
        Val::List(items) if items.iter().all(|v| matches!(v, Val::U8(_))) && !items.is_empty() => {
            items
                .iter()
                .map(|v| match v {
                    Val::U8(b) => *b,
                    _ => 0,
                })
                .collect()
        }
        Val::Option(Some(v)) => val_to_arg_bytes(v),
        other => match scalar_text(other) {
            Some(text) => text.into_bytes(),
            None => val_to_json(other).to_string().into_bytes(),
        },
    }
}

/// `text/plain; charset=utf-8`, the type of a scalar or string result.
pub(crate) fn text_type() -> ReprType {
    ReprType::new("text/plain").with_param("charset", "utf-8")
}

/// What a function's result means to the kernel: its representation, or a refusal.
///
/// `option::none` is NotFound (R-D4). A `result`'s `err` is the problem returned as a VALUE
/// (`application/problem+json`): "this input is wrong" is an answer, and a refusal would never be
/// cached (field guide). `None` (a function with no result) is an empty `text/plain`.
pub(crate) fn val_to_representation(
    ty: Option<&Type>,
    val: Option<Val>,
    target: &str,
) -> Result<Representation, Error> {
    let (Some(ty), Some(val)) = (ty, val) else {
        return Ok(Representation::new(ReprType::new("text/plain"), Vec::new()));
    };
    match (ty, val) {
        (Type::Option(o), Val::Option(inner)) => match inner {
            None => Err(Error::NotFound(format!(
                "{target}: the function answered none"
            ))),
            Some(v) => val_to_representation(Some(&o.ty()), Some(*v), target),
        },
        (Type::Result(r), Val::Result(Ok(v))) => {
            val_to_representation(r.ok().as_ref(), v.map(|v| *v), target)
        }
        (Type::Result(_), Val::Result(Err(e))) => {
            let detail = e.as_deref().map_or(Json::Null, val_to_json);
            let body = json!({
                "type": "urn:runtime:wit:err",
                "title": format!("{target} answered err"),
                "detail": detail,
            });
            Ok(Representation::new(
                ReprType::new(PROBLEM_JSON),
                body.to_string().into_bytes(),
            ))
        }
        (Type::List(l), Val::List(items)) if matches!(l.ty(), Type::U8) => {
            let bytes = items
                .iter()
                .map(|v| match v {
                    Val::U8(b) => *b,
                    _ => 0,
                })
                .collect::<Vec<u8>>();
            Ok(Representation::new(ReprType::new(BYTES), bytes))
        }
        (ty, val) => match scalar_text(&val) {
            Some(text) if media_type_of(ty) == TEXT => {
                Ok(Representation::new(text_type(), text.into_bytes()))
            }
            _ => Ok(Representation::new(
                ReprType::new(JSON),
                val_to_json(&val).to_string().into_bytes(),
            )),
        },
    }
}

/// A kernel answer as the result value of a bound import of type `ty` (the inverse of
/// [`val_to_representation`]): text for scalars, raw bytes for `list<u8>`, JSON for structures.
pub(crate) fn representation_to_val(ty: &Type, repr: &Representation) -> Result<Val, Error> {
    match ty {
        Type::Result(r) => match r.ok() {
            Some(ok) => Ok(Val::Result(Ok(Some(Box::new(representation_to_val(
                &ok, repr,
            )?))))),
            None => Ok(Val::Result(Ok(None))),
        },
        other => val_from_bytes(other, &repr.bytes, "result"),
    }
}
