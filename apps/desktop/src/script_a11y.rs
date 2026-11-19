//! `--script` checks of accessibility: controls without a name for screen readers, the
//! theme's contrast as shown, the page text exposed to assistive technology, and what the
//! read-aloud test engine was asked to say.

use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use pagebara_app_core::theme::{Rgba, checks, contrast, over};
use slint::{Color, ComponentHandle, Model};

use crate::app::State;
use crate::{AppWindow, ReadAloudState, Theme, ViewState};

/// Roles a user can operate, which therefore need a name.
const INTERACTIVE: &[AccessibleRole] = &[
    AccessibleRole::Button,
    AccessibleRole::Checkbox,
    AccessibleRole::Combobox,
    AccessibleRole::Slider,
    AccessibleRole::Spinbox,
    AccessibleRole::Tab,
    AccessibleRole::TextInput,
    AccessibleRole::Switch,
    AccessibleRole::ListItem,
    AccessibleRole::RadioButton,
];

/// Interactive elements on screen.
fn interactive(ui: &AppWindow) -> Vec<(ElementHandle, AccessibleRole)> {
    let size = ui.window().size().to_logical(ui.window().scale_factor());
    ElementQuery::from_root(ui)
        .match_descendants()
        .find_all()
        .into_iter()
        .filter(|e| shown(e, size.width, size.height))
        .filter_map(|e| {
            let role = e.accessible_role()?;
            INTERACTIVE.contains(&role).then_some((e, role))
        })
        .collect()
}

/// Interactive elements on screen without an accessible label: "Type#id (role)".
pub fn unlabelled(ui: &AppWindow) -> Vec<String> {
    interactive(ui)
        .into_iter()
        .filter_map(|(e, role)| {
            let label = e.accessible_label().unwrap_or_default();
            label.trim().is_empty().then(|| {
                format!(
                    "{}{} ({role:?})",
                    e.type_name().unwrap_or_default(),
                    e.id().map(|i| format!("#{i}")).unwrap_or_default()
                )
            })
        })
        .collect()
}

fn shown(e: &ElementHandle, w: f32, h: f32) -> bool {
    let p = e.absolute_position();
    let z = e.size();
    e.is_valid()
        && z.width > 0.0
        && z.height > 0.0
        && e.computed_opacity() > 0.0
        && p.x < w
        && p.y < h
        && p.x + z.width > 0.0
        && p.y + z.height > 0.0
}

/// Contrast failures of the colours the window shows now.
pub fn contrast_failures(ui: &AppWindow) -> Vec<String> {
    let c = ui.global::<Theme>().get_c();
    let rgba = |c: Color| Rgba(c.red(), c.green(), c.blue(), c.alpha());
    let chrome = rgba(c.chrome);
    let solid = |x: Color| over(rgba(x), chrome);
    let p = pagebara_app_core::theme::Palette {
        dark: false,
        high_contrast: false,
        text: solid(c.text),
        muted: solid(c.muted),
        faint: solid(c.faint),
        border: solid(c.border),
        control_border: solid(c.control_border),
        chrome,
        canvas: solid(c.canvas),
        track: solid(c.track),
        raised: solid(c.raised),
        hover: solid(c.hover),
        pressed: solid(c.pressed),
        accent: solid(c.accent),
        accent_hover: solid(c.accent_hover),
        accent_soft: solid(c.accent_soft),
        accent_fg: solid(c.accent_fg),
        on_accent: solid(c.on_accent),
        focus: solid(c.focus),
        danger: solid(c.danger),
        success: solid(c.success),
        warning: solid(c.warning),
        danger_soft: solid(c.danger_soft),
        success_soft: solid(c.success_soft),
        warning_soft: solid(c.warning_soft),
        shadow_near: solid(c.shadow_near),
        shadow_far: solid(c.shadow_far),
        scrim: solid(c.scrim),
    };
    checks(&p)
        .into_iter()
        .filter_map(|(what, fg, bg, min)| {
            let r = contrast(fg, bg);
            (r < min).then(|| format!("{what} {r:.2}:1"))
        })
        .collect()
}

/// The value of `key` for `assert`, or `None` when it is not one of these.
pub fn query(s: &mut State, ui: &AppWindow, key: &str) -> Option<Result<String, String>> {
    let ra = ui.global::<ReadAloudState>();
    Some(Ok(match key {
        "a11y.unlabelled" => unlabelled(ui).len().to_string(),
        "a11y.interactive" => interactive(ui).len().to_string(),
        "a11y.contrast" => {
            let f = contrast_failures(ui);
            if f.is_empty() {
                "ok".into()
            } else {
                f.join("; ")
            }
        }
        "a11y.page-text" => ui
            .global::<ViewState>()
            .get_page_text()
            .iter()
            .map(|t| t.label.to_string())
            .collect::<Vec<_>>()
            .join(" | "),
        "a11y.links" => ui
            .global::<ViewState>()
            .get_page_text()
            .iter()
            .filter(|t| t.link >= 0)
            .map(|t| format!("{} -> {}", t.label, t.description))
            .collect::<Vec<_>>()
            .join(" | "),
        "focus.region" => s.focus_region.to_string(),
        "theme.high-contrast" => ui.global::<Theme>().get_high_contrast().to_string(),
        "theme.dark" => ui.global::<Theme>().get_dark().to_string(),
        "theme.text-scale" => ui.global::<Theme>().get_text_scale().to_string(),
        "theme.reduce-motion" => ui.global::<Theme>().get_reduce_motion().to_string(),
        "speech.available" => ra.get_available().to_string(),
        "speech.playing" => (ra.get_playing() && !ra.get_paused()).to_string(),
        "speech.paused" => ra.get_paused().to_string(),
        "speech.visible" => ra.get_visible().to_string(),
        "speech.sentence" => ra.get_sentence().to_string(),
        "speech.rate" => ra.get_rate().to_string(),
        "speech.spoken" => s
            .read_aloud
            .spoken
            .as_ref()
            .map(|v| v.lock().unwrap_or_else(|e| e.into_inner()).join(" | "))
            .unwrap_or_default(),
        "speech.count" => s
            .read_aloud
            .spoken
            .as_ref()
            .map_or(0, |v| v.lock().unwrap_or_else(|e| e.into_inner()).len())
            .to_string(),
        _ => return None,
    }))
}

/// Runs `cmd`, or returns `None` when it is not one of these.
pub fn call(
    s: &mut State,
    ui: &AppWindow,
    cmd: &str,
    _args: &[String],
) -> Option<Result<(), String>> {
    Some(match cmd {
        // Fails listing the controls a screen reader could not name.
        "a11y-check" => {
            let missing = unlabelled(ui);
            if missing.is_empty() {
                Ok(())
            } else {
                Err(format!(
                    "{} control(s) without a label: {}",
                    missing.len(),
                    missing.join(", ")
                ))
            }
        }
        "contrast-check" => {
            let f = contrast_failures(ui);
            if f.is_empty() {
                Ok(())
            } else {
                Err(format!("contrast: {}", f.join("; ")))
            }
        }
        "speech-clear" => {
            if let Some(v) = &s.read_aloud.spoken {
                v.lock().unwrap_or_else(|e| e.into_inner()).clear();
            }
            Ok(())
        }
        _ => return None,
    })
}
