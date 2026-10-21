//! Headless PAL preparation player. Separation geometry is an explicit stop boundary.
mod actions;
mod clean;
mod reader;
mod selection;
#[cfg(test)]
mod tests;

use super::{CommandSpec, always, cmd};
use crate::{EngineError, Result, Session};
use reader::{Action, Element};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use vectorcraft_doc::{Document, NodeId};
use vectorcraft_geom::Rect;

const GEOMETRY: &[&str] = &["AddWhiteUnderPrint", "Choke", "Spread", "Shaper", "Rasterize", "SetTrapTags", "RemoveTrappingTags"];
const MAX_STEPS: usize = 100_000;
fn error(e: impl ToString) -> EngineError {
    EngineError::Other(e.to_string())
}
fn required<'a>(p: &'a Element, key: &str) -> Result<&'a str> {
    p.value(key).ok_or_else(|| error(format!("missing PAL parameter {key}")))
}
fn flag(p: &Element, key: &str) -> Result<bool> {
    match p.value(key).unwrap_or("0") {
        "0" => Ok(false),
        "1" => Ok(true),
        v => Err(error(format!("invalid {key}: {v}"))),
    }
}
fn number(p: &Element, key: &str) -> Result<f64> {
    required(p, key)?.parse::<f64>().ok().filter(|n| n.is_finite() && n.abs() < 1e8).ok_or_else(|| error(format!("invalid PAL number {key}")))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        "sep.runActionList",
        "Run Separation Action Lists",
        [],
        None,
        "{root?: action-list library folder (default: the reference library under the user's home), lists: [relative list names in execution order], input: PDF path, output: PDF path} run PAL preparation; returns {status, stoppedAt, traceLength, trace, layers, spots, artwork, warnings}. Trace is also written to stderr. Separation geometry or unsupported actions stop before mutation and write the document so far.",
        always,
        run
    )]
}

struct Player<'a> {
    session: &'a mut Session,
    root: PathBuf,
    stack: Vec<PathBuf>,
    layers: Vec<NodeId>,
    inks: Vec<String>,
    boxes: BTreeMap<String, Rect>,
    trace: Vec<Value>,
    stopped: Option<Value>,
    clipboard_empty: bool,
    messages: Vec<String>,
}
impl Player<'_> {
    fn doc(&self) -> Result<&Document> {
        Ok(&self.session.doc()?.doc)
    }
    fn ids(&self) -> Result<Vec<NodeId>> {
        Ok(self.session.doc()?.selection.objects.clone())
    }
    fn call(&mut self, command: &str, params: Value) -> Result<()> {
        self.session.execute(command, &params)?;
        Ok(())
    }
    fn list(&mut self, name: &str, caller: Option<&Path>) -> Result<()> {
        let path = reader::resolve(&self.root, caller, name).map_err(error)?;
        if self.stack.len() >= 32 || self.stack.contains(&path) {
            return Err(error("recursive PAL call or call depth exceeds 32"));
        }
        let actions = reader::read(&path).map_err(error)?;
        self.stack.push(path.clone());
        let result = self.play(&path, &actions);
        self.stack.pop();
        result
    }
    fn play(&mut self, path: &Path, actions: &[Action]) -> Result<()> {
        for a in actions {
            if self.stopped.is_some() {
                break;
            }
            if self.trace.len() >= MAX_STEPS {
                return Err(error("PAL execution exceeds 100000 steps"));
            }
            vectorcraft_doc::cancel::check().map_err(error)?;
            let list = path.strip_prefix(&self.root).unwrap_or(path).with_extension("").to_string_lossy().replace('\\', "/");
            let index = self.trace.len();
            let params = a
                .params
                .children
                .iter()
                .filter(|e| !matches!(e.name.as_str(), "Actions" | "Then" | "Else"))
                .map(|e| {
                    if e.name == "Rule" {
                        format!(
                            "{} {}",
                            e.attributes.get("Type").map(String::as_str).unwrap_or("Rule"),
                            e.children.iter().map(|c| format!("{}={}", c.name, c.text)).collect::<Vec<_>>().join(" ")
                        )
                    } else {
                        format!("{}={}", e.name, e.text)
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            let params: String =
                params.chars().take(4096).flat_map(|c| if c.is_control() { c.escape_default().collect::<Vec<_>>() } else { vec![c] }).collect();
            // Reserve the parent trace before a nested call, preserving execution order.
            self.trace.push(json!({"list":list, "position":a.position, "action":a.kind, "params":params}));
            let before = self.ids()?.len();
            let is_geometry = GEOMETRY.contains(&a.kind.as_str());
            let result = if is_geometry { Err(error(format!("not implemented: {} (separation geometry)", a.kind))) } else { self.action(a) };
            let (affected, reason) = match result {
                Ok(n) => (n, None),
                Err(e) => (0, Some(e.to_string())),
            };
            if let Some(step) = self.trace.get_mut(index) {
                step["objectsAffected"] = json!(affected);
                if let Some(why) = &reason {
                    step["reason"] = json!(why);
                }
            }
            // stderr keeps MCP's stdout protocol intact; a closed trace stream is reported.
            writeln!(
                std::io::stderr(),
                "[{list} #{}] {} {} → {} objects{}",
                a.position,
                a.kind,
                params,
                affected,
                reason.as_ref().map(|s| format!("; stopped: {s}")).unwrap_or_default()
            )
            .map_err(error)?;
            if let Some(reason) = reason {
                self.stopped = Some(json!({"list":list, "position":a.position, "action":a.kind, "reason":reason, "selectedObjects":before}));
                break;
            }
            let nested = match a.kind.as_str() {
                "Group" => self.play(path, &a.children),
                "Conditional" => {
                    let yes = selection::condition(self.doc()?, &self.ids()?, &a.params)?;
                    self.play(path, if yes { &a.then_actions } else { &a.else_actions })
                }
                "CallActionList" => self.list(required(&a.params, "Path")?, Some(path)),
                _ => Ok(()),
            };
            if let Err(e) = nested {
                self.stopped = Some(json!({"list":list, "position":a.position, "action":a.kind, "reason":e.to_string()}));
            }
        }
        Ok(())
    }
}

fn run(s: &mut Session, p: &Value) -> Result<Value> {
    let string = |key| p.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or_else(|| error(format!("sep.runActionList requires {key}")));
    let input = string("input")?;
    let output = string("output")?;
    if !Path::new(input).extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
        || !Path::new(output).extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
    {
        return Err(error("sep.runActionList input and output must be PDFs"));
    }
    crate::file_access::check_read(input).map_err(error)?;
    crate::file_access::check_write(output).map_err(error)?;
    let source = Path::new(input).canonicalize().map_err(error)?;
    if Path::new(output).canonicalize().ok().as_ref() == Some(&source) {
        return Err(error("output must differ from input"));
    }
    let lists = p
        .get("lists")
        .and_then(Value::as_array)
        .filter(|v| !v.is_empty() && v.len() <= 256)
        .ok_or_else(|| error("lists must contain 1 to 256 action-list names"))?;
    let lists = lists.iter().map(|v| v.as_str().ok_or_else(|| error("list names must be strings"))).collect::<Result<Vec<_>>>()?;
    let root = match p.get("root") {
        Some(Value::String(v)) => PathBuf::from(v),
        Some(_) => return Err(error("root must be a folder path")),
        None => PathBuf::from(
            std::env::var_os("VECTORCRAFT_ACTION_LISTS").ok_or_else(|| error("set root, or VECTORCRAFT_ACTION_LISTS, to the action-list folder"))?,
        ),
    };
    crate::file_access::check_read(&root.to_string_lossy()).map_err(error)?;
    let root = root.canonicalize().map_err(error)?;
    let opened = s.execute("document.open", &json!({"path":input,"printProduction":true,"textAs":"outlines"}))?;
    if s.doc()?.doc.artboards.len() != 1 {
        return Err(error("PAL preparation currently requires a single-page PDF"));
    }
    let rect = s.doc()?.doc.artboards.first().ok_or_else(|| error("PDF has no page"))?.rect;
    let source_info = vectorcraft_pdf::info(&std::fs::read(input).map_err(error)?, None).map_err(error)?;
    let page = source_info.pages.first().ok_or_else(|| error("PDF has no page"))?;
    if page.rotation != 0 {
        return Err(error("PAL preparation requires an unrotated page"));
    }
    let media = page.boxes.iter().find(|(b, _)| *b == vectorcraft_pdf::CropTo::Media).map(|(_, r)| *r).unwrap_or(rect);
    let boxes = page
        .boxes
        .iter()
        .filter_map(|(kind, r)| {
            let name = match kind {
                vectorcraft_pdf::CropTo::Media => "MediaBox",
                vectorcraft_pdf::CropTo::Crop => "CropBox",
                vectorcraft_pdf::CropTo::Trim => "TrimBox",
                vectorcraft_pdf::CropTo::Bleed => "BleedBox",
                vectorcraft_pdf::CropTo::Art => "ArtBox",
                _ => return None,
            };
            Some((name.into(), Rect::new(rect.x0 + r.x0 - media.x0, rect.y0 + media.y1 - r.y1, rect.x0 + r.x1 - media.x0, rect.y0 + media.y1 - r.y0)))
        })
        .collect();
    let mut player = Player {
        session: s,
        root,
        stack: Vec::new(),
        layers: Vec::new(),
        inks: Vec::new(),
        boxes,
        trace: Vec::new(),
        stopped: None,
        clipboard_empty: true,
        messages: Vec::new(),
    };
    for list in lists {
        if let Err(e) = player.list(list, None) {
            player.stopped = Some(json!({"list":list,"reason":e.to_string()}));
        }
        if player.stopped.is_some() {
            break;
        }
    }
    player.session.edit("Artboard Options", |d, _| vectorcraft_pdf::set_preparation_boxes(d, &player.boxes).map_err(error))?;
    let doc = player.doc()?;
    let (bytes, warnings) = super::fileio::encode_with_warnings(
        doc,
        "pdf",
        &json!({"preserveEditing":true,"createLayers":true,
        "includeNonPrinting":true,"discardImportedPrintSettings":false,"output":{"conversion":"none","profiles":"none"},"advanced":{"overprint":"preserve","outlineText":true}}),
    )?;
    let bytes = vectorcraft_pdf::preparation_output(bytes, &player.boxes, doc).map_err(error)?;
    write_output(output, &bytes)?;
    let layers: Vec<_> = selection::layer_ids(doc)
        .into_iter()
        .filter_map(|id| doc.node(id))
        .map(|n| json!({"name":n.name,"objects":selection::leaves(n).len()}))
        .collect();
    let spots: Vec<_> = doc.swatches.iter().filter(|w| w.spot).map(|w| w.name.clone()).collect();
    let art = selection::layer_ids(doc).into_iter().find(|id| doc.node(*id).is_some_and(|n| n.name.as_deref() == Some("Artwork")));
    let artwork = json!({"exists":art.is_some(),"allArt":art.is_some_and(|a|selection::all_objects(doc, false).iter().all(|id|doc.ancestry(*id).is_some_and(|v|v.contains(&a))))});
    Ok(json!({"status":if player.stopped.is_some(){"stopped"}else{"completed"},"stoppedAt":player.stopped,
        "traceLength":player.trace.len(),"trace":player.trace,"layers":layers,"spots":spots,"artwork":artwork,"warnings":warnings,"messages":player.messages,"importWarnings":opened.get("warnings")}))
}

#[cfg(not(target_arch = "wasm32"))]
fn write_output(path: &str, bytes: &[u8]) -> Result<()> {
    super::fileio::write_atomic(Path::new(path), bytes).map_err(error)
}
#[cfg(target_arch = "wasm32")]
fn write_output(_: &str, _: &[u8]) -> Result<()> {
    Err(error("PAL filesystem output is unavailable on the web"))
}
