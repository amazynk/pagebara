# Translations

`pagebara.pot` lists every string of the desktop and mobile apps; `<lang>.po` holds one
language. The Slint UI bundles the catalogs into the binary at build time
(`with_bundled_translations`, see `crates/i18n`), and Rust code reads the same catalogs
through `pagebara-i18n`, so nothing is looked up on disk at run time.

- `sh po/update.sh` collects the strings (`cargo run -p tr-extract`) and merges them into
  every catalog. `cargo test -p tr-extract` fails while `pagebara.pot` is out of date.
- A new language: add it to `LANGUAGES` and the plural rules in `crates/i18n/src/lib.rs`,
  then `msginit -i po/pagebara.pot -o po/<code>.po -l <code> --no-translator`.
- `cargo test -p pagebara-i18n` checks that every translation keeps its placeholders and that
  plural messages have the language's number of forms.

The shipped translations were written with machine assistance and await review by native
speakers; each catalog says so in its header.

## Marking strings

English is the source language and the message id, so English output must stay exactly
as it was (UI tests find controls by their English labels; they run with `PAGEBARA_LANG=en`).

Slint:

```slint
text: @tr("Open");
text: @tr("Page {} of {}", page, count);            // not "Page " + page + ...
text: @tr("{n} page" | "{n} pages" % count);        // plurals
text: busy ? @tr("Saving…") : @tr("Save");
// TRANSLATORS: the verb, on a button that crops the page
text: @tr("Crop");
```

Rust (`use pagebara_i18n::{tr, trn};`):

```rust
st.set_status(tr!("Saved as “{}”.", name).into());
let label = trn!("{n} page", "{n} pages", n);
const LABELS: [&str; 2] = [tr_noop!("Title"), tr_noop!("Author")]; // translated later
pagebara_i18n::translate(LABELS[i], &[]);
```

Placeholders: `{}` takes the next argument, `{0}`, `{1}` a numbered one (translations
reorder with these), `{n}` the count of a plural. Inline `format!` names (`{total}`) are not
placeholders; pass the value as an argument.

Not translated: action and id strings passed to callbacks, values compared in code,
keyboard shortcuts, units in file formats, log messages, command-line output, and errors
from the PDF engine's internals (shown as the detail of a translated message).

## Terminology

Translations follow the terms that Adobe Acrobat and Apple Preview use in each language
(e.g. German: Kommentar, Hervorheben, Formular ausfüllen, Schwärzen, Signieren; French:
Commentaire, Surligner, Remplir le formulaire, Biffer, Signer).
