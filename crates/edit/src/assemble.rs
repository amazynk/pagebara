//! Turning what the scan found into the page's objects: runs grouped into text blocks
//! by the text extraction's layout analysis, so selection, copying and editing agree.

use std::collections::HashMap;
use std::sync::Arc;

use pagebara_content::display::{self, Cmd, DisplayList, FontRef, Marked, NONE, TextMode};
use pagebara_content::{Rect, TextPage, extract_text};
use pagebara_syntax::{Document, Name, ObjRef};

use crate::geom::corners;
use crate::model::{
    Glyph, Kind, MarkedContent, ObjectData, ObjectId, PageObject, PageObjects, RunId, TextBlock,
    TextLine, TextRun,
};
use crate::scan::{ItemData, Scanned};

/// A parsed page: the public objects and what the writer needs.
#[derive(Debug)]
pub(crate) struct Parsed {
    pub model: PageObjects,
    pub scan: Scanned,
    /// The scan items of each object (a text block's are its runs).
    pub obj_items: Vec<Vec<usize>>,
    /// The scan item of each run.
    pub run_items: Vec<usize>,
}

pub(crate) fn assemble(
    doc: &Document,
    page: usize,
    page_ref: ObjRef,
    frame: crate::geom::PageFrame,
    scan: Scanned,
) -> Parsed {
    let run_items: Vec<usize> = scan
        .items
        .iter()
        .enumerate()
        .filter(|(_, it)| matches!(it.data, ItemData::Run(_)))
        .map(|(k, _)| k)
        .collect();
    let mut item_run: HashMap<usize, usize> = HashMap::new();
    for (r, &k) in run_items.iter().enumerate() {
        item_run.insert(k, r);
    }
    // Lay out visible and hidden runs apart, as extraction sees only visible ones.
    let mut glyph_text: Vec<Vec<String>> = run_items
        .iter()
        .map(|&k| match &scan.items[k].data {
            ItemData::Run(r) => vec![String::new(); r.glyphs.len()],
            _ => Vec::new(),
        })
        .collect();
    let mut blocks: Vec<Vec<LineAcc>> = Vec::new();
    let mut run_block: Vec<Option<usize>> = vec![None; run_items.len()];
    for hidden in [false, true] {
        let runs: Vec<usize> = (0..run_items.len())
            .filter(|&r| scan.items[run_items[r]].hidden == hidden)
            .collect();
        if runs.is_empty() {
            continue;
        }
        let (list, owners) = display_list(&scan, &run_items, &runs);
        let tp = extract_text(doc, &list);
        let first = blocks.len();
        group(&tp, &owners, &mut glyph_text, &mut blocks, &mut run_block);
        // Runs layout left out (overprinted duplicates) join the block they overlap.
        for &r in &runs {
            if run_block[r].is_some() {
                continue;
            }
            let b = scan.items[run_items[r]].bbox;
            let near = (first..blocks.len()).find(|&bi| {
                blocks[bi]
                    .iter()
                    .flat_map(|l| &l.runs)
                    .any(|&o| crate::geom::intersects(&scan.items[run_items[o]].bbox, &b))
            });
            match near {
                Some(bi) => {
                    blocks[bi].last_mut().unwrap().runs.push(r);
                    run_block[r] = Some(bi);
                }
                None => {
                    blocks.push(vec![LineAcc {
                        runs: vec![r],
                        text: None,
                    }]);
                    run_block[r] = Some(blocks.len() - 1);
                }
            }
        }
    }

    // Objects in paint order; a text block where its first run is painted.
    let mut objects: Vec<PageObject> = Vec::new();
    let mut obj_items: Vec<Vec<usize>> = Vec::new();
    let mut item_obj: HashMap<usize, ObjectId> = HashMap::new();
    let mut block_obj: Vec<Option<ObjectId>> = vec![None; blocks.len()];
    for (k, it) in scan.items.iter().enumerate() {
        let id = ObjectId(objects.len() as u32);
        let parent = it.parent.and_then(|p| item_obj.get(&p).copied());
        let marked = it
            .frames
            .iter()
            .map(|&f| scan.frames[f as usize].info.clone())
            .collect::<Vec<MarkedContent>>();
        let clip = it.snap.clips.last().map(|&c| scan.clips[c as usize].bbox);
        match &it.data {
            ItemData::Object(data) => {
                item_obj.insert(k, id);
                let kind = match data {
                    ObjectData::Text(_) => Kind::Text,
                    ObjectData::Image(_) => Kind::Image,
                    ObjectData::Path(_) => Kind::Path,
                    ObjectData::Form(_) => Kind::Form,
                    ObjectData::Shading(_) => Kind::Shading,
                };
                objects.push(PageObject {
                    id,
                    kind,
                    data: data.clone(),
                    bbox: it.bbox,
                    quad: it.quad,
                    parent,
                    clip,
                    marked,
                    hidden: it.hidden,
                    spans: vec![it.span()],
                });
                obj_items.push(vec![k]);
            }
            ItemData::Run(_) => {
                let r = item_run[&k];
                let Some(b) = run_block[r] else { continue };
                if block_obj[b].is_some() {
                    continue;
                }
                block_obj[b] = Some(id);
                let runs: Vec<usize> = blocks[b]
                    .iter()
                    .flat_map(|l| l.runs.iter().copied())
                    .collect();
                let items: Vec<usize> = runs.iter().map(|&r| run_items[r]).collect();
                let bbox = items
                    .iter()
                    .map(|&k| scan.items[k].bbox)
                    .reduce(|a, b| a.union(&b))
                    .unwrap_or_default();
                let parents: Vec<Option<usize>> =
                    items.iter().map(|&k| scan.items[k].parent).collect();
                let parent = if parents.iter().all(|p| *p == parents[0]) {
                    parent
                } else {
                    None
                };
                let mut spans: Vec<_> = items.iter().map(|&k| scan.items[k].span()).collect();
                spans.sort_by_key(|s| (s.stream, s.start));
                objects.push(PageObject {
                    id,
                    kind: Kind::Text,
                    data: ObjectData::Text(TextBlock::default()),
                    bbox,
                    quad: corners(&bbox),
                    parent,
                    clip,
                    marked,
                    hidden: it.hidden,
                    spans,
                });
                obj_items.push(items);
            }
        }
    }

    // Runs with their glyphs' text, and the blocks' lines.
    let mut runs: Vec<TextRun> = Vec::with_capacity(run_items.len());
    for (r, &k) in run_items.iter().enumerate() {
        let it = &scan.items[k];
        let ItemData::Run(rd) = &it.data else {
            unreachable!()
        };
        let s = &it.snap;
        let src = match &it.src {
            crate::scan::Source::Run(src) => src,
            _ => unreachable!(),
        };
        let m = src.tm.then(&s.ctm);
        let (asc, desc) = s.font.as_ref().map_or((0.8, -0.2), |f| {
            (f.font.ascent * s.size, f.font.descent * s.size)
        });
        let mut text = String::new();
        let glyphs: Vec<Glyph> = rd
            .glyphs
            .iter()
            .enumerate()
            .map(|(g, raw)| {
                let w = if raw.width > 0.0 {
                    raw.width
                } else {
                    0.25 * s.size
                };
                let y = raw.y + s.rise;
                let quad =
                    corners(&Rect::new(raw.x, y + desc, raw.x + w, y + asc)).map(|p| m.apply(p));
                let t = std::mem::take(&mut glyph_text[r][g]);
                text.push_str(&t);
                Glyph {
                    code: raw.code,
                    bytes: it.bytes[raw.bytes.0 as usize..raw.bytes.1 as usize].to_vec(),
                    text: t,
                    quad,
                    origin: m.apply(pagebara_content::Point::new(raw.x, y)),
                    advance: raw.advance,
                }
            })
            .collect();
        let block = run_block[r]
            .and_then(|b| block_obj[b])
            .unwrap_or(ObjectId(0));
        runs.push(TextRun {
            id: RunId(r as u32),
            block,
            font: s.font.clone(),
            size: s.size,
            text_matrix: src.tm,
            ctm: s.ctm,
            char_spacing: s.tc,
            word_spacing: s.tw,
            hscale: s.th,
            rise: s.rise,
            render_mode: s.mode,
            fill: s.fill.clone(),
            stroke: s.stroke.clone(),
            fill_alpha: s.fill_alpha,
            stroke_alpha: s.stroke_alpha,
            vertical: rd.vertical,
            glyphs,
            text,
            bbox: it.bbox,
            span: it.span(),
            parent: it.parent.and_then(|p| item_obj.get(&p).copied()),
            marked: it
                .frames
                .iter()
                .map(|&f| scan.frames[f as usize].info.clone())
                .collect(),
            hidden: it.hidden,
        });
    }
    for (b, lines) in blocks.iter().enumerate() {
        let Some(id) = block_obj[b] else { continue };
        let mut tb = TextBlock::default();
        for line in lines {
            let mut tl = TextLine::default();
            let mut bbox: Option<Rect> = None;
            for &r in &line.runs {
                tl.runs.push(RunId(r as u32));
                bbox = Some(bbox.map_or(runs[r].bbox, |x| x.union(&runs[r].bbox)));
            }
            tl.text = match &line.text {
                Some(t) => t.clone(),
                None => line.runs.iter().map(|&r| runs[r].text.as_str()).collect(),
            };
            tl.bbox = bbox.unwrap_or_default();
            if !tb.lines.is_empty() {
                tb.text.push('\n');
            }
            tb.text.push_str(&tl.text);
            tb.lines.push(tl);
        }
        objects[id.0 as usize].data = ObjectData::Text(tb);
    }
    let mut form_children: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    let mut form_uses: HashMap<ObjRef, u32> = HashMap::new();
    for o in &objects {
        if let Some(p) = o.parent {
            form_children.entry(p).or_default().push(o.id);
        }
        if let ObjectData::Form(f) = &o.data
            && let Some(r) = f.object
        {
            *form_uses.entry(r).or_default() += 1;
        }
    }
    for o in &mut objects {
        if let ObjectData::Form(f) = &mut o.data {
            f.children = form_children.remove(&o.id).unwrap_or_default();
            f.uses_on_page = f.object.map_or(1, |r| form_uses[&r]);
        }
    }
    let fingerprint = fingerprint(&scan);
    Parsed {
        model: PageObjects {
            page,
            page_ref,
            frame,
            objects,
            runs,
            fingerprint,
            lang: crate::tagged::document_lang(doc),
            mcid_lang: crate::tagged::mcid_langs(doc, page_ref),
        },
        scan,
        obj_items,
        run_items,
    }
}

/// A display list of the given runs, and the run and glyph index of each of its glyphs.
fn display_list(
    scan: &Scanned,
    run_items: &[usize],
    runs: &[usize],
) -> (DisplayList, Vec<(usize, usize)>) {
    let mut list = DisplayList::default();
    list.matrices.push(pagebara_content::Matrix::IDENTITY);
    let mut fonts: HashMap<*const pagebara_content::Font, u32> = HashMap::new();
    let mut owners = Vec::new();
    let mut open: Vec<u32> = Vec::new();
    for &r in runs {
        let it = &scan.items[run_items[r]];
        let ItemData::Run(rd) = &it.data else {
            continue;
        };
        let crate::scan::Source::Run(src) = &it.src else {
            continue;
        };
        let Some(f) = &it.snap.font else { continue };
        // Marked content (for `/ActualText`) as the runs are nested in it.
        let common = open
            .iter()
            .zip(&it.frames)
            .take_while(|(a, b)| a == b)
            .count();
        for _ in common..open.len() {
            list.cmds.push(Cmd::EndMarked);
        }
        open.truncate(common);
        for &fr in &it.frames[common..] {
            let info = &scan.frames[fr as usize].info;
            list.marked.push(Marked {
                tag: info.tag.clone(),
                properties: info.properties.clone(),
            });
            list.cmds.push(Cmd::BeginMarked {
                marked: list.marked.len() as u32 - 1,
            });
            open.push(fr);
        }
        let font = *fonts.entry(Arc::as_ptr(&f.font)).or_insert_with(|| {
            list.fonts.push(FontRef {
                name: f.name.clone().unwrap_or_else(|| Name::new(b"F")),
                object: f.object.clone(),
                font: f.font.clone(),
            });
            list.fonts.len() as u32 - 1
        });
        let start = list.glyphs.len() as u32;
        for (g, raw) in rd.glyphs.iter().enumerate() {
            list.glyphs.push(display::Glyph {
                code: raw.code,
                x: raw.x,
                y: raw.y,
                width: raw.width,
            });
            owners.push((r, g));
        }
        let s = &it.snap;
        list.runs.push(display::TextRun {
            font,
            size: s.size,
            hscale: s.th,
            rise: s.rise,
            matrix: src.tm.then(&s.ctm),
            ctm: 0,
            mode: TextMode::from_int(i64::from(s.mode)),
            fill: NONE,
            stroke: NONE,
            style: NONE,
            glyphs: (start, list.glyphs.len() as u32),
            vertical: rd.vertical,
        });
        list.cmds.push(Cmd::Text {
            run: list.runs.len() as u32 - 1,
        });
    }
    for _ in 0..open.len() {
        list.cmds.push(Cmd::EndMarked);
    }
    (list, owners)
}

/// Blocks of runs (lines of run indices) from the layout, each run in the block and line
/// of its first glyph; the text of each glyph.
fn group(
    tp: &TextPage,
    owners: &[(usize, usize)],
    glyph_text: &mut [Vec<String>],
    blocks: &mut Vec<Vec<LineAcc>>,
    run_block: &mut [Option<usize>],
) {
    for c in &tp.chars {
        if c.glyph != NONE
            && let Some(&(r, g)) = owners.get(c.glyph as usize)
        {
            glyph_text[r][g].push_str(tp.char_text(c));
        }
    }
    for b in &tp.blocks {
        let mut lines: Vec<LineAcc> = Vec::new();
        for l in &tp.lines[b.lines.0 as usize..b.lines.1 as usize] {
            let mut line = Vec::new();
            for c in &tp.chars[l.chars.0 as usize..l.chars.1 as usize] {
                if c.glyph == NONE {
                    continue;
                }
                let Some(&(r, _)) = owners.get(c.glyph as usize) else {
                    continue;
                };
                if run_block[r].is_none() {
                    run_block[r] = Some(blocks.len());
                    line.push(r);
                }
            }
            if !line.is_empty() {
                lines.push(LineAcc {
                    runs: line,
                    text: Some(tp.line_text(l).to_owned()),
                });
            }
        }
        if !lines.is_empty() {
            blocks.push(lines);
        }
    }
}

/// A line of a block being built: its runs and, from layout, its text.
#[derive(Debug)]
struct LineAcc {
    runs: Vec<usize>,
    text: Option<String>,
}

fn fingerprint(scan: &Scanned) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for s in &scan.streams {
        for chunk in s.data.chunks(8) {
            let mut v = [0u8; 8];
            v[..chunk.len()].copy_from_slice(chunk);
            h = (h ^ u64::from_le_bytes(v)).wrapping_mul(0x0100_0000_01b3);
        }
        h = (h ^ s.data.len() as u64).wrapping_mul(0x0100_0000_01b3);
    }
    h
}
