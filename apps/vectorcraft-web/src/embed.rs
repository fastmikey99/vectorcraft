//! Running inside another site's page (`?embed=<origin>`): the page that frames the editor hands it
//! files and takes the edited art back, through `window.postMessage`.
//!
//! The embedding page loads the editor in an `<iframe>` with `?embed=` set to its own origin (for
//! example `?embed=https%3A%2F%2Fexample.com`). Messages are only taken from the frame's parent
//! with that origin, and only ever sent to it. Every message is an object with a `type`:
//!
//! From the editor:
//! - `vectorcraft:ready` `{version}`: the editor runs and takes messages.
//! - `vectorcraft:dirty` `{dirty}`: the active document's unsaved changes, when that changes.
//! - `vectorcraft:exported` `{id, name, bytes, format, dirty, warnings}`: the answer to an export.
//! - `vectorcraft:error` `{id, message}`: an export that failed.
//! - `vectorcraft:download` `{name, bytes}`: File › Save, Save As or Export, which download outside
//!   a frame.
//!
//! To the editor:
//! - `vectorcraft:open` `{name, bytes}`: open the file (`bytes` an `ArrayBuffer` or `Uint8Array`)
//!   as File › Open does.
//! - `vectorcraft:export` `{id?, format?, params?, markSaved?}`: the active document written as
//!   `format` (default `pdf`, any format `document.export` writes) with that format's `params`;
//!   `markSaved` records it as saved once written. `id` comes back with the answer.
//!
//! `bytes` travel as transferred `ArrayBuffer`s.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use serde_json::Value;
use vectorcraft_engine::cmd::fileio;
use vectorcraft_ui_egui::VectorcraftApp;
use wasm_bindgen::{JsCast as _, JsValue};

use crate::web::Inbox;

/// An export the embedding page asked for, run on the next frame (it needs the app).
struct ExportRequest {
    id: JsValue,
    format: String,
    params: Value,
    mark_saved: bool,
}

/// The embedding page, once the address names one and the editor is in its frame.
pub struct Embed {
    origin: String,
    parent: web_sys::Window,
    exports: RefCell<Vec<ExportRequest>>,
    ctx: RefCell<Option<egui::Context>>,
    ready: Cell<bool>,
    /// The dirty state last told to the page.
    dirty: Cell<Option<bool>>,
}

impl Embed {
    /// The page that `?embed=<origin>` names, when the editor is framed by a page of that origin's.
    /// Messages from it start arriving at once: files go to `inbox`.
    pub fn start(inbox: Inbox) -> Option<Rc<Embed>> {
        let asked = crate::web::query_param("embed")?;
        let asked = js_sys::decode_uri_component(&asked).ok()?.as_string()?;
        let origin = web_sys::Url::new(&asked).ok()?.origin();
        if origin == "null" || origin != asked.trim_end_matches('/') {
            log::error!("vectorcraft-web: ?embed= takes an origin (scheme://host[:port]), not {asked}");
            return None;
        }
        let window = web_sys::window()?;
        let parent = window.parent().ok().flatten()?;
        if js_sys::Object::is(&parent, &window) {
            log::warn!("vectorcraft-web: ?embed= is set but the editor isn't in a frame");
            return None;
        }
        let embed =
            Rc::new(Embed { origin, parent, exports: RefCell::default(), ctx: RefCell::default(), ready: Cell::new(false), dirty: Cell::new(None) });
        let me = embed.clone();
        let on_message = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
            if e.origin() != me.origin || !e.source().is_some_and(|s| js_sys::Object::is(&s, &me.parent)) {
                return;
            }
            me.take(&e.data(), &inbox);
        });
        if let Err(e) = window.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref()) {
            log::error!("couldn't follow message events: {e:?}");
            return None;
        }
        // The listener lives as long as the page.
        on_message.forget();
        Some(embed)
    }

    /// A message from the page.
    fn take(&self, data: &JsValue, inbox: &Inbox) {
        let get = |k: &str| js_sys::Reflect::get(data, &JsValue::from_str(k)).unwrap_or(JsValue::UNDEFINED);
        match get("type").as_string().as_deref() {
            Some("vectorcraft:open") => {
                let bytes = get("bytes");
                if !(bytes.is_instance_of::<js_sys::ArrayBuffer>() || bytes.is_instance_of::<js_sys::Uint8Array>()) {
                    self.post_error(&JsValue::UNDEFINED, "vectorcraft:open needs bytes (an ArrayBuffer or Uint8Array)");
                    return;
                }
                let name = get("name").as_string().filter(|n| !n.is_empty()).unwrap_or_else(|| "untitled".into());
                inbox.lock().unwrap_or_else(|e| e.into_inner()).push((name, js_sys::Uint8Array::new(&bytes).to_vec()));
            }
            Some("vectorcraft:export") => {
                let params = js_sys::JSON::stringify(&get("params"))
                    .ok()
                    .and_then(|s| s.as_string())
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .filter(Value::is_object)
                    .unwrap_or_else(|| Value::Object(Default::default()));
                self.exports.borrow_mut().push(ExportRequest {
                    id: get("id"),
                    format: get("format").as_string().unwrap_or_else(|| "pdf".into()),
                    params,
                    mark_saved: get("markSaved").as_bool().unwrap_or(false),
                });
            }
            _ => return,
        }
        if let Some(ctx) = &*self.ctx.borrow() {
            ctx.request_repaint();
        }
    }

    /// The running app's context, to wake it when a message arrives; tells the page the editor is
    /// ready the first time.
    pub fn attach(&self, ctx: &egui::Context) {
        *self.ctx.borrow_mut() = Some(ctx.clone());
        if !self.ready.replace(true) {
            self.post(&[("type", "vectorcraft:ready".into()), ("version", env!("CARGO_PKG_VERSION").into())], None);
        }
    }

    /// Each frame: run the exports asked for and tell the page when the dirty state changed.
    pub fn frame(&self, app: &mut VectorcraftApp) {
        let asked = std::mem::take(&mut *self.exports.borrow_mut());
        for r in asked {
            match export(app, &r) {
                Ok((name, bytes, warnings)) => {
                    let dirty = app.session.active().is_some_and(|st| st.is_dirty());
                    let warnings: js_sys::Array = warnings.iter().map(|w| JsValue::from_str(w)).collect();
                    let buf = js_sys::Uint8Array::from(bytes.as_slice()).buffer();
                    self.post(
                        &[
                            ("type", "vectorcraft:exported".into()),
                            ("id", r.id.clone()),
                            ("name", name.into()),
                            ("format", r.format.as_str().into()),
                            ("bytes", buf.clone().into()),
                            ("dirty", dirty.into()),
                            ("warnings", warnings.into()),
                        ],
                        Some(&buf),
                    );
                }
                Err(e) => self.post_error(&r.id, &e),
            }
        }
        let dirty = app.session.active().map(|st| st.is_dirty());
        if self.dirty.replace(dirty) != dirty {
            self.post(&[("type", "vectorcraft:dirty".into()), ("dirty", dirty.unwrap_or(false).into())], None);
        }
    }

    /// File › Save, Save As or Export: the file goes to the page instead of a download.
    pub fn download(&self, name: &str, bytes: &[u8]) {
        let buf = js_sys::Uint8Array::from(bytes).buffer();
        self.post(&[("type", "vectorcraft:download".into()), ("name", name.into()), ("bytes", buf.clone().into())], Some(&buf));
    }

    fn post_error(&self, id: &JsValue, message: &str) {
        log::error!("vectorcraft-web: {message}");
        self.post(&[("type", "vectorcraft:error".into()), ("id", id.clone()), ("message", message.into())], None);
    }

    fn post(&self, fields: &[(&str, JsValue)], transfer: Option<&js_sys::ArrayBuffer>) {
        let msg = js_sys::Object::new();
        for (k, v) in fields {
            // Setting a property on a plain object doesn't fail.
            js_sys::Reflect::set(&msg, &JsValue::from_str(k), v).ok();
        }
        let sent = match transfer {
            Some(buf) => self.parent.post_message_with_transfer(&msg, &self.origin, &js_sys::Array::of1(buf)),
            None => self.parent.post_message(&msg, &self.origin),
        };
        if let Err(e) = sent {
            log::error!("vectorcraft-web: couldn't message the embedding page: {e:?}");
        }
    }
}

/// The active document written as `r.format` → its file name, bytes and the encoder's warnings.
fn export(app: &mut VectorcraftApp, r: &ExportRequest) -> Result<(String, Vec<u8>, Vec<String>), String> {
    let st = app.session.active().ok_or("no document is open")?;
    let ext = fileio::writable_format(Some(&r.format), None)?.extensions[0];
    let doc = fileio::export_source(st, &r.params).map_err(|e| e.to_string())?;
    let (bytes, warnings) = fileio::encode_with_warnings(&doc, &r.format, &r.params).map_err(|e| e.to_string())?;
    let stem = st.title();
    let stem = std::path::Path::new(&stem).file_stem().map_or_else(|| stem.clone(), |s| s.to_string_lossy().to_string());
    let name = format!("{stem}.{ext}");
    if r.mark_saved
        && let Some(st) = app.session.active_mut()
    {
        st.mark_saved();
    }
    Ok((name, bytes, warnings))
}
