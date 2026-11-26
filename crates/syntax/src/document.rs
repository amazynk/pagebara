//! The object store: a lazily parsed, immutable base file under a persistent edit overlay.
//!
//! Cloning a [`Document`] is O(1) and yields an independent snapshot, which is what undo,
//! revision history and background saving build on.

use std::ops::Deref;
use std::sync::{Arc, OnceLock};

use crate::crypt::{Handler, Permissions, Security};
use crate::object::{Dict, ObjRef, Object, Stream, StreamData};
use crate::objstm::ObjStm;
use crate::parser::Parser;
use crate::stream::{self, DecodedStream};
use crate::xref::{self, Entry, Kind, NumMap, Xref};
use crate::{Error, Result};

/// Reference chains (e.g. `/Length` of `/Length`) deeper than this resolve to null.
const MAX_DEPTH: u32 = 32;

#[derive(Debug)]
struct Slot {
    entry: Entry,
    obj: OnceLock<Option<Arc<Object>>>,
    /// Set when this object is an object stream that was opened.
    objstm: OnceLock<Option<Arc<ObjStm>>>,
}

#[derive(Debug)]
struct Table {
    slots: NumMap<Slot>,
    trailer: Dict,
    kind: Kind,
    startxref: usize,
}

/// The largest object number (ISO 32000-2 Annex C); objects above it are ignored, as
/// other readers do, so that a cross-reference written for the file stays small.
pub(crate) const MAX_OBJECT_NUMBER: u32 = 8_388_607;

impl From<Xref> for Table {
    fn from(x: Xref) -> Self {
        let slots = x
            .entries
            .into_iter()
            .filter(|&(n, _)| n <= MAX_OBJECT_NUMBER)
            .map(|(n, entry)| {
                let slot = Slot {
                    entry,
                    obj: OnceLock::new(),
                    objstm: OnceLock::new(),
                };
                (n, slot)
            })
            .collect();
        Table {
            slots,
            trailer: x.trailer,
            kind: x.kind,
            startxref: x.startxref,
        }
    }
}

#[derive(Debug)]
struct Base {
    src: Vec<u8>,
    /// Offset of `%PDF-`.
    shift: usize,
    version: (u8, u8),
    table: Table,
    /// Built on first need when the file's own table proves wrong.
    repaired: OnceLock<Table>,
    /// Set once a password unlocked an encrypted file.
    security: OnceLock<Security>,
    /// The `/Encrypt` dictionary's object, which is never encrypted.
    encrypt_num: Option<u32>,
    handler: OnceLock<Handler>,
}

/// A replaced object (generation and value) or, as `None`, a deleted one.
pub(crate) type Edit = Option<(u16, Arc<Object>)>;

/// Edits on top of the base. Persistent maps make snapshots cheap.
#[derive(Clone, Debug, Default)]
struct Overlay {
    /// `None` deletes the object.
    objects: imbl::HashMap<u32, Edit>,
    trailer: Option<Arc<Dict>>,
    next_num: u32,
    /// Save without encryption.
    strip_security: bool,
    /// The owner password was confirmed after opening.
    owner: bool,
    /// Protection set by [`Document::set_password`], with its `/Encrypt` object number.
    new_security: Option<(Arc<Security>, u32)>,
}

#[derive(Clone, Debug)]
pub struct Document {
    base: Arc<Base>,
    overlay: Overlay,
}

/// An object that is either borrowed in place or shared from the store.
#[derive(Debug, Clone)]
pub enum Resolved<'a> {
    Borrowed(&'a Object),
    Shared(Arc<Object>),
}

impl Deref for Resolved<'_> {
    type Target = Object;

    fn deref(&self) -> &Object {
        match self {
            Resolved::Borrowed(o) => o,
            Resolved::Shared(o) => o,
        }
    }
}

static NULL: Object = Object::Null;

/// An encrypted document waiting for its password.
#[derive(Debug)]
pub struct Locked {
    doc: Document,
    handler: Handler,
}

impl Locked {
    /// Opens with the user or owner password as typed (any Unicode), or hands the
    /// document back.
    pub fn unlock(self: Box<Self>, password: &str) -> std::result::Result<Document, Box<Locked>> {
        let sec = self.handler.unlock_str(password);
        self.finish(sec)
    }

    /// Like [`Locked::unlock`] with the exact password bytes.
    pub fn unlock_bytes(
        self: Box<Self>,
        password: &[u8],
    ) -> std::result::Result<Document, Box<Locked>> {
        let sec = self.handler.unlock(password);
        self.finish(sec)
    }

    fn finish(
        self: Box<Self>,
        sec: Option<Security>,
    ) -> std::result::Result<Document, Box<Locked>> {
        match sec {
            Some(sec) => {
                let _ = self.doc.base.security.set(sec);
                let _ = self.doc.base.handler.set(self.handler);
                Ok(self.doc)
            }
            None => Err(self),
        }
    }
}

impl Document {
    /// Opens a file from its bytes. Structural damage is repaired; only a file with no
    /// recognisable PDF objects fails.
    pub fn open(src: Vec<u8>) -> Result<Self> {
        let header = memchr::memmem::find(&src[..src.len().min(1024)], b"%PDF-");
        let shift = header.unwrap_or(0);
        let version = header.map_or((1, 7), |h| parse_version(&src[h + 5..]));
        let table = match xref::load(&src, shift) {
            Ok(x) if usable(&src, &x) => x,
            _ => crate::repair::repair(&src),
        };
        if table.entries.is_empty() {
            return Err(Error::NotPdf);
        }
        let table: Table = table.into();
        let encrypt_num = table
            .trailer
            .get(b"Encrypt")
            .and_then(Object::as_objref)
            .map(|r| r.num);
        let base = Base {
            src,
            shift,
            version,
            table,
            repaired: OnceLock::new(),
            security: OnceLock::new(),
            encrypt_num,
            handler: OnceLock::new(),
        };
        let next_num = base.max_num().saturating_add(1);
        let doc = Document {
            base: Arc::new(base),
            overlay: Overlay {
                next_num,
                ..Overlay::default()
            },
        };
        if !doc.is_encrypted() {
            return Ok(doc);
        }
        // An `/Encrypt` entry naming no dictionary is damage: read the file as unprotected,
        // as other readers do.
        if doc.get_in(doc.trailer(), b"Encrypt").as_dict().is_none() {
            let mut doc = doc;
            if let Some(base) = Arc::get_mut(&mut doc.base) {
                base.table.trailer.remove(b"Encrypt");
                base.encrypt_num = None;
            }
            return Ok(doc);
        }
        let handler = doc.security_handler()?;
        let locked = Box::new(Locked { doc, handler });
        // Most protected files only restrict permissions and open with the empty password.
        locked.unlock_bytes(b"").map_err(Error::PasswordRequired)
    }

    fn security_handler(&self) -> Result<Handler> {
        let enc = self.get_in(self.trailer(), b"Encrypt");
        let enc = enc.as_dict().ok_or(Error::UnsupportedEncryption)?;
        let id0 = self
            .trailer()
            .get(b"ID")
            .and_then(Object::as_array)
            .and_then(|a| a.first())
            .and_then(Object::as_string)
            .unwrap_or_default();
        Handler::new(enc, id0).map_err(|_| Error::UnsupportedEncryption)
    }

    /// What the file allows; everything for unencrypted files or the owner password.
    pub fn permissions(&self) -> Permissions {
        match self.base.security.get() {
            Some(s) if !self.overlay.owner && !self.overlay.strip_security => s.permissions,
            _ => Permissions::ALL,
        }
    }

    /// Unlocked with the owner password (full rights regardless of permissions).
    pub fn is_owner(&self) -> bool {
        self.overlay.owner || self.base.security.get().is_none_or(|s| s.owner)
    }

    /// Confirms the owner password of a document opened with the user password (often the
    /// empty one), granting full rights. Returns whether it was accepted.
    pub fn unlock_owner(&mut self, password: &str) -> bool {
        if self.is_owner() {
            return true;
        }
        let ok = self
            .base
            .handler
            .get()
            .and_then(|h| h.unlock_str(password))
            .is_some_and(|s| s.owner);
        self.overlay.owner |= ok;
        ok
    }

    /// Saves without password protection from now on (needs a full save).
    pub fn remove_security(&mut self) -> Result<()> {
        if !self.is_owner() {
            return Err(Error::OwnerPasswordRequired);
        }
        if let Some((_, num)) = self.overlay.new_security.take() {
            self.delete(ObjRef::new(num, 0));
        }
        let mut t = self.trailer().clone();
        t.remove(b"Encrypt");
        self.set_trailer(t);
        self.overlay.strip_security = true;
        Ok(())
    }

    /// Protects the document with AES-256 from the next full save: `user` opens it (empty
    /// for none) with `permissions`, `owner` opens it with all rights. Any Unicode password
    /// works; it is normalised with SASLprep as the standard requires.
    pub fn set_password(
        &mut self,
        user: &str,
        owner: &str,
        permissions: Permissions,
    ) -> Result<()> {
        if !self.is_owner() {
            return Err(Error::OwnerPasswordRequired);
        }
        let (sec, dict) =
            crate::crypt::protect(user, owner, permissions).map_err(|_| Error::InvalidPassword)?;
        if let Some((_, num)) = self.overlay.new_security.take() {
            self.delete(ObjRef::new(num, 0));
        }
        let r = self.add(dict);
        let mut t = self.trailer().clone();
        t.set(b"Encrypt", r);
        if !t
            .get(b"ID")
            .and_then(Object::as_array)
            .is_some_and(|a| a.len() == 2)
        {
            let mut id = [0u8; 16];
            let _ = getrandom::fill(&mut id);
            let s = Object::String(crate::PdfString {
                bytes: id.to_vec(),
                hex: true,
            });
            t.set(b"ID", vec![s.clone(), s]);
        }
        self.set_trailer(t);
        self.overlay.new_security = Some((Arc::new(sec), r.num));
        self.overlay.strip_security = false;
        Ok(())
    }

    /// The keys used when saving and the `/Encrypt` object to leave in clear: `None` for
    /// unprotected output.
    pub(crate) fn save_security(&self) -> Option<(&Security, Option<u32>)> {
        if let Some((sec, num)) = &self.overlay.new_security {
            return Some((sec, Some(*num)));
        }
        if self.overlay.strip_security {
            return None;
        }
        Some((self.base.security.get()?, self.base.encrypt_num))
    }

    /// The old `/Encrypt` object when protection was removed or replaced.
    pub(crate) fn stale_encrypt_num(&self) -> Option<u32> {
        self.security_changed()
            .then_some(self.base.encrypt_num)
            .flatten()
    }

    /// Protection differs from the file's, so every object must be rewritten.
    pub(crate) fn security_changed(&self) -> bool {
        self.overlay.new_security.is_some() || (self.overlay.strip_security && self.is_encrypted())
    }

    /// The bytes the document was opened from.
    pub fn source(&self) -> &[u8] {
        &self.base.src
    }

    /// Header version, overridden by a later catalog `/Version`.
    pub fn version(&self) -> (u8, u8) {
        let cat = self
            .catalog()
            .and_then(|c| c.as_dict()?.name(b"Version").map(parse_version));
        match cat {
            Some(v) if v > self.base.version => v,
            _ => self.base.version,
        }
    }

    /// Loads the objects of the file's own table and runs the fallback scan if one fails
    /// or refers to an object the table lacks, so that a full save lists every object.
    pub(crate) fn load_all(&self) {
        let t = &self.base.table;
        if t.kind == Kind::Repaired {
            return;
        }
        fn missing(o: &Object, slots: &NumMap<Slot>, depth: u32) -> bool {
            match o {
                Object::Ref(r) => !slots.contains_key(&r.num),
                Object::Array(a) if depth < 32 => a.iter().any(|o| missing(o, slots, depth + 1)),
                Object::Dict(d) if depth < 32 => {
                    d.iter().any(|(_, o)| missing(o, slots, depth + 1))
                }
                Object::Stream(s) => s.dict.iter().any(|(_, o)| missing(o, slots, depth + 1)),
                _ => false,
            }
        }
        for (&n, s) in &t.slots {
            if self.base.repaired.get().is_some() {
                return;
            }
            if s.entry == Entry::Free {
                continue;
            }
            match self.base.load(t, n, 0) {
                Some(o) if !missing(&o, &t.slots, 0) => {}
                _ => {
                    let _ = self
                        .base
                        .repaired
                        .get_or_init(|| crate::repair::repair(&self.base.src).into());
                }
            }
        }
    }

    /// The cross-reference was rebuilt by scanning the file.
    pub fn was_repaired(&self) -> bool {
        self.base.table.kind == Kind::Repaired || self.base.repaired.get().is_some()
    }

    pub fn is_encrypted(&self) -> bool {
        self.base.table.trailer.contains(b"Encrypt")
    }

    pub fn is_modified(&self) -> bool {
        !self.overlay.objects.is_empty() || self.overlay.trailer.is_some()
    }

    pub fn trailer(&self) -> &Dict {
        self.overlay
            .trailer
            .as_deref()
            .unwrap_or(&self.base.table.trailer)
    }

    pub fn catalog(&self) -> Option<Arc<Object>> {
        self.get(self.trailer().get(b"Root")?.as_objref()?)
    }

    /// The object `r` refers to; `None` for free, missing or unparsable objects (which PDF
    /// treats as null). Generation numbers are not enforced: many writers get them wrong.
    pub fn get(&self, r: ObjRef) -> Option<Arc<Object>> {
        match self.overlay.objects.get(&r.num) {
            Some(Some((_, o))) => Some(o.clone()),
            Some(None) => None,
            None => self.base.get(r.num, 0),
        }
    }

    /// Follows `o` if it is a reference.
    pub fn resolve<'a>(&self, o: &'a Object) -> Resolved<'a> {
        match o {
            Object::Ref(r) => self
                .get(*r)
                .map_or(Resolved::Borrowed(&NULL), Resolved::Shared),
            o => Resolved::Borrowed(o),
        }
    }

    /// `dict[key]`, resolved.
    pub fn get_in<'a>(&self, dict: &'a Dict, key: &[u8]) -> Resolved<'a> {
        dict.get(key)
            .map_or(Resolved::Borrowed(&NULL), |o| self.resolve(o))
    }

    /// A stream's encoded bytes.
    pub fn raw<'a>(&'a self, s: &'a Stream) -> &'a [u8] {
        match &s.data {
            StreamData::Source(r) => self.base.src.get(r.clone()).unwrap_or_default(),
            StreamData::Owned(b) => b,
        }
    }

    /// A stream's data through its byte filters; an image codec ending the chain is reported
    /// rather than applied.
    pub fn decode(&self, s: &Stream) -> Result<DecodedStream> {
        stream::decode(self.raw(s), &s.dict, &|o| (*self.resolve(o)).clone())
    }

    /// Scans the file for objects its own table leaves out, as looking up a missing object
    /// does. For a caller about to search every object: what [`Document::object_numbers`]
    /// lists must not depend on what was looked up before.
    pub fn scan_for_objects(&self) {
        self.base.scanned();
    }

    /// Every object number in use, ascending. Includes the objects the fallback scan
    /// found when the file's own table proved incomplete.
    pub fn object_numbers(&self) -> Vec<u32> {
        let base = &self.base.table.slots;
        let extra = self
            .base
            .repaired
            .get()
            .into_iter()
            .flat_map(|t| t.slots.iter())
            .filter(|(n, _)| !base.contains_key(n));
        let mut v: Vec<u32> = base
            .iter()
            .chain(extra)
            .filter(|(_, s)| s.entry != Entry::Free)
            .map(|(&n, _)| n)
            .filter(|n| !self.overlay.objects.contains_key(n))
            .chain(
                self.overlay
                    .objects
                    .iter()
                    .filter(|(_, o)| o.is_some())
                    .map(|(&n, _)| n),
            )
            .collect();
        v.sort_unstable();
        v
    }

    /// The reference currently naming object `num`, if it exists.
    pub fn reference(&self, num: u32) -> Option<ObjRef> {
        match self.overlay.objects.get(&num) {
            Some(Some((generation, _))) => Some(ObjRef::new(num, *generation)),
            Some(None) => None,
            None => self.base_reference(num),
        }
    }

    pub(crate) fn base_reference(&self, num: u32) -> Option<ObjRef> {
        let slot = self
            .base
            .table
            .slots
            .get(&num)
            .or_else(|| self.base.repaired.get()?.slots.get(&num))?;
        match slot.entry {
            Entry::Offset { generation, .. } => Some(ObjRef::new(num, generation)),
            Entry::InStream { .. } => Some(ObjRef::new(num, 0)),
            Entry::Free => None,
        }
    }

    // Editing.

    /// Replaces object `r`.
    pub fn set(&mut self, r: ObjRef, o: impl Into<Object>) {
        self.overlay.next_num = self.overlay.next_num.max(r.num.saturating_add(1));
        self.overlay
            .objects
            .insert(r.num, Some((r.generation, Arc::new(o.into()))));
    }

    /// Adds a new object and returns its reference.
    pub fn add(&mut self, o: impl Into<Object>) -> ObjRef {
        let r = ObjRef::new(self.fresh_num(), 0);
        self.set(r, o);
        r
    }

    /// The number the next new object gets.
    fn fresh_num(&mut self) -> u32 {
        // Objects found only by the fallback scan must not be shadowed by new ones.
        if let Some(t) = self.base.repaired.get() {
            let max = t.slots.keys().copied().max().unwrap_or(0);
            self.overlay.next_num = self.overlay.next_num.max(max.saturating_add(1));
        }
        self.overlay.next_num
    }

    pub fn delete(&mut self, r: ObjRef) {
        self.overlay.objects.insert(r.num, None);
    }

    pub fn set_trailer(&mut self, d: Dict) {
        self.overlay.trailer = Some(Arc::new(d));
    }

    pub(crate) fn root_is_object_zero(&self) -> bool {
        self.trailer()
            .get(b"Root")
            .and_then(Object::as_objref)
            .is_some_and(|r| r.num == 0)
    }

    /// Gives a catalog that repair found as object 0 an object of its own: object 0 is the
    /// head of the free list and cannot be written. Returns whether it moved.
    ///
    /// Its number is one that no reference names. Such a file is damaged, and a reference
    /// in it may name an object that is missing: a catalog given that number would answer
    /// to the reference, as its own page tree for one.
    pub fn move_root_off_zero(&mut self) -> bool {
        if !self.root_is_object_zero() {
            return false;
        }
        let Some(cat) = self.catalog() else {
            return false;
        };
        let from = self.fresh_num();
        let Some(num) = self.unnamed_num(from) else {
            return false;
        };
        let moved = ObjRef::new(num, 0);
        self.set(moved, (*cat).clone());
        let mut t = self.trailer().clone();
        t.set(b"Root", moved);
        self.set_trailer(t);
        true
    }

    /// Copies `r` and everything it reaches from `other` into this document, returning the
    /// new reference. `map` carries already-copied objects, so importing several pages that
    /// share fonts copies them once.
    pub fn import(&mut self, other: &Document, r: ObjRef, map: &mut NumMap<ObjRef>) -> ObjRef {
        if let Some(&n) = map.get(&r.num) {
            return n;
        }
        let new = self.add(Object::Null);
        map.insert(r.num, new);
        let o = other.get(r).map_or(Object::Null, |o| (*o).clone());
        let o = self.import_object(other, o, map);
        self.set(new, o);
        new
    }

    fn import_object(&mut self, other: &Document, o: Object, map: &mut NumMap<ObjRef>) -> Object {
        match o {
            Object::Ref(r) => Object::Ref(self.import(other, r, map)),
            Object::Array(a) => Object::Array(
                a.into_iter()
                    .map(|o| self.import_object(other, o, map))
                    .collect(),
            ),
            Object::Dict(d) => Object::Dict(self.import_dict(other, d, map)),
            Object::Stream(s) => {
                let data: Arc<[u8]> = other.raw(&s).into();
                Object::Stream(Stream {
                    dict: self.import_dict(other, s.dict, map),
                    data: StreamData::Owned(data),
                })
            }
            o => o,
        }
    }

    fn import_dict(&mut self, other: &Document, d: Dict, map: &mut NumMap<ObjRef>) -> Dict {
        Dict(
            d.0.into_iter()
                // A page's /Parent belongs to the source tree; the caller relinks it.
                .filter(|(k, _)| k != &b"Parent")
                .map(|(k, v)| (k, self.import_object(other, v, map)))
                .collect(),
        )
    }

    // For the writer.

    pub(crate) fn changes(&self) -> Vec<(u32, Edit)> {
        let mut v: Vec<_> = self
            .overlay
            .objects
            .iter()
            .map(|(&n, o)| (n, o.clone()))
            .collect();
        v.sort_unstable_by_key(|e| e.0);
        v
    }

    pub(crate) fn base_kind(&self) -> Kind {
        self.base.table.kind
    }

    pub(crate) fn base_startxref(&self) -> usize {
        self.base.table.startxref
    }

    pub(crate) fn next_num(&self) -> u32 {
        self.overlay.next_num
    }
}

impl Base {
    fn max_num(&self) -> u32 {
        self.table.slots.keys().copied().max().unwrap_or(0)
    }

    fn get(&self, num: u32, depth: u32) -> Option<Arc<Object>> {
        if depth > MAX_DEPTH {
            return None;
        }
        if let Some(o) = self.load(&self.table, num, depth) {
            return Some(o);
        }
        // A broken or missing entry: fall back to scanning the file, once.
        let free = self
            .table
            .slots
            .get(&num)
            .is_some_and(|s| s.entry == Entry::Free);
        if free {
            return None;
        }
        self.load(self.scanned()?, num, depth)
    }

    /// The table made by scanning the file, on first use; `None` when the file's own
    /// table is already one.
    fn scanned(&self) -> Option<&Table> {
        (self.table.kind != Kind::Repaired).then(|| {
            self.repaired
                .get_or_init(|| crate::repair::repair(&self.src).into())
        })
    }

    fn load(&self, t: &Table, num: u32, depth: u32) -> Option<Arc<Object>> {
        if depth > MAX_DEPTH {
            return None;
        }
        let slot = t.slots.get(&num)?;
        if let Some(o) = slot.obj.get() {
            return o.clone();
        }
        // Computed outside the cell: a self-referencing `/Length` must not re-enter it.
        let o = match slot.entry {
            Entry::Free => None,
            Entry::Offset { offset, .. } => self.parse_at(offset, num, depth).map(|mut o| {
                self.decrypt(num, slot.entry, &mut o);
                o
            }),
            Entry::InStream { stream, index } => self
                .objstm(t, stream, depth)
                .and_then(|s| s.object(index, num)),
        }
        .map(Arc::new);
        // An object that fails to load is not cached, so the fallback table is consulted.
        o.as_ref()?;
        slot.obj.get_or_init(|| o).clone()
    }

    fn decrypt(&self, num: u32, e: Entry, o: &mut Object) {
        let (Some(sec), Entry::Offset { generation, .. }) = (self.security.get(), e) else {
            return;
        };
        if self.encrypt_num == Some(num) {
            return;
        }
        if let Object::Stream(s) = o
            && let StreamData::Source(r) = &s.data
        {
            s.data = StreamData::Owned(self.src.get(r.clone()).unwrap_or_default().into());
        }
        sec.decrypt(ObjRef::new(num, generation), o);
    }

    fn parse_at(&self, offset: usize, num: u32, depth: u32) -> Option<Object> {
        let try_at = |off: usize| {
            let mut p = Parser::new(&self.src, off);
            let (id, o) = p
                .indirect(&mut |r| self.get(r.num, depth + 1)?.as_int())
                .ok()?;
            (id.num == num).then_some(o)
        };
        try_at(offset).or_else(|| {
            (self.shift > 0)
                .then(|| try_at(offset.checked_add(self.shift)?))
                .flatten()
        })
    }

    fn objstm(&self, t: &Table, stream: u32, depth: u32) -> Option<Arc<ObjStm>> {
        let slot = t.slots.get(&stream)?;
        if let Some(s) = slot.objstm.get() {
            return s.clone();
        }
        // Object streams are never inside object streams (§7.5.7); a table claiming so
        // would recurse.
        if !matches!(slot.entry, Entry::Offset { .. }) {
            return None;
        }
        let obj = self.load(t, stream, depth + 1)?;
        let s = obj.as_stream()?;
        let raw = match &s.data {
            StreamData::Source(r) => self.src.get(r.clone())?,
            StreamData::Owned(b) => b,
        };
        let resolve = |o: &Object| match o {
            Object::Ref(r) => self
                .get(r.num, depth + 1)
                .map_or(Object::Null, |o| (*o).clone()),
            o => o.clone(),
        };
        let data = stream::decode(raw, &s.dict, &resolve).ok()?.data;
        let stm = ObjStm::new(data, &s.dict).map(Arc::new);
        slot.objstm.get_or_init(|| stm).clone()
    }
}

/// Whether a loaded xref is trustworthy enough: its `/Root` must parse.
fn usable(src: &[u8], x: &Xref) -> bool {
    let Some(root) = x.trailer.get(b"Root").and_then(Object::as_objref) else {
        return false;
    };
    match x.entries.get(&root.num) {
        Some(Entry::Offset { offset, .. }) => Parser::new(src, *offset)
            .indirect_header()
            .is_some_and(|id| id.num == root.num),
        // Checked lazily: the object stream needs decoding.
        Some(Entry::InStream { .. }) => true,
        _ => false,
    }
}

fn parse_version(b: &[u8]) -> (u8, u8) {
    let d = |i: usize| b.get(i).filter(|c| c.is_ascii_digit()).map(|c| c - b'0');
    match (d(0), b.get(1), d(2)) {
        (Some(major), Some(b'.'), Some(minor)) => (major, minor),
        _ => (1, 7),
    }
}
