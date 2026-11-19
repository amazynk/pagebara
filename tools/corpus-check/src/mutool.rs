//! MuPDF's `mutool` as the reference reader.

use std::path::Path;
use std::time::Duration;

use crate::child::{self, Exit, Limits};

/// What `mutool pages` made of a file.
#[derive(Debug, Clone, Default)]
pub struct Verdict {
    pub pages: usize,
    /// Lines of its error output mentioning an error.
    pub errors: usize,
    pub timeout: bool,
}

impl Verdict {
    pub fn opened(&self) -> bool {
        self.pages > 0
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({ "pages": self.pages, "errors": self.errors, "timeout": self.timeout })
    }
}

pub fn available() -> bool {
    child::run(
        "mutool",
        &["-v"],
        Limits {
            timeout: Duration::from_secs(10),
            memory_mb: 0,
        },
    )
    .is_ok()
}

/// Runs `mutool pages` on `file`.
pub fn pages(file: &Path) -> Verdict {
    let Some(f) = file.to_str() else {
        return Verdict::default();
    };
    let limits = Limits {
        timeout: Duration::from_secs(30),
        memory_mb: 4096,
    };
    let Ok(o) = child::run("mutool", &["pages", f], limits) else {
        return Verdict::default();
    };
    Verdict {
        pages: o.stdout.matches("<page ").count(),
        errors: o.stderr.lines().filter(|l| l.contains("error")).count(),
        timeout: o.exit == Exit::Timeout,
    }
}
