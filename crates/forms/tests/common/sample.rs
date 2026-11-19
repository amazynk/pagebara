//! A synthetic form with every kind of field.

use pagebara_syntax::Document;

use super::*;

/// A one-page form with every kind of field (see the list in the code).
pub fn sample_form(form_extra: &str) -> Document {
    let mut doc = Document::open(one_page(612, 792, "")).unwrap();
    let d = &mut doc;
    let w = "/Type/Annot/Subtype/Widget/P 3 0 R/F 4";
    let mut fields = Vec::new();
    let mut widgets = Vec::new();
    let mut merged = |d: &mut Document, s: String| {
        let r = add(d, &format!("<<{w}{s}>>"));
        fields.push(r);
        widgets.push(r);
        r
    };
    merged(
        d,
        "/FT/Tx/T(name)/TU(Your name)/Ff 2/Rect[50 720 300 744]/DA(/Helv 12 Tf 0 0 1 rg)\
         /MK<</BC[0 0 0]/BG[1 1 0.8]>>"
            .into(),
    );
    merged(
        d,
        "/FT/Tx/T(notes)/Ff 4096/Rect[50 600 300 700]/DA(/Helv 0 Tf 0 g)/MK<</BC[0 0 1]>>".into(),
    );
    merged(
        d,
        "/FT/Tx/T(zip)/Ff 16777216/MaxLen 5/Q 1/Rect[50 560 150 584]/MK<</BC[0 0 0]>>".into(),
    );
    merged(d, "/FT/Tx/T(pin)/Ff 8192/Rect[200 560 300 584]".into());
    merged(
        d,
        "/FT/Btn/T(agree)/V/Off/Rect[50 520 66 536]/DA(/ZaDb 0 Tf 1 0 0 rg)\
         /MK<</CA(8)/BC[0 0 0]>>"
            .into(),
    );
    let on = add_stream(
        d,
        "<</Type/XObject/Subtype/Form/BBox[0 0 14 14]>>",
        b"0 g 2 2 10 10 re f",
    );
    let off = add_stream(d, "<</Type/XObject/Subtype/Form/BBox[0 0 14 14]>>", b"");
    merged(
        d,
        format!(
            "/FT/Btn/T(news)/V/Off/DV/On/AS/Off/Rect[80 520 94 534]/AP<</N<</On {} 0 R/Off {} 0 R>>>>",
            on.num, off.num
        ),
    );

    let group = |d: &mut Document, head: &str, states: &[&str], x0: f64| {
        let parent = add(d, &format!("<<{head}>>"));
        let mut kids = Vec::new();
        for (i, s) in states.iter().enumerate() {
            let x = x0 + 30.0 * i as f64;
            let k = add(
                d,
                &format!(
                    "<<{w}/Parent {} 0 R/Rect[{x} 480 {} 494]/AS/Off/MK<</BC[0 0 0]/BG[1 1 1]>>\
                     /AP<</N<</{s} {} 0 R/Off {} 0 R>>>>>>",
                    parent.num,
                    x + 14.0,
                    on.num,
                    off.num
                ),
            );
            kids.push(k);
        }
        let kl = o(&format!("[{}]", list(&kids)));
        edit(d, parent, |p| p.set(b"Kids", kl));
        (parent, kids)
    };
    let (color, ck) = group(
        d,
        "/FT/Btn/T(color)/Ff 49152/V/Off/DV/1/Opt[(Red)(Green)(Blue)]/DA(/ZaDb 0 Tf 0 g)",
        &["0", "1", "2"],
        50.0,
    );
    let (size, sk) = group(
        d,
        "/FT/Btn/T(size)/Ff 33587200/V/Off/DA(/ZaDb 0 Tf 0 g)",
        &["S", "L", "S"],
        150.0,
    );

    let mut more = vec![color, size];
    let mut kids_all = [ck, sk].concat();
    let mut merged2 = |d: &mut Document, s: String| {
        let r = add(d, &format!("<<{w}{s}>>"));
        more.push(r);
        kids_all.push(r);
        r
    };
    merged2(
        d,
        "/FT/Ch/T(country)/Ff 131072/Opt[[(fr)(France)][(de)(Germany)][(it)(Italy)]]/V(de)\
         /DV(fr)/Rect[50 440 200 460]/DA(/Helv 10 Tf 0 g)/MK<</BC[0 0 0]>>"
            .into(),
    );
    merged2(
        d,
        "/FT/Ch/T(city)/Ff 393216/Opt[(Paris)(Berlin)]/Rect[220 440 370 460]/DA(/Helv 10 Tf 0 g)"
            .into(),
    );
    merged2(
        d,
        "/FT/Ch/T(langs)/Ff 2097152/Opt[(English)(French)(German)(Spanish)(Italian)]/TI 1\
         /Rect[50 340 200 420]/DA(/Helv 10 Tf 0 g)/MK<</BC[0 0 0]>>"
            .into(),
    );
    merged2(
        d,
        "/FT/Btn/T(submit)/Ff 65536/Rect[400 720 500 744]\
         /MK<</CA(Submit)/BG[0.75 0.75 0.75]/BC[0 0 0]>>/BS<</W 2/S/B>>/DA(/Helv 0 Tf 0 g)"
            .into(),
    );
    merged2(d, "/FT/Sig/T(sig)/Rect[300 50 500 100]".into());
    let js = |d: &mut Document, s: &str| add(d, &format!("<</S/JavaScript/JS({s})>>"));
    let money_k = js(d, "AFNumber_Keystroke(2, 0, 0, 0, \"$\", true);");
    let money_f = js(d, "AFNumber_Format(2, 0, 0, 0, \"$\", true);");
    let sum = js(d, "AFSimple_Calculate(\"SUM\", new Array (\"a\", \"b\"));");
    let custom = js(d, "if (event.value > 3) event.rc = false;");
    let date_f = js(d, "AFDate_FormatEx(\"dd.mm.yyyy\");");
    let date_k = js(d, "AFDate_KeystrokeEx(\"dd.mm.yyyy\");");
    let aa_money = format!("/AA<</K {} 0 R/F {} 0 R>>", money_k.num, money_f.num);
    merged2(
        d,
        format!("/FT/Tx/T(a)/Rect[400 600 500 620]/DA(/Helv 10 Tf 0 g){aa_money}"),
    );
    merged2(
        d,
        format!("/FT/Tx/T(b)/Rect[400 570 500 590]/DA(/Helv 10 Tf 0 g){aa_money}"),
    );
    let total = merged2(
        d,
        format!(
            "/FT/Tx/T(total)/Ff 1/Rect[400 540 500 560]/DA(/Helv 10 Tf 0 g)/AA<</C {} 0 R/F {} 0 R>>",
            sum.num, money_f.num
        ),
    );
    merged2(
        d,
        format!(
            "/FT/Tx/T(custom)/Rect[400 500 500 520]/AA<</K {} 0 R>>",
            custom.num
        ),
    );
    merged2(
        d,
        format!(
            "/FT/Tx/T(date)/Rect[400 470 500 490]/DA(/Helv 10 Tf 0 g)/AA<</F {} 0 R/K {} 0 R>>",
            date_f.num, date_k.num
        ),
    );
    let secret = merged2(d, "/FT/Tx/T(secret)/Ff 4/V(x)/Rect[400 440 500 460]".into());
    edit(d, secret, |s| s.set(b"F", 2));
    merged2(
        d,
        "/FT/Tx/T(rot)/Rect[550 400 580 600]/DA(/Helv 12 Tf 0 g)/MK<</R 90/BC[0 0 0]>>".into(),
    );

    let addr = add(d, "<</T(addr)/FT/Tx/DA(/Helv 9 Tf 0 g)>>");
    let street = add(
        d,
        &format!(
            "<<{w}/T(street)/Parent {} 0 R/Rect[50 300 300 320]>>",
            addr.num
        ),
    );
    let town = add(
        d,
        &format!(
            "<<{w}/T(town)/Parent {} 0 R/Ff 1/V(Springfield)/Rect[50 270 300 290]>>",
            addr.num
        ),
    );
    let kl = o(&format!("[{} 0 R {} 0 R]", street.num, town.num));
    edit(d, addr, |p| p.set(b"Kids", kl));
    more.push(addr);
    kids_all.extend([street, town]);

    fields.extend(more);
    widgets.extend(kids_all);
    let extra = format!("/CO[{} 0 R]{form_extra}", total.num);
    finish(doc, &extra, &fields, &widgets)
}
