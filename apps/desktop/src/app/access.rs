//! Accessibility settings: the colour theme (with high contrast), text size and reduced
//! motion, following the desktop's preferences unless chosen in the app.

use std::rc::Rc;
use std::sync::atomic::Ordering;

use pagebara_app_core::os_prefs::{self, OsPrefs};
use pagebara_app_core::settings::ThemeChoice;
use pagebara_app_core::theme::{Look, Palette, Rgba};
use std::time::Duration;

use slint::platform::{Key, WindowEvent};
use slint::{Color, ComponentHandle, Timer};

use super::{App, State, post_act};
use crate::{AppState, AppWindow, FocusNav, Mode, Theme, ThemeColors};

pub fn wire(app: &Rc<App>, ui: &AppWindow) {
    let st = ui.global::<AppState>();
    let a = app.clone();
    st.on_system_theme_changed(move || a.act(|_app, s, ui| apply(s, ui)));
    let a = app.clone();
    st.on_set_text_scale(move |v| {
        a.act(move |app, s, ui| {
            s.settings.text_scale = v.clamp(0.8, 2.0);
            apply(s, ui);
            app.save_settings(s);
        })
    });
    let a = app.clone();
    st.on_set_motion(move |i| {
        a.act(move |app, s, ui| {
            s.settings.reduce_motion = match i {
                1 => Some(true),
                2 => Some(false),
                _ => None,
            };
            apply(s, ui);
            app.save_settings(s);
        })
    });
    let a = app.clone();
    ui.global::<FocusNav>()
        .on_entered(move |r| a.act(move |_app, s, _ui| s.focus_region = r));
    // Arrow keys in lists and menus move the focus like Tab does.
    let w = ui.as_weak();
    ui.global::<FocusNav>().on_step(move |forward| {
        let w = w.clone();
        Timer::single_shot(Duration::ZERO, move || {
            if let Some(ui) = w.upgrade() {
                let k = char::from(if forward { Key::Tab } else { Key::Backtab }).to_string();
                ui.window().dispatch_event(WindowEvent::KeyPressed {
                    text: k.as_str().into(),
                });
                ui.window()
                    .dispatch_event(WindowEvent::KeyReleased { text: k.into() });
            }
        });
    });
    // The desktop's preferences take a moment to read.
    pagebara_thread::spawn("os-prefs", || {
        let prefs = os_prefs::read();
        post_act(move |_app, s, ui| {
            s.os_prefs = prefs;
            apply(s, ui);
        });
    });
}

pub fn choice_index(c: ThemeChoice) -> i32 {
    match c {
        ThemeChoice::System => 0,
        ThemeChoice::Light => 1,
        ThemeChoice::Dark => 2,
        ThemeChoice::HighContrastDark => 3,
        ThemeChoice::HighContrastLight => 4,
    }
}

pub fn choice_of(i: i32) -> ThemeChoice {
    match i {
        1 => ThemeChoice::Light,
        2 => ThemeChoice::Dark,
        3 => ThemeChoice::HighContrastDark,
        4 => ThemeChoice::HighContrastLight,
        _ => ThemeChoice::System,
    }
}

/// The colours for `choice`, given the system's light or dark mode and high contrast.
pub fn look(choice: ThemeChoice, system_dark: bool, os: OsPrefs) -> Look {
    match choice {
        ThemeChoice::Light => Look::Light,
        ThemeChoice::Dark => Look::Dark,
        ThemeChoice::HighContrastDark => Look::HighContrastDark,
        ThemeChoice::HighContrastLight => Look::HighContrastLight,
        ThemeChoice::System => match (os.high_contrast == Some(true), system_dark) {
            (true, true) => Look::HighContrastDark,
            (true, false) => Look::HighContrastLight,
            (false, true) => Look::Dark,
            (false, false) => Look::Light,
        },
    }
}

/// Shows the theme, text size and motion the settings and the system ask for.
pub fn apply(s: &State, ui: &AppWindow) {
    let choice = s.theme_override.unwrap_or(s.settings.theme);
    let p = look(choice, ui.get_system_dark(), s.os_prefs).palette();
    let t = ui.global::<Theme>();
    t.set_dark(p.dark);
    t.set_high_contrast(p.high_contrast);
    t.set_c(colors(&p));
    t.set_text_scale(s.settings.text_scale);
    let reduce = s
        .settings
        .reduce_motion
        .or(s.os_prefs.reduce_motion)
        .unwrap_or(false);
    t.set_reduce_motion(reduce);
    crate::page_view::REDUCE_MOTION.store(reduce, Ordering::Relaxed);
    let st = ui.global::<AppState>();
    st.set_theme_choice(choice_index(choice));
    st.set_text_scale(s.settings.text_scale);
    st.set_motion_choice(match s.settings.reduce_motion {
        None => 0,
        Some(true) => 1,
        Some(false) => 2,
    });
    st.set_system_high_contrast(s.os_prefs.high_contrast == Some(true));
}

fn color(c: Rgba) -> Color {
    Color::from_argb_u8(c.3, c.0, c.1, c.2)
}

fn colors(p: &Palette) -> ThemeColors {
    ThemeColors {
        text: color(p.text),
        muted: color(p.muted),
        faint: color(p.faint),
        border: color(p.border),
        control_border: color(p.control_border),
        chrome: color(p.chrome),
        canvas: color(p.canvas),
        track: color(p.track),
        raised: color(p.raised),
        hover: color(p.hover),
        pressed: color(p.pressed),
        accent: color(p.accent),
        accent_hover: color(p.accent_hover),
        accent_soft: color(p.accent_soft),
        accent_fg: color(p.accent_fg),
        on_accent: color(p.on_accent),
        focus: color(p.focus),
        danger: color(p.danger),
        success: color(p.success),
        warning: color(p.warning),
        danger_soft: color(p.danger_soft),
        success_soft: color(p.success_soft),
        warning_soft: color(p.warning_soft),
        shadow_near: color(p.shadow_near),
        shadow_far: color(p.shadow_far),
        scrim: color(p.scrim),
    }
}

/// The areas F6 goes through, in order: toolbar, tools, sidebar, pages, side panel, the
/// page number, the search field and the read-aloud bar.
const REGIONS: [i32; 8] = [1, 8, 2, 3, 4, 5, 6, 7];

/// Moves the keyboard focus to the next (or previous) area of the window that is shown.
pub fn cycle_region(s: &mut State, ui: &AppWindow, forward: bool) {
    let st = ui.global::<AppState>();
    let view = st.get_mode() == Mode::View && !s.tabs.is_empty();
    let panel = view && ui.global::<crate::Chrome>().get_inspector_open();
    let shown = |r: i32| match r {
        1 => true,
        2 => st.get_sidebar_shown(),
        3 => view,
        4 => panel || ui.global::<crate::SigningState>().get_panel_open(),
        5 => view,
        6 => view && st.get_find_visible(),
        7 => view && ui.global::<crate::ReadAloudState>().get_visible(),
        8 => st.get_mode() != Mode::Welcome,
        _ => false,
    };
    let n = REGIONS.len();
    // From the pages when the focus is nowhere known.
    let at_region = |r: i32| REGIONS.iter().position(|&x| x == r);
    let mut at = at_region(s.focus_region)
        .or_else(|| at_region(3))
        .unwrap_or(0);
    for _ in 0..n {
        at = if forward {
            (at + 1) % n
        } else {
            (at + n - 1) % n
        };
        if shown(REGIONS[at]) {
            break;
        }
    }
    let r = REGIONS[at];
    s.focus_region = r;
    match r {
        3 => ui.invoke_focus_view(),
        6 => ui.invoke_focus_find(),
        _ => ui.global::<FocusNav>().invoke_request(r),
    }
}
