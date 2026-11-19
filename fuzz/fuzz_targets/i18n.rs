//! Translation catalogs (PO files, read by the build) and the runtime formatting of
//! translated strings with their `{0}`-style arguments and plural forms.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let s = String::from_utf8_lossy(data);
    let entries = pagebara_i18n::po::parse(&s);
    let _ = pagebara_i18n::po::plural_count(&entries);
    for e in entries.iter().take(8) {
        for t in &e.strs {
            let _ = pagebara_i18n::format(t, &[&1, &"x", &2.5], Some(3));
        }
    }
    let _ = pagebara_i18n::format(&s, &[&"a", &-7], None);
    for code in ["pl", "ar", "ru", "fr"] {
        let _ = pagebara_i18n::plural_form(code, i64::from(data.first().copied().unwrap_or(0)));
    }
});
