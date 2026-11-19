//! Hunspell dictionaries: the input is an `.aff` file and a `.dic` file separated by a
//! NUL byte; words of both are checked and a few get suggestions.

#![no_main]

use libfuzzer_sys::fuzz_target;
use pagebara_spell::Dictionary;

fuzz_target!(|data: &[u8]| {
    let (aff, dic) = match data.iter().position(|&b| b == 0) {
        Some(i) => (&data[..i], &data[i + 1..]),
        None => (data, &b"1\nword\n"[..]),
    };
    let Ok(d) = Dictionary::from_bytes(aff, dic) else {
        return;
    };
    let text = String::from_utf8_lossy(&data[..data.len().min(512)]);
    for (i, t) in d.tokens(&text).take(16).enumerate() {
        if !d.check(t.word) && i < 2 {
            let _ = d.suggest_n(t.word, 3);
        }
    }
    let _ = d.misspellings("Teh qick brwn fox, can't won't Straße naïve").count();
});
