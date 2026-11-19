//! Digital signatures: every document is verified off the UI thread when opened (and after
//! each save), shown in a banner, the signatures panel and the forms panel; trusting
//! certificates; signing with a certificate (an ID file, a self-signed one made here, or
//! one of the system's key store, listed off the UI thread), with a visible appearance, certification, field locks and an optional timestamp; and
//! adding validation data (LTV, with a document timestamp when a timestamp server is set).
//! Signing and LTV write a new file through Save As.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use pagebara_annot::{EmbeddedFont, FontDb, TextFont};
use pagebara_app_core::forms::FieldKind;
use pagebara_app_core::session::write_atomic;
use pagebara_app_core::signing::keystore::{self, KeyStore, StoreIdentity};
use pagebara_app_core::signing::{self as core, SigView, Verdict};
use pagebara_i18n::{tr, tr_noop, trn};
use pagebara_sign::{
    Appearance, Certificate, DigestAlgorithm, HttpTimestampAuthority, Identity, MdpPermissions,
    SignOptions, Time, TimestampAuthority, TrustSource, TrustStore, VerifyOptions,
};
use pagebara_syntax::Document;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use super::{App, State, pick};
use crate::{AppState, AppWindow, ChangeRow, DialogKind, SigCard, SigningState, StoreId, TrustRow};

/// The verification of one tab's document.
struct Checked {
    /// Identifies the file the document was read from.
    source: (usize, usize),
    /// `None` while checking.
    views: Option<Vec<SigView>>,
}

/// Where the signature goes.
#[derive(Clone, Debug)]
enum Place {
    /// A new field with a box drawn on a page (`[x0, y0, x1, y1]` in user space).
    Box {
        page: usize,
        rect: [f64; 4],
    },
    Invisible,
    /// An existing empty field and the page of its widget.
    Field {
        name: String,
        page: usize,
    },
}

#[derive(Default)]
pub struct Signing {
    trust: Option<Arc<TrustStore>>,
    checked: HashMap<u64, Checked>,
    expanded: HashSet<usize>,
    /// Empty signature fields offered in the sign dialog, after the two fixed choices.
    fields: Vec<(String, usize)>,
    identity: Option<Arc<Identity>>,
    /// The key store's identities fit for signing, as last listed.
    store: Vec<StoreIdentity>,
    store_loading: bool,
    /// Waiting for a box to be drawn on a page to sign there.
    pub awaiting_box: bool,
}

fn source_key(doc: &Document) -> (usize, usize) {
    (doc.source().as_ptr() as usize, doc.source().len())
}

fn strings(v: &[String]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        v.iter().map(SharedString::from).collect::<Vec<_>>(),
    ))
}

fn card(v: &SigView, expanded: bool) -> SigCard {
    SigCard {
        summary: v.summary.as_str().into(),
        kind: v.kind.as_str().into(),
        field: v.field.as_str().into(),
        page: v.page.map_or(0, |p| p as i32 + 1),
        level: v.verdict.level(),
        signer: v.signer.as_str().into(),
        time: v.time.as_str().into(),
        timestamp: v.timestamp.as_str().into(),
        reason: v.reason.as_str().into(),
        location: v.location.as_str().into(),
        trust: v.trust.as_str().into(),
        chain: strings(&v.chain),
        revocation: v.revocation.as_str().into(),
        revision: v.revision.as_str().into(),
        ltv: v.ltv.as_str().into(),
        changes: ModelRc::new(VecModel::from(
            v.changes
                .iter()
                .map(|(t, allowed)| ChangeRow {
                    text: t.as_str().into(),
                    allowed: *allowed,
                })
                .collect::<Vec<_>>(),
        )),
        problems: strings(&v.problems),
        warnings: strings(&v.warnings),
        can_trust: v.anchor.is_some(),
        expanded,
    }
}

/// The verified signatures of the active tab, once checked.
pub fn views(s: &State) -> Option<&[SigView]> {
    let t = s.tab_ref()?;
    s.signing.checked.get(&t.uid)?.views.as_deref()
}

/// The forms panel's line for each verified signature field, by field name.
pub fn field_lines(s: &State) -> HashMap<String, String> {
    views(s)
        .unwrap_or_default()
        .iter()
        .map(|v| (v.field.clone(), core::field_status(v)))
        .collect()
}

/// Verifies the active document when its file changed, and shows what is known.
pub fn sync(app: &Rc<App>, s: &mut State, ui: &AppWindow) {
    let Some(t) = s.tab_ref().filter(|t| !t.held) else {
        show(s, ui);
        return;
    };
    let uid = t.uid;
    let key = source_key(t.session.doc());
    if s.signing.checked.get(&uid).is_none_or(|c| c.source != key) {
        s.signing.checked.insert(
            uid,
            Checked {
                source: key,
                views: None,
            },
        );
        s.signing.expanded.clear();
        start_verify(app, s, uid);
    }
    show(s, ui);
}

fn start_verify(_app: &Rc<App>, s: &mut State, uid: u64) {
    let Some(t) = s.tabs.iter().find(|t| t.uid == uid) else {
        return;
    };
    let doc = t.session.doc().clone();
    let key = source_key(&doc);
    let password = first_password(t);
    let trust = s.signing.trust.clone();
    let trust_path = s.profile.trust_path.clone();
    pagebara_thread::spawn("verify", move || {
        // Unsigned documents need neither the trust store nor the checks.
        let signed = pagebara_forms::Form::load(&doc)
            .signatures(&doc)
            .iter()
            .any(|i| i.signed);
        let (views, loaded) = if signed {
            let trust = trust.unwrap_or_else(|| Arc::new(core::trust_store(trust_path.as_deref())));
            let opts = VerifyOptions {
                password: password.as_deref(),
                ..VerifyOptions::new(&trust)
            };
            let views: Vec<SigView> = pagebara_sign::verify(&doc, &opts)
                .iter()
                .map(core::view)
                .collect();
            (views, Some(trust))
        } else {
            (Vec::new(), None)
        };
        super::post_act(move |app, s, ui| {
            if s.signing.trust.is_none() {
                s.signing.trust = loaded;
            }
            if let Some(c) = s.signing.checked.get_mut(&uid)
                && c.source == key
            {
                c.views = Some(views);
            }
            show(s, ui);
            super::forms::sync_fields(s, ui);
            let _ = app;
        });
    });
}

fn first_password(t: &super::Tab) -> Option<String> {
    let job = t
        .session
        .save_job(PathBuf::new(), pagebara_app_core::session::SaveMode::Full);
    job.passwords.into_iter().next()
}

/// The banner and the panel for the active tab.
pub fn show(s: &State, ui: &AppWindow) {
    let g = ui.global::<SigningState>();
    let checked = s.tab_ref().and_then(|t| s.signing.checked.get(&t.uid));
    let checking = checked.is_some_and(|c| c.views.is_none());
    let views = checked.and_then(|c| c.views.as_deref()).unwrap_or_default();
    g.set_checking(checking && s.tab_ref().is_some_and(has_signed_fields));
    match core::banner(views) {
        Some((v, text)) => {
            g.set_level(v.level());
            g.set_banner(text.into());
        }
        None => {
            g.set_level(-1);
            g.set_banner("".into());
        }
    }
    let cards: Vec<SigCard> = views
        .iter()
        .enumerate()
        .map(|(i, v)| card(v, s.signing.expanded.contains(&i)))
        .collect();
    g.set_cards(ModelRc::new(VecModel::from(cards)));
}

/// Cheap: whether the document has signature fields with a value (banner while checking).
fn has_signed_fields(t: &super::Tab) -> bool {
    let doc = t.session.doc();
    let Some(cat) = doc.catalog() else {
        return false;
    };
    let Some(cat) = cat.as_dict() else {
        return false;
    };
    let acro = doc.get_in(cat, b"AcroForm");
    acro.as_dict()
        .is_some_and(|d| doc.get_in(d, b"SigFlags").as_int().unwrap_or(0) & 1 != 0)
}

pub fn wire(app: &Rc<App>, ui: &AppWindow) {
    let g = ui.global::<SigningState>();
    let a = app.clone();
    g.on_action(move |what, value| {
        let (what, value) = (what.to_string(), value.to_string());
        a.act(move |app, s, ui| app.signing_action(s, ui, &what, &value))
    });
    let a = app.clone();
    g.on_card_toggled(move |i| {
        a.act(move |_app, s, ui| {
            let i = i as usize;
            if !s.signing.expanded.remove(&i) {
                s.signing.expanded.insert(i);
            }
            show(s, ui);
        })
    });
    let a = app.clone();
    g.on_card_clicked(move |i| a.act(move |app, s, ui| app.jump_to_signature(s, ui, i as usize)));
}

impl App {
    pub fn signing_action(self: &Rc<Self>, s: &mut State, ui: &AppWindow, what: &str, value: &str) {
        let st = ui.global::<AppState>();
        match what {
            "open-sign" => self.open_sign_dialog(s, ui),
            "choose-id" => pick::open(
                self,
                &tr!("Choose your digital ID"),
                (
                    tr_noop!("Digital IDs"),
                    &["p12", "pfx", "pem", "P12", "PFX", "PEM"],
                ),
                |_app, _s, ui, p| use_id_file(ui, &p, None),
            ),
            "use-id" => {
                let i: usize = value.parse().unwrap_or(0);
                if let Some(p) = saved_ids(s).get(i) {
                    use_id_file(ui, p, Some(""));
                }
            }
            "open-create-id" => {
                st.set_dialog_error("".into());
                st.set_dialog(DialogKind::CreateId);
            }
            "create-id" => {
                let mut parts = value.splitn(3, '\u{1f}');
                let (name, email, pw) = (
                    parts.next().unwrap_or_default().to_owned(),
                    parts.next().unwrap_or_default().to_owned(),
                    parts.next().unwrap_or_default().to_owned(),
                );
                self.create_id(s, ui, &name, &email, &pw);
            }
            "sign" => self.sign_clicked(s, ui),
            "ltv" => self.add_ltv(s, ui),
            "open-trust" => {
                self.ensure_trust(s);
                show_trust(s, ui);
                st.set_dialog_error("".into());
                st.set_dialog(DialogKind::TrustedCerts);
            }
            "trust-card" => {
                let anchor = views(s)
                    .and_then(|v| v.get(value.parse::<usize>().unwrap_or(usize::MAX)))
                    .and_then(|v| v.anchor.clone());
                if let Some(c) = anchor {
                    let name = c.display_name();
                    self.change_trust(s, ui, |t| {
                        t.add(c, TrustSource::User);
                    });
                    self.message(s, &tr!("You now trust signatures from “{}”.", name));
                }
            }
            "untrust" => {
                let fp = value.to_owned();
                self.change_trust(s, ui, move |t| {
                    t.remove(&fp);
                });
                show_trust(s, ui);
            }
            "add-trust" => pick::open(
                self,
                &tr!("Add a trusted certificate"),
                (
                    tr_noop!("Certificates"),
                    &["pem", "crt", "cer", "der", "PEM", "CRT", "CER", "DER"],
                ),
                |app, s, ui, p| app.add_trust_file(s, ui, &p),
            ),
            _ => {}
        }
    }

    fn ensure_trust(&self, s: &mut State) -> Arc<TrustStore> {
        if s.signing.trust.is_none() {
            s.signing.trust = Some(Arc::new(core::trust_store(s.profile.trust_path.as_deref())));
        }
        s.signing.trust.clone().expect("just loaded")
    }

    /// Changes the trust store, saves the user's list and verifies every document again.
    fn change_trust(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        f: impl FnOnce(&mut TrustStore),
    ) {
        let mut t = (*self.ensure_trust(s)).clone();
        f(&mut t);
        if let Some(p) = &s.profile.trust_path
            && let Err(e) = core::save_user_trust(&t, p)
        {
            self.show_message(
                s,
                ui,
                &tr!("Could not save"),
                &tr!("The trusted certificates could not be saved: {}", e),
            );
        }
        s.signing.trust = Some(Arc::new(t));
        s.signing.checked.clear();
        sync(self, s, ui);
    }

    pub fn add_trust_file(self: &Rc<Self>, s: &mut State, ui: &AppWindow, path: &Path) {
        let certs = std::fs::read(path)
            .map_err(|e| e.to_string())
            .and_then(|b| {
                Certificate::from_pem(&b)
                    .ok()
                    .filter(|c| !c.is_empty())
                    .or_else(|| Certificate::from_der(&b).ok().map(|c| vec![c]))
                    .ok_or_else(|| tr!("no certificate in the file"))
            });
        match certs {
            Ok(certs) => {
                let n = certs.len();
                self.change_trust(s, ui, move |t| {
                    for c in certs {
                        t.add(c, TrustSource::User);
                    }
                });
                show_trust(s, ui);
                self.message(
                    s,
                    &trn!(
                        "Added {n} trusted certificate.",
                        "Added {n} trusted certificates.",
                        n
                    ),
                );
            }
            Err(e) => ui
                .global::<AppState>()
                .set_dialog_error(tr!("The certificate could not be read: {}.", e).into()),
        }
    }

    /// Scrolls to the field of signature `i` of the panel.
    fn jump_to_signature(self: &Rc<Self>, s: &mut State, ui: &AppWindow, i: usize) {
        let Some(name) = views(s).and_then(|v| v.get(i)).map(|v| v.field.clone()) else {
            return;
        };
        let backend = s.forms.clone();
        let Some(t) = s.tab() else { return };
        let w = t
            .form(&*backend)
            .field(&name)
            .and_then(|f| f.widgets.first().cloned());
        match w {
            Some(w) => {
                let g = t.session.geoms[w.page];
                let [_, y, _, _] = g.rect_to_view(w.rect);
                self.jump(s, ui, w.page, Some(y));
            }
            None => self.message(s, &tr!("“{}” is an invisible signature.", name)),
        }
    }

    fn open_sign_dialog(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let backend = s.forms.clone();
        let Some(t) = s.tab() else { return };
        let doc = t.session.doc().clone();
        let form = t.form(&*backend);
        let signed: HashSet<String> = form
            .signatures(&doc)
            .into_iter()
            .filter(|i| i.signed)
            .map(|i| i.field)
            .collect();
        s.signing.fields = form
            .fields()
            .into_iter()
            .filter(|f| f.kind == FieldKind::Signature && !signed.contains(&f.name))
            .filter_map(|f| Some((f.name.clone(), f.widgets.first()?.page)))
            .collect();
        let g = ui.global::<SigningState>();
        let mut choices = vec![tr!("Draw a box on the page"), tr!("Invisible signature")];
        choices.extend(
            s.signing
                .fields
                .iter()
                .map(|(n, p)| tr!("Field “{}” on page {}", n, p + 1)),
        );
        g.set_field_choices(strings(&choices));
        g.set_field_choice(if s.signing.fields.is_empty() { 0 } else { 2 });
        let ink = s
            .sign
            .selected
            .as_ref()
            .and_then(|id| s.sign.library.as_ref()?.get(id))
            .map(|g| g.name.clone());
        g.set_has_ink(ink.is_some());
        g.set_ink_name(ink.unwrap_or_default().into());
        let ids: Vec<String> = saved_ids(s)
            .iter()
            .filter_map(|p| Some(p.file_stem()?.to_string_lossy().into_owned()))
            .collect();
        g.set_saved_ids(strings(&ids));
        g.set_busy(false);
        let st = ui.global::<AppState>();
        st.set_dialog_error("".into());
        st.set_dialog(DialogKind::SignCert);
        self.list_key_store(s, ui);
    }

    /// Lists the key store's identities again off the UI thread (a store may ask the user
    /// or a smart card), keeping the chosen one.
    fn list_key_store(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        if s.signing.store_loading {
            return;
        }
        s.signing.store_loading = true;
        ui.global::<SigningState>().set_store_loading(true);
        pagebara_thread::spawn("key-store", move || {
            let listed = key_store().and_then(|k| keystore::signing_identities(&*k, Time::now()));
            super::post_act(move |_app, s, ui| {
                s.signing.store_loading = false;
                let g = ui.global::<SigningState>();
                g.set_store_loading(false);
                let chosen = chosen_store_id(s, ui).map(|i| i.identity.certificate.fingerprint());
                s.signing.store = listed.unwrap_or_else(|e| {
                    log::warn!("key store: {e}");
                    Vec::new()
                });
                let at = chosen.and_then(|fp| {
                    s.signing
                        .store
                        .iter()
                        .position(|i| i.identity.certificate.fingerprint() == fp)
                });
                show_store(s, ui);
                // The store's first identity when no ID file was chosen either.
                let source = match at {
                    Some(i) => i + 1,
                    None if g.get_id_path().is_empty() && !s.signing.store.is_empty() => 1,
                    None => 0,
                };
                g.set_id_source(source as i32);
            });
        });
    }

    fn create_id(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        name: &str,
        email: &str,
        pw: &str,
    ) {
        let Some(dir) = s.profile.ids_dir.clone() else {
            return ui
                .global::<AppState>()
                .set_dialog_error(tr!("There is no folder for IDs on this system.").into());
        };
        let (name, email, pw) = (name.to_owned(), email.to_owned(), pw.to_owned());
        ui.global::<SigningState>().set_busy(true);
        pagebara_thread::spawn("make-id", move || {
            let result = core::self_signed_id(&name, &email, &pw).and_then(|p12| {
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let mut path = dir.join(core::id_file_name(&name));
                let mut n = 2;
                while path.exists() {
                    path = dir.join(core::id_file_name(&format!("{name} {n}")));
                    n += 1;
                }
                write_atomic(&path, &p12).map_err(|e| e.to_string())?;
                Ok(path)
            });
            super::post_act(move |app, s, ui| {
                let g = ui.global::<SigningState>();
                g.set_busy(false);
                match result {
                    Ok(path) => {
                        use_id_file(ui, &path, Some(&pw));
                        app.open_sign_dialog(s, ui);
                        app.message(s, &tr!("Created your ID “{}”.", name));
                    }
                    Err(e) => ui.global::<AppState>().set_dialog_error(e.into()),
                }
            });
        });
    }

    /// "Sign": reads the ID, then asks for the place (a box) or the file to write.
    fn sign_clicked(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let st = ui.global::<AppState>();
        let Some(t) = s.tab_ref() else { return };
        if t.session.is_dirty() {
            return st.set_dialog_error(
                tr!("Save your changes first: the signature is added to the saved file.").into(),
            );
        }
        let g = ui.global::<SigningState>();
        if g.get_id_source() > 0 {
            let Some(id) = chosen_store_id(s, ui) else {
                return st.set_dialog_error(tr!("That certificate is no longer available.").into());
            };
            s.signing.identity = Some(id.identity.clone());
            st.set_dialog_error("".into());
            return self.choose_place(s, ui);
        }
        let path = PathBuf::from(g.get_id_path().as_str());
        let password = g.get_id_password().to_string();
        g.set_busy(true);
        g.set_busy_text(tr!("Opening your ID…").into());
        st.set_dialog_error("".into());
        pagebara_thread::spawn("read-id", move || {
            let id = std::fs::read(&path)
                .map_err(|e| tr!("The ID file could not be read: {}.", e))
                .and_then(|b| core::read_identity(&b, &password));
            super::post_act(move |app, s, ui| {
                let g = ui.global::<SigningState>();
                g.set_busy(false);
                match id {
                    Ok(id) => {
                        s.signing.identity = Some(Arc::new(id));
                        app.choose_place(s, ui);
                    }
                    Err(e) => ui.global::<AppState>().set_dialog_error(e.into()),
                }
            });
        });
    }

    fn choose_place(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let choice = ui.global::<SigningState>().get_field_choice().max(0) as usize;
        let place = match choice {
            0 => {
                s.signing.awaiting_box = true;
                ui.global::<AppState>().set_dialog(DialogKind::None);
                ui.global::<crate::ViewState>().set_cursor(3);
                return self.message(
                    s,
                    &tr!("Drag a box on the page where the signature should go."),
                );
            }
            1 => Place::Invisible,
            i => match s.signing.fields.get(i - 2) {
                Some((name, page)) => Place::Field {
                    name: name.clone(),
                    page: *page,
                },
                None => Place::Invisible,
            },
        };
        self.sign_to_file(s, ui, place);
    }

    /// A box was drawn for the signature (displayed points on `page`).
    pub fn sign_box_drawn(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        page: usize,
        from: (f64, f64),
        to: (f64, f64),
    ) {
        s.signing.awaiting_box = false;
        let Some(t) = s.tab_ref() else { return };
        let (w, h) = ((to.0 - from.0).abs(), (to.1 - from.1).abs());
        if w < 12.0 || h < 8.0 {
            self.overlay_changed(s, ui);
            return self.message(
                s,
                &tr!("That box is too small. Choose Sign with a certificate again."),
            );
        }
        let g = t.session.geoms[page];
        let rect = g.rect_to_pdf([from.0.min(to.0), from.1.min(to.1), w, h]);
        self.overlay_changed(s, ui);
        self.sign_to_file(s, ui, Place::Box { page, rect });
    }

    fn sign_to_file(self: &Rc<Self>, s: &mut State, ui: &AppWindow, place: Place) {
        let Some(t) = s.tab_ref() else { return };
        let name = pick::derived_name(&t.session.title(), "signed");
        let near = t.session.path.clone();
        let uid = t.uid;
        let ink = ui.global::<SigningState>().get_show_ink();
        let ink = if ink && !matches!(place, Place::Invisible) {
            s.sign.selected.clone().and_then(|id| {
                let lib = s.sign.library.as_ref()?;
                Some((lib.get(&id)?.clone(), lib.image(&id).ok()?))
            })
        } else {
            None
        };
        pick::save_pdf(
            self,
            &tr!("Save the signed document"),
            near,
            &name,
            move |app, s, ui, out| {
                app.run_sign(s, ui, uid, place, ink, out);
            },
        );
    }

    fn run_sign(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        uid: u64,
        place: Place,
        ink: Option<(pagebara_signatures::Signature, pagebara_signatures::Rgba)>,
        out: PathBuf,
    ) {
        let Some(t) = s.tabs.iter().find(|t| t.uid == uid) else {
            return;
        };
        let Some(identity) = s.signing.identity.clone() else {
            return;
        };
        let doc = t.session.doc().clone();
        let g = ui.global::<SigningState>();
        let reason = g.get_reason().trim().to_owned();
        let location = g.get_location().trim().to_owned();
        let certify = match g.get_certify() {
            1 => Some(MdpPermissions::NoChanges),
            2 => Some(MdpPermissions::FormFilling),
            3 => Some(MdpPermissions::Annotations),
            _ => None,
        };
        let lock = g.get_lock();
        let all_fields: Vec<String> = pagebara_forms::Form::load(&doc)
            .fields
            .iter()
            .map(|f| f.name.clone())
            .collect();
        let tsa = tsa_url(ui);
        let ltv = g.get_ltv().then(|| self.ensure_trust(s));
        g.set_busy(true);
        g.set_busy_text(tr!("Signing…").into());
        ui.global::<AppState>().set_busy(true);
        pagebara_thread::spawn("sign", move || {
            let result = (|| -> Result<(), String> {
                let mut d = doc;
                let image = match &ink {
                    Some((sig, rgba)) => Some(
                        pagebara_signatures::embed_signature(&mut d, sig, rgba)
                            .map_err(|e| e.to_string())?,
                    ),
                    None => None,
                };
                let (field, appearance) = match &place {
                    Place::Box { page, rect } => (
                        None,
                        Some(Appearance {
                            page: *page,
                            rect: *rect,
                            image,
                            text: true,
                        }),
                    ),
                    Place::Field { name, page } => (
                        Some(name.as_str()),
                        Some(Appearance {
                            page: *page,
                            rect: [0.0; 4],
                            image,
                            text: true,
                        }),
                    ),
                    Place::Invisible => (None, None),
                };
                let locked: Vec<&str> = all_fields.iter().map(String::as_str).collect();
                let tsa = tsa.map(|url| HttpTimestampAuthority {
                    url,
                    post: crate::net::post,
                });
                let font = (!matches!(place, Place::Invisible))
                    .then(appearance_font)
                    .flatten();
                let opts = SignOptions {
                    field,
                    appearance: appearance.as_ref(),
                    reason: (!reason.is_empty()).then_some(reason.as_str()),
                    location: (!location.is_empty()).then_some(location.as_str()),
                    certify,
                    lock_fields: match lock {
                        1 => Some(&[]),
                        2 => Some(&locked),
                        _ => None,
                    },
                    timestamp: tsa.as_ref().map(|t| t as &dyn TimestampAuthority),
                    font: font.map(|f| f as &dyn TextFont),
                    ..Default::default()
                };
                let mut bytes = pagebara_sign::sign(&d, &identity, &opts).map_err(sign_error)?;
                if let Some(trust) = ltv {
                    let tsa = tsa.as_ref().map(|t| t as &dyn TimestampAuthority);
                    let signed = Document::open(bytes)
                        .map_err(|e| tr!("The signed file could not be read: {}.", e))?;
                    bytes = long_term(&signed, &trust, tsa)?;
                }
                write_atomic(&out, &bytes)
                    .map_err(|e| tr!("The file could not be written: {}.", e))?;
                Ok(())
            })();
            super::post_act(move |app, s, ui| {
                ui.global::<AppState>().set_busy(false);
                ui.global::<SigningState>().set_busy(false);
                match result {
                    Ok(()) => {
                        ui.global::<AppState>().set_dialog(DialogKind::None);
                        app.message(s, &tr!("Signed. The signed copy is open now."));
                        app.open_fresh(s, ui, out);
                    }
                    Err(e) => {
                        let st = ui.global::<AppState>();
                        if st.get_dialog() == DialogKind::SignCert {
                            st.set_dialog_error(e.into());
                        } else {
                            app.show_message(s, ui, &tr!("Could not sign"), &e);
                        }
                    }
                }
            });
        });
    }

    /// Adds certificates and revocation data for every signature, as a new file.
    fn add_ltv(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let Some(t) = s.tab_ref() else { return };
        if t.session.is_dirty() {
            return self.show_message(
                s,
                ui,
                &tr!("Save first"),
                &tr!("Save your changes first: validation info is added to the saved file."),
            );
        }
        let doc = t.session.doc().clone();
        let name = pick::derived_name(&t.session.title(), "with validation info");
        let near = t.session.path.clone();
        let trust = self.ensure_trust(s);
        let tsa = tsa_url(ui);
        pick::save_pdf(
            self,
            &tr!("Save with validation info"),
            near,
            &name,
            move |app, s, ui, out| {
                ui.global::<AppState>().set_busy(true);
                app.message(
                    s,
                    &tr!("Fetching validation info from the certificate issuers…"),
                );
                pagebara_thread::spawn("ltv", move || {
                    let tsa = tsa.map(|url| HttpTimestampAuthority {
                        url,
                        post: crate::net::post,
                    });
                    let tsa = tsa.as_ref().map(|t| t as &dyn TimestampAuthority);
                    let result = long_term(&doc, &trust, tsa)
                        .and_then(|b| write_atomic(&out, &b).map_err(|e| e.to_string()));
                    super::post_act(move |app, s, ui| {
                        ui.global::<AppState>().set_busy(false);
                        match result {
                            Ok(()) => {
                                app.message(s, &tr!("Validation info added."));
                                app.open_fresh(s, ui, out);
                            }
                            Err(e) => {
                                app.show_message(s, ui, &tr!("Could not add validation info"), &e)
                            }
                        }
                    });
                });
            },
        );
    }
}

/// The key store the sign dialog lists: the system's, or with `PAGEBARA_KEYSTORE=test` a
/// stand-in whose identities are made at start (scripted tests).
fn key_store() -> Result<Box<dyn KeyStore>, String> {
    if std::env::var("PAGEBARA_KEYSTORE").as_deref() == Ok("test") {
        return Ok(Box::new(keystore::TestKeyStore::new()?));
    }
    Ok(Box::new(keystore::SystemKeyStore))
}

/// The identity choice: an ID file, then the store's identities.
fn show_store(s: &State, ui: &AppWindow) {
    let g = ui.global::<SigningState>();
    let mut choices = vec![tr!("An ID file (.p12, .pfx or .pem)")];
    choices.extend(s.signing.store.iter().map(StoreIdentity::label));
    g.set_id_choices(strings(&choices));
    let rows: Vec<StoreId> = s
        .signing
        .store
        .iter()
        .map(|i| StoreId {
            subject: i.subject.as_str().into(),
            issuer: i.issuer.as_str().into(),
            expires: i.expires.as_str().into(),
            location: i.location.as_str().into(),
        })
        .collect();
    g.set_store_ids(ModelRc::new(VecModel::from(rows)));
}

/// The store identity chosen in the dialog, if one is.
fn chosen_store_id<'a>(s: &'a State, ui: &AppWindow) -> Option<&'a StoreIdentity> {
    let i = ui.global::<SigningState>().get_id_source();
    s.signing
        .store
        .get(usize::try_from(i).ok()?.checked_sub(1)?)
}

/// Signs with the ID file `path` (and `password`, when known).
pub fn use_id_file(ui: &AppWindow, path: &Path, password: Option<&str>) {
    let g = ui.global::<SigningState>();
    g.set_id_source(0);
    g.set_id_path(path.to_string_lossy().as_ref().into());
    if let Some(pw) = password {
        g.set_id_password(pw.into());
    }
}

/// For scripts: the identity choices, joined by " | ".
pub fn id_choices(ui: &AppWindow) -> String {
    let g = ui.global::<SigningState>();
    g.get_id_choices().iter().collect::<Vec<_>>().join(" | ")
}

/// The timestamp server, when timestamps are asked for and one is set.
fn tsa_url(ui: &AppWindow) -> Option<String> {
    let g = ui.global::<SigningState>();
    let url = g.get_tsa_url().trim().to_owned();
    (g.get_timestamp() && !url.is_empty()).then_some(url)
}

/// Adds validation data for every signature (PAdES B-LT) and, with a timestamp server, a
/// document timestamp over it all (B-LTA).
fn long_term(
    doc: &Document,
    trust: &TrustStore,
    tsa: Option<&dyn TimestampAuthority>,
) -> Result<Vec<u8>, String> {
    let lt = pagebara_sign::add_ltv(doc, trust, &crate::net::HttpRevocation).map_err(sign_error)?;
    let Some(tsa) = tsa else {
        return Ok(lt);
    };
    let doc = Document::open(lt).map_err(|e| tr!("The file could not be read: {}.", e))?;
    pagebara_sign::timestamp_document(&doc, tsa, DigestAlgorithm::Sha256).map_err(sign_error)
}

/// The appearance text's font: Inter, the app's own, with the system's fonts for scripts
/// it lacks (CJK, Arabic, …); `None` leaves the choice to the engine.
fn appearance_font() -> Option<&'static EmbeddedFont> {
    static FONT: OnceLock<Option<EmbeddedFont>> = OnceLock::new();
    FONT.get_or_init(|| {
        let key = Path::new("pagebara/InterVariable.ttf");
        let mut db = FontDb::system();
        db.add_data(key, crate::fonts::INTER.into());
        let face = db.faces().iter().find(|f| f.path == key)?.clone();
        EmbeddedFont::with_primary(Arc::new(db), &face)
    })
    .as_ref()
}

fn sign_error(e: pagebara_sign::Error) -> String {
    match e {
        pagebara_sign::Error::NotPermitted => {
            tr!("The document is certified and allows no more signatures.")
        }
        pagebara_sign::Error::Timestamp(t) => tr!("The timestamp server did not answer: {}.", t),
        e => tr!("Signing failed: {}.", e),
    }
}

/// The IDs made in the app, by file name.
fn saved_ids(s: &State) -> Vec<PathBuf> {
    let Some(dir) = &s.profile.ids_dir else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "p12"))
        .collect();
    out.sort();
    out
}

fn show_trust(s: &State, ui: &AppWindow) {
    let g = ui.global::<SigningState>();
    let Some(t) = &s.signing.trust else { return };
    let rows: Vec<TrustRow> = core::user_anchors(t)
        .into_iter()
        .map(|(name, detail, fp)| TrustRow {
            name: name.into(),
            detail: detail.into(),
            fingerprint: fp.into(),
        })
        .collect();
    g.set_trusted(ModelRc::new(VecModel::from(rows)));
    g.set_system_count(
        t.anchors()
            .filter(|(_, src)| *src == TrustSource::System)
            .count() as i32,
    );
}

/// For scripts: the panel's text, all cards joined.
pub fn panel_text(ui: &AppWindow) -> String {
    let g = ui.global::<SigningState>();
    let mut out = vec![g.get_banner().to_string()];
    for c in g.get_cards().iter() {
        out.push(format!(
            "{} | {} | {} | {} | {} | {} | {} | {} | {}",
            c.summary,
            c.kind,
            c.signer,
            c.time,
            c.timestamp,
            c.reason,
            c.location,
            c.trust,
            c.revocation
        ));
        out.push(format!("{} | {}", c.revision, c.ltv));
        out.extend(c.chain.iter().map(|x| x.to_string()));
        out.extend(
            c.changes
                .iter()
                .map(|ch| format!("{} allowed={}", ch.text, ch.allowed)),
        );
        out.extend(c.problems.iter().map(|x| x.to_string()));
        out.extend(c.warnings.iter().map(|x| x.to_string()));
    }
    out.join("\n")
}

/// For scripts: the worst verdict as a word ("none" without signatures).
pub fn verdict_word(s: &State) -> String {
    match views(s) {
        None => "checking".into(),
        Some(v) => v
            .iter()
            .map(|v| v.verdict)
            .max()
            .map_or("none", Verdict::word)
            .into(),
    }
}
