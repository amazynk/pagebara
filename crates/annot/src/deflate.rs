//! Compressed streams.

use pagebara_syntax::{Dict, Object, Stream};

/// Below this, compression saves too little to be worth a filter.
const MIN: usize = 128;

/// A zlib stream of `data` (what FlateDecode reads).
pub fn zlib(data: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(data, 6)
}

/// A stream of `data`, FlateDecode-compressed when that pays.
pub(crate) fn stream(mut dict: Dict, data: &[u8]) -> Stream {
    if data.len() < MIN {
        return Stream::new(dict, data);
    }
    let z = zlib(data);
    if z.len() >= data.len() {
        return Stream::new(dict, data);
    }
    dict.set(b"Filter", Object::name(b"FlateDecode"));
    Stream::new(dict, z)
}
