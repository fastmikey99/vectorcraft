//! Headless PAL player for preparation and vector separation geometry.
mod actions;
mod clean;
mod coverage;
mod geometry;
pub(super) mod images;
mod intents;
mod marks;
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
    let message = e.to_string();
    EngineError::Other(if message == "the shapes are too degenerate to combine" {
        "The builder cannot combine these artwork outlines accurately; no separation PDF was written".into()
    } else {
        message
    })
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

fn is_trap_ticket(name: &str) -> bool {
    let normalized = name.replace('\\', "/");
    let path = Path::new(&normalized);
    // Match the reader's case-insensitive PAL extension handling.
    let name = if path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("pal")) { path.file_stem() } else { path.file_name() };
    matches!(name.and_then(|name| name.to_str()), Some("Trapping_Supacolour_Standard" | "Trapping_Supacolour_Standard (NEW)"))
}

/// Inspect both conditional branches and resolved calls, independent of the current
/// selection. A wrapper must not disguise a trapping recipe as a Light recipe.
fn nested_trapping(root: &Path, lists: &[&str], intents: &intents::Intents) -> Result<bool> {
    fn visit(root: &Path, path: &Path, stack: &mut Vec<PathBuf>, steps: &mut usize) -> Result<bool> {
        if stack.len() >= 32 || stack.iter().any(|p| p == path) {
            return Err(error("recursive PAL call or call depth exceeds 32"));
        }
        stack.push(path.to_path_buf());
        let actions = reader::read(path).map_err(error)?;
        let result = walk(root, path, &actions, stack, steps);
        stack.pop();
        result
    }
    fn walk(root: &Path, path: &Path, actions: &[Action], stack: &mut Vec<PathBuf>, steps: &mut usize) -> Result<bool> {
        for action in actions {
            crate::cancel::check()?;
            *steps += 1;
            if *steps > MAX_STEPS {
                return Err(error("PAL inspection exceeds 100000 steps"));
            }
            if action.kind == "TrapTask" {
                return Ok(true);
            }
            if action.kind == "CallActionList" {
                let name = required(&action.params, "Path")?;
                if is_trap_ticket(name) {
                    return Ok(true);
                }
                let called = reader::resolve(root, Some(path), name).map_err(error)?;
                if visit(root, &called, stack, steps)? {
                    return Ok(true);
                }
            }
            for children in [&action.children, &action.then_actions, &action.else_actions] {
                if walk(root, path, children, stack, steps)? {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
    let mut steps = 0;
    for name in lists.iter().filter(|name| !matches!(**name, "TrapTask" | "MetallicIntents")) {
        let path = reader::resolve(root, None, name).map_err(error)?;
        if visit(root, &path, &mut Vec::new(), &mut steps)? {
            return Ok(true);
        }
    }
    // Intent/mark lists are executed by apply_intents, even when absent from lists.
    // They are real PAL names, not the top-level synthetic task/intent markers.
    for name in intents.lists() {
        if is_trap_ticket(name) {
            return Ok(true);
        }
        let path = reader::resolve(root, None, name).map_err(error)?;
        if visit(root, &path, &mut Vec::new(), &mut steps)? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn specs() -> Vec<CommandSpec> {
    let mut specs = marks::specs();
    specs.push(cmd!(
        "sep.runActionList",
        "Run Separation Action Lists",
        [],
        None,
        "{root?: action-list library folder (default: VECTORCRAFT_ACTION_LISTS), profiles?: ICC folder (default: VECTORCRAFT_PROFILES), lists: [relative list names in execution order], intents?: {noTrap?: {objectIds?: [prepared object ids], rectangles?: [{x, y, width, height in mm}]}, metallic?: [{objects?: {ids: [id]} | {spot: name} | {rectMm: [x,y,width,height]}, list: relative PAL name}]}, input: PDF path, output: PDF path} run PAL actions; returns {status, stoppedAt, traceLength, trace, layers, spots, artwork, warnings}. Trace is also written to stderr. Unsupported actions and fidelity losses stop without publishing output.",
        always,
        run
    ));
    specs
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
    crosshair: Option<vectorcraft_geom::Point>,
    marked_intents: bool,
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
        if caller.is_none() && is_trap_ticket(name) && self.marked_intents {
            self.no_trap = intents::from_marks(self.doc()?).no_trap;
        }
        if caller.is_none()
            && is_trap_ticket(name)
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
    let marked_intents = p.get("intents").is_none();
    let profiles = profiles::folder(p)?;
    let input = string("input")?;
    let output = string("output")?;
    let is_pdf = Path::new(input).extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"));
    if !Path::new(input).extension().is_some_and(|e| ["pdf", "png", "jpg", "jpeg"].iter().any(|kind| e.eq_ignore_ascii_case(kind)))
        || !Path::new(output).extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
    {
        return Err(error("sep.runActionList requires a PDF, PNG or JPEG input and a PDF output"));
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
        let tickets: Vec<_> = lists.iter().enumerate().filter(|(_, name)| is_trap_ticket(name)).map(|(i, _)| i).collect();
        if tickets.len() != 1 || tickets.first().and_then(|i| lists.get(i + 1)).copied() != Some("TrapTask") {
            return Err(error(
                "intents.noTrap requires exactly one explicit Trapping_Supacolour_Standard (optional legacy (NEW) suffix) list immediately followed by TrapTask",
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
    if no_trap.is_some() && nested_trapping(&root, &lists, &intents)? {
        return Err(error("intents.noTrap requires an explicit trap ticket/TrapTask sequence without nested trapping"));
    }
    let opened = s.execute("document.open", &json!({"path":input,"printProduction":true,"textAs":"outlines"}))?;
    if let Some(reason) = opened
        .get("warnings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find(|w| vectorcraft_pdf::classify_diagnostic(w) == vectorcraft_pdf::DiagnosticClass::FidelityLoss)
    {
        return Ok(
            json!({"status":"stopped", "stoppedAt":{"list":"Import","action":"document.open","reason":format!("import fidelity loss: {reason}")},
            "traceLength":0,"trace":[],"layers":[],"spots":[],"artwork":{},"warnings":[],"messages":[],"importWarnings":opened.get("warnings")}),
        );
    }
    s.edit("Clean Up", |d, _| {
        clean::stabilize_input(d);
        Ok(())
    })?;
    if marked_intents {
        s.edit("Separation Marks", |d, _| {
            marks::inherit(d);
            Ok(())
        })?;
        let marked = intents::from_marks(&s.doc()?.doc);
        if marked.no_trap.is_some() {
            let tickets: Vec<_> = lists.iter().enumerate().filter(|(_, name)| is_trap_ticket(name)).map(|(i, _)| i).collect();
            // Light recipes do no trapping, so persistent NoTrap marks are already satisfied.
            let has_trapping = !tickets.is_empty() || lists.contains(&"TrapTask");
            if has_trapping && (tickets.len() != 1 || tickets.first().and_then(|i| lists.get(i + 1)).copied() != Some("TrapTask")) {
                return Err(error("NoTrap marks require exactly one trap ticket immediately followed by TrapTask"));
            }
            // Even an otherwise valid explicit sequence cannot permit extra traps in wrappers.
            if nested_trapping(&root, &lists, &marked)? {
                return Err(error("NoTrap marks require exactly one trap ticket immediately followed by TrapTask; nested trapping is unsupported"));
            }
        }
    }
    if s.doc()?.doc.artboards.len() != 1 {
        return Err(error("PAL preparation currently requires a single-page PDF"));
    }
    let rect = s.doc()?.doc.artboards.first().ok_or_else(|| error("PDF has no page"))?.rect;
    let boxes = if is_pdf {
        let source_info = vectorcraft_pdf::info(&std::fs::read(input).map_err(error)?, None).map_err(error)?;
        let page = source_info.pages.first().ok_or_else(|| error("PDF has no page"))?;
        if page.rotation != 0 {
            return Err(error("PAL preparation requires an unrotated page"));
        }
        let media = page.boxes.iter().find(|(b, _)| *b == vectorcraft_pdf::CropTo::Media).map(|(_, r)| *r).unwrap_or(rect);
        page.boxes
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
                Some((
                    name.into(),
                    Rect::new(rect.x0 + r.x0 - media.x0, rect.y0 + media.y1 - r.y1, rect.x0 + r.x1 - media.x0, rect.y0 + media.y1 - r.y0),
                ))
            })
            .collect()
    } else {
        ["MediaBox", "CropBox", "TrimBox", "BleedBox", "ArtBox"].into_iter().map(|name| (name.into(), rect)).collect()
    };
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
        crosshair: None,
        marked_intents,
    };
    if let Err(e) = player.remove_background() {
        player.stopped = Some(json!({"list":"Preparation","action":"DeleteBackground","reason":e.to_string()}));
    }
    let intent_position = lists
        .iter()
        .position(|name| {
            let name = name.replace('\\', "/");
            name == "MetallicIntents" || name == "TrapTask" || name.starts_with("(2b)") || name.starts_with("(3)") || is_trap_ticket(&name)
        })
        .unwrap_or(0);
    for (position, list) in lists.into_iter().enumerate() {
        if player.stopped.is_some() {
            break;
        }
        if position == intent_position {
            if marked_intents {
                intents = intents::from_marks(player.doc()?);
            }
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
    let warnings = if player.stopped.is_none() {
        let encoded = (|| -> Result<(Vec<u8>, Vec<String>)> {
            let (bytes, mut warnings) = super::fileio::encode_with_warnings(
                doc,
                "pdf",
                &json!({"preserveEditing":true,"createLayers":true,
        "includeNonPrinting":true,"discardImportedPrintSettings":false,"output":output_colors,"advanced":{"overprint":"preserve","outlineText":true}}),
            )?;
            let bytes = vectorcraft_pdf::preparation_output(bytes, &player.boxes, doc).map_err(error)?;
            // The preparation finalizer above restores these settings explicitly after conversion.
            warnings.retain(|w| {
                w != "imported PDF print settings were not restored because the export requests a standard, flattening or colour conversion"
            });
            Ok((bytes, warnings))
        })();
        match encoded {
            Err(e) => {
                player.stopped = Some(json!({"list":"Export","action":"document.exportPdf","reason":format!("export refused: {e}")}));
                Vec::new()
            }
            Ok((bytes, warnings)) => {
                if let Some(reason) =
                    warnings.iter().find(|w| vectorcraft_pdf::classify_diagnostic(w) == vectorcraft_pdf::DiagnosticClass::FidelityLoss)
                {
                    player.stopped = Some(json!({"list":"Export","action":"document.exportPdf","reason":format!("export fidelity loss: {reason}")}));
                } else {
                    write_output(output, &bytes)?;
                }
                warnings
            }
        }
    } else {
        Vec::new()
    };
    let doc = player.doc()?;
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
