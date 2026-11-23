//! Reading text aloud with the system's speech engine, a sentence at a time, so the reader
//! can follow along and pause between sentences: Speech Dispatcher on Linux (see
//! [`ssip`]), else eSpeak; `say` on macOS; SAPI through PowerShell on Windows. The engines
//! but Speech Dispatcher run as a command per sentence, with no native bindings.
//!
//! Each engine lists its voices with a rough idea of how natural they sound, so that
//! reading can use a natural voice when one is installed (see [`pick`]) and the reader can
//! choose another.

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
mod ssip;

use std::io::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A voice of a speech engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Voice {
    /// What the engine knows it by (see [`Speaker::speak`]); kept in the settings.
    pub id: String,
    /// The name shown.
    pub name: String,
    /// Its language as a BCP 47 tag (`en-GB`), when known. A voice without one is an
    /// engine that picks its voice by language.
    pub lang: Option<String>,
    /// How natural it sounds: 0 robotic or a novelty, 1 plain, 2 good, 3 natural.
    pub quality: u8,
}

/// Voices of this quality or better are used without being chosen.
pub const GOOD: u8 = 2;

/// Something that speaks.
pub trait Speaker: Send + Sync {
    /// The engine's name, for the UI.
    fn name(&self) -> &str;
    /// The voices to choose from, in no particular order. Asks the engine, which can take
    /// a moment; empty when it cannot tell.
    fn voices(&self) -> Vec<Voice> {
        Vec::new()
    }
    /// Where to get more natural voices for this engine, said to the reader when none is
    /// installed.
    fn more_voices(&self) -> Option<String> {
        None
    }
    /// Speaks `text` and returns when done, or early when [`Speaker::stop`] is called.
    /// `rate` is 1.0 for normal speed; `lang` a BCP 47 tag when known; `voice` the id of
    /// one of [`Speaker::voices`], else the engine chooses (by `lang`). An error means the
    /// engine could not speak (it is missing or broken), not that it was stopped.
    fn speak(
        &self,
        text: &str,
        rate: f32,
        lang: Option<&str>,
        voice: Option<&str>,
    ) -> std::io::Result<()>;
    /// Stops what is being spoken, from any thread.
    fn stop(&self);
}

/// The voice to read text in `lang` with: the one `chosen` (when it is still there), else
/// the most natural [`GOOD`] voice for the language, preferring its region, else `None`
/// for the engine's own choice.
pub fn pick<'a>(
    voices: &'a [Voice],
    chosen: Option<&'a str>,
    lang: Option<&str>,
) -> Option<&'a str> {
    if let Some(c) = chosen
        && (voices.is_empty() || voices.iter().any(|v| v.id == c))
    {
        return Some(c);
    }
    let lang = lang?;
    voices
        .iter()
        .filter(|v| v.quality >= GOOD)
        .filter_map(|v| {
            let l = v.lang.as_deref()?;
            same_language(l, lang).then(|| (v.quality, l.eq_ignore_ascii_case(lang), v))
        })
        .max_by(|a, b| {
            (a.0, a.1)
                .cmp(&(b.0, b.1))
                .then_with(|| b.2.name.cmp(&a.2.name))
        })
        .map(|(_, _, v)| v.id.as_str())
}

/// Whether two language tags (`en-GB`, `en_US`) are of the same language.
pub fn same_language(a: &str, b: &str) -> bool {
    primary(a).eq_ignore_ascii_case(primary(b))
}

/// A locale (`de_DE.UTF-8`, `pt-BR`) as a BCP 47 tag (`de-DE`), if it is one.
pub fn locale_tag(locale: &str) -> Option<String> {
    let tag = locale.split(['.', '@']).next()?.replace('_', "-");
    (is_tag(&tag) && tag.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) && tag != "C")
        .then_some(tag)
}

/// The voices in the order to offer them: those for `lang` first, the most natural first,
/// then the others by language.
pub fn sort_voices(voices: &mut [Voice], lang: Option<&str>) {
    let ours = |v: &Voice| match (v.lang.as_deref(), lang) {
        (Some(l), Some(u)) => same_language(l, u),
        // Engines that choose by language read any.
        (None, _) => true,
        _ => false,
    };
    voices.sort_by(|a, b| {
        ours(b)
            .cmp(&ours(a))
            .then(b.quality.cmp(&a.quality))
            .then_with(|| a.lang.cmp(&b.lang))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

/// The speech engine of this system, if there is one.
pub fn detect() -> Option<Arc<dyn Speaker>> {
    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "ios", target_os = "android"))
    ))]
    if let Some(s) = ssip::Ssip::detect() {
        return Some(Arc::new(s));
    }
    CommandSpeaker::detect().map(|s| Arc::new(s) as Arc<dyn Speaker>)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Engine {
    Espeak,
    Say,
    PowerShell,
}

/// A speech engine run as a child process per sentence.
#[derive(Debug)]
pub struct CommandSpeaker {
    engine: Engine,
    program: PathBuf,
    child: Mutex<Option<Child>>,
}

impl CommandSpeaker {
    /// The first engine found on this system.
    pub fn detect() -> Option<CommandSpeaker> {
        let candidates: &[(&str, Engine)] = if cfg!(target_os = "macos") {
            &[("say", Engine::Say)]
        } else if cfg!(windows) {
            // Windows PowerShell and PowerShell 7 both reach SAPI through COM.
            &[
                ("powershell.exe", Engine::PowerShell),
                ("pwsh.exe", Engine::PowerShell),
            ]
        } else {
            &[("espeak-ng", Engine::Espeak), ("espeak", Engine::Espeak)]
        };
        candidates.iter().find_map(|&(name, engine)| {
            find_program(name).map(|program| CommandSpeaker {
                engine,
                program,
                child: Mutex::new(None),
            })
        })
    }

    /// The command for `text`, and whether the text goes to its standard input.
    fn command(
        &self,
        text: &str,
        rate: f32,
        lang: Option<&str>,
        voice: Option<&str>,
    ) -> (Command, bool) {
        let mut c = Command::new(&self.program);
        let wpm = (175.0 * rate).clamp(80.0, 450.0).round() as i32;
        // Voice ids come from the settings file: nothing that reads as an option.
        let voice = voice.filter(|v| !v.is_empty() && !v.starts_with('-'));
        let stdin = match self.engine {
            Engine::Espeak => {
                c.args(["-s", &wpm.to_string()]);
                if let Some(v) = voice {
                    c.args(["-v", v]);
                } else if let Some(l) = lang.map(primary).filter(|l| is_tag(l)) {
                    c.args(["-v", l]);
                }
                c.args(["--", text]);
                false
            }
            Engine::Say => {
                // With no text argument `say` reads standard input.
                c.args(["-r", &wpm.to_string()]);
                if let Some(v) = voice {
                    c.args(["-v", v]);
                }
                true
            }
            Engine::PowerShell => {
                // The voice's name goes in the environment, so it needs no quoting.
                c.env("PAGEBARA_VOICE", voice.unwrap_or(""));
                c.args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    &sapi_script(rate, if voice.is_some() { None } else { lang }),
                ]);
                true
            }
        };
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: no console window flashing up per sentence.
            c.creation_flags(0x0800_0000);
        }
        (c, stdin)
    }
}

/// The PowerShell script that reads its standard input aloud with SAPI: the rate is
/// -10..10, and a voice for `lang` is chosen when one is installed. The text comes as
/// UTF-8 on standard input, so nothing in it needs quoting.
fn sapi_script(rate: f32, lang: Option<&str>) -> String {
    let r = ((rate - 1.0) * 10.0).clamp(-10.0, 10.0).round() as i32;
    let voice = match lang.filter(|l| is_tag(l)) {
        Some(l) => format!(
            "try {{ $id = '{{0:X}}' -f [Globalization.CultureInfo]::GetCultureInfo('{l}').LCID; \
             $vs = $v.GetVoices('Language=' + $id); \
             if ($vs.Count -gt 0) {{ $v.Voice = $vs.Item(0) }} }} catch {{ }}; "
        ),
        None => String::new(),
    };
    format!(
        "$ErrorActionPreference = 'Stop'; [Console]::InputEncoding = [Text.Encoding]::UTF8; \
         $v = New-Object -ComObject SAPI.SpVoice; {voice}\
         foreach ($t in $v.GetVoices()) {{ \
         if ($env:PAGEBARA_VOICE -and $t.GetAttribute('Name') -eq $env:PAGEBARA_VOICE) {{ $v.Voice = $t }} }}; \
         $v.Rate = {r}; $null = $v.Speak([Console]::In.ReadToEnd())"
    )
}

/// The PowerShell script that lists the SAPI voices: a name and a language tag a line.
const SAPI_VOICES: &str = "[Console]::OutputEncoding = [Text.Encoding]::UTF8; \
    $v = New-Object -ComObject SAPI.SpVoice; \
    foreach ($t in $v.GetVoices()) { $l = ''; \
    try { $l = [Globalization.CultureInfo]::GetCultureInfo([Convert]::ToInt32(\
    ($t.GetAttribute('Language') -split ';')[0], 16)).Name } catch { }; \
    [Console]::Out.WriteLine($t.GetAttribute('Name') + \"`t\" + $l) }";

/// The voices `say -v '?'` lists: a name, a locale and a sample sentence a line.
fn say_voices(out: &str) -> Vec<Voice> {
    // Voices made for fun, and the old robotic ones.
    const NOVELTY: &[&str] = &[
        "Albert",
        "Bad News",
        "Bahh",
        "Bells",
        "Boing",
        "Bubbles",
        "Cellos",
        "Deranged",
        "Fred",
        "Good News",
        "Hysterical",
        "Jester",
        "Junior",
        "Kathy",
        "Organ",
        "Princess",
        "Ralph",
        "Superstar",
        "Trinoids",
        "Whisper",
        "Wobble",
        "Zarvox",
    ];
    out.lines()
        .filter_map(|line| {
            let (left, _sample) = line.split_once('#').unwrap_or((line, ""));
            let (name, locale) = left.trim_end().rsplit_once(char::is_whitespace)?;
            let name = name.trim();
            if name.is_empty() {
                return None;
            }
            let quality = if name.contains("(Premium)") {
                3
            } else if name.contains("(Enhanced)") {
                2
            } else if NOVELTY.contains(&name) {
                0
            } else {
                1
            };
            Some(Voice {
                id: name.to_owned(),
                name: name.to_owned(),
                lang: locale_tag(locale),
                quality,
            })
        })
        .collect()
}

/// The voices `espeak-ng --voices` lists, in columns: priority, language, age and gender,
/// name, file.
fn espeak_voices(out: &str) -> Vec<Voice> {
    out.lines()
        .skip(1)
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            let (_pty, lang, _gender, name, file) =
                (f.next()?, f.next()?, f.next()?, f.next()?, f.next()?);
            Some(Voice {
                id: file.to_owned(),
                name: name.replace('_', " "),
                lang: locale_tag(lang),
                quality: 0,
            })
        })
        .collect()
}

/// The SAPI voices listed by [`SAPI_VOICES`]. Natural voices (Microsoft's online and
/// neural ones, through an adapter) say so in their names.
fn sapi_voices(out: &str) -> Vec<Voice> {
    out.lines()
        .filter_map(|line| {
            let (name, lang) = line
                .trim_end()
                .split_once('\t')
                .unwrap_or((line.trim_end(), ""));
            let name = name.trim();
            if name.is_empty() {
                return None;
            }
            let natural = ["Natural", "Neural"].iter().any(|w| name.contains(w));
            Some(Voice {
                id: name.to_owned(),
                name: name.to_owned(),
                lang: locale_tag(lang),
                quality: if natural { 3 } else { 1 },
            })
        })
        .collect()
}

/// A language tag safe to pass on: letters, digits and hyphens.
fn is_tag(l: &str) -> bool {
    !l.is_empty() && l.len() <= 35 && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// `en` of `en-GB`.
fn primary(lang: &str) -> &str {
    lang.split(['-', '_']).next().unwrap_or(lang)
}

impl CommandSpeaker {
    /// What the engine prints when asked for its voices.
    fn voice_list(&self) -> std::io::Result<String> {
        let mut c = Command::new(&self.program);
        match self.engine {
            Engine::Espeak => c.arg("--voices"),
            Engine::Say => c.args(["-v", "?"]),
            Engine::PowerShell => {
                c.args(["-NoProfile", "-NonInteractive", "-Command", SAPI_VOICES])
            }
        };
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000);
        }
        let out = c.stdin(Stdio::null()).stderr(Stdio::null()).output()?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

impl Speaker for CommandSpeaker {
    fn name(&self) -> &str {
        match self.engine {
            Engine::Espeak => "eSpeak",
            Engine::Say => "macOS speech",
            Engine::PowerShell => "Windows speech",
        }
    }

    fn voices(&self) -> Vec<Voice> {
        let out = match self.voice_list() {
            Ok(out) => out,
            Err(e) => {
                log::warn!("speech: cannot list the voices: {e}");
                return Vec::new();
            }
        };
        match self.engine {
            Engine::Espeak => espeak_voices(&out),
            Engine::Say => say_voices(&out),
            Engine::PowerShell => sapi_voices(&out),
        }
    }

    fn more_voices(&self) -> Option<String> {
        Some(match self.engine {
            Engine::Espeak => pagebara_i18n::tr!(
                "eSpeak voices sound robotic. For natural voices, install Speech Dispatcher with Piper or RHVoice."
            ),
            Engine::Say => pagebara_i18n::tr!(
                "More natural voices can be downloaded in System Settings > Accessibility > Spoken Content > System voice > Manage Voices."
            ),
            Engine::PowerShell => pagebara_i18n::tr!(
                "More voices can be added in Settings > Time & language > Speech."
            ),
        })
    }

    fn speak(
        &self,
        text: &str,
        rate: f32,
        lang: Option<&str>,
        voice: Option<&str>,
    ) -> std::io::Result<()> {
        let (mut cmd, stdin) = self.command(text, rate, lang, voice);
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        cmd.stdin(if stdin { Stdio::piped() } else { Stdio::null() });
        let mut child = cmd.spawn()?;
        if stdin && let Some(mut input) = child.stdin.take() {
            // Dropping `input` closes it, which ends the text.
            input.write_all(text.as_bytes())?;
        }
        *self.child.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
        loop {
            let mut guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
            // Taken by `stop`.
            let Some(c) = guard.as_mut() else {
                return Ok(());
            };
            if let Some(status) = c.try_wait()? {
                *guard = None;
                return if status.success() {
                    Ok(())
                } else {
                    Err(std::io::Error::other(format!(
                        "{} failed ({status})",
                        self.name()
                    )))
                };
            }
            drop(guard);
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn stop(&self) {
        if let Some(mut c) = self.child.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Another engine, keeping what it was asked to say: for scripted runs that check what
/// was read with the real engine.
pub struct Logged {
    inner: Arc<dyn Speaker>,
    pub spoken: Arc<Mutex<Vec<String>>>,
}

impl Logged {
    pub fn new(inner: Arc<dyn Speaker>) -> Self {
        Logged {
            inner,
            spoken: Arc::default(),
        }
    }
}

impl std::fmt::Debug for Logged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Logged")
            .field("engine", &self.inner.name())
            .finish()
    }
}

impl Speaker for Logged {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn voices(&self) -> Vec<Voice> {
        self.inner.voices()
    }

    fn more_voices(&self) -> Option<String> {
        self.inner.more_voices()
    }

    fn speak(
        &self,
        text: &str,
        rate: f32,
        lang: Option<&str>,
        voice: Option<&str>,
    ) -> std::io::Result<()> {
        self.spoken
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(text.to_owned());
        self.inner.speak(text, rate, lang, voice)
    }

    fn stop(&self) {
        self.inner.stop();
    }
}

/// Records what it is asked to say instead of speaking, taking `pace` per sentence: for
/// tests and machines without a speech engine.
#[derive(Debug, Default)]
pub struct Recorder {
    pub spoken: Arc<Mutex<Vec<String>>>,
    pace: Duration,
    stopped: Mutex<bool>,
}

impl Recorder {
    pub fn new(pace: Duration) -> Self {
        Recorder {
            pace,
            ..Recorder::default()
        }
    }
}

impl Speaker for Recorder {
    fn name(&self) -> &str {
        "test recorder"
    }

    /// A robotic voice and natural ones for English and French, as an engine might have.
    fn voices(&self) -> Vec<Voice> {
        let voice = |id: &str, lang: &str, quality| Voice {
            id: id.into(),
            name: id.into(),
            lang: Some(lang.into()),
            quality,
        };
        vec![
            voice("Robot", "en", 0),
            voice("Natural English", "en-US", 3),
            voice("Naturel français", "fr-FR", 3),
        ]
    }

    fn speak(
        &self,
        text: &str,
        _rate: f32,
        _lang: Option<&str>,
        _voice: Option<&str>,
    ) -> std::io::Result<()> {
        *self.stopped.lock().unwrap_or_else(|e| e.into_inner()) = false;
        self.spoken
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(text.to_owned());
        let step = Duration::from_millis(10);
        let mut waited = Duration::ZERO;
        while waited < self.pace && !*self.stopped.lock().unwrap_or_else(|e| e.into_inner()) {
            std::thread::sleep(step);
            waited += step;
        }
        Ok(())
    }

    fn stop(&self) {
        *self.stopped.lock().unwrap_or_else(|e| e.into_inner()) = true;
    }
}

/// `name` on the `PATH` (with the usual extensions on Windows).
pub fn find_program(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let p = dir.join(name);
        is_file(&p).then_some(p)
    })
}

fn is_file(p: &Path) -> bool {
    p.metadata().is_ok_and(|m| m.is_file())
}

/// Byte ranges of the sentences of `text`, trimmed. A sentence ends at `.`, `!`, `?` or `…`
/// (with closing quotes or brackets) before a space and an upper-case letter, digit or
/// opening quote; common abbreviations and initials do not end one.
pub fn sentences(text: &str) -> Vec<Range<usize>> {
    const ABBREVIATIONS: &[&str] = &[
        "mr", "mrs", "ms", "dr", "prof", "st", "e.g", "i.e", "etc", "vs", "no", "fig", "p", "pp",
        "vol", "approx", "inc", "ltd", "jr", "sr",
    ];
    let mut out = Vec::new();
    let mut start = 0;
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut i = 0;
    while i < chars.len() {
        let (at, c) = chars[i];
        if matches!(c, '.' | '!' | '?' | '…' | '。' | '！' | '？') {
            let mut j = i + 1;
            while j < chars.len() && matches!(chars[j].1, '"' | '\'' | '”' | '’' | ')' | ']') {
                j += 1;
            }
            let end = chars.get(j).map_or(text.len(), |&(b, _)| b);
            let at_space = j >= chars.len() || chars[j].1.is_whitespace();
            let next = chars[j..].iter().find(|(_, c)| !c.is_whitespace());
            let starts = next.is_none_or(|&(_, n)| {
                n.is_uppercase() || n.is_ascii_digit() || matches!(n, '"' | '“' | '‘' | '(')
            });
            let word = text[start..at]
                .rsplit(|c: char| c.is_whitespace())
                .next()
                .unwrap_or("")
                .to_lowercase();
            let abbreviation = c == '.'
                && (ABBREVIATIONS.contains(&word.as_str())
                    || (word.chars().count() == 1 && word.chars().all(char::is_alphabetic)));
            let cjk = matches!(c, '。' | '！' | '？');
            if (at_space && starts && !abbreviation) || cjk {
                push_trimmed(text, start..end, &mut out);
                start = end;
                i = j;
                continue;
            }
        }
        i += 1;
    }
    push_trimmed(text, start..text.len(), &mut out);
    out
}

fn push_trimmed(text: &str, r: Range<usize>, out: &mut Vec<Range<usize>>) {
    let s = &text[r.clone()];
    let lead = s.len() - s.trim_start().len();
    let trail = s.len() - s.trim_end().len();
    if lead + trail < s.len() {
        out.push(r.start + lead..r.end - trail);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(t: &str) -> Vec<&str> {
        sentences(t).into_iter().map(|r| &t[r]).collect()
    }

    #[test]
    fn splits_sentences() {
        assert_eq!(
            split("Hello there. How are you? Fine!  Thanks."),
            ["Hello there.", "How are you?", "Fine!", "Thanks."]
        );
        assert_eq!(
            split("See e.g. the report by Dr. Smith. It is 3.5 pages."),
            ["See e.g. the report by Dr. Smith.", "It is 3.5 pages."]
        );
        assert_eq!(
            split("He said “stop.” Then left. J. R. Tolkien wrote it"),
            ["He said “stop.”", "Then left.", "J. R. Tolkien wrote it"]
        );
        assert_eq!(split("no end"), ["no end"]);
        assert_eq!(split("  "), Vec::<&str>::new());
        assert_eq!(split("一つ。二つ。"), ["一つ。", "二つ。"]);
    }

    #[test]
    fn engine_commands() {
        let s = CommandSpeaker {
            engine: Engine::Espeak,
            program: "espeak-ng".into(),
            child: Mutex::new(None),
        };
        let (c, stdin) = s.command("-Hi there", 1.2, Some("en-GB"), None);
        let args: Vec<_> = c.get_args().map(|a| a.to_string_lossy()).collect();
        assert_eq!(args, ["-s", "210", "-v", "en", "--", "-Hi there"]);
        assert!(!stdin);
        let (c, _) = s.command("Hi", 1.0, Some("en-GB"), Some("gmw/en-GB-scotland"));
        let args: Vec<_> = c.get_args().map(|a| a.to_string_lossy()).collect();
        assert_eq!(args, ["-s", "175", "-v", "gmw/en-GB-scotland", "--", "Hi"]);
        // An id that would read as an option is not passed on.
        let (c, _) = s.command("Hi", 1.0, None, Some("--stdout"));
        assert_eq!(c.get_args().count(), 4);
        let s = CommandSpeaker {
            engine: Engine::Say,
            ..s
        };
        let (c, stdin) = s.command("Hi", 1.0, None, None);
        assert!(stdin);
        assert_eq!(c.get_args().collect::<Vec<_>>(), ["-r", "175"]);
        let (c, _) = s.command("Hi", 1.0, None, Some("Zoe (Premium)"));
        assert_eq!(
            c.get_args().collect::<Vec<_>>(),
            ["-r", "175", "-v", "Zoe (Premium)"]
        );
        let s = CommandSpeaker {
            engine: Engine::PowerShell,
            ..s
        };
        let (c, _) = s.command("Hi", 1.0, Some("fr-FR"), Some("Microsoft Hortense"));
        let script = c.get_args().last().unwrap().to_string_lossy().into_owned();
        // A chosen voice is not replaced by one for the language.
        assert!(!script.contains("GetCultureInfo('fr-FR')"), "{script}");
        assert!(
            c.get_envs().any(|(k, v)| k == "PAGEBARA_VOICE"
                && v == Some(std::ffi::OsStr::new("Microsoft Hortense")))
        );
        let (c, stdin) = s.command("Hi", 0.5, Some("fr-FR"), None);
        assert!(stdin);
        let script = c.get_args().last().unwrap().to_string_lossy();
        assert!(script.contains("$v.Rate = -5"), "{script}");
        assert!(script.contains("GetCultureInfo('fr-FR')"), "{script}");
        assert!(
            script.contains("InputEncoding = [Text.Encoding]::UTF8"),
            "{script}"
        );
        // Nothing but a plain tag reaches the script.
        assert!(!sapi_script(1.0, Some("en'); evil; ('")).contains("evil"));
    }

    #[test]
    fn engine_failures_are_errors() {
        // `false` exits with an error, as an engine without a voice or audio does.
        let Some(program) = find_program("false") else {
            return;
        };
        let s = CommandSpeaker {
            engine: Engine::Say,
            program,
            child: Mutex::new(None),
        };
        assert!(s.speak("Hello", 1.0, None, None).is_err());
        let s = CommandSpeaker {
            program: std::env::temp_dir().join("no-such-speech-engine"),
            ..s
        };
        assert!(s.speak("Hello", 1.0, None, None).is_err());
    }

    #[test]
    fn recorder_records_and_stops() {
        let r = Arc::new(Recorder::new(Duration::from_secs(5)));
        let r2 = r.clone();
        let t = pagebara_thread::spawn("speak", move || r2.speak("One.", 1.0, None, None));
        std::thread::sleep(Duration::from_millis(50));
        r.stop();
        t.join().unwrap().unwrap();
        assert_eq!(*r.spoken.lock().unwrap(), ["One."]);
    }

    fn voice(id: &str, lang: Option<&str>, quality: u8) -> Voice {
        Voice {
            id: id.into(),
            name: id.into(),
            lang: lang.map(Into::into),
            quality,
        }
    }

    #[test]
    fn natural_voices_are_picked_for_the_language() {
        let voices = [
            voice("robot", Some("en-US"), 0),
            voice("plain", Some("en-US"), 1),
            voice("enhanced-gb", Some("en-GB"), 2),
            voice("premium-us", Some("en-US"), 3),
            voice("premium-gb", Some("en-GB"), 3),
            voice("french", Some("fr-FR"), 3),
            voice("module", None, 3),
        ];
        // The most natural, in the region asked for when there is one.
        assert_eq!(pick(&voices, None, Some("en-GB")), Some("premium-gb"));
        assert_eq!(pick(&voices, None, Some("en")), Some("premium-gb"));
        assert_eq!(pick(&voices, None, Some("fr")), Some("french"));
        // No natural voice for the language, or no language: the engine's choice.
        assert_eq!(pick(&voices, None, Some("de")), None);
        assert_eq!(pick(&voices, None, None), None);
        let plain = [voice("robot", Some("en"), 0), voice("plain", Some("en"), 1)];
        assert_eq!(pick(&plain, None, Some("en")), None);
        // The reader's choice, unless it is gone; with no list it is taken as it is.
        assert_eq!(pick(&voices, Some("robot"), Some("fr")), Some("robot"));
        assert_eq!(pick(&voices, Some("gone"), Some("fr")), Some("french"));
        assert_eq!(pick(&[], Some("gone"), Some("fr")), Some("gone"));
    }

    #[test]
    fn voices_sort_by_language_then_quality() {
        let mut v = vec![
            voice("Thomas", Some("fr-FR"), 1),
            voice("Alex", Some("en-US"), 1),
            voice("Zoe (Premium)", Some("en-US"), 3),
            voice("Engine", None, 2),
            voice("Amélie", Some("fr-CA"), 3),
        ];
        sort_voices(&mut v, Some("en-GB"));
        let names: Vec<&str> = v.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(
            names,
            ["Zoe (Premium)", "Engine", "Alex", "Amélie", "Thomas"]
        );
        assert_eq!(locale_tag("de_DE.UTF-8").as_deref(), Some("de-DE"));
        assert_eq!(
            locale_tag("en_GB-u-sd-gbsct").as_deref(),
            Some("en-GB-u-sd-gbsct")
        );
        assert_eq!(locale_tag("C"), None);
        assert_eq!(locale_tag(""), None);
    }

    #[test]
    fn engines_voice_lists() {
        let say = "Albert              en_US    # Hello! My name is Albert.\n\
                   Eddy (English (UK)) en_GB    # Hello! My name is Eddy.\n\
                   Samantha            en_US    # Hello! My name is Samantha.\n\
                   Samantha (Enhanced) en_US    # Hello! My name is Samantha.\n\
                   Zoe (Premium)       en_US    # Hello! My name is Zoe.\n\
                   Amélie              fr_CA    # Bonjour, je m’appelle Amélie.\n";
        let v = say_voices(say);
        let got: Vec<(&str, Option<&str>, u8)> = v
            .iter()
            .map(|v| (v.id.as_str(), v.lang.as_deref(), v.quality))
            .collect();
        assert_eq!(
            got,
            [
                ("Albert", Some("en-US"), 0),
                ("Eddy (English (UK))", Some("en-GB"), 1),
                ("Samantha", Some("en-US"), 1),
                ("Samantha (Enhanced)", Some("en-US"), 2),
                ("Zoe (Premium)", Some("en-US"), 3),
                ("Amélie", Some("fr-CA"), 1),
            ]
        );
        let espeak = "Pty Language       Age/Gender VoiceName          File                 Other Languages\n \
                      5  af              --/M      Afrikaans          gmw/af\n \
                      2  en-gb           --/M      English_(Great_Britain) gmw/en            (en 2)\n";
        let v = espeak_voices(espeak);
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].id, "gmw/en");
        assert_eq!(v[1].name, "English (Great Britain)");
        assert_eq!(v[1].lang.as_deref(), Some("en-gb"));
        let sapi = "Microsoft David Desktop\ten-US\r\n\
                    Microsoft Aria Online (Natural) - English (United States)\ten-US\r\n\
                    Odd voice\t\r\n";
        let v = sapi_voices(sapi);
        let got: Vec<(&str, Option<&str>, u8)> = v
            .iter()
            .map(|v| (v.id.as_str(), v.lang.as_deref(), v.quality))
            .collect();
        assert_eq!(
            got,
            [
                ("Microsoft David Desktop", Some("en-US"), 1),
                (
                    "Microsoft Aria Online (Natural) - English (United States)",
                    Some("en-US"),
                    3
                ),
                ("Odd voice", None, 1),
            ]
        );
    }
}
