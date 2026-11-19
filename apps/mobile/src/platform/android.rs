//! Android: the system document picker and photo picker, the share sheet and writing saved
//! documents back go through `MainActivity` (android/java), called over JNI. The activity
//! copies picked documents into the app and queues events, which are fetched while the
//! app is likely to expect some (after a request, or on coming back to the foreground).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use jni::objects::{GlobalRef, JObject, JString, JValue};
use jni::{JNIEnv, JavaVM};
use slint::android::AndroidApp;
use slint::{Timer, TimerMode};

use super::{Event, Platform, Sink, WriteBack};

/// How often events are fetched, and for how long after a wake-up.
const POLL: Duration = Duration::from_millis(250);
const POLL_FOR: Duration = Duration::from_secs(20);

thread_local! {
    static WAKE: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
}

/// Starts fetching the activity's events for a while.
pub fn wake() {
    WAKE.with(|w| {
        if let Some(f) = w.borrow().as_ref() {
            f();
        }
    });
}

pub struct Android {
    app: AndroidApp,
    vm: JavaVM,
    activity: GlobalRef,
    sink: RefCell<Option<Sink>>,
}

impl std::fmt::Debug for Android {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Android")
    }
}

impl Android {
    pub fn new(app: AndroidApp) -> Result<Android, jni::errors::Error> {
        let (vm, activity) = vm_and_activity(&app)?;
        let activity = {
            let env = vm.attach_current_thread_permanently()?;
            env.new_global_ref(activity)?
        };
        Ok(Android {
            app,
            vm,
            activity,
            sink: RefCell::new(None),
        })
    }

    /// Runs `f` with the thread's JNI environment and the activity; Java exceptions and
    /// errors are logged.
    fn with_env<T>(
        &self,
        what: &str,
        f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<T>,
    ) -> Option<T> {
        let mut env = self.vm.get_env().ok()?;
        match f(&mut env, self.activity.as_obj()) {
            Ok(v) => Some(v),
            Err(e) => {
                if env.exception_check().unwrap_or(false) {
                    let _ = env.exception_describe();
                    let _ = env.exception_clear();
                }
                log::error!("MainActivity.{what}: {e}");
                None
            }
        }
    }

    /// Calls `name(String...)` returning nothing.
    fn call_str(&self, name: &str, args: &[&str]) {
        self.with_env(name, |env, act| {
            let strings = args
                .iter()
                .map(|a| env.new_string(a))
                .collect::<Result<Vec<JString>, _>>()?;
            let values: Vec<JValue> = strings.iter().map(|s| JValue::Object(s)).collect();
            let sig = format!("({})V", "Ljava/lang/String;".repeat(args.len()));
            env.call_method(act, name, &sig, &values)?;
            Ok(())
        });
    }

    fn call_void(&self, name: &str) {
        self.with_env(name, |env, act| {
            env.call_method(act, name, "()V", &[]).map(|_| ())
        });
    }

    /// Calls `name()` returning a String, or null.
    fn string_result(&self, name: &str) -> Option<String> {
        self.with_env(name, |env, act| {
            let obj = env
                .call_method(act, name, "()Ljava/lang/String;", &[])?
                .l()?;
            if obj.is_null() {
                return Ok(None);
            }
            let s: String = env.get_string(&JString::from(obj))?.into();
            Ok(Some(s))
        })
        .flatten()
    }

    /// Delivers the activity's queued events.
    fn drain(&self) -> bool {
        let Some(sink) = self.sink.borrow().clone() else {
            return false;
        };
        let mut any = false;
        while let Some(e) = self.string_result("nextEvent") {
            let (kind, value) = e.split_once('\n').unwrap_or((e.as_str(), ""));
            let event = match kind {
                "open" => Event::Open(PathBuf::from(value)),
                "image" => Event::Image(PathBuf::from(value)),
                "data" => Event::Data(PathBuf::from(value)),
                _ => Event::Message(value.to_owned()),
            };
            sink(event);
            any = true;
        }
        any
    }
}

/// The process's Java VM and the activity, as android-activity hands them out.
#[allow(unsafe_code)]
fn vm_and_activity(app: &AndroidApp) -> Result<(JavaVM, JObject<'static>), jni::errors::Error> {
    // SAFETY: android-activity gives the pointers of the process's JavaVM and of the
    // activity's global reference; both stay valid while the activity lives, which is
    // longer than this app runs.
    unsafe {
        let vm = JavaVM::from_raw(app.vm_as_ptr().cast())?;
        let activity = JObject::from_raw(app.activity_as_ptr().cast());
        Ok((vm, activity))
    }
}

/// Fetches events every [`POLL`] until [`POLL_FOR`] after the last wake-up.
fn start_polling(platform: std::rc::Weak<Android>) -> impl Fn() {
    let timer = std::rc::Rc::new(Timer::default());
    let until = std::rc::Rc::new(std::cell::Cell::new(Instant::now()));
    move || {
        until.set(Instant::now() + POLL_FOR);
        if timer.running() {
            return;
        }
        let (p, t, u) = (
            platform.clone(),
            std::rc::Rc::downgrade(&timer),
            until.clone(),
        );
        timer.start(TimerMode::Repeated, POLL, move || {
            if let Some(p) = p.upgrade() {
                p.drain();
            }
            if Instant::now() > u.get()
                && let Some(t) = t.upgrade()
            {
                t.stop();
            }
        });
    }
}

impl Platform for Android {
    fn data_dir(&self) -> Option<PathBuf> {
        self.app.internal_data_path()
    }

    fn cache_dir(&self) -> PathBuf {
        self.string_result("cacheDir")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
    }

    fn connect(&self, sink: Sink) {
        *self.sink.borrow_mut() = Some(sink);
    }

    fn pick_pdf(&self) {
        self.call_void("pickPdf");
        wake();
    }

    fn pick_image(&self) {
        self.call_void("pickImage");
        wake();
    }

    fn pick_form_data(&self) {
        self.call_void("pickData");
        wake();
    }

    fn can_share(&self) -> bool {
        true
    }

    fn share(&self, path: &Path, name: &str) {
        self.call_str("share", &[&path.to_string_lossy(), name]);
    }

    fn write_back(&self, path: &Path) -> WriteBack {
        let r = self.with_env("writeBack", |env, act| {
            let p = env.new_string(path.to_string_lossy())?;
            env.call_method(
                act,
                "writeBack",
                "(Ljava/lang/String;)I",
                &[JValue::Object(&p)],
            )?
            .i()
        });
        match r {
            Some(0) => WriteBack::Local,
            Some(1) => WriteBack::Done,
            _ => WriteBack::Failed,
        }
    }

    fn save_copy(&self, path: &Path, name: &str) {
        self.call_str("saveCopy", &[&path.to_string_lossy(), name]);
        wake();
    }

    fn dark(&self) -> Option<bool> {
        self.with_env("isDark", |env, act| {
            env.call_method(act, "isDark", "()Z", &[])?.z()
        })
    }

    fn leave(&self) -> bool {
        self.with_env("moveTaskToBack", |env, act| {
            env.call_method(act, "moveTaskToBack", "(Z)Z", &[JValue::Bool(1)])?
                .z()
        })
        .is_some()
    }
}

/// Makes `platform` fetch the activity's events when [`wake`] is called, and fetches the
/// first ones (a document the app was opened with).
pub fn install(platform: &std::rc::Rc<Android>) {
    let f = start_polling(std::rc::Rc::downgrade(platform));
    WAKE.with(|w| *w.borrow_mut() = Some(Box::new(f)));
    wake();
}
