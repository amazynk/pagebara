//! The bundled American English dictionary.

use pagebara_spell::{en_us, tokens};

#[test]
fn checks_words() {
    let d = en_us();
    for w in [
        "the",
        "The",
        "THE",
        "Paris",
        "PARIS",
        "receive",
        "received",
        "receiving",
        "colors",
        "don't",
        "don’t",
        "unhappiness",
        "well-known",
        "NASA",
        "can't",
        "1st",
        "21st",
        "11th",
        "e-mail",
        "children's",
        "I",
    ] {
        assert!(d.check(w), "{w} should be correct");
    }
    for w in [
        "recieve",
        "teh",
        "colour",
        "paris",
        "Nasa",
        "speling",
        "2th",
        "accomodate",
    ] {
        assert!(!d.check(w), "{w} should be flagged");
    }
}

#[test]
fn suggests() {
    let d = en_us();
    let first = |w: &str| d.suggest(w).first().cloned().unwrap_or_default();
    assert_eq!(first("recieve"), "receive");
    assert_eq!(first("teh"), "the");
    assert_eq!(first("Teh"), "The");
    assert_eq!(first("speling"), "spelling");
    assert_eq!(first("paris"), "Paris");
    assert!(d.suggest("accomodate").contains(&"accommodate".into()));
    assert!(d.suggest("alot").contains(&"a lot".into()));
    assert!(d.suggest("colour").contains(&"color".into()));
    assert!(d.suggest("recieve").len() <= pagebara_spell::MAX_SUGGESTIONS);
}

#[test]
fn finds_misspellings_in_text() {
    let d = en_us();
    let text = "I recieve teh letter from Paris, see https://example.com or etc. Thanks!";
    let bad: Vec<&str> = d.misspellings(text).map(|t| t.word).collect();
    assert_eq!(bad, ["recieve", "teh"]);
    let t = tokens(text).nth(1).unwrap();
    assert_eq!(&text[t.range], "recieve");
    assert!(pagebara_spell::bundled_size() < 400_000);
}
