# Changelog

All notable changes to Pagebara are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

- Saving a damaged file whose trailer names object 0 as the catalog, when the file has no
  such object, no longer crashes the app.
- The Linux AppStream file names its developer in the form AppStream accepts.

## [0.1.0]

The first public release.

### Added

- **Our own PDF engine in safe Rust.** A parser with repair, encryption, incremental and
  full saving; a renderer with colour management, transparency and shadings; font programs;
  and JPEG, JPEG 2000, JBIG2, CCITT, PNG and Flate decoders.
- **A desktop app for Linux, Windows and macOS.** The toolbar holds the document's name, the
  modes (View, Annotate, Fill & Sign, Edit, Organize), search, All tools and Save. One row
  under it has undo, the tools of the chosen mode with their names, colour and size, and the
  page and zoom. Tabs, undo and redo, thumbnails, outline, links, reading layouts and page
  colours, a reading view, presenting, and bookmarks.
- **All tools** (Ctrl+K) lists every command, the recent files included, and narrows as you
  type.
- **Search.** Find lists each match with its page number and a line of context, and can
  match capitalization, whole words, or both. Command+G on macOS, or F3 on Windows and
  Linux, goes to the next result from the page. Text selection and copy, and a library that
  searches a folder of PDFs.
- **Live text.** Text in pictures (screenshots, scans, titles drawn as outlines) becomes
  selectable and copyable when the pointer rests on it, or with a long press on phones,
  without changing the document. Copied text keeps columns, table rows, line breaks and code
  indentation. Builds that carry the text detection and Latin models
  (`packaging/ocr-bundle.sh`) do this offline.
- **Annotations.** Highlight, underline, strike-out, ink, shapes, arrows, text boxes and
  sticky notes with replies. Text selected before choosing Highlight or Redact is marked at
  once, and each drawing tool remembers its colour.
- **Editing page text**, with the text colour and size beside Bold and Italic in the tool
  row. Dragging a handle past the opposite edge mirrors the selection.
- **Filling in forms**, with generated appearances, reset and flattening.
- **Preparing forms.** Text (multiline, comb, password, maximum length), date, check box,
  radio button, drop-down, list box, button and signature fields, created, moved and resized
  with snapping, aligned, distributed, duplicated, copied to other pages, renamed and
  removed; field properties, tab order, and fields detected from the lines, underscores and
  boxes of printed forms.
- **Form data.** FDF and XFDF export and import (XFDF with comments), in the apps and as
  `pagebara --action fdf-export | fdf-import | xfdf-export | xfdf-import | detect-fields`.
- **Signing.** A signature library (photographed signatures with background removal, or
  drawn ones), and digital signatures: PAdES/CMS signing and verification, timestamps, LTV
  and digital IDs.
- **Page organisation.** Rotate, duplicate, delete, reorder, insert, extract, split,
  combine, crop, and page labels.
- **True redaction with verification.**
- **Text recognition** (PP-OCRv5) with an invisible, searchable text layer. Models that a
  build does not carry are downloaded on request.
- **Password protection and permissions.**
- **Printing with a preview.**
- **Accessibility.** Screen reader support in tagged reading order, read aloud, keyboard
  use, high contrast and larger text.
- **Vim keys over the pages** (Settings > Editing, off by default): motions, counts, marks,
  search, visual mode with yank, the `:` command line and tabs. `:help` lists them.
- **Settings** under General, Editing, System and About, with a document preview for each
  appearance choice. About has the version, the licences, the Amazynk website and the
  community Discord.
- **Finder Quick Actions on macOS**, turned on one at a time from the Pagebara menu.
- **A phone and tablet app for Android and iOS** (`apps/mobile`) on the same engine, with
  page colours and live text. It is built in CI and is not packaged for release yet.
- **Packages.** AppImage, .deb, MSI and DMG.

[Unreleased]: https://github.com/amazynk/pagebara/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/amazynk/pagebara/releases/tag/v0.1.0
