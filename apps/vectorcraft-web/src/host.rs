//! The `postMessage` bridge to a same-origin parent page (`?host=parent`), for pages that embed
//! VectorCraft in an `<iframe>` (such as a Nextcloud app). The protocol is described for host
//! authors in `docs/web-embed.md`; its rules are in `protocol` (tested natively). Messages are plain
//! objects with a `type`:
//!
//! - `vectorcraft:ready` (to the parent): `{ version }`, the app is listening; send a file now.
//! - `vectorcraft:open` (from the parent): `{ name, bytes }`, bytes an `ArrayBuffer` or
//!   `Uint8Array`. It opens like a file picked with File › Open. Without a `name` or `bytes` it is
//!   refused with `vectorcraft:failed`.
//! - `vectorcraft:save` (to the parent): `{ name, bytes }` (a transferred `ArrayBuffer`) for every
//!   file VectorCraft would otherwise download (`Services::download`): Save, Save As, Save a
//!   Copy and exports. The parent stores it and reports failures itself.
//! - `vectorcraft:export` (from the parent): `{ id, format?, markSaved? }`, the active document
//!   written as `format` (PDF without it), answered with a `vectorcraft:save` carrying `id`, or a
//!   `vectorcraft:failed` carrying it when nothing is open or the format can't be written.
//! - `vectorcraft:dirty` (to the parent): `{ dirty }` whenever unsaved changes appear or go away.
//! - `vectorcraft:failed` (to the parent): `{ error }`. Before `ready`, the app couldn't start;
//!   after it, the last `vectorcraft:open` was refused (the app keeps running). With an `id`, the
//!   `vectorcraft:export` of that `id` was refused.
//!
//! Only the parent window on the page's own origin is heard or answered, and a page with an opaque
//! origin (`null`) has no bridge. The parent guards leaving the page with unsaved changes.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use vectorcraft_ui_egui::{Services, VectorcraftApp};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;

use crate::protocol;

type Inbox = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

#[derive(Clone)]
pub struct Bridge {
    window: web_sys::Window,
    parent: web_sys::Window,
    origin: String,
    /// The unsaved-changes state last reported to the parent.
    reported: Rc<Cell<Option<bool>>>,
    /// Exports the parent asked for, run on the next frame (they need the app).
    exports: Rc<RefCell<Vec<(JsValue, protocol::Export)>>>,
}

impl Bridge {
    /// `None` when the page isn't framed (there is no parent to talk to).
    pub fn new() -> Option<Self> {
        let window = web_sys::window()?;
        let parent = window.parent().ok().flatten()?;
        if js_sys::Object::is(&parent, &window) {
            return None;
        }
        let origin = window.location().origin().ok().filter(|o| !protocol::is_opaque(o))?;
        Some(Self { window, parent, origin, reported: Rc::default(), exports: Rc::default() })
    }

    /// Send what VectorCraft would download to the parent instead (each time the services are
    /// made, also after a graphics restart).
    pub fn connect(&self, services: &mut Services) {
        let bridge = self.clone();
        services.download = Some(Box::new(move |name: &str, bytes: &[u8]| {
            if let Err(e) = bridge.post_file(name, bytes, None) {
                log::error!("couldn't hand {name} to the page: {e}");
            }
        }));
    }

    pub fn post_ready(&self) {
        self.send(&[("type", "vectorcraft:ready".into()), ("version", env!("CARGO_PKG_VERSION").into())]);
    }

    pub fn post_failed(&self, error: &str) {
        self.send(&[("type", "vectorcraft:failed".into()), ("error", error.into())]);
    }

    /// The refusal of the `vectorcraft:export` with this `id`.
    fn post_export_failed(&self, id: &JsValue, error: &str) {
        log::warn!("refused an export for the page: {error}");
        self.send(&[("type", "vectorcraft:failed".into()), ("error", error.into()), ("id", id.clone())]);
    }

    /// Each frame: write the exports the parent asked for and send them back.
    pub fn run_exports(&self, app: &mut VectorcraftApp) {
        let asked = std::mem::take(&mut *self.exports.borrow_mut());
        for (id, export) in asked {
            match write_export(app, &export) {
                Ok((name, bytes)) => {
                    if let Err(e) = self.post_file(&name, &bytes, Some(&id)) {
                        log::error!("couldn't hand {name} to the page: {e}");
                    }
                }
                Err(error) => self.post_export_failed(&id, &error),
            }
        }
    }

    /// After each frame: tell the parent when unsaved changes appear or go away.
    pub fn report_dirty(&self, app: &VectorcraftApp) {
        let dirty = app.session.documents().iter().any(|d| d.is_dirty());
        if self.reported.get() != Some(dirty) && self.send(&[("type", "vectorcraft:dirty".into()), ("dirty", dirty.into())]) {
            self.reported.set(Some(dirty));
        }
    }

    /// A `vectorcraft:save`; `id` when it answers a `vectorcraft:export`.
    fn post_file(&self, name: &str, bytes: &[u8], id: Option<&JsValue>) -> Result<(), String> {
        let buffer = js_sys::Uint8Array::from(bytes).buffer();
        let mut fields = vec![("type", "vectorcraft:save".into()), ("name", name.into()), ("bytes", buffer.clone().into())];
        if let Some(id) = id {
            fields.push(("id", id.clone()));
        }
        let message = message(&fields).ok_or_else(|| "couldn't build the message".to_string())?;
        self.parent
            .post_message_with_transfer(&message, &self.origin, &js_sys::Array::of1(&buffer))
            .map_err(|e| e.as_string().unwrap_or_else(|| format!("{e:?}")))
    }

    /// Hear files from the parent: they go to the open inbox, which outlives graphics restarts.
    /// `wake` asks the current runner for a frame. A `vectorcraft:open` that can't be opened is
    /// answered with `vectorcraft:failed`; a `vectorcraft:export` waits for the next frame
    /// ([`Bridge::run_exports`]); every other message is ignored (`protocol::accepts`).
    pub fn listen(&self, inbox: Inbox, wake: impl Fn() + 'static) {
        let bridge = self.clone();
        let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            let from_parent = event.source().is_some_and(|s| js_sys::Object::is(&s, &bridge.parent));
            let data = event.data();
            if !protocol::accepts(&bridge.origin, &event.origin(), from_parent, string(&data, "type").as_deref()) {
                return;
            }
            if string(&data, "type").as_deref() == Some("vectorcraft:export") {
                let id = js_sys::Reflect::get(&data, &JsValue::from_str("id")).unwrap_or(JsValue::UNDEFINED);
                let mark_saved = js_sys::Reflect::get(&data, &JsValue::from_str("markSaved")).ok().and_then(|v| v.as_bool());
                match protocol::export(string(&data, "format"), mark_saved) {
                    Ok(export) => {
                        bridge.exports.borrow_mut().push((id, export));
                        wake();
                    }
                    Err(error) => bridge.post_export_failed(&id, &error),
                }
                return;
            }
            match protocol::open(string(&data, "name"), bytes(&data)) {
                Ok(open) => {
                    inbox.lock().unwrap_or_else(|e| e.into_inner()).push((open.name, open.bytes));
                    wake();
                }
                Err(error) => {
                    log::warn!("refused a file from the page: {error}");
                    bridge.post_failed(&error);
                }
            }
        });
        if self.window.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref()).is_ok() {
            // The listener lives as long as the page.
            on_message.forget();
        }
    }

    /// Post a message to the parent; false if it couldn't be sent.
    fn send(&self, fields: &[(&str, JsValue)]) -> bool {
        message(fields).is_some_and(|m| self.parent.post_message(&m, &self.origin).is_ok())
    }
}

fn message(fields: &[(&str, JsValue)]) -> Option<js_sys::Object> {
    let m = js_sys::Object::new();
    for (key, value) in fields {
        js_sys::Reflect::set(&m, &JsValue::from_str(key), value).ok()?;
    }
    Some(m)
}

fn string(data: &JsValue, key: &str) -> Option<String> {
    js_sys::Reflect::get(data, &JsValue::from_str(key)).ok()?.as_string()
}

/// `bytes` as an `ArrayBuffer` or `Uint8Array`.
fn bytes(data: &JsValue) -> Option<Vec<u8>> {
    let bytes = js_sys::Reflect::get(data, &JsValue::from_str("bytes")).ok()?;
    if let Some(array) = bytes.dyn_ref::<js_sys::Uint8Array>() {
        Some(array.to_vec())
    } else if bytes.is_instance_of::<js_sys::ArrayBuffer>() {
        Some(js_sys::Uint8Array::new(&bytes).to_vec())
    } else {
        None
    }
}

/// The active document written as `export` asks → its file name and bytes.
fn write_export(app: &mut VectorcraftApp, export: &protocol::Export) -> Result<(String, Vec<u8>), String> {
    use vectorcraft_engine::cmd::fileio;
    let st = app.session.active().ok_or("vectorcraft:export: no document is open")?;
    let params = serde_json::Value::Object(Default::default());
    let doc = fileio::export_source(st, &params).map_err(|e| e.to_string())?;
    let (bytes, _warnings) = fileio::encode_with_warnings(&doc, export.format, &params).map_err(|e| e.to_string())?;
    let title = st.title();
    let stem = std::path::Path::new(&title).file_stem().map_or_else(|| title.clone(), |s| s.to_string_lossy().to_string());
    if export.mark_saved
        && let Some(st) = app.session.active_mut()
    {
        st.mark_saved();
    }
    Ok((format!("{stem}.{}", export.extension), bytes))
}
