//! Reading text aloud with the system's speech engine, a sentence at a time, so the reader
//! can follow along and pause between sentences: Speech Dispatcher on Linux (see
//! [`ssip`]), else eSpeak; `say` on macOS; SAPI through PowerShell on Windows. The engines
//! but Speech Dispatcher run as a command per sentence, with no native bindings.

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

/// Something that speaks.
pub trait Speaker: Send + Sync {
    /// The engine's name, for the UI.
    fn name(&self) -> &str;
    /// Speaks `text` and returns when done, or early when [`Speaker::stop`] is called.
    /// `rate` is 1.0 for normal speed; `lang` a BCP 47 tag when known. An error means the
    /// engine could not speak (it is missing or broken), not that it was stopped.
    fn speak(&self, text: &str, rate: f32, lang: Option<&str>) -> std::io::Result<()>;
    /// Stops what is being spoken, from any thread.
    fn stop(&self);
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
    fn command(&self, text: &str, rate: f32, lang: Option<&str>) -> (Command, bool) {
        let mut c = Command::new(&self.program);
        let wpm = (175.0 * rate).clamp(80.0, 450.0).round() as i32;
        let stdin = match self.engine {
            Engine::Espeak => {
                c.args(["-s", &wpm.to_string()]);
                if let Some(l) = lang.map(primary).filter(|l| is_tag(l)) {
                    c.args(["-v", l]);
                }
                c.args(["--", text]);
                false
            }
            Engine::Say => {
                // With no text argument `say` reads standard input.
                c.args(["-r", &wpm.to_string()]);
                true
            }
            Engine::PowerShell => {
                c.args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    &sapi_script(rate, lang),
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
         $v = New-Object -ComObject SAPI.SpVoice; {voice}$v.Rate = {r}; \
         $null = $v.Speak([Console]::In.ReadToEnd())"
    )
}

/// A language tag safe to pass on: letters, digits and hyphens.
fn is_tag(l: &str) -> bool {
    !l.is_empty() && l.len() <= 35 && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// `en` of `en-GB`.
fn primary(lang: &str) -> &str {
    lang.split(['-', '_']).next().unwrap_or(lang)
}

impl Speaker for CommandSpeaker {
    fn name(&self) -> &str {
        match self.engine {
            Engine::Espeak => "eSpeak",
            Engine::Say => "macOS speech",
            Engine::PowerShell => "Windows speech",
        }
    }

    fn speak(&self, text: &str, rate: f32, lang: Option<&str>) -> std::io::Result<()> {
        let (mut cmd, stdin) = self.command(text, rate, lang);
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

    fn speak(&self, text: &str, rate: f32, lang: Option<&str>) -> std::io::Result<()> {
        self.spoken
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(text.to_owned());
        self.inner.speak(text, rate, lang)
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

    fn speak(&self, text: &str, _rate: f32, _lang: Option<&str>) -> std::io::Result<()> {
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
        let (c, stdin) = s.command("-Hi there", 1.2, Some("en-GB"));
        let args: Vec<_> = c.get_args().map(|a| a.to_string_lossy()).collect();
        assert_eq!(args, ["-s", "210", "-v", "en", "--", "-Hi there"]);
        assert!(!stdin);
        let s = CommandSpeaker {
            engine: Engine::Say,
            ..s
        };
        let (c, stdin) = s.command("Hi", 1.0, None);
        assert!(stdin);
        assert_eq!(c.get_args().collect::<Vec<_>>(), ["-r", "175"]);
        let s = CommandSpeaker {
            engine: Engine::PowerShell,
            ..s
        };
        let (c, stdin) = s.command("Hi", 0.5, Some("fr-FR"));
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
        assert!(s.speak("Hello", 1.0, None).is_err());
        let s = CommandSpeaker {
            program: std::env::temp_dir().join("no-such-speech-engine"),
            ..s
        };
        assert!(s.speak("Hello", 1.0, None).is_err());
    }

    #[test]
    fn recorder_records_and_stops() {
        let r = Arc::new(Recorder::new(Duration::from_secs(5)));
        let r2 = r.clone();
        let t = pagebara_thread::spawn("speak", move || r2.speak("One.", 1.0, None));
        std::thread::sleep(Duration::from_millis(50));
        r.stop();
        t.join().unwrap().unwrap();
        assert_eq!(*r.spoken.lock().unwrap(), ["One."]);
    }
}
