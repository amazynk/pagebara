//! Text recognition as the app offers it: finding pages that look scanned, the choice of
//! script and pages, and what the models cost to download.

use pagebara_i18n::{tr, tr_noop};
use pagebara_ocr::{ModelSource, Script};
use pagebara_syntax::{Dict, Document, ObjRef};

use crate::page_ops::parse_ranges;

/// Pages with fewer non-space characters than this count as having no text.
pub const MIN_TEXT_CHARS: usize = 16;

/// The scripts in the order the dialog lists them.
pub fn script_labels() -> Vec<String> {
    Script::ALL
        .iter()
        .map(|&s| match s {
            Script::Latin => tr!("Latin (English, European languages)"),
            s => pagebara_i18n::translate(script_name(s), &[]),
        })
        .collect()
}

/// The name of a script in a sentence ("Greek", "Chinese and Japanese"), in English (it
/// is also a saved key); show it through [`pagebara_i18n::translate`].
pub fn script_name(s: Script) -> &'static str {
    match s {
        // TRANSLATORS: names of writing systems (scripts) that text recognition reads
        Script::Latin => tr_noop!("Latin"),
        Script::Chinese => tr_noop!("Chinese and Japanese"),
        Script::Korean => tr_noop!("Korean"),
        Script::Arabic => tr_noop!("Arabic"),
        Script::Cyrillic => tr_noop!("Cyrillic"),
        Script::Devanagari => tr_noop!("Devanagari"),
        Script::Greek => tr_noop!("Greek"),
        Script::Thai => tr_noop!("Thai"),
    }
}

/// Model files copied from a folder holding them under their names, in place of
/// downloading them (for tests).
#[derive(Debug)]
pub struct DirFetch(pub std::path::PathBuf);

impl pagebara_ocr::Fetch for DirFetch {
    fn fetch(
        &self,
        url: &str,
        out: &mut dyn std::io::Write,
        progress: &dyn Fn(u64),
    ) -> std::io::Result<()> {
        let file = Script::ALL
            .iter()
            .flat_map(|s| s.files())
            .find(|f| f.url == url)
            .ok_or_else(|| std::io::Error::other("unknown model"))?;
        let data = std::fs::read(self.0.join(file.name))?;
        out.write_all(&data)?;
        progress(data.len() as u64);
        Ok(())
    }
}

pub fn script(i: usize) -> Script {
    Script::ALL.get(i).copied().unwrap_or_default()
}

/// The pages of `spec` ("1-3, 5"); `None` for all when it is empty.
pub fn pages(spec: &str, count: usize) -> Result<Option<Vec<usize>>, String> {
    if spec.trim().is_empty() {
        return Ok(None);
    }
    let mut out: Vec<usize> = parse_ranges(spec, count)?.into_iter().flatten().collect();
    out.sort_unstable();
    out.dedup();
    Ok(Some(out))
}

fn resources(doc: &Document, mut page: Dict) -> Option<Dict> {
    for _ in 0..32 {
        if let Some(r) = doc.get_in(&page, b"Resources").as_dict() {
            return Some(r.clone());
        }
        page = doc.get_in(&page, b"Parent").as_dict()?.clone();
    }
    None
}

/// Whether a page draws an image from its resources (directly or in a form).
fn has_image(doc: &Document, res: &Dict, depth: u8) -> bool {
    let Some(xo) = doc.get_in(res, b"XObject").as_dict().cloned() else {
        return false;
    };
    xo.0.iter().any(|(_, v)| {
        let o = doc.resolve(v);
        let Some(s) = o.as_stream() else { return false };
        match s.dict.name(b"Subtype") {
            Some(b"Image") => true,
            Some(b"Form") if depth < 3 => doc
                .get_in(&s.dict, b"Resources")
                .as_dict()
                .is_some_and(|r| has_image(doc, r, depth + 1)),
            _ => false,
        }
    })
}

/// Whether page `r` looks scanned: an image and (almost) no text. `text_chars` counts the
/// non-space characters of its text.
pub fn looks_scanned(doc: &Document, r: ObjRef, text_chars: usize) -> bool {
    if text_chars >= MIN_TEXT_CHARS {
        return false;
    }
    let Some(d) = doc.get(r).and_then(|o| o.as_dict().cloned()) else {
        return false;
    };
    resources(doc, d).is_some_and(|res| has_image(doc, &res, 0))
}

/// Non-space characters of a text.
pub fn chars(text: &str) -> usize {
    text.chars().filter(|c| !c.is_whitespace()).count()
}

/// Bytes still to download for `script` given the sources that may hold the models.
pub fn missing_bytes(sources: &[ModelSource], script: Script) -> u64 {
    script
        .files()
        .iter()
        .filter(|f| !sources.iter().any(|s| s.has(f)))
        .map(|f| f.size.max(4096))
        .sum()
}

/// "16 MB", "800 KB".
pub fn human_size(bytes: u64) -> String {
    if bytes >= 1 << 20 {
        tr!("{} MB", format!("{:.0}", bytes as f64 / f64::from(1 << 20)))
    } else {
        tr!("{} KB", format!("{:.0}", (bytes as f64 / 1024.0).max(1.0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_pdf;

    #[test]
    fn page_lists() {
        assert_eq!(pages("", 5).unwrap(), None);
        assert_eq!(pages("3, 1-2, 2", 5).unwrap(), Some(vec![0, 1, 2]));
        assert!(pages("9", 5).is_err());
        assert_eq!(script(1), Script::Chinese);
        assert_eq!(script(99), Script::Latin);
        assert_eq!(script_labels().len(), Script::ALL.len());
    }

    #[test]
    fn scanned_pages_have_an_image_and_no_text() {
        let image = "<</Type/XObject/Subtype/Image/Width 1/Height 1/ColorSpace/DeviceGray/BitsPerComponent 8/Length 1>>stream\nA\nendstream";
        let scan = test_pdf::classic(
            &[
                "<</Type/Catalog/Pages 2 0 R>>",
                "<</Type/Pages/Kids[3 0 R]/Count 1/Resources<</XObject<</Im0 5 0 R>>>>>>",
                "<</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Contents 4 0 R>>",
                "<</Length 22>>stream\nq 100 0 0 100 0 0 cm /Im0 Do Q\nendstream",
                image,
            ],
            1,
        );
        let d = Document::open(scan).unwrap();
        let r = pagebara_pages::PageTree::load(&d).pages()[0].r;
        assert!(looks_scanned(&d, r, 0));
        assert!(!looks_scanned(&d, r, MIN_TEXT_CHARS));
        let d = Document::open(test_pdf::text(&["Hello"])).unwrap();
        let r = pagebara_pages::PageTree::load(&d).pages()[0].r;
        assert!(!looks_scanned(&d, r, chars("Hello")));
    }

    #[test]
    fn download_sizes() {
        let none = std::env::temp_dir().join("pagebara-no-models-here");
        let all = missing_bytes(&[ModelSource::Dir(none)], Script::Latin);
        assert!(all >= Script::Latin.download_size());
        assert_eq!(human_size(3 << 20), "3 MB");
        assert_eq!(human_size(10), "1 KB");
        if let Some(dir) = std::env::var_os("PAGEBARA_OCR_MODELS") {
            assert_eq!(
                missing_bytes(&[ModelSource::Dir(dir.into())], Script::Latin),
                0
            );
        }
    }
}
