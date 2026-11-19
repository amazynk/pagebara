# Contributing

Thanks for helping. Bug reports, ideas and pull requests are welcome. Chat is on
[Discord](https://amazynk.com/discord/), bugs go to
[issues](https://github.com/amazynk/pagebara/issues/new/choose), and security problems go
privately, as [SECURITY.md](SECURITY.md) describes. For anything large, start with an issue
or a message on Discord so we can agree on the approach first. Everyone taking part is
expected to follow the [code of conduct](CODE_OF_CONDUCT.md).

## Ground rules

- **Safe Rust only.** `unsafe_code` is forbidden in every crate (the Slint-generated UI module
  is the one exception). Add a dependency only when it is worth it, and check its licence:
  it must be compatible with MIT OR Apache-2.0.
- **Local-first.** No telemetry, and no network access that the user did not ask for.
- **Small, focused changes** with tests. Engine fixes come with a test case; UI changes come
  with an e2e script step (`apps/desktop/tests/e2e/*.pdfs`) when they change behaviour.
- Short comments, only where the code cannot say it itself.
- Never commit real people's documents. Use generated samples
  (`pagebara --make-samples DIR`) or files with a clear licence.

## Before you open a pull request

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
apps/desktop/tests/e2e/run.sh /tmp/pagebara-e2e      # UI changes; needs Xvfb
```

If you add or update a dependency, regenerate the licence list with
`packaging/third-party.py` (CI checks that it is current).

For parser, renderer or writer changes, run the engine over public corpora too (fetched
outside the repository; never commit them):

```sh
tools/corpora/fetch.sh ~/pdf-corpora                     # pinned and checksummed
cargo run -p corpus-check -- run --out OUT ~/pdf-corpora/*  # crashes, hangs, saves, vs MuPDF
cargo run -p corpus-check -- conform ~/pdf-corpora/verapdf  # PDF/A and PDF/UA verdicts
cargo run -p pixel-diff -- run --known tools/pixel-diff/known.txt --out OUT ~/pdf-corpora/pdfium
CORPORA=~/pdf-corpora fuzz/seed.sh && SECS=600 fuzz/run.sh  # cargo-fuzz targets
```

Both runners take `--baseline OLD/results.jsonl` to report what changed.

## Licence

Unless you explicitly state otherwise, any contribution that you intentionally submit for
inclusion in the work, as defined in the Apache-2.0 licence, is dual licensed under MIT OR
Apache-2.0, without any additional terms or conditions.
