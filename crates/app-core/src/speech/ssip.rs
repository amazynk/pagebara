//! Speech Dispatcher, the speech service of Linux desktops (and what Orca speaks through),
//! over its Unix socket with SSIP, its text protocol. Talking to it directly rather than
//! through `spd-say` lets reading stop and cancel only this app's own messages, never a
//! screen reader's, and tells when a sentence has been spoken.

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::{Speaker, Voice, find_program, locale_tag, primary};

const POLL: Duration = Duration::from_millis(100);
/// How long a sentence may take to be spoken before the engine counts as stuck.
fn deadline(text: &str, rate: f32) -> Duration {
    Duration::from_secs(15)
        + Duration::from_millis(200 * text.chars().count() as u64).div_f32(rate.max(0.25))
}

/// A connection to Speech Dispatcher, made when first needed.
#[derive(Debug)]
pub struct Ssip {
    socket: PathBuf,
    session: Mutex<Option<Session>>,
    /// A second handle on the session's socket, for cancelling from another thread.
    canceller: Mutex<Option<UnixStream>>,
    cancelled: AtomicBool,
}

impl Ssip {
    /// Speech Dispatcher, when installed or running.
    pub fn detect() -> Option<Ssip> {
        let socket = socket_path()?;
        (socket.exists() || find_program("speech-dispatcher").is_some()).then(|| Ssip {
            socket,
            session: Mutex::new(None),
            canceller: Mutex::new(None),
            cancelled: AtomicBool::new(false),
        })
    }

    /// Connects, starting the service when it is not running (as its client library does).
    fn connect(&self) -> std::io::Result<Session> {
        let s = self.open()?;
        *self
            .canceller
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(s.writer.try_clone()?);
        Ok(s)
    }

    /// A new session, named, with events for the end of messages.
    fn open(&self) -> std::io::Result<Session> {
        let stream = match UnixStream::connect(&self.socket) {
            Ok(s) => s,
            Err(_) => {
                let started = Command::new("speech-dispatcher")
                    .arg("--spawn")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                if let Err(e) = started {
                    return Err(std::io::Error::new(
                        e.kind(),
                        format!("Speech Dispatcher: {e}"),
                    ));
                }
                let until = Instant::now() + Duration::from_secs(5);
                loop {
                    match UnixStream::connect(&self.socket) {
                        Ok(s) => break s,
                        Err(e) if Instant::now() > until => return Err(e),
                        Err(_) => std::thread::sleep(POLL),
                    }
                }
            }
        };
        stream.set_read_timeout(Some(POLL))?;
        let mut s = Session {
            reader: BufReader::new(stream.try_clone()?),
            writer: stream,
            partial: String::new(),
            voice: None,
        };
        let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
        let user: String = user.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        s.command(
            &format!("SET SELF CLIENT_NAME {user}:pagebara:read-aloud"),
            208,
        )?;
        s.command("SET SELF NOTIFICATION END on", 220)?;
        s.command("SET SELF NOTIFICATION CANCEL on", 220)?;
        Ok(s)
    }

    fn say(
        &self,
        s: &mut Session,
        text: &str,
        rate: f32,
        lang: Option<&str>,
        voice: Option<&str>,
    ) -> std::io::Result<()> {
        s.command(&format!("SET SELF RATE {}", ssip_rate(rate)), 203)?;
        let (module, name) = match voice {
            Some(v) => v.split_once('/').map_or((v, None), |(m, n)| (m, Some(n))),
            None => ("", None),
        };
        if voice.is_some() && s.voice.as_deref() != voice {
            s.command(&format!("SET SELF OUTPUT_MODULE {module}"), 216)?;
            if let Some(n) = name {
                s.command(&format!("SET SELF SYNTHESIS_VOICE {n}"), 209)?;
            }
            s.voice = voice.map(str::to_owned);
        }
        // A voice of its own is not replaced by the module's voice for the language.
        if name.is_none()
            && let Some(l) = lang.map(primary).filter(|l| is_language(l))
        {
            // A language without a voice is not an error: the default voice reads it.
            let _ = s.command(&format!("SET SELF LANGUAGE {l}"), 201);
        }
        s.command("SPEAK", 230)?;
        s.send(&escape(text))?;
        let queued = s.reply(225)?;
        let id = queued.first().cloned().unwrap_or_default();
        if self.cancelled.load(Ordering::SeqCst) {
            s.send("CANCEL SELF\r\n")?;
        }
        let until = Instant::now() + deadline(text, rate);
        loop {
            if let Some((code, lines)) = s.next()? {
                // 702 END and 703 CANCELED end a message; their first line is its id.
                if matches!(code, 702 | 703) && lines.first() == Some(&id) {
                    return Ok(());
                }
            } else if Instant::now() > until {
                return Err(std::io::Error::new(
                    ErrorKind::TimedOut,
                    pagebara_i18n::tr!("Speech Dispatcher did not finish speaking"),
                ));
            }
        }
    }
}

impl Ssip {
    /// The output modules and the voices of those that are not eSpeak (which has a voice
    /// for every language and variant, hundreds, all robotic), on a session of their own.
    fn list(&self) -> std::io::Result<Vec<Voice>> {
        let mut s = self.open()?;
        let mut modules = s.command("LIST OUTPUT_MODULES", 250)?;
        // The last line ends the reply.
        modules.pop();
        let mut out = Vec::new();
        for m in modules {
            if m == "dummy" || !is_word(&m) {
                continue;
            }
            let quality = module_quality(&m);
            out.push(Voice {
                id: m.clone(),
                name: module_name(&m),
                lang: None,
                quality,
            });
            if m.starts_with("espeak") {
                continue;
            }
            if s.command(&format!("SET SELF OUTPUT_MODULE {m}"), 216)
                .is_err()
            {
                continue;
            }
            let Ok(mut voices) = s.command("LIST SYNTHESIS_VOICES", 249) else {
                continue;
            };
            voices.pop();
            for v in voices {
                // Name, language, variant.
                let mut f = v.split('\t');
                let (Some(name), lang) = (f.next(), f.next()) else {
                    continue;
                };
                // A name with a space cannot be set over SSIP.
                if !is_word(name) {
                    continue;
                }
                out.push(Voice {
                    id: format!("{m}/{name}"),
                    name: format!("{name} ({})", module_name(&m)),
                    lang: lang.and_then(locale_tag),
                    quality,
                });
            }
        }
        Ok(out)
    }
}

/// How natural an output module sounds: the neural and the better recorded voices above
/// the formant synthesizers.
fn module_quality(module: &str) -> u8 {
    let m = module.to_ascii_lowercase();
    if m.contains("piper") {
        3
    } else if ["rhvoice", "voxin", "baratinoo", "mary", "pico", "openjtalk"]
        .iter()
        .any(|k| m.contains(k))
    {
        2
    } else if m.starts_with("espeak") || m == "flite" || m == "cicero" {
        0
    } else {
        1
    }
}

/// An output module's name as its makers write it.
fn module_name(module: &str) -> String {
    let known = [
        ("espeak-ng-mbrola", "eSpeak NG with MBROLA"),
        ("espeak-ng", "eSpeak NG"),
        ("espeak", "eSpeak"),
        ("rhvoice", "RHVoice"),
        ("piper", "Piper"),
        ("pico", "SVOX Pico"),
        ("festival", "Festival"),
        ("flite", "Flite"),
        ("voxin", "Voxin"),
        ("baratinoo", "Baratinoo"),
        ("kali", "Kali"),
        ("mary-generic", "MaryTTS"),
        ("openjtalk", "Open JTalk"),
    ];
    known
        .iter()
        .find(|(m, _)| *m == module)
        .map_or_else(|| module.to_owned(), |(_, n)| (*n).to_owned())
}

/// Something that can go on an SSIP command line: no spaces, no line breaks.
fn is_word(s: &str) -> bool {
    !s.is_empty() && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

impl Speaker for Ssip {
    fn name(&self) -> &str {
        "Speech Dispatcher"
    }

    fn voices(&self) -> Vec<Voice> {
        self.list().unwrap_or_else(|e| {
            log::warn!("speech: cannot list Speech Dispatcher's voices: {e}");
            Vec::new()
        })
    }

    fn more_voices(&self) -> Option<String> {
        Some(pagebara_i18n::tr!(
            "For natural voices, add Piper or RHVoice to Speech Dispatcher."
        ))
    }

    fn speak(
        &self,
        text: &str,
        rate: f32,
        lang: Option<&str>,
        voice: Option<&str>,
    ) -> std::io::Result<()> {
        if text.trim().is_empty() {
            return Ok(());
        }
        let voice = voice.filter(|v| is_word(v));
        let mut guard = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        self.cancelled.store(false, Ordering::SeqCst);
        // Another voice: set on a new session, which starts with the engine's own.
        if guard.as_ref().is_some_and(|s| s.voice.as_deref() != voice) {
            *guard = None;
        }
        if guard.is_none() {
            *guard = Some(self.connect()?);
        }
        let Some(s) = guard.as_mut() else {
            return Ok(());
        };
        let r = self.say(s, text, rate, lang, voice);
        if r.is_err() {
            // Start afresh next time: the service may have gone away.
            *guard = None;
        }
        r
    }

    fn stop(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(w) = self
            .canceller
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
        {
            let _ = w.write_all(b"CANCEL SELF\r\n");
        }
    }
}

#[derive(Debug)]
struct Session {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    /// A line read in part before a read timed out.
    partial: String,
    /// The voice set on the session, if not the default.
    voice: Option<String>,
}

impl Session {
    fn send(&mut self, s: &str) -> std::io::Result<()> {
        self.writer.write_all(s.as_bytes())
    }

    /// Sends a command and waits for its reply, which must have code `ok`.
    fn command(&mut self, cmd: &str, ok: u16) -> std::io::Result<Vec<String>> {
        self.send(&format!("{cmd}\r\n"))?;
        self.reply(ok)
    }

    /// The reply to the last command: events in between, and replies to cancels sent from
    /// another thread, are skipped.
    fn reply(&mut self, ok: u16) -> std::io::Result<Vec<String>> {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            match self.next()? {
                Some((code, lines)) if code == ok => return Ok(lines),
                Some((code, lines)) if (300..700).contains(&code) => {
                    return Err(std::io::Error::other(format!(
                        "Speech Dispatcher: {code} {}",
                        lines.last().map_or("", String::as_str)
                    )));
                }
                Some(_) => {}
                None if Instant::now() > until => {
                    return Err(std::io::Error::new(
                        ErrorKind::TimedOut,
                        pagebara_i18n::tr!("Speech Dispatcher did not answer"),
                    ));
                }
                None => {}
            }
        }
    }

    /// The next complete reply or event, or `None` when nothing came within [`POLL`].
    fn next(&mut self) -> std::io::Result<Option<(u16, Vec<String>)>> {
        let mut lines = Vec::new();
        loop {
            match self.reader.read_line(&mut self.partial) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        ErrorKind::UnexpectedEof,
                        pagebara_i18n::tr!("Speech Dispatcher closed the connection"),
                    ));
                }
                Ok(_) => {}
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    if lines.is_empty() {
                        return Ok(None);
                    }
                    continue;
                }
                Err(e) => return Err(e),
            }
            if !self.partial.ends_with('\n') {
                continue;
            }
            let line = std::mem::take(&mut self.partial);
            let (code, last, text) = parse_line(line.trim_end())?;
            lines.push(text.to_owned());
            if last {
                return Ok(Some((code, lines)));
            }
        }
    }
}

/// A reply line: `NNN-text` continues, `NNN text` ends a reply.
fn parse_line(line: &str) -> std::io::Result<(u16, bool, &str)> {
    let bad = || std::io::Error::new(ErrorKind::InvalidData, format!("SSIP: {line:?}"));
    let code = line.get(..3).and_then(|c| c.parse().ok()).ok_or_else(bad)?;
    match line.as_bytes().get(3) {
        Some(b'-') => Ok((code, false, &line[4..])),
        Some(b' ') => Ok((code, true, &line[4..])),
        None => Ok((code, true, "")),
        _ => Err(bad()),
    }
}

/// The data of `SPEAK`: CRLF line ends, a lone `.` ends the data, so lines starting with
/// one get another.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for line in text.lines() {
        if line.starts_with('.') {
            out.push('.');
        }
        out.push_str(line.trim_end_matches('\r'));
        out.push_str("\r\n");
    }
    out.push_str(".\r\n");
    out
}

/// -100..100 around the user's default rate.
fn ssip_rate(rate: f32) -> i32 {
    ((rate - 1.0) * 100.0).clamp(-100.0, 100.0).round() as i32
}

fn is_language(l: &str) -> bool {
    !l.is_empty() && l.len() <= 8 && l.chars().all(|c| c.is_ascii_alphabetic())
}

/// `$SPEECHD_ADDRESS` (`unix_socket:PATH`), else the per-user socket in the runtime
/// folder, or the cache folder without one (as GLib's `g_get_user_runtime_dir`).
fn socket_path() -> Option<PathBuf> {
    if let Ok(a) = std::env::var("SPEECHD_ADDRESS") {
        // TCP addresses are for remote setups; not supported here.
        return a.strip_prefix("unix_socket:").map(PathBuf::from);
    }
    let var = |v: &str| {
        std::env::var_os(v)
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
    };
    let dir = var("XDG_RUNTIME_DIR")
        .or_else(|| var("XDG_CACHE_HOME"))
        .or_else(|| var("HOME").map(|h| h.join(".cache")))?;
    Some(dir.join("speech-dispatcher/speechd.sock"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn protocol_pieces() {
        assert_eq!(parse_line("225-17").unwrap(), (225, false, "17"));
        assert_eq!(
            parse_line("225 OK MESSAGE QUEUED").unwrap(),
            (225, true, "OK MESSAGE QUEUED")
        );
        assert!(parse_line("hello").is_err());
        assert_eq!(escape("One.\n.Two\r\n"), "One.\r\n..Two\r\n.\r\n");
        assert_eq!(ssip_rate(1.5), 50);
        assert_eq!(ssip_rate(9.0), 100);
        assert!(is_language("en") && !is_language("en GB") && !is_language(""));
    }

    /// A stand-in Speech Dispatcher: answers commands as the real one does and ends each
    /// message with an END event, or a CANCELED one when cancelled.
    fn fake_dispatcher(listener: UnixListener, log: std::sync::Arc<Mutex<Vec<String>>>) {
        let (stream, _) = listener.accept().unwrap();
        let mut r = BufReader::new(stream.try_clone().unwrap());
        let mut w = stream;
        let mut id = 0;
        let mut line = String::new();
        let mut pending: Option<u32> = None;
        while {
            line.clear();
            r.read_line(&mut line).unwrap() > 0
        } {
            let cmd = line.trim_end().to_owned();
            log.lock().unwrap().push(cmd.clone());
            let reply = match cmd.as_str() {
                "SPEAK" => {
                    w.write_all(b"230 OK RECEIVING DATA\r\n").unwrap();
                    let mut text = String::new();
                    loop {
                        line.clear();
                        r.read_line(&mut line).unwrap();
                        if line == ".\r\n" {
                            break;
                        }
                        text.push_str(&line);
                    }
                    log.lock().unwrap().push(text.trim_end().to_owned());
                    id += 1;
                    let queued = format!("225-{id}\r\n225 OK MESSAGE QUEUED\r\n");
                    w.write_all(queued.as_bytes()).unwrap();
                    if text.contains("long") {
                        // Spoken until cancelled.
                        pending = Some(id);
                        continue;
                    }
                    format!("702-{id}\r\n702-1\r\n702 END\r\n")
                }
                "CANCEL SELF" => {
                    let ev = pending
                        .take()
                        .map(|p| format!("703-{p}\r\n703-1\r\n703 CANCELED\r\n"))
                        .unwrap_or_default();
                    format!("210 OK CANCELED\r\n{ev}")
                }
                c if c.starts_with("SET SELF CLIENT_NAME") => "208 OK CLIENT NAME SET\r\n".into(),
                c if c.starts_with("SET SELF NOTIFICATION") => "220 OK NOTIFICATION SET\r\n".into(),
                c if c.starts_with("SET SELF RATE") => "203 OK RATE SET\r\n".into(),
                c if c.starts_with("SET SELF LANGUAGE xx") => "410 ERR INVALID LANGUAGE\r\n".into(),
                c if c.starts_with("SET SELF LANGUAGE") => "201 OK LANGUAGE SET\r\n".into(),
                c if c.starts_with("SET SELF OUTPUT_MODULE") => {
                    "216 OK OUTPUT MODULE SET\r\n".into()
                }
                c if c.starts_with("SET SELF SYNTHESIS_VOICE") => "209 OK VOICE SET\r\n".into(),
                "LIST OUTPUT_MODULES" => {
                    "250-espeak-ng\r\n250-rhvoice\r\n250-dummy\r\n250 OK MODULE LIST SENT\r\n"
                        .into()
                }
                "LIST SYNTHESIS_VOICES" => "249-Alan\ten-GB\tnone\r\n249-Bad name\ten\tnone\r\n\
                    249-Elena\tru\tnone\r\n249 OK VOICE LIST SENT\r\n"
                    .into(),
                _ => "300 ERR UNKNOWN COMMAND\r\n".into(),
            };
            w.write_all(reply.as_bytes()).unwrap();
        }
    }

    #[test]
    fn speaks_and_cancels_its_own_messages() {
        let dir = std::env::temp_dir().join(format!("pagebara-ssip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("speechd.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let log = std::sync::Arc::new(Mutex::new(Vec::new()));
        let l2 = log.clone();
        let server = pagebara_thread::spawn("fake-speechd", move || fake_dispatcher(listener, l2));
        let s = std::sync::Arc::new(Ssip {
            socket,
            session: Mutex::new(None),
            canceller: Mutex::new(None),
            cancelled: AtomicBool::new(false),
        });
        s.speak("Hello there.", 1.5, Some("en-GB"), None).unwrap();
        s.speak("Bonjour.", 1.0, Some("xx"), None).unwrap();
        // A stop from another thread ends a sentence being spoken.
        let s2 = s.clone();
        let t = pagebara_thread::spawn("speak", move || {
            s2.speak("A long sentence", 1.0, None, None)
        });
        std::thread::sleep(Duration::from_millis(300));
        s.stop();
        t.join().unwrap().unwrap();
        drop(s);
        server.join().unwrap();
        let log = log.lock().unwrap().clone();
        assert_eq!(
            log[..8],
            [
                "SET SELF CLIENT_NAME".to_owned() + &log[0][20..],
                "SET SELF NOTIFICATION END on".into(),
                "SET SELF NOTIFICATION CANCEL on".into(),
                "SET SELF RATE 50".into(),
                "SET SELF LANGUAGE en".into(),
                "SPEAK".into(),
                "Hello there.".into(),
                "SET SELF RATE 0".into(),
            ]
        );
        assert!(log[0].ends_with(":pagebara:read-aloud"));
        assert!(log.contains(&"CANCEL SELF".to_owned()));
        assert!(!log.iter().any(|l| l.contains("CANCEL ALL")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lists_and_sets_voices() {
        let dir = std::env::temp_dir().join(format!("pagebara-ssip-v-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("speechd.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let log = std::sync::Arc::new(Mutex::new(Vec::new()));
        let l2 = log.clone();
        // One session lists the voices, the next speaks.
        let server = pagebara_thread::spawn("fake-speechd", move || {
            let l = listener.try_clone().unwrap();
            fake_dispatcher(listener, l2.clone());
            fake_dispatcher(l, l2);
        });
        let s = Ssip {
            socket,
            session: Mutex::new(None),
            canceller: Mutex::new(None),
            cancelled: AtomicBool::new(false),
        };
        let voices: Vec<(String, String, Option<String>, u8)> = s
            .voices()
            .into_iter()
            .map(|v| (v.id, v.name, v.lang, v.quality))
            .collect();
        let v = |id: &str, name: &str, lang: Option<&str>, q| {
            (id.to_owned(), name.to_owned(), lang.map(str::to_owned), q)
        };
        assert_eq!(
            voices,
            [
                v("espeak-ng", "eSpeak NG", None, 0),
                v("rhvoice", "RHVoice", None, 2),
                v("rhvoice/Alan", "Alan (RHVoice)", Some("en-GB"), 2),
                v("rhvoice/Elena", "Elena (RHVoice)", Some("ru"), 2),
            ]
        );
        s.speak("Hello.", 1.0, Some("en"), Some("rhvoice/Alan"))
            .unwrap();
        s.speak("Again.", 1.0, Some("en"), Some("rhvoice/Alan"))
            .unwrap();
        drop(s);
        server.join().unwrap();
        let log = log.lock().unwrap().clone();
        let speaking = log.iter().position(|l| l == "Hello.").unwrap();
        // Set once, and no language over the voice chosen.
        let sets: Vec<&String> = log
            .iter()
            .filter(|l| {
                l.starts_with("SET SELF OUTPUT_MODULE") || l.starts_with("SET SELF SYNTHESIS_VOICE")
            })
            .collect();
        assert_eq!(
            sets.iter()
                .filter(|l| l.ends_with("rhvoice") || l.ends_with("Alan"))
                .count(),
            3,
            "{log:?}"
        );
        assert!(
            log[speaking - 5..].contains(&"SET SELF OUTPUT_MODULE rhvoice".to_owned()),
            "{log:?}"
        );
        assert!(log.contains(&"SET SELF SYNTHESIS_VOICE Alan".to_owned()));
        assert!(
            !log[speaking - 5..]
                .iter()
                .any(|l| l.starts_with("SET SELF LANGUAGE")),
            "{log:?}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
