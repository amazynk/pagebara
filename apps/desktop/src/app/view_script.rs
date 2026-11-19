//! `--script` commands and properties for reading modes, the library and printing.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use i_slint_backend_testing::ElementHandle;
use pagebara_app_core::page_theme::PageTheme;
use pagebara_app_core::view::LayoutMode;
use slint::{ComponentHandle, Model};

use super::{App, State, library, print, reading};
use crate::{AppWindow, LibraryState, PrintState, ReadingState};

/// Runs `call CMD ARGS`; an error for commands it does not know.
pub fn call(
    app: &Rc<App>,
    s: &mut State,
    ui: &AppWindow,
    cmd: &str,
    args: &[String],
) -> Result<(), String> {
    let arg = |i: usize| -> Result<&str, String> {
        args.get(i)
            .map(String::as_str)
            .ok_or_else(|| format!("{cmd} needs an argument"))
    };
    let ps = ui.global::<PrintState>();
    match cmd {
        "layout" => {
            let m = LayoutMode::parse(arg(0)?)
                .ok_or_else(|| format!("unknown layout {}", arg(0).unwrap_or("")))?;
            reading::set_layout(app, s, ui, m);
        }
        "page-theme" => {
            let t = PageTheme::parse(arg(0)?)
                .ok_or_else(|| format!("unknown theme {}", arg(0).unwrap_or("")))?;
            reading::set_theme(app, s, ui, t);
        }
        "reflow" => reading::set_reflow(app, s, ui, arg(0)? != "off"),
        "fit" => match arg(0)? {
            "width" => app.view_op(s, ui, |v| v.fit_width(pagebara_app_core::view::zoom::MAX)),
            "page" => app.view_op(s, ui, |v| v.fit_page()),
            _ => app.view_op(s, ui, |v| v.zoom_to(1.0)),
        },
        "bookmarks" => ui
            .global::<ReadingState>()
            .invoke_show_bookmarks(arg(0)? != "hide"),
        "bookmark-rename" => ui.global::<ReadingState>().invoke_bookmark_rename(
            arg(0)?
                .parse()
                .map_err(|_| "bookmark-rename needs an index")?,
            arg(1)?.into(),
        ),
        "bookmark-delete" => ui.global::<ReadingState>().invoke_bookmark_delete(
            arg(0)?
                .parse()
                .map_err(|_| "bookmark-delete needs an index")?,
        ),
        "bookmarks-to-outline" => ui.global::<ReadingState>().invoke_bookmarks_to_outline(),
        "library" => library::show(app, s, ui, arg(0)? != "hide"),
        "library-add" => library::add_folder(app, s, ui, Path::new(arg(0)?)),
        "library-search" => library::search(app, s, ui, arg(0)?),
        "library-open-hit" => library::open_hit(
            app,
            s,
            arg(0)?
                .parse::<usize>()
                .map_err(|_| "library-open-hit needs a number")?
                .saturating_sub(1),
        ),
        "print" => print::open(app, s, ui),
        "print-set" => {
            let v = arg(1)?;
            let on = v == "true" || v == "on";
            match arg(0)? {
                "copies" => ps.set_copies(v.parse().map_err(|_| "copies needs a number")?),
                "pages" => {
                    ps.set_range(2);
                    ps.set_range_text(v.into());
                }
                "range" => ps.set_range(match v {
                    "current" => 1,
                    "pages" => 2,
                    "selected" => 3,
                    _ => 0,
                }),
                "scale" => match v {
                    "fit" => ps.set_scale(0),
                    "actual" => ps.set_scale(1),
                    p => {
                        ps.set_scale(2);
                        ps.set_custom_scale(
                            p.parse().map_err(|_| "scale: fit, actual or a percent")?,
                        );
                    }
                },
                "paper" => {
                    let i = pagebara_app_core::print::Paper::ALL
                        .iter()
                        .position(|p| p.label().eq_ignore_ascii_case(v))
                        .ok_or_else(|| format!("unknown paper {v}"))?;
                    ps.set_paper(i as i32);
                }
                "greyscale" => ps.set_greyscale(on),
                "annotations" => ps.set_annotations(on),
                "forms" => ps.set_forms(on),
                "rotate" => ps.set_auto_rotate(on),
                "centre" => ps.set_centre(on),
                "duplex" => ps.set_duplex(match v {
                    "long" => 1,
                    "short" => 2,
                    _ => 0,
                }),
                "quality" => ps.set_quality(match v {
                    "150" => 0,
                    "600" => 2,
                    _ => 1,
                }),
                k => return Err(format!("unknown print option {k}")),
            }
            ps.invoke_changed();
        }
        "print-go" => print::start(app, s, ui, None),
        "print-save" => print::start(app, s, ui, Some(PathBuf::from(arg(0)?))),
        "print-close" => ps.invoke_close(),
        _ => return Err(format!("unknown command {cmd}")),
    }
    Ok(())
}

/// The value of a property for `assert`, or `None` when it is not one of these.
pub fn query(s: &mut State, ui: &AppWindow, key: &str) -> Option<Result<String, String>> {
    let (name, arg) = key.split_once(':').unwrap_or((key, ""));
    let rs = ui.global::<ReadingState>();
    let ls = ui.global::<LibraryState>();
    let ps = ui.global::<PrintState>();
    let v = match name {
        "layout" => LayoutMode::from_index(rs.get_layout() as usize)
            .name()
            .to_owned(),
        "page-theme" => PageTheme::from_index(rs.get_page_theme() as usize)
            .name()
            .to_owned(),
        "reflow" => rs.get_reflow().to_string(),
        "reflow.busy" => rs.get_reflow_busy().to_string(),
        "reflow.blocks" => rs.get_reflow_rows().row_count().to_string(),
        "reflow.text" => s
            .tab_ref()
            .map(|t| t.reading.reflow_text())
            .unwrap_or_default(),
        "presenting" => rs.get_presenting().to_string(),
        "present.page" => s
            .reading
            .present
            .as_ref()
            .map_or(0, |p| p.page + 1)
            .to_string(),
        "present.black" => rs.get_present_black().to_string(),
        "present.ready" => (rs.get_present_image().size().width > 0).to_string(),
        "bookmarks" => rs.get_bookmarks().row_count().to_string(),
        "bookmarks.visible" => rs.get_bookmarks_visible().to_string(),
        "bookmarks.titles" => rs
            .get_bookmarks()
            .iter()
            .map(|b| b.title.to_string())
            .collect::<Vec<_>>()
            .join("; "),
        "library.visible" => ls.get_visible().to_string(),
        "library.indexing" => s.library.is_indexing().to_string(),
        "library.docs" => s.library.doc_count().to_string(),
        "library.hits" => ls.get_hits().row_count().to_string(),
        "library.hit" => {
            let n: usize = arg.parse().unwrap_or(1);
            s.library
                .hits
                .get(n.saturating_sub(1))
                .map(hit_label)
                .unwrap_or_default()
        }
        "library.hit-list" => s
            .library
            .hits
            .iter()
            .map(hit_label)
            .collect::<Vec<_>>()
            .join("; "),
        "library.status" => ls.get_status().to_string(),
        "print.visible" => ps.get_visible().to_string(),
        "print.printers" => ps
            .get_printers()
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(", "),
        "print.busy" => ps.get_busy().to_string(),
        "print.error" => ps.get_error().to_string(),
        "print.summary" => ps.get_summary().to_string(),
        "print.previews" => ps.get_previews().row_count().to_string(),
        "print.result" => s
            .print
            .last_result
            .as_ref()
            .map(|r| r.2.clone())
            .unwrap_or_default(),
        "file.text" => std::fs::read_to_string(arg).unwrap_or_default(),
        "pdf.pages" => match std::fs::read(arg)
            .map_err(|e| e.to_string())
            .and_then(|b| pagebara_syntax::Document::open(b).map_err(|e| e.to_string()))
        {
            Ok(d) => pagebara_app_core::session::Session::new(None, d)
                .page_count()
                .to_string(),
            Err(e) => format!("error: {e}"),
        },
        "qpdf" => qpdf_check(arg),
        "luma" => return Some(luma(s, ui, arg)),
        _ => return None,
    };
    Some(Ok(v))
}

/// `qpdf --check FILE`: "ok" or "failed: …"; "ok" without qpdf installed (logged).
fn qpdf_check(file: &str) -> String {
    if pagebara_app_core::print::system::find_on_path("qpdf").is_none() {
        log::warn!("qpdf is not installed; {file} is not checked");
        return "ok".into();
    }
    match std::process::Command::new("qpdf")
        .args(["--check", file])
        .output()
    {
        Ok(o) if o.status.success() => "ok".into(),
        Ok(o) => format!(
            "failed: {}",
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .last()
                .unwrap_or("")
        ),
        Err(e) => format!("failed: {e}"),
    }
}

/// `luma:P,x,y,w,h`: the mean brightness (0–255) of that area of page P on screen.
fn luma(s: &mut State, ui: &AppWindow, arg: &str) -> Result<String, String> {
    let v: Vec<f64> = arg
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .collect();
    let [page, x, y, w, h] = v[..] else {
        return Err(format!("luma needs page,x,y,w,h: {arg}"));
    };
    let t = s.tab().ok_or("no document is open")?;
    let page = (page as usize).saturating_sub(1);
    let origin = ElementHandle::find_by_element_id(ui, "AppWindow::view")
        .next()
        .map(|e| e.absolute_position())
        .ok_or("the page view is not shown")?;
    let (Some(a), Some(b)) = (
        t.view.page_to_screen(page, x, y),
        t.view.page_to_screen(page, x + w, y + h),
    ) else {
        return Err(format!("page {} is not on screen", page + 1));
    };
    let shot = ui.window().take_snapshot().map_err(|e| e.to_string())?;
    let dpr = f64::from(ui.window().scale_factor());
    let to_px =
        |v: f64, o: f32, max: u32| (((v + f64::from(o)) * dpr).round().max(0.0) as u32).min(max);
    let (x0, y0) = (
        to_px(a.0, origin.x, shot.width()),
        to_px(a.1, origin.y, shot.height()),
    );
    let (x1, y1) = (
        to_px(b.0, origin.x, shot.width()),
        to_px(b.1, origin.y, shot.height()),
    );
    let px = shot.as_slice();
    let (mut sum, mut n) = (0u64, 0u64);
    for yy in y0..y1 {
        for xx in x0..x1 {
            let p = px[(yy * shot.width() + xx) as usize];
            sum += (u64::from(p.r) * 3 + u64::from(p.g) * 6 + u64::from(p.b)) / 10;
            n += 1;
        }
    }
    if n == 0 {
        return Err("the area is empty".into());
    }
    Ok((sum / n).to_string())
}

fn hit_label(h: &pagebara_app_core::library::Hit) -> String {
    let name = h.path.file_name().unwrap_or_default().to_string_lossy();
    format!("{name} p{}", h.page + 1)
}
