//! The dictionaries pagebara knows: the bundled en_US and the ones downloaded on request, each
//! with its source, pinned hashes and licence (shown in the app's licences list).

/// One downloadable file, pinned to a revision and hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DictFile {
    pub url: &'static str,
    /// Lower-case hex SHA-256 of the file.
    pub sha256: &'static str,
    pub size: u64,
}

/// A dictionary with its provenance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DictInfo {
    /// Language and region, as Hunspell names it ("de_DE").
    pub code: &'static str,
    /// English name of the language ("German").
    pub name: &'static str,
    /// SPDX licence expression.
    pub license: &'static str,
    /// Authors and copyright.
    pub attribution: &'static str,
    /// Where the word list is maintained.
    pub homepage: &'static str,
    /// `None` for the bundled dictionary.
    pub files: Option<[DictFile; 2]>,
}

impl DictInfo {
    /// Bytes to download (the `.aff` and `.dic` files).
    pub fn download_size(&self) -> u64 {
        self.files.map_or(0, |f| f[0].size + f[1].size)
    }

    /// The language subtag ("de").
    pub fn language(&self) -> &'static str {
        self.code.split('_').next().unwrap_or(self.code)
    }

    /// A line for an about box or licences list.
    pub fn notice(&self) -> String {
        format!(
            "{} spelling dictionary: {} Licence: {}. {}",
            self.name, self.attribution, self.license, self.homepage
        )
    }
}

/// Files in the LibreOffice dictionaries repository at a pinned commit.
macro_rules! files {
    ($path:literal, $aff:literal, $aff_size:literal, $dic:literal, $dic_size:literal) => {
        Some([
            DictFile {
                url: concat!(
                    "https://raw.githubusercontent.com/LibreOffice/dictionaries/",
                    "c0865a85eb67dc377f2cf8e8fe2c4fa9e53f0dcb/",
                    $path,
                    ".aff"
                ),
                sha256: $aff,
                size: $aff_size,
            },
            DictFile {
                url: concat!(
                    "https://raw.githubusercontent.com/LibreOffice/dictionaries/",
                    "c0865a85eb67dc377f2cf8e8fe2c4fa9e53f0dcb/",
                    $path,
                    ".dic"
                ),
                sha256: $dic,
                size: $dic_size,
            },
        ])
    };
}

/// American English, built into the program (SCOWL size 60, release 2026.02.25).
pub const EN_US: DictInfo = DictInfo {
    code: "en_US",
    name: "English (United States)",
    license: "LicenseRef-SCOWL AND LicenseRef-Ispell-BSD",
    attribution: "SCOWL word list, Copyright 2000-2026 Kevin Atkinson, and its sources \
                  (12dicts and ENABLE2K by Alan Beale, public domain); affix file derived from \
                  Ispell's english.aff, Copyright 1993 Geoff Kuenning (BSD licence). \
                  Maintained by Marco A.G. Pinto.",
    homepage: "https://wordlist.aspell.net",
    files: None,
};

/// The full licence of the bundled en_US dictionary (its README).
pub const EN_US_LICENSE: &str = include_str!("../data/README_en_US.txt");

/// Dictionaries downloaded on request, from LibreOffice's dictionaries repository.
pub const DOWNLOADS: &[DictInfo] = &[
    DictInfo {
        code: "de_DE",
        name: "German (Germany)",
        license: "GPL-2.0-only OR GPL-3.0-only OR LicenseRef-OASIS-0.1",
        attribution: "igerman98 by Björn Jacke, \"frami\" extension by Franz Michael Baumann.",
        homepage: "https://www.j3e.de/ispell/igerman98/",
        files: files!(
            "de/de_DE_frami",
            "646bf3333ac69c23e9d794533ee5241d6f755c359e8fe10a648f87613743d594",
            19_067,
            "4ca3c958b0e5545910999bc246f668840bf8ede3df8e5e6790d05edd5a586c38",
            4_356_903
        ),
    },
    DictInfo {
        code: "fr_FR",
        name: "French",
        license: "MPL-2.0",
        attribution: "Dictionnaires orthographiques français 7.7 by Olivier R. and the \
                      Grammalecte contributors.",
        homepage: "https://grammalecte.net/",
        files: files!(
            "fr_FR/dictionaries/fr",
            "c176610cd5dc4846806a65ddd029f422d87978bf58f224aa44222662a16a2de5",
            199_870,
            "b78a868e31dd6e373b6c3217969afb898a9acde828a5e7ef97308da42218c88c",
            1_236_490
        ),
    },
    DictInfo {
        code: "es_ES",
        name: "Spanish (Spain)",
        license: "GPL-3.0-or-later OR LGPL-3.0-or-later OR MPL-1.1",
        attribution: "RLA-ES, the Spanish Language Resources project (Santiago Bosio and \
                      contributors).",
        homepage: "https://github.com/sbosio/rla-es",
        files: files!(
            "es/es_ES",
            "e73a9bf8e1383f4986a5dc9e2fbed49371c0c61f511c626d15586bd433c1cad9",
            169_202,
            "6975dddec3d5d2c676069537bc67b4b5f786c65c5d4cf6703a82acf779ac9ec1",
            715_989
        ),
    },
    DictInfo {
        code: "it_IT",
        name: "Italian",
        license: "GPL-3.0-only",
        attribution: "Copyright 2001-2015 Gianluca Turconi, Davide Prina, Andrea Pescetti; \
                      2020 LibreItalia (Marina Latini).",
        homepage: "https://libreitalia.org",
        files: files!(
            "it_IT/it_IT",
            "951afaa19272f13555b8823e8bcf9ccf78f8fe1a07835bdfb912ab3e4d537c2b",
            70_054,
            "bae1e3501dcd2a923669592493b3fde6c02aae7c7aab83bf5e5b49077e73dd64",
            1_295_083
        ),
    },
    DictInfo {
        code: "pt_BR",
        name: "Portuguese (Brazil)",
        license: "LGPL-3.0-only OR MPL-2.0",
        attribution: "VERO, Copyright 2006-2013 Raimundo Santos Moura and the Brazilian \
                      LibreOffice community.",
        homepage: "https://pt-br.libreoffice.org/projetos/projeto-vero-verificador-ortografico/",
        files: files!(
            "pt_BR/pt_BR",
            "21d8ad2a769a60e17e2b5ea4ef11d4d593a58b9e2a82d642ef82d6a4c5523865",
            979_792,
            "a38bfb26b68ece2834e79fe83e48d5792652970ace12db89d1b9674bf9933183",
            4_477_695
        ),
    },
    DictInfo {
        code: "nl_NL",
        name: "Dutch",
        license: "BSD-3-Clause OR CC-BY-3.0",
        attribution: "OpenTaal (Simon Brouwer, Sander van Geloven, Ruud Baars) and the \
                      Nederlandstalige TeX Gebruikersgroep.",
        homepage: "https://www.opentaal.org",
        files: files!(
            "nl_NL/nl_NL",
            "f0233d4f721f4661cf5f4d05ed2739549322bf3b6b66764b55a38257e1e16e6f",
            46_557,
            "bc28af45307700a9927ad5719184da44dfd7eed4f707b8c1477f6d8a21b586a6",
            2_488_246
        ),
    },
    DictInfo {
        code: "pl_PL",
        name: "Polish",
        license: "GPL-2.0-only OR LGPL-2.1-only OR MPL-1.1 OR Apache-2.0 OR CC-BY-4.0",
        attribution: "Słownik SJP.PL (sjp.pl), version of 2026-05-11.",
        homepage: "https://sjp.pl/sl/en/",
        files: files!(
            "pl_PL/pl_PL",
            "82973651651aa930335c865b339b98db376ca3dbf3a661b70b9eeb71fdf41dca",
            257_542,
            "c0848440599eb88e5aca500418d5f389e562ec2c157b63dbe39d354658ffba49",
            5_252_096
        ),
    },
    DictInfo {
        code: "ru_RU",
        name: "Russian",
        license: "LicenseRef-Lebedev-BSD",
        attribution: "Copyright 1997-2008 Alexander I. Lebedev (BSD-style licence; modified \
                      versions must be marked as such).",
        homepage: "https://github.com/LibreOffice/dictionaries/tree/master/ru_RU",
        files: files!(
            "ru_RU/ru_RU",
            "38ce7d4af78e211e9bafe4bf7e3d6a2c420591136cb738ec6648f8fdf6524cd7",
            71_236,
            "f6047416a0204adbecf3a451b874ec8a97ee37e2cbc714466ef04d8dbcc0d6fc",
            3_473_191
        ),
    },
    DictInfo {
        code: "uk_UA",
        name: "Ukrainian",
        license: "MPL-1.1",
        attribution: "dict_uk project (Andriy Rysin and contributors).",
        homepage: "https://github.com/brown-uk/dict_uk",
        files: files!(
            "uk_UA/uk_UA",
            "2219dd15e9802adebc45722c60943b1472640260491af38dd3e43b07e75585e6",
            211_759,
            "2e5a9e67be63bdb089b3459addb5d71113319d13768e277bcae20f3cc1ad5a93",
            8_919_324
        ),
    },
    DictInfo {
        code: "tr_TR",
        name: "Turkish",
        license: "MPL-2.0",
        attribution: "hunspell-tr 1.1.1 by the Turkish Data Depository group (Ali Safaya, \
                      Arda Göktoğan, Deniz Yuret, Emirhan Kurtuluş, Taner Sezer).",
        homepage: "https://github.com/tdd-ai/hunspell-tr",
        files: files!(
            "tr_TR/tr_TR",
            "a04a227b2ee45574000b876a1afa982e3436d49ebb97a7a796ddc5ec0cc8191b",
            2_283_891,
            "48e5352c5770956b24f355265aef7649b25bcb4d933c0b254ea8b419c241f4bd",
            36_136_702
        ),
    },
    DictInfo {
        code: "ar",
        name: "Arabic",
        license: "GPL-2.0-or-later OR LGPL-2.1-or-later OR MPL-1.1",
        attribution: "Ayaspell project, packaged by Ahmad Farghal.",
        homepage: "http://ayaspell.sourceforge.net/",
        files: files!(
            "ar/ar",
            "cec30b8621001e49618feb05aec1984c5fcfbf7d2ec309901d5cbf66585217a3",
            86_949,
            "2a3e5367f61c1583734db9d66734f5603e6be5c2d227cf5c5cd7e4ca586e34fe",
            7_217_161
        ),
    },
    DictInfo {
        code: "he_IL",
        name: "Hebrew",
        license: "AGPL-3.0-or-later",
        attribution: "Generated from Hspell 1.4, Copyright 2000-2017 Nadav Har'El and Dan \
                      Kenigsberg.",
        homepage: "http://hspell.ivrix.org.il/",
        files: files!(
            "he_IL/he_IL",
            "6caf86b3a545be5614f135d33a48baa244a59aca43f051dda6173d5d9cbc7700",
            78_883,
            "5f5331f90ed775bd527f6fb7ad1ead9a1b7d8ce46ad640c2387d9d1dc91d3058",
            7_796_259
        ),
    },
];

/// The language and region of a BCP 47 or POSIX tag: "de-DE", "de_DE.UTF-8" and "DE" give
/// ("de", Some("DE")) or ("de", None).
pub fn parse_tag(tag: &str) -> Option<(String, Option<String>)> {
    let tag = tag.split(['.', '@']).next()?;
    let mut parts = tag.split(['-', '_']);
    let lang = parts.next()?.to_ascii_lowercase();
    if !(2..=3).contains(&lang.len()) || !lang.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    // The region is the first two-letter or three-digit subtag after any script subtag.
    let region = parts
        .find(|p| {
            (p.len() == 2 && p.bytes().all(|b| b.is_ascii_alphabetic()))
                || (p.len() == 3 && p.bytes().all(|b| b.is_ascii_digit()))
        })
        .map(|r| r.to_ascii_uppercase());
    Some((lang, region))
}

/// The known dictionary for a language tag: the same language and region, else the same
/// language ("de-AT" gets de_DE, "pt" gets pt_BR).
pub fn find(tag: &str) -> Option<&'static DictInfo> {
    let (lang, region) = parse_tag(tag)?;
    let all = || std::iter::once(&EN_US).chain(DOWNLOADS);
    let exact = region.and_then(|r| {
        let code = format!("{lang}_{r}");
        all().find(|d| d.code == code)
    });
    exact.or_else(|| all().find(|d| d.language() == lang))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags() {
        assert_eq!(parse_tag("de-DE"), Some(("de".into(), Some("DE".into()))));
        assert_eq!(
            parse_tag("sr-Latn-RS"),
            Some(("sr".into(), Some("RS".into())))
        );
        assert_eq!(
            parse_tag("en_GB.UTF-8"),
            Some(("en".into(), Some("GB".into())))
        );
        assert_eq!(parse_tag("FR"), Some(("fr".into(), None)));
        assert_eq!(parse_tag("C"), None);
        assert_eq!(find("de-AT").unwrap().code, "de_DE");
        assert_eq!(find("en-GB").unwrap().code, "en_US");
        assert_eq!(find("pt").unwrap().code, "pt_BR");
        assert_eq!(find("ar-EG").unwrap().code, "ar");
        assert!(find("ja").is_none());
    }

    #[test]
    fn pinned_files() {
        for d in DOWNLOADS {
            let files = d.files.unwrap();
            for f in files {
                assert_eq!(f.sha256.len(), 64, "{}", d.code);
                assert!(
                    f.sha256
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                );
                assert!(
                    f.url
                        .starts_with("https://raw.githubusercontent.com/LibreOffice/dictionaries/")
                );
            }
            assert!(files[0].url.ends_with(".aff") && files[1].url.ends_with(".dic"));
            assert!(d.download_size() > 0 && !d.license.is_empty());
        }
        assert!(EN_US_LICENSE.contains("Kevin Atkinson"));
    }
}
