//! The PDF object model (ISO 32000-2 §7.3).

use std::fmt;
use std::sync::Arc;

/// An indirect reference `num generation R`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjRef {
    pub num: u32,
    pub generation: u16,
}

impl ObjRef {
    pub const fn new(num: u32, generation: u16) -> Self {
        ObjRef { num, generation }
    }
}

impl fmt::Debug for ObjRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} R", self.num, self.generation)
    }
}

/// A name, without the slash and with `#xx` escapes decoded.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Name(pub Box<[u8]>);

impl Name {
    pub fn new(b: &[u8]) -> Self {
        Name(b.into())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl PartialEq<[u8]> for Name {
    fn eq(&self, o: &[u8]) -> bool {
        *self.0 == *o
    }
}

impl<const N: usize> PartialEq<&[u8; N]> for Name {
    fn eq(&self, o: &&[u8; N]) -> bool {
        *self.0 == o[..]
    }
}

impl fmt::Debug for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "/{}", self.0.escape_ascii())
    }
}

/// A string object. `hex` records the source form so edits round-trip.
#[derive(Clone, PartialEq, Eq, Hash, Default)]
pub struct PdfString {
    pub bytes: Vec<u8>,
    pub hex: bool,
}

impl fmt::Debug for PdfString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({})", self.bytes.escape_ascii())
    }
}

/// A dictionary. Kept as a small vector: real dictionaries have a handful of keys, and
/// linear search beats hashing there while preserving key order for saving.
#[derive(Clone, PartialEq, Default)]
pub struct Dict(pub Vec<(Name, Object)>);

impl Dict {
    pub fn new() -> Self {
        Dict(Vec::new())
    }

    pub fn get(&self, key: &[u8]) -> Option<&Object> {
        self.0.iter().find(|(k, _)| *k.0 == *key).map(|(_, v)| v)
    }

    pub fn get_mut(&mut self, key: &[u8]) -> Option<&mut Object> {
        self.0
            .iter_mut()
            .find(|(k, _)| *k.0 == *key)
            .map(|(_, v)| v)
    }

    pub fn contains(&self, key: &[u8]) -> bool {
        self.get(key).is_some()
    }

    /// Inserts or replaces, keeping the position of an existing key.
    pub fn set(&mut self, key: &[u8], value: impl Into<Object>) {
        let value = value.into();
        match self.get_mut(key) {
            Some(v) => *v = value,
            None => self.0.push((Name::new(key), value)),
        }
    }

    pub fn remove(&mut self, key: &[u8]) -> Option<Object> {
        let i = self.0.iter().position(|(k, _)| *k.0 == *key)?;
        Some(self.0.remove(i).1)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Name, &Object)> {
        self.0.iter().map(|(k, v)| (k, v))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `/Type` (or `/Subtype` when asked) as bytes, if it is a direct name.
    pub fn name(&self, key: &[u8]) -> Option<&[u8]> {
        self.get(key).and_then(Object::as_name)
    }

    pub fn int(&self, key: &[u8]) -> Option<i64> {
        self.get(key).and_then(Object::as_int)
    }
}

impl fmt::Debug for Dict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(self.0.iter().map(|(k, v)| (k, v)))
            .finish()
    }
}

/// Where a stream's encoded bytes live.
#[derive(Clone, PartialEq)]
pub enum StreamData {
    /// A range of the file the document was opened from.
    Source(std::ops::Range<usize>),
    /// Bytes created or replaced by an edit.
    Owned(Arc<[u8]>),
}

impl fmt::Debug for StreamData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamData::Source(r) => write!(f, "source {r:?}"),
            StreamData::Owned(b) => write!(f, "{} owned bytes", b.len()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stream {
    pub dict: Dict,
    pub data: StreamData,
}

impl Stream {
    /// A new stream; `/Length` is written on save.
    pub fn new(dict: Dict, data: impl Into<Arc<[u8]>>) -> Self {
        Stream {
            dict,
            data: StreamData::Owned(data.into()),
        }
    }
}

#[derive(Clone, PartialEq, Default)]
pub enum Object {
    #[default]
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    String(PdfString),
    Name(Name),
    Array(Vec<Object>),
    Dict(Dict),
    Stream(Stream),
    Ref(ObjRef),
}

impl Object {
    pub fn name(b: &[u8]) -> Self {
        Object::Name(Name::new(b))
    }

    pub fn string(b: &[u8]) -> Self {
        Object::String(PdfString {
            bytes: b.to_vec(),
            hex: false,
        })
    }

    pub fn as_int(&self) -> Option<i64> {
        match *self {
            Object::Int(i) => Some(i),
            // Some writers emit integral reals where integers are required.
            Object::Real(r) if r.fract() == 0.0 && r.abs() < 9.0e15 => Some(r as i64),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Object::Int(i) => Some(i as f64),
            Object::Real(r) => Some(r),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match *self {
            Object::Bool(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_name(&self) -> Option<&[u8]> {
        match self {
            Object::Name(n) => Some(&n.0),
            _ => None,
        }
    }

    pub fn as_string(&self) -> Option<&[u8]> {
        match self {
            Object::String(s) => Some(&s.bytes),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Object]> {
        match self {
            Object::Array(a) => Some(a),
            _ => None,
        }
    }

    /// The dictionary of a dictionary or of a stream.
    pub fn as_dict(&self) -> Option<&Dict> {
        match self {
            Object::Dict(d) => Some(d),
            Object::Stream(s) => Some(&s.dict),
            _ => None,
        }
    }

    pub fn as_stream(&self) -> Option<&Stream> {
        match self {
            Object::Stream(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_objref(&self) -> Option<ObjRef> {
        match *self {
            Object::Ref(r) => Some(r),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Object::Null)
    }
}

impl fmt::Debug for Object {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Object::Null => f.write_str("null"),
            Object::Bool(b) => write!(f, "{b}"),
            Object::Int(i) => write!(f, "{i}"),
            Object::Real(r) => write!(f, "{r}"),
            Object::String(s) => s.fmt(f),
            Object::Name(n) => n.fmt(f),
            Object::Array(a) => f.debug_list().entries(a).finish(),
            Object::Dict(d) => d.fmt(f),
            Object::Stream(s) => write!(f, "{:?} stream {:?}", s.dict, s.data),
            Object::Ref(r) => r.fmt(f),
        }
    }
}

macro_rules! from {
    ($($t:ty => $v:ident),*) => {$(
        impl From<$t> for Object {
            fn from(x: $t) -> Self {
                Object::$v(x.into())
            }
        }
    )*};
}

from!(bool => Bool, i64 => Int, i32 => Int, u32 => Int, f64 => Real, Name => Name,
    Vec<Object> => Array, Dict => Dict, Stream => Stream, ObjRef => Ref, PdfString => String);
