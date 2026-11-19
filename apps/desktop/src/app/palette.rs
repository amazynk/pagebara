//! The All tools list (Ctrl+K): which of its commands match what was typed.

use pagebara_i18n::tr;
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::{AppState, AppWindow, PaletteItem, PaletteState};

pub fn wire(ui: &AppWindow) {
    let weak = ui.as_weak();
    ui.global::<PaletteState>().on_filter(move |items, query| {
        let recent = weak.upgrade().map(|ui| recent(&ui)).unwrap_or_default();
        let all: Vec<PaletteItem> = recent.into_iter().chain(items.iter()).collect();
        ModelRc::new(VecModel::from(filter(all, &query)))
    });
}

/// The last files opened, as commands that open them again.
fn recent(ui: &AppWindow) -> Vec<PaletteItem> {
    let group = tr!("Recent files");
    ui.global::<AppState>()
        .get_recent()
        .iter()
        .take(4)
        .enumerate()
        .map(|(i, f)| PaletteItem {
            id: format!("recent-{i}").into(),
            title: f.name,
            group: group.as_str().into(),
            enabled: true,
            ..PaletteItem::default()
        })
        .collect()
}

/// The enabled items whose title or group has every word of `query`, in their order, each
/// marked when it starts a group.
fn filter(items: Vec<PaletteItem>, query: &str) -> Vec<PaletteItem> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut out: Vec<PaletteItem> = Vec::new();
    let mut heads = 0;
    for mut item in items {
        if !item.enabled {
            continue;
        }
        let text = format!("{} {}", item.title, item.group).to_lowercase();
        if !words.iter().all(|w| text.contains(w.as_str())) {
            continue;
        }
        item.heading = out.last().is_none_or(|last| last.group != item.group);
        heads += i32::from(item.heading);
        item.heads = heads;
        out.push(item);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str, group: &str, enabled: bool) -> PaletteItem {
        PaletteItem {
            title: title.into(),
            group: group.into(),
            enabled,
            ..PaletteItem::default()
        }
    }

    fn titles(items: &[PaletteItem]) -> Vec<String> {
        items.iter().map(|i| i.title.to_string()).collect()
    }

    #[test]
    fn matches_every_word_in_title_or_group() {
        let all = vec![
            item("Open…", "File", true),
            item("Print…", "File", true),
            item("Redact", "Protect and sign", true),
            item("Sign with a certificate…", "Protect and sign", true),
            item("Save", "File", false),
        ];
        assert_eq!(
            titles(&filter(all.clone(), "")),
            ["Open…", "Print…", "Redact", "Sign with a certificate…"]
        );
        assert_eq!(titles(&filter(all.clone(), "PRI")), ["Print…"]);
        assert_eq!(
            titles(&filter(all.clone(), "sign cert")),
            ["Sign with a certificate…"]
        );
        assert_eq!(
            titles(&filter(all.clone(), "protect")),
            ["Redact", "Sign with a certificate…"]
        );
        assert!(filter(all, "save").is_empty());
    }

    #[test]
    fn marks_where_groups_start() {
        let all = vec![
            item("Open…", "File", true),
            item("Print…", "File", true),
            item("Redact", "Protect and sign", true),
        ];
        let shown = filter(all, "");
        let marks: Vec<(bool, i32)> = shown.iter().map(|i| (i.heading, i.heads)).collect();
        assert_eq!(marks, [(true, 1), (false, 1), (true, 2)]);
    }
}
