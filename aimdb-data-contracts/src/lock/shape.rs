//! A contract's wire shape: traced with `serde-reflection`, written as
//! canonical text.
//!
//! # Tracing
//!
//! Each contract is traced in its own session, after the `helpers` listed
//! in `catalog!`. The tracer deserializes the contract until every variant
//! of the contract (when it is an enum) was seen, then serializes the
//! values it built into the same session, so a type that deserializes
//! under other names than it serializes fails. The session runs twice: for
//! a binary format (postcard, CDR) and for a human-readable one (JSON).
//!
//! # Supported contracts
//!
//! The check promises to detect structural changes in contracts of this
//! profile; anything it can tell is outside fails the check:
//!
//! - derived structs and externally tagged enums; fixed-width primitives,
//!   strings, bytes, arrays, tuples, `Option`, sequences and maps
//! - symmetric `rename` / `rename_all`
//! - every enum nested in a contract (behind a field, `Option`, `Vec`, a
//!   map or another enum) listed in `helpers`; a missing one fails
//! - recursive enums need a first variant that ends the recursion
//!
//! Rejected: aliases and asymmetric renames, `#[serde(skip)]` variants,
//! types sent differently to human-readable and binary formats (such as
//! `Ipv4Addr`), `#[serde(flatten)]`, untagged and internally tagged enums,
//! and two instances of one generic type with different shapes.
//!
//! Not checked, so documented: every container in a session needs its own
//! serde name. The tracer knows types only by that name, so two types with
//! one name (or a helper standing in for another type of its name) are
//! merged into one shape. `skip_serializing_if` is not visible either.
//!
//! # Format
//!
//! The text is this crate's own, so a dependency update cannot change every
//! hash at once. A change to it bumps [`FORMAT_VERSION`](super::FORMAT_VERSION).
//!
//! - Primitives: `unit bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64
//!   char str bytes`
//! - `option<T>`, `seq<T>`, `map<K, V>`, `tuple(A, B)`, `array<T; N>`
//! - Containers inline, without their name (it never reaches the wire):
//!   `struct { "a": T, "b": U }`, `unit_struct`, `newtype(T)`,
//!   `tuple_struct(A, B)`
//! - `enum { 0 "A", 1 "B" newtype(T), 2 "C" tuple(T, U), 3 "D" struct { "x": T } }`:
//!   index, wire name and payload kind of every variant, since postcard and
//!   CDR send the index and JSON the name, and a newtype variant is sent
//!   differently from a one-element tuple variant
//! - Field and variant names are quoted and escaped like TOML strings, so
//!   no name can read as other text
//! - A container that contains itself: `rec(n)`, `n` levels up
//!
//! The serde data model is kept as is: `newtype(u32)` is not `u32`,
//! although JSON and postcard write both the same. That can report a change
//! that is not one, never miss one.

use std::collections::BTreeSet;
use std::fmt::Write;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_reflection::{
    ContainerFormat, Error, Format, Named, Registry, Samples, Tracer, TracerConfig, VariantFormat,
};
use sha2::{Digest, Sha256};

use super::quote;

/// A type `catalog!` traces before each contract: an enum nested in one.
pub struct Helper {
    name: &'static str,
    trace: fn(&mut Tracer) -> Result<(), Error>,
}

/// A [`Helper`] for `H`.
pub fn helper<H: Serialize + DeserializeOwned>() -> Helper {
    Helper {
        name: std::any::type_name::<H>(),
        trace: |tracer| {
            let (_, values) = tracer.trace_simple_type::<H>()?;
            serialize_into(tracer, &values)
        },
    }
}

/// The canonical shape text of `T`, or why it is outside the supported
/// profile.
pub(crate) fn shape_of<T: Serialize + DeserializeOwned>(
    helpers: &[Helper],
) -> Result<String, String> {
    let binary = trace::<T>(helpers, false)?;
    let readable = trace::<T>(helpers, true).map_err(|e| format!("human-readable: {e}"))?;
    if binary != readable {
        return Err(format!(
            "sent differently to human-readable formats (JSON) and binary ones, \
             which is not supported\n    binary:   {binary}\n    readable: {readable}"
        ));
    }
    Ok(binary)
}

/// `sha256:<hex>` of the shape text.
pub(crate) fn hash(shape: &str) -> String {
    let mut out = String::from("sha256:");
    for byte in Sha256::digest(shape.as_bytes()) {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn trace<T: Serialize + DeserializeOwned>(
    helpers: &[Helper],
    readable: bool,
) -> Result<String, String> {
    let mut tracer = Tracer::new(TracerConfig::default().is_human_readable(readable));
    for h in helpers {
        (h.trace)(&mut tracer).map_err(|e| format!("helper `{}`: {}", h.name, reason(&e)))?;
    }
    let samples = Samples::new();
    let (format, values) = tracer.trace_type::<T>(&samples).map_err(|e| reason(&e))?;
    serialize_into(&mut tracer, &values).map_err(|e| reason(&e))?;
    let registry = tracer.registry().map_err(|e| reason(&e))?;
    check_enums(&registry)?;
    render(&format, &registry)
}

/// Serialize `values` into the session: the formats must match what
/// deserialization traced.
fn serialize_into<T: Serialize>(tracer: &mut Tracer, values: &[T]) -> Result<(), Error> {
    let mut samples = Samples::new();
    for value in values {
        tracer.trace_value(&mut samples, value)?;
    }
    Ok(())
}

/// Why a type is outside the supported profile, for the check's report.
fn reason(e: &Error) -> String {
    match e {
        Error::MissingVariants(names) => format!(
            "not every variant of {} was seen: list {} in `helpers: [...]` of catalog!",
            names.join(", "),
            if names.len() == 1 { "it" } else { "them" }
        ),
        Error::Incompatible(..) => format!(
            "{e}: a type serializes differently than it deserializes (alias, asymmetric \
             rename), or two instances of a generic type differ; not supported"
        ),
        Error::NotSupported(_) => format!(
            "{e}: #[serde(flatten)], untagged and internally tagged enums are not supported"
        ),
        Error::Custom(_) => format!(
            "{e}: a Deserialize rejected a traced value (a hand-written or codec-dependent \
             type?); not supported"
        ),
        _ => e.to_string(),
    }
}

/// Every enum has the variants `0..n`, each under its own name. A
/// `#[serde(skip)]` variant breaks this: it is sent at its declared index
/// but deserialized at another.
fn check_enums(registry: &Registry) -> Result<(), String> {
    for (name, container) in registry {
        let ContainerFormat::Enum(variants) = container else {
            continue;
        };
        let names: BTreeSet<_> = variants.values().map(|v| &v.name).collect();
        let indices = variants.keys().copied().eq(0..variants.len() as u32);
        if !indices || names.len() != variants.len() {
            return Err(format!(
                "enum `{name}`: its variant indices do not match its variants \
                 (a #[serde(skip)] variant, or a nested enum missing from `helpers`?)"
            ));
        }
    }
    Ok(())
}

fn render(format: &Format, registry: &Registry) -> Result<String, String> {
    let mut out = String::new();
    write_format(format, registry, &mut Vec::new(), &mut out)?;
    Ok(out)
}

fn write_format<'a>(
    format: &'a Format,
    registry: &'a Registry,
    stack: &mut Vec<&'a str>,
    out: &mut String,
) -> Result<(), String> {
    let primitive = match format {
        Format::Variable(_) => return Err("a part was left untraced".into()),
        Format::TypeName(name) => {
            if let Some(pos) = stack.iter().rposition(|n| n == name) {
                let _ = write!(out, "rec({})", stack.len() - pos);
                return Ok(());
            }
            let container = registry
                .get(name)
                .ok_or_else(|| format!("`{name}` is missing from the trace"))?;
            stack.push(name);
            write_container(container, registry, stack, out)?;
            stack.pop();
            return Ok(());
        }
        Format::Unit => "unit",
        Format::Bool => "bool",
        Format::I8 => "i8",
        Format::I16 => "i16",
        Format::I32 => "i32",
        Format::I64 => "i64",
        Format::I128 => "i128",
        Format::U8 => "u8",
        Format::U16 => "u16",
        Format::U32 => "u32",
        Format::U64 => "u64",
        Format::U128 => "u128",
        Format::F32 => "f32",
        Format::F64 => "f64",
        Format::Char => "char",
        Format::Str => "str",
        Format::Bytes => "bytes",
        Format::Option(inner) => return wrap("option<", inner, ">", registry, stack, out),
        Format::Seq(inner) => return wrap("seq<", inner, ">", registry, stack, out),
        Format::Map { key, value } => {
            out.push_str("map<");
            write_format(key, registry, stack, out)?;
            out.push_str(", ");
            write_format(value, registry, stack, out)?;
            out.push('>');
            return Ok(());
        }
        Format::Tuple(items) => {
            out.push_str("tuple");
            return write_tuple(items, registry, stack, out);
        }
        Format::TupleArray { content, size } => {
            out.push_str("array<");
            write_format(content, registry, stack, out)?;
            let _ = write!(out, "; {size}>");
            return Ok(());
        }
    };
    out.push_str(primitive);
    Ok(())
}

fn wrap<'a>(
    open: &str,
    inner: &'a Format,
    close: &str,
    registry: &'a Registry,
    stack: &mut Vec<&'a str>,
    out: &mut String,
) -> Result<(), String> {
    out.push_str(open);
    write_format(inner, registry, stack, out)?;
    out.push_str(close);
    Ok(())
}

fn write_container<'a>(
    container: &'a ContainerFormat,
    registry: &'a Registry,
    stack: &mut Vec<&'a str>,
    out: &mut String,
) -> Result<(), String> {
    match container {
        ContainerFormat::UnitStruct => out.push_str("unit_struct"),
        ContainerFormat::NewTypeStruct(inner) => {
            wrap("newtype(", inner, ")", registry, stack, out)?
        }
        ContainerFormat::TupleStruct(items) => {
            out.push_str("tuple_struct");
            write_tuple(items, registry, stack, out)?;
        }
        ContainerFormat::Struct(fields) => {
            out.push_str("struct ");
            write_fields(fields, registry, stack, out)?;
        }
        ContainerFormat::Enum(variants) => {
            out.push_str("enum {");
            for (i, (index, variant)) in variants.iter().enumerate() {
                out.push_str(if i == 0 { " " } else { ", " });
                let _ = write!(out, "{index} {}", quote(&variant.name));
                // The payload kind is spelled out: serde sends a newtype
                // variant and a one-element tuple variant differently.
                match &variant.value {
                    VariantFormat::Variable(_) => {
                        return Err(format!("variant `{}` was left untraced", variant.name))
                    }
                    VariantFormat::Unit => {}
                    VariantFormat::NewType(inner) => {
                        wrap(" newtype(", inner, ")", registry, stack, out)?
                    }
                    VariantFormat::Tuple(items) => {
                        out.push_str(" tuple");
                        write_tuple(items, registry, stack, out)?;
                    }
                    VariantFormat::Struct(fields) => {
                        out.push_str(" struct ");
                        write_fields(fields, registry, stack, out)?;
                    }
                }
            }
            out.push_str(if variants.is_empty() { "}" } else { " }" });
        }
    }
    Ok(())
}

/// `(A, B)`
fn write_tuple<'a>(
    items: &'a [Format],
    registry: &'a Registry,
    stack: &mut Vec<&'a str>,
    out: &mut String,
) -> Result<(), String> {
    out.push('(');
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        write_format(item, registry, stack, out)?;
    }
    out.push(')');
    Ok(())
}

/// `{ "a": T, "b": U }`, or `{}`
fn write_fields<'a>(
    fields: &'a [Named<Format>],
    registry: &'a Registry,
    stack: &mut Vec<&'a str>,
    out: &mut String,
) -> Result<(), String> {
    out.push('{');
    for (i, field) in fields.iter().enumerate() {
        out.push_str(if i == 0 { " " } else { ", " });
        let _ = write!(out, "{}: ", quote(&field.name));
        write_format(&field.value, registry, stack, out)?;
    }
    out.push_str(if fields.is_empty() { "}" } else { " }" });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::collections::BTreeMap;

    fn shape<T: Serialize + DeserializeOwned>() -> Result<String, String> {
        shape_of::<T>(&[])
    }

    #[derive(Serialize, Deserialize)]
    struct Plain {
        schema_version: u32,
        celsius: f64,
        timestamp: u64,
    }

    #[test]
    fn struct_fields_in_wire_order() {
        assert_eq!(
            shape::<Plain>().unwrap(),
            r#"struct { "schema_version": u32, "celsius": f64, "timestamp": u64 }"#
        );
    }

    #[derive(Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Renamed {
        schema_version: u32,
        #[serde(rename = "c")]
        celsius: f64,
    }

    #[test]
    fn wire_names_after_symmetric_rename() {
        assert_eq!(
            shape::<Renamed>().unwrap(),
            r#"struct { "schemaVersion": u32, "c": f64 }"#
        );
    }

    #[derive(Serialize, Deserialize)]
    struct Aliased {
        #[serde(alias = "t")]
        temp: u8,
    }

    #[derive(Serialize, Deserialize)]
    struct Asymmetric {
        #[serde(rename(serialize = "out", deserialize = "in"))]
        x: u8,
    }

    #[test]
    fn aliases_and_asymmetric_renames_are_rejected() {
        for err in [shape::<Aliased>(), shape::<Asymmetric>()] {
            let err = err.unwrap_err();
            assert!(err.contains("serializes differently"), "{err}");
        }
    }

    #[derive(Serialize, Deserialize)]
    struct Compatible {
        schema_version: u32,
        #[serde(default)]
        celsius: Option<f64>,
    }

    #[derive(Serialize, Deserialize)]
    struct CompatibleBare {
        schema_version: u32,
        celsius: Option<f64>,
    }

    #[test]
    fn default_does_not_show() {
        assert_eq!(shape::<Compatible>(), shape::<CompatibleBare>());
    }

    #[allow(dead_code)]
    #[derive(Serialize, Deserialize)]
    struct Skipped {
        a: u8,
        #[serde(skip)]
        b: u8,
        c: u8,
    }

    #[test]
    fn skipped_field_is_not_sent() {
        assert_eq!(
            shape::<Skipped>().unwrap(),
            r#"struct { "a": u8, "c": u8 }"#
        );
    }

    #[derive(Serialize, Deserialize)]
    enum Mode {
        Off,
        Level(u8),
        Pair(u8, u8),
        Target { celsius: f32 },
    }

    const MODE: &str = r#"enum { 0 "Off", 1 "Level" newtype(u8), 2 "Pair" tuple(u8, u8), 3 "Target" struct { "celsius": f32 } }"#;

    #[derive(Serialize, Deserialize)]
    struct Unit;

    #[derive(Serialize, Deserialize)]
    struct Id(u32);

    #[derive(Serialize, Deserialize)]
    struct Point(i16, i16);

    #[derive(Serialize, Deserialize)]
    struct Nested {
        mode: Mode,
        inner: Plain,
        unit: Unit,
        id: Id,
        at: Point,
        label: Option<String>,
        samples: Vec<f32>,
        tags: BTreeMap<String, bool>,
        pair: (u8, char),
        raw: [u8; 4],
        any: (),
        boxed: Box<i64>,
    }

    #[test]
    fn nested_types_inline() {
        assert_eq!(
            shape_of::<Nested>(&[helper::<Mode>()]).unwrap(),
            format!(
                concat!(
                    r#"struct {{ "mode": {MODE}, "#,
                    r#""inner": struct {{ "schema_version": u32, "celsius": f64, "timestamp": u64 }}, "#,
                    r#""unit": unit_struct, "id": newtype(u32), "at": tuple_struct(i16, i16), "#,
                    r#""label": option<str>, "samples": seq<f32>, "tags": map<str, bool>, "#,
                    r#""pair": tuple(u8, char), "raw": array<u8; 4>, "any": unit, "boxed": i64 }}"#,
                ),
                MODE = MODE
            )
        );
    }

    #[test]
    fn top_level_enum_needs_no_helper() {
        assert_eq!(shape::<Mode>().unwrap(), MODE);
    }

    #[derive(Serialize, Deserialize)]
    enum Inner {
        A,
        B(u8),
        C,
    }

    #[derive(Serialize, Deserialize)]
    enum Outer {
        X,
        Y(Inner),
    }

    #[derive(Serialize, Deserialize)]
    struct Deep {
        outer: Outer,
        again: Vec<Inner>,
        maybe: Option<Inner>,
        keyed: BTreeMap<u8, Inner>,
    }

    #[test]
    fn nested_enums_need_helpers() {
        let err = shape::<Deep>().unwrap_err();
        assert!(err.contains("`helpers: [...]`"), "{err}");

        let inner = r#"enum { 0 "A", 1 "B" newtype(u8), 2 "C" }"#;
        assert_eq!(
            shape_of::<Deep>(&[helper::<Inner>(), helper::<Outer>()]).unwrap(),
            format!(
                r#"struct {{ "outer": enum {{ 0 "X", 1 "Y" newtype({inner}) }}, "again": seq<{inner}>, "maybe": option<{inner}>, "keyed": map<u8, {inner}> }}"#
            )
        );
    }

    mod before {
        #[derive(serde::Serialize, serde::Deserialize)]
        pub enum Payload {
            V(u8),
        }

        #[derive(serde::Serialize, serde::Deserialize)]
        pub struct Fields {
            pub a: u8,
            pub b: u8,
        }
    }

    mod after {
        #[allow(dead_code)]
        #[derive(serde::Serialize, serde::Deserialize)]
        pub enum Payload {
            V(u8, #[serde(skip)] u8),
        }

        #[derive(serde::Serialize, serde::Deserialize)]
        pub struct Fields {
            #[serde(rename = "a: u8, b")]
            pub value: u8,
        }
    }

    #[test]
    fn newtype_and_one_element_tuple_variants_differ() {
        let newtype = shape::<before::Payload>().unwrap();
        let tuple = shape::<after::Payload>().unwrap();
        assert_eq!(newtype, r#"enum { 0 "V" newtype(u8) }"#);
        assert_eq!(tuple, r#"enum { 0 "V" tuple(u8) }"#);
    }

    #[test]
    fn names_cannot_read_as_other_text() {
        let two = shape::<before::Fields>().unwrap();
        let one = shape::<after::Fields>().unwrap();
        assert_eq!(two, r#"struct { "a": u8, "b": u8 }"#);
        assert_eq!(one, r#"struct { "a: u8, b": u8 }"#);
        assert_ne!(hash(&two), hash(&one));
    }

    #[allow(dead_code)]
    #[derive(Serialize, Deserialize)]
    enum WithSkipped {
        A,
        #[serde(skip)]
        B,
        C(u8),
    }

    #[test]
    fn skipped_variant_is_rejected() {
        let err = shape::<WithSkipped>().unwrap_err();
        assert!(err.contains("variant indices"), "{err}");
    }

    #[derive(Serialize, Deserialize)]
    struct Tree {
        value: u8,
        children: Vec<Tree>,
    }

    #[test]
    fn recursion_refers_up() {
        assert_eq!(
            shape::<Tree>().unwrap(),
            r#"struct { "value": u8, "children": seq<rec(1)> }"#
        );
    }

    #[derive(Serialize, Deserialize)]
    enum Expr {
        Lit(u8),
        Neg(Box<Expr>),
        Add(Box<Expr>, Box<Expr>),
    }

    #[test]
    fn recursive_enum_with_an_ending_first_variant() {
        assert_eq!(
            shape::<Expr>().unwrap(),
            r#"enum { 0 "Lit" newtype(u8), 1 "Neg" newtype(rec(1)), 2 "Add" tuple(rec(1), rec(1)) }"#
        );
    }

    #[derive(Serialize, Deserialize)]
    struct Flattened {
        #[serde(flatten)]
        inner: Plain,
    }

    #[derive(Serialize, Deserialize)]
    #[serde(tag = "kind")]
    enum Tagged {
        A { x: u8 },
    }

    #[derive(Serialize, Deserialize)]
    #[serde(untagged)]
    enum Untagged {
        A(u8),
    }

    #[test]
    fn flatten_and_tagged_enums_are_rejected() {
        for err in [shape::<Flattened>(), shape::<Tagged>(), shape::<Untagged>()] {
            let err = err.unwrap_err();
            assert!(err.contains("not supported"), "{err}");
        }
    }

    #[derive(Serialize, Deserialize)]
    struct Reading<T> {
        value: T,
    }

    #[derive(Serialize, Deserialize)]
    struct Readings {
        celsius: Reading<f32>,
        count: Reading<u8>,
    }

    #[test]
    fn generic_instances_of_other_shapes_are_rejected() {
        let err = shape::<Readings>().unwrap_err();
        assert!(err.contains("generic"), "{err}");
    }

    #[derive(Serialize, Deserialize)]
    struct Address {
        ip: std::net::Ipv4Addr,
    }

    #[test]
    fn codec_dependent_types_are_rejected() {
        let err = shape::<Address>().unwrap_err();
        assert!(err.starts_with("human-readable: "), "{err}");
    }

    #[test]
    fn a_failing_helper_is_named() {
        let err = shape_of::<Plain>(&[helper::<Address>()]).unwrap_err();
        assert!(err.contains("helper `"), "{err}");
    }

    #[test]
    fn hash_is_full_sha256() {
        assert_eq!(
            hash(""),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
