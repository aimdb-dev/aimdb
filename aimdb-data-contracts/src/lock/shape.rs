//! A contract's wire shape: traced through serde, written as canonical text.
//!
//! # Tracing
//!
//! A [`Generator`] (a serde `Deserializer`) builds values of the contract,
//! pass after pass, until every variant of every enum inside it has been
//! built once, and every `Option`, sequence and map has held a value. A
//! [`Recorder`] (a serde `Serializer`) writes down what those values send:
//! wire names after `rename` / `rename_all`, field order, and the variant
//! index serde's derive emits, which postcard and CDR depend on. So the
//! shape is serde's actual behaviour, not a reading of its attributes.
//!
//! The values are recorded twice: for a binary format (postcard, CDR) and
//! for a human-readable one (JSON). Serde lets a type send different
//! shapes to each: `Ipv4Addr` is four bytes in one, a string in the other.
//! Values are built in binary mode only, where such types deserialize from
//! plain parts.
//!
//! Not traceable, and reported as such: `#[serde(flatten)]`, untagged and
//! internally tagged enums (they need a self-describing format), and
//! hand-written `Deserialize` impls that need one. A variant marked
//! `skip_deserializing` is never built, so it is not in the shape.
//!
//! # Format
//!
//! The text is this crate's own, so a dependency update cannot change every
//! hash at once. A change to it bumps [`FORMAT_VERSION`](super::FORMAT_VERSION).
//!
//! - Primitives: `unit bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64
//!   char str bytes`
//! - `option<T>`, `seq<T>`, `map<K, V>`, `tuple(A, B)`; a tuple of two or
//!   more equal elements (an array on the wire) is `array<T; N>`
//! - Containers inline, without their name (it never reaches the wire):
//!   `struct { a: T, b: U }`, `unit_struct`, `newtype(T)`,
//!   `tuple_struct(A, B)`
//! - `enum { 0 A, 1 B(T), 2 C(T, U), 3 D { x: T } }`: index and wire name
//!   of every variant, since postcard and CDR send the index and JSON the
//!   name
//! - A container that contains itself: `rec(n)`, `n` levels up
//! - When the two modes differ: `binary: <shape>; readable: <shape>`;
//!   otherwise the one shape
//!
//! The serde data model is kept as is: `newtype(u32)` is not `u32`,
//! although JSON and postcard write both the same. That can report a change
//! that is not one, never miss one. `#[serde(default)]` and
//! `skip_serializing_if` do not show: they make a change compatible, not
//! different.
//!
//! Containers are told apart by their serde name, so that `T` and `Box<T>`
//! are one container. A name that several Rust types use (`a::Status` and
//! `b::Status`, `Reading<f32>` and `Reading<u8>`) is told apart by the Rust
//! type of the value too, so each keeps its own shape.

use std::any::TypeId;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::{self, Display, Write};

use serde::de::{
    self, value::BorrowedStrDeserializer, DeserializeOwned, DeserializeSeed, EnumAccess, MapAccess,
    SeqAccess, VariantAccess, Visitor,
};
use serde::ser::{self, Serialize};
use sha2::{Digest, Sha256};

/// Upper bound on passes over one contract. Each pass builds at least one
/// new variant or fills one unknown part, so a real contract ends far below.
const MAX_PASSES: usize = 10_000;

/// Upper bound on nested containers while building one value.
const MAX_DEPTH: usize = 64;

/// Upper bound on the fields of one struct.
const MAX_FIELDS: usize = 4096;

/// The canonical shape text of `T`, or why it cannot be traced.
pub(crate) fn shape_of<T: Serialize + DeserializeOwned>() -> Result<String, String> {
    let (values, shared) = generate::<T>().map_err(|e| e.0)?;
    let binary = record_all(&values, false, &shared)?;
    let readable = record_all(&values, true, &shared)?;
    Ok(if binary == readable {
        binary
    } else {
        format!("binary: {binary}; readable: {readable}")
    })
}

/// Values of `T` that together build every variant of every enum inside
/// it, and the serde names that more than one Rust type uses.
fn generate<T: DeserializeOwned>() -> Result<(Vec<T>, HashSet<&'static str>), Error> {
    let mut explorer = Explorer::default();
    let mut probe = Probe::Unknown;
    let mut values = Vec::new();
    for _ in 0..MAX_PASSES {
        let mut out = Probe::Unknown;
        values.push(T::deserialize(Generator {
            ex: &mut explorer,
            known: probe.clone(),
            out: &mut out,
        })?);
        merge_probe(&mut probe, out);
        if !explorer.incomplete(&probe) {
            let shared = explorer
                .names
                .into_iter()
                .filter(|(_, types)| types.len() > 1)
                .map(|(name, _)| name)
                .collect();
            return Ok((values, shared));
        }
    }
    Err(Error("tracing did not finish".into()))
}

/// The shape `values` send to a human-readable format (JSON) or a binary
/// one (postcard, CDR).
fn record_all<T: Serialize>(
    values: &[T],
    readable: bool,
    shared: &HashSet<&'static str>,
) -> Result<String, String> {
    let mode = if readable { "human-readable: " } else { "" };
    let mut cx = Recording {
        defs: Defs::new(),
        readable,
        shared: shared.clone(),
    };
    let mut shape = Shape::Unknown;
    for value in values {
        let out = record(&mut cx, value).map_err(|e| format!("{mode}{e}"))?;
        merge_shape(&mut shape, out).map_err(|e| format!("{mode}{e}"))?;
    }
    render(&shape, &cx.defs).map_err(|e| format!("{mode}{e}"))
}

/// `sha256:<hex>` of the shape text.
pub(crate) fn hash(shape: &str) -> String {
    let mut out = String::from("sha256:");
    for byte in Sha256::digest(shape.as_bytes()) {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[derive(Debug)]
struct Error(String);

impl Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl de::Error for Error {
    fn custom<T: Display>(msg: T) -> Self {
        Error(msg.to_string())
    }
}

impl ser::Error for Error {
    fn custom<T: Display>(msg: T) -> Self {
        Error(msg.to_string())
    }
}

fn not_supported(method: &str, why: &str) -> Error {
    Error(format!("{method} is not supported ({why})"))
}

// ═══════════════════════════════════════════════════════════════════
// GENERATOR: builds values until every variant was built once
// ═══════════════════════════════════════════════════════════════════

/// What the generator knows of a part of the contract, to choose where to
/// go next.
#[derive(Clone, Debug)]
enum Probe {
    Unknown,
    Leaf,
    Option(Box<Probe>),
    Seq(Box<Probe>),
    Map(Box<Probe>, Box<Probe>),
    Tuple(Vec<Probe>),
    Node(TypeId),
}

/// A container, by the type its `Deserialize` builds.
#[derive(Debug)]
enum Node {
    /// Struct, tuple struct, newtype or unit struct: the parts read.
    Slots(Vec<Probe>),
    /// Per name in serde's `VARIANTS` (aliases included): the parts read,
    /// once built.
    Enum(Vec<Option<Vec<Probe>>>),
}

#[derive(Debug, Default)]
struct Explorer {
    nodes: HashMap<TypeId, Node>,
    /// The Rust types built under each serde container name.
    names: HashMap<&'static str, HashSet<TypeId>>,
    /// Containers being built, with the variant chosen for enums.
    stack: Vec<(TypeId, Option<usize>)>,
}

impl Explorer {
    fn enter(&mut self, id: TypeId, variant: Option<usize>) -> Result<(), Error> {
        if self.stack.len() >= MAX_DEPTH {
            return Err(Error(format!(
                "containers nest deeper than {MAX_DEPTH}: a recursive type without a way out?"
            )));
        }
        self.stack.push((id, variant));
        Ok(())
    }

    /// Inside a container that is already being built further up: take
    /// the way out (`None`, empty sequences, an ending variant).
    fn cut(&self) -> bool {
        let s = &self.stack;
        (0..s.len()).any(|i| s[..i].iter().any(|(id, _)| *id == s[i].0))
    }

    /// Whether some part reachable from `p` was never built.
    fn incomplete(&self, p: &Probe) -> bool {
        self.incomplete_in(p, &mut Vec::new())
    }

    fn incomplete_in(&self, p: &Probe, seen: &mut Vec<TypeId>) -> bool {
        match p {
            Probe::Unknown => true,
            Probe::Leaf => false,
            Probe::Option(x) | Probe::Seq(x) => self.incomplete_in(x, seen),
            Probe::Map(k, v) => self.incomplete_in(k, seen) || self.incomplete_in(v, seen),
            Probe::Tuple(xs) => xs.iter().any(|x| self.incomplete_in(x, seen)),
            Probe::Node(id) => {
                if seen.contains(id) {
                    return false;
                }
                seen.push(*id);
                match self.nodes.get(id) {
                    None => true,
                    Some(Node::Slots(xs)) => xs.iter().any(|x| self.incomplete_in(x, seen)),
                    Some(Node::Enum(vs)) => vs.iter().any(|v| match v {
                        None => true,
                        Some(xs) => xs.iter().any(|x| self.incomplete_in(x, seen)),
                    }),
                }
            }
        }
    }

    /// Whether `p` can reach one of `targets`.
    fn reaches(&self, p: &Probe, targets: &[TypeId], seen: &mut Vec<TypeId>) -> bool {
        match p {
            Probe::Unknown | Probe::Leaf => false,
            Probe::Option(x) | Probe::Seq(x) => self.reaches(x, targets, seen),
            Probe::Map(k, v) => self.reaches(k, targets, seen) || self.reaches(v, targets, seen),
            Probe::Tuple(xs) => xs.iter().any(|x| self.reaches(x, targets, seen)),
            Probe::Node(id) => {
                if targets.contains(id) {
                    return true;
                }
                if seen.contains(id) {
                    return false;
                }
                seen.push(*id);
                match self.nodes.get(id) {
                    None => false,
                    Some(Node::Slots(xs)) => xs.iter().any(|x| self.reaches(x, targets, seen)),
                    Some(Node::Enum(vs)) => vs
                        .iter()
                        .flatten()
                        .any(|xs| xs.iter().any(|x| self.reaches(x, targets, seen))),
                }
            }
        }
    }

    /// The variant of enum `id` to build next: one not built yet, else one
    /// leading to a part not built yet. Inside a recursion, or when all is
    /// built, a variant that ends without coming back.
    fn choose(&self, id: TypeId) -> usize {
        let Some(Node::Enum(vs)) = self.nodes.get(&id) else {
            return 0;
        };
        let reentry = self.cut() || self.stack.iter().any(|(s, _)| *s == id);
        if !reentry {
            if let Some(i) = vs.iter().position(Option::is_none) {
                return i;
            }
            let leads_on = |xs: &Vec<Probe>| xs.iter().any(|x| self.incomplete(x));
            if let Some(i) = vs.iter().position(|v| v.as_ref().is_some_and(leads_on)) {
                return i;
            }
        }
        let mut open: Vec<TypeId> = self.stack.iter().map(|(s, _)| *s).collect();
        open.push(id);
        let ends = |xs: &Vec<Probe>| !xs.iter().any(|x| self.reaches(x, &open, &mut Vec::new()));
        if let Some(i) = vs.iter().position(|v| v.as_ref().is_some_and(ends)) {
            return i;
        }
        let in_progress = |i: usize| self.stack.iter().any(|&(s, v)| s == id && v == Some(i));
        (0..vs.len())
            .find(|&i| vs[i].is_none() && !in_progress(i))
            .unwrap_or(0)
    }
}

fn merge_probe(into: &mut Probe, from: Probe) {
    if matches!(from, Probe::Unknown) {
        return;
    }
    if matches!(into, Probe::Unknown) {
        *into = from;
        return;
    }
    match (into, from) {
        (Probe::Option(a), Probe::Option(b)) | (Probe::Seq(a), Probe::Seq(b)) => merge_probe(a, *b),
        (Probe::Map(ak, av), Probe::Map(bk, bv)) => {
            merge_probe(ak, *bk);
            merge_probe(av, *bv);
        }
        (Probe::Tuple(a), Probe::Tuple(b)) => merge_probes(a, b),
        _ => {}
    }
}

fn merge_probes(into: &mut Vec<Probe>, from: Vec<Probe>) {
    for (i, p) in from.into_iter().enumerate() {
        match into.get_mut(i) {
            Some(slot) => merge_probe(slot, p),
            None => into.push(p),
        }
    }
}

/// Deserializes one part of a contract value, choosing what to build.
struct Generator<'a> {
    ex: &'a mut Explorer,
    /// What earlier passes learned of this part.
    known: Probe,
    /// What this pass learns of it.
    out: &'a mut Probe,
}

impl Generator<'_> {
    /// Whether to build the inside of an `Option`, sequence or map.
    fn explore(&self, inner: &Probe) -> bool {
        !self.ex.cut() && self.ex.incomplete(inner)
    }

    /// Build a struct-like container of type `T` from its parts.
    fn container<T: ?Sized, R>(
        self,
        name: &'static str,
        build: impl FnOnce(Parts<'_>) -> Result<R, Error>,
    ) -> Result<R, Error> {
        let id = typeid::of::<T>();
        self.ex.names.entry(name).or_default().insert(id);
        let known = match self.ex.nodes.get(&id) {
            Some(Node::Slots(xs)) => xs.clone(),
            _ => Vec::new(),
        };
        let mut outs = Vec::new();
        self.ex.enter(id, None)?;
        let built = build(Parts {
            ex: &mut *self.ex,
            known,
            outs: &mut outs,
            limit: MAX_FIELDS,
        });
        self.ex.stack.pop();
        let value = built?;
        if let Node::Slots(xs) = self
            .ex
            .nodes
            .entry(id)
            .or_insert_with(|| Node::Slots(Vec::new()))
        {
            merge_probes(xs, outs);
        }
        *self.out = Probe::Node(id);
        Ok(value)
    }
}

macro_rules! leaf {
    ($($method:ident => $visit:ident($value:expr);)*) => {
        $(fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            *self.out = Probe::Leaf;
            visitor.$visit($value)
        })*
    };
}

impl<'de> de::Deserializer<'de> for Generator<'_> {
    type Error = Error;

    // Not zero or empty, so that `skip_serializing_if` on a default value
    // does not hide a field from the recorder.
    leaf! {
        deserialize_bool => visit_bool(true);
        deserialize_i8 => visit_i8(1);
        deserialize_i16 => visit_i16(1);
        deserialize_i32 => visit_i32(1);
        deserialize_i64 => visit_i64(1);
        deserialize_i128 => visit_i128(1);
        deserialize_u8 => visit_u8(1);
        deserialize_u16 => visit_u16(1);
        deserialize_u32 => visit_u32(1);
        deserialize_u64 => visit_u64(1);
        deserialize_u128 => visit_u128(1);
        deserialize_f32 => visit_f32(1.0);
        deserialize_f64 => visit_f64(1.0);
        deserialize_char => visit_char('a');
        deserialize_str => visit_str("a");
        deserialize_string => visit_string("a".into());
        deserialize_bytes => visit_bytes(b"a");
        deserialize_byte_buf => visit_byte_buf(b"a".to_vec());
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        *self.out = Probe::Leaf;
        visitor.visit_unit()
    }

    fn deserialize_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value, Error> {
        Err(not_supported(
            "deserialize_any",
            "an untagged or internally tagged enum needs a self-describing format",
        ))
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, _: V) -> Result<V::Value, Error> {
        Err(not_supported(
            "deserialize_identifier",
            "#[serde(flatten)] or a map-only Deserialize",
        ))
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value, Error> {
        Err(not_supported(
            "deserialize_ignored_any",
            "needs a self-describing format",
        ))
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let inner = match &self.known {
            Probe::Option(x) => (**x).clone(),
            _ => Probe::Unknown,
        };
        if !self.explore(&inner) {
            *self.out = Probe::Option(Box::new(Probe::Unknown));
            return visitor.visit_none();
        }
        let mut out = Probe::Unknown;
        let value = visitor.visit_some(Generator {
            ex: self.ex,
            known: inner,
            out: &mut out,
        })?;
        *self.out = Probe::Option(Box::new(out));
        Ok(value)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let inner = match &self.known {
            Probe::Seq(x) => (**x).clone(),
            _ => Probe::Unknown,
        };
        let limit = usize::from(self.explore(&inner));
        let mut outs = Vec::new();
        let value = visitor.visit_seq(Parts {
            ex: self.ex,
            known: vec![inner],
            outs: &mut outs,
            limit,
        })?;
        *self.out = Probe::Seq(Box::new(outs.pop().unwrap_or(Probe::Unknown)));
        Ok(value)
    }

    fn deserialize_tuple<V: Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value, Error> {
        let known = match &self.known {
            Probe::Tuple(xs) => xs.clone(),
            _ => Vec::new(),
        };
        let mut outs = Vec::new();
        let value = visitor.visit_seq(Parts {
            ex: self.ex,
            known,
            outs: &mut outs,
            limit: len,
        })?;
        *self.out = Probe::Tuple(outs);
        Ok(value)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let (key, value) = match &self.known {
            Probe::Map(k, v) => ((**k).clone(), (**v).clone()),
            _ => (Probe::Unknown, Probe::Unknown),
        };
        let explore = self.explore(&Probe::Tuple(vec![key.clone(), value.clone()]));
        let (mut key_out, mut value_out) = (Probe::Unknown, Probe::Unknown);
        let built = visitor.visit_map(Entry {
            ex: self.ex,
            key: explore.then_some((key, &mut key_out)),
            value: Some((value, &mut value_out)),
        })?;
        *self.out = Probe::Map(Box::new(key_out), Box::new(value_out));
        Ok(built)
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.container::<V::Value, _>(name, |_| visitor.visit_unit())
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.container::<V::Value, _>(name, |mut parts| visitor.visit_newtype_struct(parts.next()))
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.container::<V::Value, _>(name, |mut parts| {
            parts.limit = len;
            visitor.visit_seq(parts)
        })
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        // As a sequence: `FIELDS` holds aliases too, so its names are not
        // the fields. The recorder sees the wire names.
        self.container::<V::Value, _>(name, |parts| visitor.visit_seq(parts))
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        if variants.is_empty() {
            return Err(Error(format!("enum `{name}` has no variant to build")));
        }
        let id = typeid::of::<V::Value>();
        self.ex.names.entry(name).or_default().insert(id);
        self.ex
            .nodes
            .entry(id)
            .or_insert_with(|| Node::Enum(vec![None; variants.len()]));
        let index = self.ex.choose(id);
        let known = match self.ex.nodes.get(&id) {
            Some(Node::Enum(vs)) => vs.get(index).cloned().flatten().unwrap_or_default(),
            _ => Vec::new(),
        };
        let mut outs = Vec::new();
        self.ex.enter(id, Some(index))?;
        let built = visitor.visit_enum(Variant {
            name: variants[index],
            parts: Parts {
                ex: &mut *self.ex,
                known,
                outs: &mut outs,
                limit: MAX_FIELDS,
            },
        });
        self.ex.stack.pop();
        let value = built?;
        if let Some(Node::Enum(vs)) = self.ex.nodes.get_mut(&id) {
            match &mut vs[index] {
                Some(xs) => merge_probes(xs, outs),
                slot => *slot = Some(outs),
            }
        }
        *self.out = Probe::Node(id);
        Ok(value)
    }

    fn is_human_readable(&self) -> bool {
        false
    }
}

/// The parts of a container, sequence or tuple, read one after another.
struct Parts<'a> {
    ex: &'a mut Explorer,
    known: Vec<Probe>,
    outs: &'a mut Vec<Probe>,
    limit: usize,
}

impl Parts<'_> {
    fn next(&mut self) -> Generator<'_> {
        let i = self.outs.len();
        let known = self.known.get(i).cloned().unwrap_or(Probe::Unknown);
        self.outs.push(Probe::Unknown);
        Generator {
            ex: self.ex,
            known,
            out: &mut self.outs[i],
        }
    }
}

impl<'de> SeqAccess<'de> for Parts<'_> {
    type Error = Error;

    fn next_element_seed<S: DeserializeSeed<'de>>(
        &mut self,
        seed: S,
    ) -> Result<Option<S::Value>, Error> {
        if self.outs.len() >= self.limit {
            return Ok(None);
        }
        seed.deserialize(self.next()).map(Some)
    }
}

/// A map of zero or one entry.
struct Entry<'a> {
    ex: &'a mut Explorer,
    key: Option<(Probe, &'a mut Probe)>,
    value: Option<(Probe, &'a mut Probe)>,
}

impl<'de> MapAccess<'de> for Entry<'_> {
    type Error = Error;

    fn next_key_seed<S: DeserializeSeed<'de>>(
        &mut self,
        seed: S,
    ) -> Result<Option<S::Value>, Error> {
        let Some((known, out)) = self.key.take() else {
            return Ok(None);
        };
        seed.deserialize(Generator {
            ex: self.ex,
            known,
            out,
        })
        .map(Some)
    }

    fn next_value_seed<S: DeserializeSeed<'de>>(&mut self, seed: S) -> Result<S::Value, Error> {
        let (known, out) = self
            .value
            .take()
            .ok_or_else(|| Error("map value read twice".into()))?;
        seed.deserialize(Generator {
            ex: self.ex,
            known,
            out,
        })
    }
}

/// The chosen variant of an enum, named as serde's `VARIANTS` names it.
struct Variant<'a> {
    name: &'static str,
    parts: Parts<'a>,
}

impl<'de, 'a> EnumAccess<'de> for Variant<'a> {
    type Error = Error;
    type Variant = Self;

    fn variant_seed<S: DeserializeSeed<'de>>(self, seed: S) -> Result<(S::Value, Self), Error> {
        let name = seed.deserialize(BorrowedStrDeserializer::<Error>::new(self.name))?;
        Ok((name, self))
    }
}

impl<'de> VariantAccess<'de> for Variant<'_> {
    type Error = Error;

    fn unit_variant(self) -> Result<(), Error> {
        Ok(())
    }

    fn newtype_variant_seed<S: DeserializeSeed<'de>>(mut self, seed: S) -> Result<S::Value, Error> {
        seed.deserialize(self.parts.next())
    }

    fn tuple_variant<V: Visitor<'de>>(mut self, len: usize, visitor: V) -> Result<V::Value, Error> {
        self.parts.limit = len;
        visitor.visit_seq(self.parts)
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_seq(self.parts)
    }
}

// ═══════════════════════════════════════════════════════════════════
// RECORDER: writes down what the built values send
// ═══════════════════════════════════════════════════════════════════

/// The wire shape of a part of the contract, as far as recorded.
#[derive(Clone, Debug, PartialEq)]
enum Shape {
    /// Never sent: a `None`, an empty sequence, a skipped field.
    Unknown,
    Prim(&'static str),
    Option(Box<Shape>),
    Seq(Box<Shape>),
    Map(Box<Shape>, Box<Shape>),
    Tuple(Vec<Shape>),
    Def(Key),
}

/// A container: its serde name, and its Rust type when other types share
/// that name, so they are recorded apart. A name used by one type is
/// recorded under the name alone, so `T` and `Box<T>` stay one container.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Key {
    name: &'static str,
    ty: Option<TypeId>,
}

type Fields = Vec<(&'static str, Shape)>;

#[derive(Clone, Debug)]
enum Def {
    UnitStruct,
    Newtype(Shape),
    TupleStruct(Vec<Shape>),
    Struct(Fields),
    /// By the variant index serde emits.
    Enum(BTreeMap<u32, (&'static str, Payload)>),
}

#[derive(Clone, Debug)]
enum Payload {
    Unit,
    Newtype(Shape),
    Tuple(Vec<Shape>),
    Struct(Fields),
}

type Defs = HashMap<Key, Def>;

fn conflict(what: &str) -> Error {
    Error(format!(
        "{what} is sent with different shapes from value to value"
    ))
}

fn merge_shape(into: &mut Shape, from: Shape) -> Result<(), Error> {
    if matches!(from, Shape::Unknown) {
        return Ok(());
    }
    if matches!(into, Shape::Unknown) {
        *into = from;
        return Ok(());
    }
    match (into, from) {
        (Shape::Prim(a), Shape::Prim(b)) if *a == b => Ok(()),
        (Shape::Def(a), Shape::Def(b)) if *a == b => Ok(()),
        (Shape::Option(a), Shape::Option(b)) | (Shape::Seq(a), Shape::Seq(b)) => merge_shape(a, *b),
        (Shape::Map(ak, av), Shape::Map(bk, bv)) => {
            merge_shape(ak, *bk)?;
            merge_shape(av, *bv)
        }
        (Shape::Tuple(a), Shape::Tuple(b)) => merge_shapes(a, b),
        _ => Err(conflict("a part")),
    }
}

fn merge_shapes(into: &mut [Shape], from: Vec<Shape>) -> Result<(), Error> {
    if into.len() != from.len() {
        return Err(conflict("a tuple"));
    }
    into.iter_mut()
        .zip(from)
        .try_for_each(|(a, b)| merge_shape(a, b))
}

fn merge_fields(into: &mut Fields, from: Fields) -> Result<(), Error> {
    if into.len() != from.len() || into.iter().zip(&from).any(|(a, b)| a.0 != b.0) {
        return Err(conflict("a struct"));
    }
    into.iter_mut()
        .zip(from)
        .try_for_each(|(a, b)| merge_shape(&mut a.1, b.1))
}

fn merge_payload(into: &mut Payload, from: Payload) -> Result<(), Error> {
    match (into, from) {
        (Payload::Unit, Payload::Unit) => Ok(()),
        (Payload::Newtype(a), Payload::Newtype(b)) => merge_shape(a, b),
        (Payload::Tuple(a), Payload::Tuple(b)) => merge_shapes(a, b),
        (Payload::Struct(a), Payload::Struct(b)) => merge_fields(a, b),
        _ => Err(conflict("a variant")),
    }
}

fn merge_def(defs: &mut Defs, key: Key, def: Def) -> Result<(), Error> {
    let Some(into) = defs.get_mut(&key) else {
        defs.insert(key, def);
        return Ok(());
    };
    let what = || conflict(&format!("`{}`", key.name));
    match (into, def) {
        (Def::UnitStruct, Def::UnitStruct) => Ok(()),
        (Def::Newtype(a), Def::Newtype(b)) => merge_shape(a, b),
        (Def::TupleStruct(a), Def::TupleStruct(b)) => merge_shapes(a, b),
        (Def::Struct(a), Def::Struct(b)) => merge_fields(a, b),
        (Def::Enum(a), Def::Enum(b)) => {
            for (index, (variant, payload)) in b {
                match a.get_mut(&index) {
                    None => {
                        a.insert(index, (variant, payload));
                    }
                    Some((v, p)) if *v == variant => merge_payload(p, payload)?,
                    Some(_) => return Err(what()),
                }
            }
            Ok(())
        }
        _ => Err(what()),
    }
    .map_err(|_| what())
}

/// What the recorder writes down, for one mode.
struct Recording {
    defs: Defs,
    readable: bool,
    /// Serde names that more than one Rust type uses.
    shared: HashSet<&'static str>,
}

/// Serializes one part of a contract value, writing down its shape.
struct Recorder<'a> {
    cx: &'a mut Recording,
    /// The Rust type of the value being serialized.
    ty: TypeId,
    out: &'a mut Shape,
}

impl Recorder<'_> {
    fn def(self, name: &'static str, def: Def) -> Result<(), Error> {
        let ty = self.cx.shared.contains(name).then_some(self.ty);
        let key = Key { name, ty };
        merge_def(&mut self.cx.defs, key, def)?;
        *self.out = Shape::Def(key);
        Ok(())
    }

    fn variant(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
        payload: Payload,
    ) -> Result<(), Error> {
        self.def(
            name,
            Def::Enum(BTreeMap::from([(index, (variant, payload))])),
        )
    }
}

fn record<T: ?Sized + Serialize>(cx: &mut Recording, value: &T) -> Result<Shape, Error> {
    let mut out = Shape::Unknown;
    value.serialize(Recorder {
        cx,
        ty: typeid::of::<T>(),
        out: &mut out,
    })?;
    Ok(out)
}

macro_rules! prim {
    ($($method:ident($ty:ty) => $name:literal;)*) => {
        $(fn $method(self, _: $ty) -> Result<(), Error> {
            *self.out = Shape::Prim($name);
            Ok(())
        })*
    };
}

impl<'a> ser::Serializer for Recorder<'a> {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = Compound<'a>;
    type SerializeTuple = Compound<'a>;
    type SerializeTupleStruct = Compound<'a>;
    type SerializeTupleVariant = Compound<'a>;
    type SerializeMap = Compound<'a>;
    type SerializeStruct = Compound<'a>;
    type SerializeStructVariant = Compound<'a>;

    prim! {
        serialize_bool(bool) => "bool";
        serialize_i8(i8) => "i8";
        serialize_i16(i16) => "i16";
        serialize_i32(i32) => "i32";
        serialize_i64(i64) => "i64";
        serialize_i128(i128) => "i128";
        serialize_u8(u8) => "u8";
        serialize_u16(u16) => "u16";
        serialize_u32(u32) => "u32";
        serialize_u64(u64) => "u64";
        serialize_u128(u128) => "u128";
        serialize_f32(f32) => "f32";
        serialize_f64(f64) => "f64";
        serialize_char(char) => "char";
        serialize_str(&str) => "str";
        serialize_bytes(&[u8]) => "bytes";
    }

    fn serialize_unit(self) -> Result<(), Error> {
        *self.out = Shape::Prim("unit");
        Ok(())
    }

    fn serialize_none(self) -> Result<(), Error> {
        *self.out = Shape::Option(Box::new(Shape::Unknown));
        Ok(())
    }

    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<(), Error> {
        *self.out = Shape::Option(Box::new(record(self.cx, value)?));
        Ok(())
    }

    fn serialize_unit_struct(self, name: &'static str) -> Result<(), Error> {
        self.def(name, Def::UnitStruct)
    }

    fn serialize_unit_variant(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
    ) -> Result<(), Error> {
        self.variant(name, index, variant, Payload::Unit)
    }

    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        name: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        let inner = record(self.cx, value)?;
        self.def(name, Def::Newtype(inner))
    }

    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        let inner = record(self.cx, value)?;
        self.variant(name, index, variant, Payload::Newtype(inner))
    }

    fn serialize_seq(self, _: Option<usize>) -> Result<Compound<'a>, Error> {
        Ok(Compound::new(self, Kind::Seq))
    }

    fn serialize_tuple(self, _: usize) -> Result<Compound<'a>, Error> {
        Ok(Compound::new(self, Kind::Tuple))
    }

    fn serialize_tuple_struct(self, name: &'static str, _: usize) -> Result<Compound<'a>, Error> {
        Ok(Compound::new(self, Kind::TupleStruct(name)))
    }

    fn serialize_tuple_variant(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Compound<'a>, Error> {
        Ok(Compound::new(
            self,
            Kind::TupleVariant(name, index, variant),
        ))
    }

    fn serialize_map(self, _: Option<usize>) -> Result<Compound<'a>, Error> {
        Ok(Compound::new(self, Kind::Map))
    }

    fn serialize_struct(self, name: &'static str, _: usize) -> Result<Compound<'a>, Error> {
        Ok(Compound::new(self, Kind::Struct(name)))
    }

    fn serialize_struct_variant(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Compound<'a>, Error> {
        Ok(Compound::new(
            self,
            Kind::StructVariant(name, index, variant),
        ))
    }

    fn is_human_readable(&self) -> bool {
        self.cx.readable
    }
}

enum Kind {
    Seq,
    Tuple,
    Map,
    TupleStruct(&'static str),
    Struct(&'static str),
    TupleVariant(&'static str, u32, &'static str),
    StructVariant(&'static str, u32, &'static str),
}

/// A sequence, tuple, map, or struct-like container being serialized.
struct Compound<'a> {
    rec: Recorder<'a>,
    kind: Kind,
    /// Positional parts, named for structs.
    parts: Fields,
    /// Sequence elements and map keys and values, merged.
    keys: Shape,
    values: Shape,
}

impl<'a> Compound<'a> {
    fn new(rec: Recorder<'a>, kind: Kind) -> Self {
        Compound {
            rec,
            kind,
            parts: Vec::new(),
            keys: Shape::Unknown,
            values: Shape::Unknown,
        }
    }

    fn part<T: ?Sized + Serialize>(&mut self, name: &'static str, value: &T) -> Result<(), Error> {
        let shape = record(self.rec.cx, value)?;
        self.parts.push((name, shape));
        Ok(())
    }

    fn finish(self) -> Result<(), Error> {
        let positional = || self.parts.iter().map(|(_, s)| s.clone()).collect();
        match self.kind {
            Kind::Seq => {
                *self.rec.out = Shape::Seq(Box::new(self.values));
                Ok(())
            }
            Kind::Tuple => {
                *self.rec.out = Shape::Tuple(positional());
                Ok(())
            }
            Kind::Map => {
                *self.rec.out = Shape::Map(Box::new(self.keys), Box::new(self.values));
                Ok(())
            }
            Kind::TupleStruct(name) => {
                let parts = positional();
                self.rec.def(name, Def::TupleStruct(parts))
            }
            Kind::Struct(name) => self.rec.def(name, Def::Struct(self.parts)),
            Kind::TupleVariant(name, index, variant) => {
                let parts = positional();
                self.rec
                    .variant(name, index, variant, Payload::Tuple(parts))
            }
            Kind::StructVariant(name, index, variant) => {
                self.rec
                    .variant(name, index, variant, Payload::Struct(self.parts))
            }
        }
    }
}

impl ser::SerializeSeq for Compound<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
        let shape = record(self.rec.cx, value)?;
        merge_shape(&mut self.values, shape)
    }

    fn end(self) -> Result<(), Error> {
        self.finish()
    }
}

impl ser::SerializeTuple for Compound<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
        self.part("", value)
    }

    fn end(self) -> Result<(), Error> {
        self.finish()
    }
}

impl ser::SerializeTupleStruct for Compound<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
        self.part("", value)
    }

    fn end(self) -> Result<(), Error> {
        self.finish()
    }
}

impl ser::SerializeTupleVariant for Compound<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
        self.part("", value)
    }

    fn end(self) -> Result<(), Error> {
        self.finish()
    }
}

impl ser::SerializeMap for Compound<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<(), Error> {
        let shape = record(self.rec.cx, key)?;
        merge_shape(&mut self.keys, shape)
    }

    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
        let shape = record(self.rec.cx, value)?;
        merge_shape(&mut self.values, shape)
    }

    fn end(self) -> Result<(), Error> {
        self.finish()
    }
}

impl ser::SerializeStruct for Compound<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.part(key, value)
    }

    fn skip_field(&mut self, key: &'static str) -> Result<(), Error> {
        self.parts.push((key, Shape::Unknown));
        Ok(())
    }

    fn end(self) -> Result<(), Error> {
        self.finish()
    }
}

impl ser::SerializeStructVariant for Compound<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.part(key, value)
    }

    fn skip_field(&mut self, key: &'static str) -> Result<(), Error> {
        self.parts.push((key, Shape::Unknown));
        Ok(())
    }

    fn end(self) -> Result<(), Error> {
        self.finish()
    }
}

// ═══════════════════════════════════════════════════════════════════
// CANONICAL TEXT
// ═══════════════════════════════════════════════════════════════════

fn render(shape: &Shape, defs: &Defs) -> Result<String, String> {
    let mut w = Writer {
        defs,
        stack: Vec::new(),
        out: String::new(),
        unknown: false,
    };
    w.shape(shape);
    if w.unknown {
        return Err(format!(
            "a part (`?`) was never sent; is it skipped for every value? {}",
            w.out
        ));
    }
    Ok(w.out)
}

struct Writer<'a> {
    defs: &'a Defs,
    stack: Vec<Key>,
    out: String,
    unknown: bool,
}

impl Writer<'_> {
    fn shape(&mut self, shape: &Shape) {
        match shape {
            Shape::Unknown => {
                self.unknown = true;
                self.out.push('?');
            }
            Shape::Prim(p) => self.out.push_str(p),
            Shape::Option(x) => self.wrap("option<", x, ">"),
            Shape::Seq(x) => self.wrap("seq<", x, ">"),
            Shape::Map(k, v) => {
                self.out.push_str("map<");
                self.shape(k);
                self.out.push_str(", ");
                self.shape(v);
                self.out.push('>');
            }
            Shape::Tuple(xs) if xs.len() >= 2 && xs.iter().all(|x| *x == xs[0]) => {
                self.out.push_str("array<");
                self.shape(&xs[0]);
                let _ = write!(self.out, "; {}>", xs.len());
            }
            Shape::Tuple(xs) => {
                self.out.push_str("tuple");
                self.tuple(xs);
            }
            Shape::Def(key) => self.def(*key),
        }
    }

    fn wrap(&mut self, open: &str, inner: &Shape, close: &str) {
        self.out.push_str(open);
        self.shape(inner);
        self.out.push_str(close);
    }

    fn def(&mut self, key: Key) {
        if let Some(pos) = self.stack.iter().rposition(|k| *k == key) {
            let _ = write!(self.out, "rec({})", self.stack.len() - pos);
            return;
        }
        let Some(def) = self.defs.get(&key) else {
            self.unknown = true;
            self.out.push('?');
            return;
        };
        self.stack.push(key);
        match def {
            Def::UnitStruct => self.out.push_str("unit_struct"),
            Def::Newtype(inner) => self.wrap("newtype(", inner, ")"),
            Def::TupleStruct(xs) => {
                self.out.push_str("tuple_struct");
                self.tuple(xs);
            }
            Def::Struct(fields) => {
                self.out.push_str("struct ");
                self.fields(fields);
            }
            Def::Enum(variants) => {
                self.out.push_str("enum {");
                for (i, (index, (variant, payload))) in variants.iter().enumerate() {
                    self.out.push_str(if i == 0 { " " } else { ", " });
                    let _ = write!(self.out, "{index} {variant}");
                    match payload {
                        Payload::Unit => {}
                        Payload::Newtype(inner) => self.wrap("(", inner, ")"),
                        Payload::Tuple(xs) => self.tuple(xs),
                        Payload::Struct(fields) => {
                            self.out.push(' ');
                            self.fields(fields);
                        }
                    }
                }
                self.out
                    .push_str(if variants.is_empty() { "}" } else { " }" });
            }
        }
        self.stack.pop();
    }

    /// `(A, B)`
    fn tuple(&mut self, xs: &[Shape]) {
        self.out.push('(');
        for (i, x) in xs.iter().enumerate() {
            if i > 0 {
                self.out.push_str(", ");
            }
            self.shape(x);
        }
        self.out.push(')');
    }

    /// `{ a: T, b: U }`, or `{}`
    fn fields(&mut self, fields: &Fields) {
        self.out.push('{');
        for (i, (name, shape)) in fields.iter().enumerate() {
            self.out.push_str(if i == 0 { " " } else { ", " });
            let _ = write!(self.out, "{name}: ");
            self.shape(shape);
        }
        self.out
            .push_str(if fields.is_empty() { "}" } else { " }" });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    struct Plain {
        schema_version: u32,
        celsius: f64,
        timestamp: u64,
    }

    #[test]
    fn struct_fields_in_wire_order() {
        assert_eq!(
            shape_of::<Plain>().unwrap(),
            "struct { schema_version: u32, celsius: f64, timestamp: u64 }"
        );
    }

    #[derive(Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Renamed {
        schema_version: u32,
        #[serde(rename = "c")]
        celsius: f64,
        #[serde(alias = "t", alias = "zz")]
        temp: u8,
        #[serde(rename(serialize = "out", deserialize = "in"))]
        both: u8,
    }

    #[test]
    fn wire_names_after_rename_and_alias() {
        assert_eq!(
            shape_of::<Renamed>().unwrap(),
            "struct { schemaVersion: u32, c: f64, temp: u8, out: u8 }"
        );
    }

    #[derive(Serialize, Deserialize)]
    struct Compatible {
        schema_version: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        celsius: Option<f64>,
        #[serde(default, skip_serializing_if = "is_zero")]
        count: u32,
    }

    fn is_zero(n: &u32) -> bool {
        *n == 0
    }

    #[derive(Serialize, Deserialize)]
    struct CompatibleBare {
        schema_version: u32,
        celsius: Option<f64>,
        count: u32,
    }

    #[test]
    fn default_and_skip_serializing_if_do_not_show() {
        assert_eq!(
            shape_of::<Compatible>().unwrap(),
            shape_of::<CompatibleBare>().unwrap()
        );
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
        assert_eq!(shape_of::<Skipped>().unwrap(), "struct { a: u8, c: u8 }");
    }

    #[derive(Serialize, Deserialize)]
    enum Mode {
        Off,
        Level(u8),
        Pair(u8, u8),
        Target { celsius: f32 },
    }

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
            shape_of::<Nested>().unwrap(),
            "struct { \
             mode: enum { 0 Off, 1 Level(u8), 2 Pair(u8, u8), 3 Target { celsius: f32 } }, \
             inner: struct { schema_version: u32, celsius: f64, timestamp: u64 }, \
             unit: unit_struct, \
             id: newtype(u32), \
             at: tuple_struct(i16, i16), \
             label: option<str>, \
             samples: seq<f32>, \
             tags: map<str, bool>, \
             pair: tuple(u8, char), \
             raw: array<u8; 4>, \
             any: unit, \
             boxed: i64 \
             }"
        );
    }

    #[test]
    fn top_level_enum_has_every_variant() {
        assert_eq!(
            shape_of::<Mode>().unwrap(),
            "enum { 0 Off, 1 Level(u8), 2 Pair(u8, u8), 3 Target { celsius: f32 } }"
        );
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
    fn enums_behind_options_sequences_maps_and_variants() {
        let inner = "enum { 0 A, 1 B(u8), 2 C }";
        assert_eq!(
            shape_of::<Deep>().unwrap(),
            format!(
                "struct {{ outer: enum {{ 0 X, 1 Y({inner}) }}, again: seq<{inner}>, \
                 maybe: option<{inner}>, keyed: map<u8, {inner}> }}"
            )
        );
    }

    #[derive(Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Leaf {
        First,
        #[serde(alias = "two")]
        SecondOne(u8),
        Third {
            x: i8,
        },
    }

    #[derive(Serialize, Deserialize)]
    enum Middle {
        None,
        Some(Option<Leaf>),
    }

    #[derive(Serialize, Deserialize)]
    struct OnlyThroughVariants {
        middle: Middle,
    }

    #[test]
    fn enum_reached_only_through_a_variant() {
        assert_eq!(
            shape_of::<OnlyThroughVariants>().unwrap(),
            "struct { middle: enum { 0 None, 1 Some(option<\
             enum { 0 first, 1 second_one(u8), 2 third { x: i8 } }>) } }"
        );
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
    fn variant_index_is_the_one_sent() {
        assert_eq!(shape_of::<WithSkipped>().unwrap(), "enum { 0 A, 2 C(u8) }");
    }

    #[derive(Serialize, Deserialize)]
    struct Tree {
        value: u8,
        children: Vec<Tree>,
    }

    #[test]
    fn recursion_refers_up() {
        assert_eq!(
            shape_of::<Tree>().unwrap(),
            "struct { value: u8, children: seq<rec(1)> }"
        );
    }

    #[derive(Serialize, Deserialize)]
    enum Expr {
        Add(Box<Expr>, Box<Expr>),
        Neg(Box<Expr>),
        Lit(u8),
    }

    #[test]
    fn recursion_through_variants_ends() {
        assert_eq!(
            shape_of::<Expr>().unwrap(),
            "enum { 0 Add(rec(1), rec(1)), 1 Neg(rec(1)), 2 Lit(u8) }"
        );
    }

    #[derive(Serialize, Deserialize)]
    struct Flattened {
        #[serde(flatten)]
        inner: Plain,
    }

    #[test]
    fn flatten_is_untraceable() {
        let err = shape_of::<Flattened>().unwrap_err();
        assert!(err.contains("deserialize_identifier"), "{err}");
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
    fn tagged_and_untagged_enums_are_untraceable() {
        assert!(shape_of::<Tagged>()
            .unwrap_err()
            .contains("deserialize_any"));
        assert!(shape_of::<Untagged>()
            .unwrap_err()
            .contains("deserialize_any"));
    }

    mod a {
        #[derive(serde::Serialize, serde::Deserialize)]
        pub enum Status {
            Ready,
        }

        #[derive(serde::Serialize, serde::Deserialize)]
        pub struct Point {
            pub x: u8,
        }
    }

    mod b {
        #[derive(serde::Serialize, serde::Deserialize)]
        pub enum Status {
            Ready,
            Failed,
        }

        #[derive(serde::Serialize, serde::Deserialize)]
        pub struct Point {
            pub y: u8,
        }
    }

    #[derive(Serialize, Deserialize)]
    struct TwoStatus {
        a: a::Status,
        b: b::Status,
    }

    #[test]
    fn enums_of_one_name_stay_apart() {
        assert_eq!(
            shape_of::<TwoStatus>().unwrap(),
            "struct { a: enum { 0 Ready }, b: enum { 0 Ready, 1 Failed } }"
        );
    }

    #[derive(Serialize, Deserialize)]
    struct TwoPoints {
        a: a::Point,
        b: Option<b::Point>,
    }

    #[test]
    fn structs_of_one_name_stay_apart() {
        assert_eq!(
            shape_of::<TwoPoints>().unwrap(),
            "struct { a: struct { x: u8 }, b: option<struct { y: u8 }> }"
        );
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
    fn generic_instances_stay_apart() {
        assert_eq!(
            shape_of::<Readings>().unwrap(),
            "struct { celsius: struct { value: f32 }, count: struct { value: u8 } }"
        );
    }

    #[derive(Serialize, Deserialize)]
    struct Boxed {
        plain: Mode,
        boxed: Box<Mode>,
    }

    #[test]
    fn a_boxed_type_is_the_same_container() {
        let mode = "enum { 0 Off, 1 Level(u8), 2 Pair(u8, u8), 3 Target { celsius: f32 } }";
        assert_eq!(
            shape_of::<Boxed>().unwrap(),
            format!("struct {{ plain: {mode}, boxed: {mode} }}")
        );
    }

    #[derive(Serialize, Deserialize)]
    struct Octets {
        ip: [u8; 4],
    }

    #[derive(Serialize, Deserialize)]
    struct Address {
        ip: std::net::Ipv4Addr,
    }

    #[test]
    fn human_readable_shape_is_part_of_the_text() {
        assert_eq!(shape_of::<Octets>().unwrap(), "struct { ip: array<u8; 4> }");
        assert_eq!(
            shape_of::<Address>().unwrap(),
            "binary: struct { ip: array<u8; 4> }; readable: struct { ip: str }"
        );
    }

    #[test]
    fn hash_is_full_sha256() {
        assert_eq!(
            hash(""),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
