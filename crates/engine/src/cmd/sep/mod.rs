//! Headless PAL player for preparation and vector separation geometry.
mod actions;
mod clean;
mod geometry;
mod intents;
mod metallic;
pub(super) mod profiles;
mod reader;
mod selection;
#[cfg(test)]
mod tests;
mod trap;

use super::{CommandSpec, always, cmd};
use crate::{EngineError, Result, Session};
use reader::{Action, Element};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use vectorcraft_doc::{Document, NodeId};
use vectorcraft_geom::Rect;

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
        "{root?: action-list library folder (default: VECTORCRAFT_ACTION_LISTS), profiles?: ICC folder (default: VECTORCRAFT_PROFILES), lists: [relative list names in execution order], intents?: {noTrap?: {objectIds?: [prepared object ids], rectangles?: [{x, y, width, height in mm}]}, metallic?: [{objects?: {ids: [id]} | {spot: name} | {rectMm: [x,y,width,height]}, list: relative PAL name}]}, input: PDF path, output: PDF path} run PAL actions; returns {status, stoppedAt, traceLength, trace, layers, spots, artwork, warnings}. Trace is also written to stderr. Unsupported actions stop before mutation and write the document so far.",
        always,
        run
    )]
}

struct Player<'a> {
    session: &'a mut Session,
    root: PathBuf,
    profiles: Option<PathBuf>,
    stack: Vec<PathBuf>,
    layers: Vec<NodeId>,
    inks: Vec<String>,
    boxes: BTreeMap<String, Rect>,
    trace: Vec<Value>,
    stopped: Option<Value>,
    clipboard_empty: bool,
    intent_scope: Option<Vec<NodeId>>,
    trap_footprints: BTreeMap<NodeId, (vectorcraft_doc::Node, vectorcraft_geom::PathData)>,
    messages: Vec<String>,
    no_trap: Option<intents::NoTrap>,
}
impl Player<'_> {
    fn doc(&self) -> Result<&Document> {
        Ok(&self.session.doc()?.doc)
    }
    fn active_layer(&self) -> Result<NodeId> {
        let layers = selection::layer_ids(self.doc()?);
        layers.iter().rev().find(|id| self.layers.contains(id)).or_else(|| layers.last()).copied().ok_or_else(|| error("PAL has no active layer"))
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
        if caller.is_none()
            && path.file_stem().and_then(|s| s.to_str()) == Some("Trapping_Supacolour_Standard (NEW)")
            && let Some(intent) = self.no_trap.take()
        {
            self.apply_no_trap(intent)?;
        }
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
            let result = self.action(a);
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
    let mut intents = intents::read(p)?;
    let no_trap = intents.no_trap.take();
    let profiles = profiles::folder(p)?;
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
    if no_trap.is_some() {
        let tickets: Vec<_> = lists
            .iter()
            .enumerate()
            .filter(|(_, name)| name.replace('\\', "/").trim_end_matches(".pal").rsplit('/').next() == Some("Trapping_Supacolour_Standard (NEW)"))
            .map(|(i, _)| i)
            .collect();
        if tickets.len() != 1 || tickets.first().and_then(|i| lists.get(i + 1)).copied() != Some("TrapTask") {
            return Err(error(
                "intents.noTrap requires exactly one explicit Trapping_Supacolour_Standard (NEW) list immediately followed by TrapTask",
            ));
        }
    }
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
        profiles,
        stack: Vec::new(),
        layers: Vec::new(),
        inks: Vec::new(),
        boxes,
        trace: Vec::new(),
        stopped: None,
        clipboard_empty: true,
        intent_scope: None,
        trap_footprints: BTreeMap::new(),
        messages: Vec::new(),
        no_trap,
    };
    player.remove_background()?;
    let intent_position = lists
        .iter()
        .position(|name| {
            let name = name.replace('\\', "/");
            name == "MetallicIntents"
                || name == "TrapTask"
                || name.starts_with("(2b)")
                || name.starts_with("(3)")
                || name.trim_end_matches(".pal").rsplit('/').next() == Some("Trapping_Supacolour_Standard (NEW)")
        })
        .unwrap_or(0);
    for (position, list) in lists.into_iter().enumerate() {
        if position == intent_position {
            if let Err(e) = player.apply_intents(&intents) {
                player.stopped = Some(json!({"list":"MetallicIntents","reason":e.to_string()}));
            }
            if player.stopped.is_some() {
                break;
            }
        }
        if list == "MetallicIntents" {
            continue;
        }
        let result = if list == "TrapTask" {
            player.play(
                &player.root.join("AE"),
                &[Action {
                    kind: "TrapTask".into(),
                    position: 1,
                    params: Element::default(),
                    children: vec![],
                    then_actions: vec![],
                    else_actions: vec![],
                }],
            )
        } else {
            player.list(list, None)
        };
        if let Err(e) = result {
            player.stopped = Some(json!({"list":list,"reason":e.to_string()}));
        }
        if player.stopped.is_some() {
            break;
        }
    }
    player.session.edit("Artboard Options", |d, _| vectorcraft_pdf::set_preparation_boxes(d, &player.boxes).map_err(error))?;
    let doc = player.doc()?;
    let output_colors = if let Some(name) = doc.color_profiles.cmyk.as_deref() {
        json!({"conversion":"preserveNumbers","destination":name,"profiles":"none"})
    } else {
        json!({"conversion":"none","profiles":"none"})
    };
    let (bytes, warnings) = super::fileio::encode_with_warnings(
        doc,
        "pdf",
        &json!({"preserveEditing":true,"createLayers":true,
        "includeNonPrinting":true,"discardImportedPrintSettings":false,"output":output_colors,"advanced":{"overprint":"preserve","outlineText":true}}),
    )?;
    let bytes = vectorcraft_pdf::preparation_output(bytes, &player.boxes, doc).map_err(error)?;
    write_output(output, &bytes)?;
    let layers: Vec<_> = selection::layer_ids(doc)
        .into_iter()
        .filter_map(|id| doc.node(id))
        .map(|n| {
            let leaves = actions::leaf_targets(doc, &[n.id]);
            let mut fills = std::collections::BTreeSet::new();
            let mut strokes = std::collections::BTreeSet::new();
            for id in &leaves {
                if let Some(node) = doc.node(*id) {
                    if matches!(node.kind, vectorcraft_doc::NodeKind::Image(_)) && node.appearance.items.is_empty() {
                        fills.insert(false);
                    }
                    for item in &node.appearance.items {
                        match item {
                            vectorcraft_doc::AppearanceItem::Fill(f) if !f.paint.is_none() => {
                                fills.insert(f.overprint);
                            }
                            vectorcraft_doc::AppearanceItem::Stroke(s) if !s.paint.is_none() => {
                                strokes.insert(s.overprint);
                            }
                            _ => {}
                        }
                    }
                }
            }
            json!({"name":n.name,"objects":leaves.len(),"fillOverprint":fills,"strokeOverprint":strokes})
        })
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
