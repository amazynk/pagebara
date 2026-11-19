//! `IFilter`: the document's text and title for Windows Search. The indexer hands the file
//! over through `IPersistStream` (or `IPersistFile` / `IInitializeWithStream`).
#![allow(unsafe_code)]

use std::sync::{Mutex, MutexGuard};

use pagebara_syntax::Document;
use windows::Win32::Foundation::{E_FAIL, E_INVALIDARG, E_NOTIMPL, E_POINTER, S_FALSE, S_OK};
use windows::Win32::Storage::IndexServer::{
    CHUNK_EOP, CHUNK_EOS, CHUNK_TEXT, FILTER_E_END_OF_CHUNKS, FILTER_E_NO_MORE_TEXT,
    FILTER_E_NO_TEXT, FILTER_E_NO_VALUES, FILTER_S_LAST_TEXT, FILTERREGION, FULLPROPSPEC, IFilter,
    IFilter_Impl, STAT_CHUNK,
};
use windows::Win32::System::Com::StructuredStorage::{
    PROPSPEC, PROPSPEC_0, PROPVARIANT, PRSPEC_PROPID,
};
use windows::Win32::System::Com::{
    IPersist_Impl, IPersistFile, IPersistFile_Impl, IPersistStream, IPersistStream_Impl, IStream,
    STGM,
};
use windows::Win32::UI::Shell::PropertiesSystem::{
    IInitializeWithStream, IInitializeWithStream_Impl,
};
use windows_core::{BOOL, GUID, HRESULT, PCWSTR, PWSTR, Ref, Result, implement};

use super::{clsid, guard, guard_hr, read_stream};
use crate::MAX_FILE;
use crate::chunks::{Chunker, Prop, Read, document_chunks};

/// `PKEY_Search_Contents` (PSGUID_STORAGE, PID_STG_CONTENTS).
const CONTENTS: (GUID, u32) = (GUID::from_u128(0xb725f130_47ef_101a_a5f1_02608c9eebac), 19);
/// `PKEY_Title` (FMTID_SummaryInformation, PIDSI_TITLE).
const TITLE: (GUID, u32) = (GUID::from_u128(0xf29f85e0_4ff9_1068_ab91_08002b27b3d9), 2);
/// `LOCALE_USER_DEFAULT`.
const LOCALE: u32 = 0x0400;

#[implement(IFilter, IPersistStream, IPersistFile, IInitializeWithStream)]
#[derive(Default)]
pub(super) struct Filter {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    doc: Option<Document>,
    chunks: Chunker,
}

impl Filter_Impl {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn load(&self, bytes: Vec<u8>) -> Result<()> {
        let doc = crate::pdf::open(bytes).ok_or(E_FAIL)?;
        self.state().doc = Some(doc);
        Ok(())
    }
}

fn is_prop(a: &FULLPROPSPEC, (set, id): (GUID, u32)) -> bool {
    // SAFETY: `propid` is the union's active field when `ulKind` is PRSPEC_PROPID.
    a.guidPropSet == set
        && a.psProperty.ulKind == PRSPEC_PROPID
        && unsafe { a.psProperty.Anonymous.propid } == id
}

impl IFilter_Impl for Filter_Impl {
    fn Init(
        &self,
        _flags: u32,
        count: u32,
        attrs: *const FULLPROPSPEC,
        out_flags: *mut u32,
    ) -> i32 {
        guard_hr(|| {
            let (contents, title) = if count == 0 {
                (true, true)
            } else if attrs.is_null() {
                return E_INVALIDARG;
            } else {
                // SAFETY: the indexer passes `count` property specs at `attrs` (non-null).
                let attrs = unsafe { std::slice::from_raw_parts(attrs, count as usize) };
                (
                    attrs.iter().any(|a| is_prop(a, CONTENTS)),
                    attrs.iter().any(|a| is_prop(a, TITLE)),
                )
            };
            let mut st = self.state();
            let Some(doc) = st.doc.clone() else {
                return E_FAIL;
            };
            st.chunks = Chunker::new(document_chunks(&doc, contents, title));
            if !out_flags.is_null() {
                // SAFETY: non-null out-pointer from the caller.
                unsafe { *out_flags = 0 };
            }
            S_OK
        })
        .0
    }

    fn GetChunk(&self, stat: *mut STAT_CHUNK) -> i32 {
        guard_hr(|| {
            if stat.is_null() {
                return E_POINTER;
            }
            let mut st = self.state();
            let Some((id, c)) = st.chunks.next_chunk() else {
                return FILTER_E_END_OF_CHUNKS;
            };
            let (set, propid) = match c.prop {
                Prop::Contents => CONTENTS,
                Prop::Title => TITLE,
            };
            let chunk = STAT_CHUNK {
                idChunk: id,
                breakType: if c.end_of_page { CHUNK_EOP } else { CHUNK_EOS },
                flags: CHUNK_TEXT,
                locale: LOCALE,
                attribute: FULLPROPSPEC {
                    guidPropSet: set,
                    psProperty: PROPSPEC {
                        ulKind: PRSPEC_PROPID,
                        Anonymous: PROPSPEC_0 { propid },
                    },
                },
                idChunkSource: id,
                cwcStartSource: 0,
                cwcLenSource: 0,
            };
            // SAFETY: non-null out-pointer from the caller.
            unsafe { *stat = chunk };
            S_OK
        })
        .0
    }

    fn GetText(&self, len: *mut u32, buf: PWSTR) -> i32 {
        guard_hr(|| {
            if len.is_null() || buf.is_null() {
                return E_POINTER;
            }
            // SAFETY: `len` is non-null and holds the buffer's size in UTF-16 units.
            let cap = unsafe { *len } as usize;
            // SAFETY: `buf` is non-null and writable for `cap` units, as the caller said.
            let out = unsafe { std::slice::from_raw_parts_mut(buf.0, cap) };
            let (n, hr) = match self.state().chunks.read(out) {
                Read::Copied { n, last } => (n, if last { FILTER_S_LAST_TEXT } else { S_OK }),
                Read::NoMore => (0, FILTER_E_NO_MORE_TEXT),
                Read::NoChunk => (0, FILTER_E_NO_TEXT),
            };
            // SAFETY: as above.
            unsafe { *len = n as u32 };
            hr
        })
        .0
    }

    fn GetValue(&self, _value: *mut *mut PROPVARIANT) -> i32 {
        FILTER_E_NO_VALUES.0
    }

    fn BindRegion(
        &self,
        _pos: &FILTERREGION,
        _riid: *const GUID,
        _out: *mut *mut core::ffi::c_void,
    ) -> i32 {
        E_NOTIMPL.0
    }
}

impl IPersist_Impl for Filter_Impl {
    fn GetClassID(&self) -> Result<GUID> {
        Ok(clsid("filter"))
    }
}

impl IPersistStream_Impl for Filter_Impl {
    fn IsDirty(&self) -> HRESULT {
        S_FALSE
    }

    fn Load(&self, stream: Ref<IStream>) -> Result<()> {
        guard(|| self.load(read_stream(stream.ok()?)?))
    }

    fn Save(&self, _stream: Ref<IStream>, _clear: BOOL) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn GetSizeMax(&self) -> Result<u64> {
        Err(E_NOTIMPL.into())
    }
}

impl IPersistFile_Impl for Filter_Impl {
    fn IsDirty(&self) -> HRESULT {
        S_FALSE
    }

    fn Load(&self, name: &PCWSTR, _mode: STGM) -> Result<()> {
        guard(|| {
            if name.is_null() {
                return Err(E_POINTER.into());
            }
            // SAFETY: a non-null, NUL-terminated wide string from the caller.
            let path = unsafe { name.to_string() }.map_err(|_| E_INVALIDARG)?;
            let len = std::fs::metadata(&path).map_err(|_| E_FAIL)?.len();
            if len > MAX_FILE {
                return Err(E_FAIL.into());
            }
            self.load(std::fs::read(&path).map_err(|_| E_FAIL)?)
        })
    }

    fn Save(&self, _name: &PCWSTR, _remember: BOOL) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn SaveCompleted(&self, _name: &PCWSTR) -> Result<()> {
        Ok(())
    }

    fn GetCurFile(&self) -> Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }
}

impl IInitializeWithStream_Impl for Filter_Impl {
    fn Initialize(&self, stream: Ref<IStream>, _mode: u32) -> Result<()> {
        guard(|| self.load(read_stream(stream.ok()?)?))
    }
}
