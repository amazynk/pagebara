//! Real dictionaries, when they are at hand: the system's (`/usr/share/hunspell`) and those
//! in `PAGEBARA_SPELL_DICTS` (a folder of `<code>.aff`/`<code>.dic` pairs). Ignored by default;
//! run with `--ignored --nocapture` (in release mode for meaningful timings).

use std::path::{Path, PathBuf};
use std::time::Instant;

use pagebara_spell::Dictionary;

fn find(code: &str) -> Option<(PathBuf, PathBuf)> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PAGEBARA_SPELL_DICTS")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    dirs.push("/usr/share/hunspell".into());
    dirs.into_iter().find_map(|d| {
        let aff = d.join(format!("{code}.aff"));
        let dic = d.join(format!("{code}.dic"));
        (aff.is_file() && dic.is_file()).then_some((aff, dic))
    })
}

fn time<T>(f: impl FnOnce() -> T) -> (T, f64) {
    let t = Instant::now();
    let v = f();
    (v, t.elapsed().as_secs_f64() * 1000.0)
}

fn bench(code: &str, aff: &Path, dic: &Path, good: &[&str], bad: &[&str]) -> Dictionary {
    let (d, load) = time(|| Dictionary::open(aff, dic).unwrap());
    let (_, check) = time(|| {
        for _ in 0..100 {
            for w in good.iter().chain(bad) {
                std::hint::black_box(d.check(w));
            }
        }
    });
    let per_word = check * 1000.0 / (100 * (good.len() + bad.len())) as f64;
    let (sugs, suggest) = time(|| bad.iter().map(|w| d.suggest(w)).collect::<Vec<_>>());
    println!(
        "{code}: {} entries, load {load:.0} ms, check {per_word:.1} µs/word, suggest {:.1} ms/word",
        d.len(),
        suggest / bad.len().max(1) as f64
    );
    for (w, s) in bad.iter().zip(&sugs) {
        println!("  {w} -> {s:?}");
    }
    for w in good {
        assert!(d.check(w), "{code}: {w} should be correct");
    }
    for w in bad {
        assert!(!d.check(w), "{code}: {w} should be flagged");
    }
    d
}

#[test]
#[ignore]
fn real_dictionaries() {
    let cases: &[(&str, &[&str], &[&str])] = &[
        (
            "en_US",
            &["receive", "Paris", "unhappiness", "colors"],
            &["recieve", "teh", "colour"],
        ),
        (
            "en_GB",
            &["colour", "organisation", "London"],
            &["color", "recieve"],
        ),
        (
            "de_DE",
            &[
                "Haus",
                "Haustür",
                "Arbeitszimmer",
                "Straße",
                "STRASSE",
                "gemacht",
                "Donaudampfschifffahrt",
                "Bundesverfassungsgericht",
                "Häuser",
            ],
            &["Hauss", "Strase", "gemachd", "Arbeitzimmer"],
        ),
        (
            "fr_FR",
            &["maison", "l'homme", "aujourd'hui", "étaient"],
            &["maisonn", "aujourdhui"],
        ),
        (
            "es_ES",
            &["casa", "corazón", "hablábamos"],
            &["corazon", "kasa"],
        ),
        (
            "it_IT",
            &["casa", "perché", "mangiavamo"],
            &["perche", "casaa"],
        ),
        ("pt_BR", &["casa", "coração", "não"], &["coracao", "naum"]),
        (
            "nl_NL",
            &["huis", "fietsenstalling", "gezellig"],
            &["huiss", "gezelig"],
        ),
        ("pl_PL", &["dom", "źdźbło", "zażółć"], &["domm", "zazolc"]),
        ("ru_RU", &["дом", "привет", "говорить"], &["превет", "дoм"]),
        ("uk_UA", &["дім", "привіт", "говорити"], &["превіт"]),
        (
            "tr_TR",
            &["ev", "evler", "İstanbul", "kitaplarımızdan"],
            &["evlr", "kitapalr"],
        ),
        ("ar", &["كتاب", "مدرسة"], &["كتابب"]),
        ("he_IL", &["שלום", "ספר"], &["שלוםם"]),
    ];
    for (code, good, bad) in cases {
        match find(code) {
            Some((aff, dic)) => {
                bench(code, &aff, &dic, good, bad);
            }
            None => println!("{code}: not available"),
        }
    }
}
