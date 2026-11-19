//! Spell checking as the app offers it: the dictionary for a language (built in, downloaded
//! or the system's), what a missing one costs to download, the user's word list ("Add to
//! dictionary") and the words ignored for the session.
//!
//! [`Spelling`] is created once and shared; [`Spelling::checker`] loads a dictionary (do it
//! on a worker, it takes up to a few hundred milliseconds) and hands out a [`SpellChecker`],
//! which is cheap to clone and can check text on any thread.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use pagebara_spell::catalog::{self, DictInfo};
use pagebara_spell::{Dictionary, download};
use pagebara_syntax::Document;
use parking_lot::{Mutex, RwLock};

pub use pagebara_spell::{Error, Fetch, Progress};

/// Where dictionaries and the word list live.
#[derive(Clone, Debug, Default)]
pub struct Config {
    /// The app's data folder: downloaded dictionaries go to `dictionaries/`, the word list
    /// is `spelling/words.txt`.
    pub data_dir: Option<PathBuf>,
    /// Folders of system Hunspell dictionaries (`<code>.aff` and `<code>.dic`).
    pub system_dirs: Vec<PathBuf>,
}

impl Config {
    /// The app's data folder and, on Linux, the system's dictionary folders.
    pub fn standard() -> Config {
        let system_dirs = if cfg!(target_os = "linux") {
            [
                "/usr/share/hunspell",
                "/usr/share/myspell/dicts",
                "/usr/share/myspell",
            ]
            .iter()
            .map(PathBuf::from)
            .collect()
        } else {
            Vec::new()
        };
        Config {
            data_dir: pagebara_identity::data_dir(),
            system_dirs,
        }
    }

    fn downloads(&self) -> Option<PathBuf> {
        self.data_dir.as_ref().map(|d| d.join("dictionaries"))
    }
}

/// Where a dictionary comes from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    Bundled,
    /// Downloaded into the app's data folder.
    Downloaded(&'static DictInfo),
    /// The system's, with its `.aff` and `.dic` files.
    System(PathBuf, PathBuf),
}

/// Whether a language can be checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Availability {
    /// A dictionary is at hand (`code` such as "de_DE").
    Ready { code: String, source: Source },
    /// One can be downloaded (show its size and licence first).
    Download(&'static DictInfo),
    /// No dictionary for this language (Chinese, Japanese, ...).
    Unsupported,
}

/// A misspelled word in a text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Misspelling {
    /// Byte range in the text.
    pub range: Range<usize>,
    pub word: String,
}

/// The user's word list and the session's ignored words, shared by every checker.
#[derive(Debug, Default)]
struct Words {
    file: Option<PathBuf>,
    personal: RwLock<HashSet<String>>,
    ignored: RwLock<HashSet<String>>,
}

fn is_all_caps(w: &str) -> bool {
    w.chars().any(char::is_uppercase) && !w.chars().any(char::is_lowercase)
}

fn capitalize(w: &str) -> String {
    let mut cs = w.chars();
    cs.next()
        .map(|c| c.to_uppercase().chain(cs).collect())
        .unwrap_or_default()
}

impl Words {
    fn load(file: Option<PathBuf>) -> Words {
        let personal = file
            .as_deref()
            .and_then(|f| fs::read_to_string(f).ok())
            .map(|s| {
                s.lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        Words {
            file,
            personal: RwLock::new(personal),
            ignored: RwLock::default(),
        }
    }

    /// Whether `w` is in the list or ignored: a word added in small letters also counts
    /// capitalised and in capitals, a capitalised one also in capitals.
    fn accepts(&self, w: &str) -> bool {
        if self.ignored.read().contains(w) {
            return true;
        }
        let p = self.personal.read();
        if p.is_empty() {
            return false;
        }
        if p.contains(w) {
            return true;
        }
        let lower = w.to_lowercase();
        let all = is_all_caps(w);
        let initial = capitalize(&lower) == w;
        ((initial || all) && p.contains(&lower)) || (all && p.contains(&capitalize(&lower)))
    }

    fn add(&self, w: &str) -> io::Result<()> {
        let w = w.trim();
        if w.is_empty()
            || w.contains(char::is_whitespace)
            || !self.personal.write().insert(w.to_owned())
        {
            return Ok(());
        }
        let Some(file) = &self.file else {
            return Ok(());
        };
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)?;
        writeln!(f, "{w}")
    }

    fn remove(&self, w: &str) -> io::Result<()> {
        let mut p = self.personal.write();
        if !p.remove(w.trim()) {
            return Ok(());
        }
        let Some(file) = &self.file else {
            return Ok(());
        };
        let mut words: Vec<&String> = p.iter().collect();
        words.sort();
        let text: String = words.iter().map(|w| format!("{w}\n")).collect();
        // Written aside and renamed, so a crash never leaves half a list.
        let tmp = file.with_extension("txt.part");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, file)
    }
}

/// The app's spelling service: dictionaries by language, loaded once, and the word lists.
#[derive(Debug)]
pub struct Spelling {
    config: Config,
    loaded: Mutex<HashMap<Source, Arc<Dictionary>>>,
    words: Arc<Words>,
}

impl Spelling {
    pub fn new(config: Config) -> Spelling {
        let file = config
            .data_dir
            .as_ref()
            .map(|d| d.join("spelling").join("words.txt"));
        Spelling {
            config,
            loaded: Mutex::default(),
            words: Arc::new(Words::load(file)),
        }
    }

    /// The dictionaries at hand for a language, best first: the built-in one, then the
    /// downloaded ones, then the system's; one for the tag's region before the others.
    fn local(&self, tag: &str) -> Option<(String, Source)> {
        let (lang, region) = catalog::parse_tag(tag)?;
        let mut found: Vec<(String, Source)> = Vec::new();
        if catalog::EN_US.language() == lang {
            found.push((catalog::EN_US.code.into(), Source::Bundled));
        }
        if let Some(root) = self.config.downloads() {
            for d in catalog::DOWNLOADS {
                if d.language() == lang && download::is_installed(&root, d) {
                    found.push((d.code.into(), Source::Downloaded(d)));
                }
            }
        }
        for dir in &self.config.system_dirs {
            found.extend(system_dictionaries(dir, &lang));
        }
        let region_of = |code: &str| catalog::parse_tag(code).and_then(|(_, r)| r);
        let exact = region.as_ref().and_then(|r| {
            found
                .iter()
                .position(|(c, _)| region_of(c).as_ref() == Some(r))
        });
        let i = exact.unwrap_or(0);
        (i < found.len()).then(|| found.swap_remove(i))
    }

    /// Whether `tag` (BCP 47 such as "de-DE", or POSIX such as "de_DE.UTF-8") can be
    /// checked now, after a download, or not at all.
    pub fn availability(&self, tag: &str) -> Availability {
        if let Some((code, source)) = self.local(tag) {
            return Availability::Ready { code, source };
        }
        match catalog::find(tag) {
            Some(d) if d.files.is_some() && self.config.downloads().is_some() => {
                Availability::Download(d)
            }
            _ => Availability::Unsupported,
        }
    }

    /// Bytes to download before `tag` can be checked, if a download would help.
    pub fn needs_download(&self, tag: &str) -> Option<u64> {
        match self.availability(tag) {
            Availability::Download(d) => Some(d.download_size()),
            _ => None,
        }
    }

    /// Downloads the dictionary for `tag` (the user agreed) into the data folder.
    pub fn download(
        &self,
        tag: &str,
        fetch: &dyn Fetch,
        progress: &dyn Fn(Progress),
        cancel: &AtomicBool,
    ) -> Result<(), Error> {
        let unavailable = || Error::Unavailable(tag.into());
        let info = catalog::find(tag)
            .filter(|d| d.files.is_some())
            .ok_or_else(unavailable)?;
        let root = self.config.downloads().ok_or_else(unavailable)?;
        download::install(&root, info, fetch, progress, cancel)
    }

    /// A checker for `tag`, loading its dictionary the first time (slow: call on a worker).
    pub fn checker(&self, tag: &str) -> Result<SpellChecker, Error> {
        let (code, source) = self
            .local(tag)
            .ok_or_else(|| Error::Unavailable(tag.into()))?;
        let cached = self.loaded.lock().get(&source).cloned();
        let dict = match cached {
            Some(d) => d,
            None => {
                let d = match &source {
                    Source::Bundled => pagebara_spell::en_us(),
                    Source::Downloaded(info) => {
                        let root = self
                            .config
                            .downloads()
                            .ok_or_else(|| Error::Unavailable(tag.into()))?;
                        Arc::new(download::load_installed(&root, info)?)
                    }
                    Source::System(aff, dic) => Arc::new(Dictionary::open(aff, dic)?),
                };
                self.loaded.lock().insert(source, d.clone());
                d
            }
        };
        Ok(SpellChecker {
            code,
            dict,
            words: self.words.clone(),
        })
    }

    /// Adds `word` to the user's word list (kept in the data folder).
    pub fn add_to_dictionary(&self, word: &str) -> io::Result<()> {
        self.words.add(word)
    }

    /// Removes `word` from the user's word list.
    pub fn remove_from_dictionary(&self, word: &str) -> io::Result<()> {
        self.words.remove(word)
    }

    /// The user's word list, sorted.
    pub fn personal_words(&self) -> Vec<String> {
        let mut v: Vec<String> = self.words.personal.read().iter().cloned().collect();
        v.sort();
        v
    }

    /// Ignores `word` until the app quits.
    pub fn ignore(&self, word: &str) {
        self.words.ignored.write().insert(word.to_owned());
    }

    /// Credits of the built-in dictionary and of those downloaded, for the About box.
    pub fn notices(&self) -> Vec<String> {
        let root = self.config.downloads();
        std::iter::once(&catalog::EN_US)
            .chain(catalog::DOWNLOADS.iter().filter(|d| {
                root.as_deref()
                    .is_some_and(|r| download::is_installed(r, d))
            }))
            .map(DictInfo::notice)
            .collect()
    }
}

/// The system dictionaries in `dir` for language `lang`, by file name (`de_DE.aff`).
fn system_dictionaries(dir: &Path, lang: &str) -> Vec<(String, Source)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, Source)> = entries
        .flatten()
        .filter_map(|e| {
            let aff = e.path();
            if aff.extension()? != "aff" {
                return None;
            }
            let stem = aff.file_stem()?.to_str()?.to_owned();
            let (l, region) = catalog::parse_tag(&stem)?;
            let dic = aff.with_extension("dic");
            (l == lang && dic.is_file()).then(|| {
                let code = region.map_or(l.clone(), |r| format!("{l}_{r}"));
                (code, Source::System(aff, dic))
            })
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Checks text in one language. Cheap to clone; usable from any thread.
#[derive(Clone, Debug)]
pub struct SpellChecker {
    code: String,
    dict: Arc<Dictionary>,
    words: Arc<Words>,
}

impl SpellChecker {
    /// The dictionary's language and region ("en_US").
    pub fn language(&self) -> &str {
        &self.code
    }

    /// Whether `word` is spelled correctly (or in the word list, or ignored).
    pub fn check_word(&self, word: &str) -> bool {
        self.words.accepts(word) || self.dict.check(word)
    }

    /// The misspelled words of `text`, with their byte ranges (for squiggles).
    pub fn check_text(&self, text: &str) -> Vec<Misspelling> {
        self.dict
            .misspellings(text)
            .filter(|t| !self.words.accepts(t.word))
            .map(|t| Misspelling {
                range: t.range,
                word: t.word.to_owned(),
            })
            .collect()
    }

    /// Corrections for `word`, best first.
    pub fn suggest(&self, word: &str) -> Vec<String> {
        self.dict.suggest(word)
    }

    /// Adds `word` to the user's word list (for every language).
    pub fn add_to_dictionary(&self, word: &str) -> io::Result<()> {
        self.words.add(word)
    }

    /// Ignores `word` until the app quits.
    pub fn ignore(&self, word: &str) {
        self.words.ignored.write().insert(word.to_owned());
    }
}

/// Adapts the OCR model fetch (the app's HTTPS client) for dictionary downloads.
#[derive(Clone)]
pub struct OcrFetch(pub Arc<dyn pagebara_ocr::Fetch>);

impl std::fmt::Debug for OcrFetch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OcrFetch")
    }
}

impl Fetch for OcrFetch {
    fn fetch(&self, url: &str, out: &mut dyn Write, progress: &dyn Fn(u64)) -> io::Result<()> {
        self.0.fetch(url, out, progress)
    }
}

/// The document's language (the catalog's `/Lang`), if it names one.
pub fn document_language(doc: &Document) -> Option<String> {
    let cat = doc.catalog()?;
    let d = cat.as_dict()?.clone();
    let lang = doc.get_in(&d, b"Lang");
    let lang = crate::text::decode(lang.as_string()?);
    catalog::parse_tag(&lang).is_some().then_some(lang)
}

/// The language to check text in: the field's or structure element's `/Lang`, else the
/// document's, else the app's interface language.
pub fn language_for(element: Option<&str>, document: Option<&str>, ui: &str) -> String {
    [element, document, Some(ui)]
        .into_iter()
        .flatten()
        .find(|t| catalog::parse_tag(t).is_some())
        .unwrap_or("en-US")
        .to_owned()
}

/// Points of a red wavy underline for a word whose line box runs from `a` to `b` along its
/// bottom edge, `top` being the corner above `a` (view units, y down); `px` is one screen
/// pixel in those units, so the wave keeps its size on screen at any zoom.
pub fn squiggle(a: (f64, f64), b: (f64, f64), top: (f64, f64), px: f64) -> Vec<(f64, f64)> {
    let len = (b.0 - a.0).hypot(b.1 - a.1);
    let (tx, ty) = (top.0 - a.0, top.1 - a.1);
    let h = tx.hypot(ty);
    if len < 1e-3 || h < 1e-3 || px <= 0.0 {
        return Vec::new();
    }
    // Along the line, and towards the top: the wave's middle sits in the descent.
    let (ux, uy) = ((b.0 - a.0) / len, (b.1 - a.1) / len);
    let (nx, ny) = (tx / h, ty / h);
    let (base, period, amp) = (h * 0.1, 4.0 * px, px);
    let steps = ((len / period) * 8.0).ceil().max(2.0) as usize;
    (0..=steps)
        .map(|i| {
            let d = len * i as f64 / steps as f64;
            let off = base + amp * (d / period * std::f64::consts::TAU).sin();
            (a.0 + ux * d + nx * off, a.1 + uy * d + ny * off)
        })
        .collect()
}

/// Credits and licences of every dictionary the app may use, for the licences list.
pub fn notices() -> Vec<String> {
    std::iter::once(&catalog::EN_US)
        .chain(catalog::DOWNLOADS)
        .map(DictInfo::notice)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_pdf;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pagebara-spell-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn bundled_english_and_word_lists() {
        let data = temp("words");
        let s = Spelling::new(Config {
            data_dir: Some(data.clone()),
            system_dirs: Vec::new(),
        });
        assert!(matches!(
            s.availability("en-GB"),
            Availability::Ready {
                source: Source::Bundled,
                ..
            }
        ));
        let c = s.checker("en-US").unwrap();
        assert_eq!(c.language(), "en_US");
        let text = "I recieve pagebara files from Paris.";
        let bad: Vec<String> = c.check_text(text).into_iter().map(|m| m.word).collect();
        assert_eq!(bad, ["recieve", "pagebara"]);
        assert_eq!(c.check_text(text)[0].range, 2..9);
        assert_eq!(c.suggest("recieve")[0], "receive");
        c.add_to_dictionary("pagebara").unwrap();
        s.ignore("recieve");
        assert!(c.check_text(text).is_empty());
        assert!(c.check_word("Pagebara") && c.check_word("PAGEBARA") && !c.check_word("pageBara"));
        // The list is kept for the next session; ignored words are not.
        let s2 = Spelling::new(Config {
            data_dir: Some(data.clone()),
            system_dirs: Vec::new(),
        });
        let c2 = s2.checker("en").unwrap();
        assert!(c2.check_word("pagebara") && !c2.check_word("recieve"));
        s2.remove_from_dictionary("pagebara").unwrap();
        assert!(s2.personal_words().is_empty());
        assert!(
            !Spelling::new(Config {
                data_dir: Some(data.clone()),
                system_dirs: Vec::new()
            })
            .checker("en")
            .unwrap()
            .check_word("pagebara")
        );
        let _ = fs::remove_dir_all(&data);
    }

    #[test]
    fn availability_and_downloads() {
        let data = temp("avail");
        let sys = temp("sys");
        fs::create_dir_all(&sys).unwrap();
        fs::write(sys.join("de_CH.aff"), "SET UTF-8\n").unwrap();
        fs::write(sys.join("de_CH.dic"), "1\nGrüezi\n").unwrap();
        fs::write(sys.join("hyph_de_CH.dic"), "").unwrap();
        let s = Spelling::new(Config {
            data_dir: Some(data.clone()),
            system_dirs: vec![sys.clone()],
        });
        assert!(
            matches!(s.availability("de-CH"), Availability::Ready { ref code, source: Source::System(..) } if code == "de_CH")
        );
        // de-DE falls back to the language: the system's Swiss German.
        assert!(matches!(
            s.availability("de-DE"),
            Availability::Ready { .. }
        ));
        assert!(s.checker("de").unwrap().check_word("Grüezi"));
        assert_eq!(s.availability("ja-JP"), Availability::Unsupported);
        let fr = s.needs_download("fr-FR").unwrap();
        assert!(fr > 1_000_000);
        assert!(matches!(s.availability("fr"), Availability::Download(d) if d.code == "fr_FR"));
        assert!(s.checker("fr").is_err());
        let none = Spelling::new(Config::default());
        assert_eq!(none.needs_download("fr"), None);
        assert_eq!(notices().len(), 1 + catalog::DOWNLOADS.len());
        let _ = fs::remove_dir_all(&data);
        let _ = fs::remove_dir_all(&sys);
    }

    #[test]
    fn underline_waves_under_the_word() {
        let pts = squiggle((10.0, 20.0), (30.0, 20.0), (10.0, 10.0), 0.5);
        assert!(pts.len() > 10);
        assert!(
            pts.iter()
                .all(|p| p.0 >= 10.0 && p.0 <= 30.0 && (p.1 - 19.0).abs() <= 0.51)
        );
        assert!(squiggle((0.0, 0.0), (0.0, 0.0), (0.0, -1.0), 1.0).is_empty());
    }

    #[test]
    fn languages() {
        assert_eq!(language_for(Some("fr-CA"), Some("de"), "en-US"), "fr-CA");
        assert_eq!(language_for(None, Some("de-DE"), "en-US"), "de-DE");
        assert_eq!(language_for(Some(""), None, "it_IT.UTF-8"), "it_IT.UTF-8");
        assert_eq!(language_for(None, None, "C"), "en-US");
        let pdf = test_pdf::classic(
            &[
                "<</Type/Catalog/Pages 2 0 R/Lang(de-AT)>>",
                "<</Type/Pages/Kids[3 0 R]/Count 1>>",
                "<</Type/Page/Parent 2 0 R/MediaBox[0 0 10 10]>>",
            ],
            1,
        );
        let doc = Document::open(pdf).unwrap();
        assert_eq!(document_language(&doc).as_deref(), Some("de-AT"));
    }

    #[test]
    fn checks_on_other_threads() {
        let s = Arc::new(Spelling::new(Config::default()));
        let c = s.checker("en").unwrap();
        let h = pagebara_thread::spawn("spell-test", move || c.check_text("teh cat").len());
        assert_eq!(h.join().unwrap(), 1);
    }
}
