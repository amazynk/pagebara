//! Shortcut names as this system writes them. The UI names keys the way Linux and Windows
//! do (Ctrl+Shift+S); on macOS, where the toolkit's Ctrl is the Command key, they become
//! ⇧⌘S, with the modifiers in the order of the system's menus (⌃⌥⇧⌘).

/// `text` with every Ctrl+…, Shift+… and Alt+… in it written for this system, in key lists
/// and in sentences alike. `physical`: Ctrl means the Control key itself (the vim keys),
/// ⌃ on macOS.
pub fn label(text: &str, physical: bool) -> String {
    if cfg!(target_os = "macos") {
        mac(text, physical)
    } else {
        text.to_owned()
    }
}

#[derive(Default)]
struct Mods {
    ctrl: bool,
    shift: bool,
    alt: bool,
}

fn mac(text: &str, physical: bool) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    let mut i = 0;
    while i < text.len() {
        let starts_word = text[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        if starts_word && let Some((mods, key, len)) = combo(&text[i..]) {
            out.push_str(&written(&mods, key, physical));
            i += len;
            continue;
        }
        let c = text[i..].chars().next().unwrap_or(' ');
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// A shortcut at the start of `s`: its modifiers, its key and its length in bytes.
fn combo(s: &str) -> Option<(Mods, &str, usize)> {
    let mut mods = Mods::default();
    let mut at = 0;
    loop {
        let rest = &s[at..];
        let (name, flag) = if rest.starts_with("Ctrl") {
            ("Ctrl", &mut mods.ctrl)
        } else if rest.starts_with("Shift") {
            ("Shift", &mut mods.shift)
        } else if rest.starts_with("Alt") {
            ("Alt", &mut mods.alt)
        } else {
            break;
        };
        let sep = rest[name.len()..].chars().next();
        if !matches!(sep, Some('+' | '-')) {
            break;
        }
        *flag = true;
        at += name.len() + 1;
    }
    if at == 0 {
        return None;
    }
    let rest = &s[at..];
    let first = rest.chars().next()?;
    let key_len = if first.is_alphanumeric() {
        rest.find(|c: char| !c.is_alphanumeric())
            .unwrap_or(rest.len())
    } else if first.is_whitespace() {
        return None;
    } else {
        first.len_utf8()
    };
    Some((mods, &rest[..key_len], at + key_len))
}

fn written(mods: &Mods, key: &str, physical: bool) -> String {
    let mut out = String::new();
    if mods.ctrl && physical {
        out.push('⌃');
    }
    if mods.alt {
        out.push('⌥');
    }
    if mods.shift {
        out.push('⇧');
    }
    if mods.ctrl && !physical {
        out.push('⌘');
    }
    let key = match key {
        "Left" => "←",
        "Right" => "→",
        "Up" => "↑",
        "Down" => "↓",
        "Enter" | "Return" => "↩",
        "Plus" => "+",
        "Minus" => "−",
        "Delete" => "⌫",
        k => k,
    };
    // Words in sentences (click, drag, arrows) keep a hyphen: ⌘-click.
    if key.chars().count() > 1 && key.starts_with(|c: char| c.is_lowercase()) {
        out.push('-');
    }
    out.push_str(key);
    out
}

#[cfg(test)]
mod tests {
    use super::mac;
    use crate::{AppState, AppWindow, Keys, Mode};
    use slint::ComponentHandle;
    use slint::platform::{Key, WindowEvent};
    use std::{cell::Cell, rc::Rc};

    #[test]
    fn find_shortcuts_follow_the_platform() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let state = ui.global::<AppState>();
        state.set_mode(Mode::View);
        ui.show().unwrap();
        ui.invoke_focus_view();

        let direction = Rc::new(Cell::new(None));
        let observed = direction.clone();
        state.on_find_next(move |forward| observed.set(Some(forward)));
        let chord = |keys: &[char]| {
            for key in keys {
                ui.window().dispatch_event(WindowEvent::KeyPressed {
                    text: key.to_string().into(),
                });
            }
            for key in keys.iter().rev() {
                ui.window().dispatch_event(WindowEvent::KeyReleased {
                    text: key.to_string().into(),
                });
            }
            direction.take()
        };
        let (cmd, shift, alt, f3) = (
            Key::Control.into(),
            Key::Shift.into(),
            Key::Alt.into(),
            Key::F3.into(),
        );
        for macos in [true, false] {
            ui.global::<Keys>().set_macos(macos);
            assert_eq!(chord(&[cmd, 'g']), macos.then_some(true));
            assert_eq!(chord(&[cmd, shift, 'G']), macos.then_some(false));
            assert_eq!(chord(&[f3]), (!macos).then_some(true));
            assert_eq!(chord(&[shift, f3]), (!macos).then_some(false));
            assert_eq!(chord(&['g']), None);
            assert_eq!(chord(&[cmd, f3]), None);
            assert_eq!(chord(&[alt, f3]), None);
            assert_eq!(chord(&[cmd, alt, 'g']), None);
        }
    }

    #[test]
    fn mac_names() {
        assert_eq!(mac("Ctrl+O", false), "⌘O");
        assert_eq!(mac("Ctrl+S, Ctrl+Shift+S", false), "⌘S, ⇧⌘S");
        assert_eq!(mac("Ctrl+Plus, Ctrl+Minus", false), "⌘+, ⌘−");
        assert_eq!(mac("F1, Ctrl+/", false), "F1, ⌘/");
        assert_eq!(mac("Alt+Left, Alt+Right", false), "⌥←, ⌥→");
        assert_eq!(mac("Ctrl+Shift+]", false), "⇧⌘]");
        assert_eq!(mac("Tab, Shift+Tab", false), "Tab, ⇧Tab");
        assert_eq!(mac("Alt+drag", false), "⌥-drag");
        assert_eq!(mac("Esc, Ctrl+Enter", false), "Esc, ⌘↩");
        // The vim keys: the Control key itself.
        assert_eq!(mac("u, Ctrl+R", true), "u, ⌃R");
        assert_eq!(mac("h j k l, Ctrl+E, Ctrl+Y", true), "h j k l, ⌃E, ⌃Y");
    }

    #[test]
    fn mac_sentences() {
        assert_eq!(
            mac("Shift with the arrows selects, Ctrl+X, C and V cut", false),
            "Shift with the arrows selects, ⌘X, C and V cut"
        );
        assert_eq!(
            mac(
                "Shift-click for a range · Ctrl-click to add · Ctrl+arrows",
                false
            ),
            "⇧-click for a range · ⌘-click to add · ⌘-arrows"
        );
        assert_eq!(
            mac("Press Ctrl+D to bookmark", false),
            "Press ⌘D to bookmark"
        );
        // Not shortcuts: no key after the sign, or part of a longer word.
        assert_eq!(mac("Ctrl+ alone, NoCtrl+X", false), "Ctrl+ alone, NoCtrl+X");
        assert_eq!(mac("Altitude", false), "Altitude");
    }
}
