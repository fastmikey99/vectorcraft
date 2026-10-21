use super::{Action, Player, Result, clean, error, flag, number, profiles, required, selection};
use serde_json::json;
use vectorcraft_color::{Color, Paint, Swatch};
use vectorcraft_doc::{AppearanceItem, Document, LineJoin, Node, NodeId, NodeKind, StrokeLayer};
const PROCESS: [&str; 4] = ["Cyan", "Magenta", "Yellow", "Black"];
fn process(name: &str) -> Option<usize> {
    PROCESS.iter().position(|n| *n == name)
}
pub(super) fn roots(d: &Document, ids: &[NodeId]) -> Vec<NodeId> {
    let members: std::collections::BTreeSet<_> = ids.iter().copied().collect();
    ids.iter()
        .copied()
        .filter(|id| d.node(*id).is_some() && !d.ancestry(*id).is_some_and(|v| v.iter().any(|p| p != id && members.contains(p))))
        .collect()
}
pub(super) fn leaf_targets(d: &Document, ids: &[NodeId]) -> Vec<NodeId> {
    let mut out = Vec::new();
    for id in roots(d, ids) {
        if let Some(n) = d.node(id) {
            fn visit(n: &Node, out: &mut Vec<NodeId>) {
                if matches!(n.kind, NodeKind::Compound { .. }) || n.children().is_none() {
                    out.push(n.id);
                } else if let Some(cs) = n.children() {
                    for c in cs {
                        visit(c, out);
                    }
                }
            }
            visit(n, &mut out);
        }
    }
    out
}
fn ink_names(d: &Document) -> Vec<String> {
    PROCESS.iter().map(|n| n.to_string()).chain(d.swatches.iter().filter(|s| s.spot).map(|s| s.name.clone())).collect()
}
impl Player<'_> {
    pub(super) fn action(&mut self, a: &Action) -> Result<usize> {
        let p = &a.params;
        let ids = self.ids()?;
        let count = ids.len();
        match a.kind.as_str() {
            "Group" | "Conditional" | "CallActionList" => {
                if a.kind == "CallActionList" && p.child("CallParameters").is_some_and(|p| !p.children.is_empty() || !p.text.trim().is_empty()) {
                    return Err(error("PAL call parameters are not implemented"));
                }
                Ok(0)
            }
            "RemoveFill" => {
                let targets = leaf_targets(self.doc()?, &ids);
                self.session.edit("Appearance", |d, _| {
                    for id in &targets {
                        if let Some(n) = d.node_mut(*id) {
                            n.appearance.items.retain(|item| !matches!(item, AppearanceItem::Fill(_)));
                        }
                    }
                    Ok(targets.len())
                })
            }
            "ClipOnShape" => self.clip_on_shape(),
            "TrapTask" => self.trap_task(),
            "SetTrapTags" | "RemoveTrappingTags" => self.trap_tags(p, a.kind == "RemoveTrappingTags"),
            "Rasterize" => self.rasterize(p),
            "ShowMessage" => {
                self.messages.push(p.children.iter().map(|e| format!("{}={}", e.name, e.text)).collect::<Vec<_>>().join(" "));
                if p.value("Severity") == Some("Error") {
                    return Err(error(p.value("Message").unwrap_or("PAL error message")));
                }
                Ok(0)
            }
            "SelectAllObjects" | "InvertObjectSelection" | "SelectObjectsInLayer" | "SelectObjectsByAttributes" => {
                let d = self.doc()?;
                let mut all = selection::all_objects(d, true);
                if a.kind == "SelectObjectsByAttributes"
                    && let Some(scope) = &self.intent_scope
                {
                    all.retain(|id| scope.contains(id));
                }
                let mut selected = ids.clone();
                let found = match a.kind.as_str() {
                    "SelectAllObjects" => all,
                    "InvertObjectSelection" => all.into_iter().filter(|id| !ids.contains(id)).collect(),
                    "SelectObjectsInLayer" => {
                        all.into_iter().filter(|id| d.ancestry(*id).is_some_and(|v| v.iter().any(|id| self.layers.contains(id)))).collect()
                    }
                    _ => {
                        let rules: Vec<_> = p.children.iter().filter(|e| e.name == "Rule").collect();
                        if rules.is_empty() {
                            return Err(error("SelectObjectsByAttributes requires a rule"));
                        }
                        selection::combine_rules(&[], p.value("Operator").unwrap_or("All"))?;
                        let mut found = Vec::new();
                        for id in all {
                            let n = d.node(id).ok_or_else(|| error("selected object disappeared"))?;
                            let matches = rules.iter().map(|r| selection::rule(d, &ids, Some(n), r)).collect::<Result<Vec<_>>>()?;
                            if selection::combine_rules(&matches, p.value("Operator").unwrap_or("All"))? {
                                found.push(id);
                            }
                        }
                        found
                    }
                };
                // An artist-supplied selection is authoritative for the opening ink selector
                // of a 2a list (including lists using NewSelection). Nested cleanup retains
                // its literal PAL selection semantics; a bare/self-selecting list is unchanged.
                let artist_selection = self.intent_scope.is_some()
                    && self.stack.len() == 1
                    && a.position == 1
                    && a.kind == "SelectObjectsByAttributes"
                    && p.children.iter().filter(|e| e.name == "Rule").all(|r| r.attributes.get("Type").is_some_and(|t| t == "SeparationNameRule"));
                if !artist_selection {
                    selection::merge(&mut selected, found, p.value("SelectionMode").unwrap_or("NewSelection"))?;
                }
                let n = selected.len();
                self.session.doc_mut()?.selection.set(selected);
                Ok(n)
            }
            "SelectLayerByName" | "SelectEmptyLayers" | "SelectPrintingLayers" => {
                let d = self.doc()?;
                let mut found = Vec::new();
                for id in selection::layer_ids(d) {
                    let Some(n) = d.node(id) else { continue };
                    let yes = match a.kind.as_str() {
                        "SelectLayerByName" => selection::matches(n.name.as_deref().unwrap_or(""), required(p, "LayerName")?, flag(p, "IgnoreCase")?),
                        "SelectEmptyLayers" => n.children().is_none_or(|c| c.is_empty()) != flag(p, "NonEmpty")?,
                        _ => matches!(n.kind, NodeKind::Layer { printable: true, .. }) != flag(p, "NonPrinting")?,
                    };
                    if yes {
                        found.push(id);
                    }
                }
                if flag(p, "IncludeSublayers")? {
                    let selected = found.clone();
                    for id in selection::layer_ids(d) {
                        if !found.contains(&id) && d.ancestry(id).is_some_and(|v| v.iter().any(|x| selected.contains(x))) {
                            found.push(id);
                        }
                    }
                }
                selection::merge(&mut self.layers, found, p.value("Mode").unwrap_or("NewSelection"))?;
                Ok(self.layers.len())
            }
            "SelectSeparationByName" | "SelectSeparationsInPrintingLayers" | "InvertSeparationSelection" => {
                let all = ink_names(self.doc()?);
                let mut found = Vec::new();
                if a.kind == "SelectSeparationsInPrintingLayers" {
                    for id in selection::layer_ids(self.doc()?) {
                        let Some(n) = self.doc()?.node(id) else { continue };
                        if matches!(n.kind, NodeKind::Layer { printable: true, .. }) {
                            for (name, v) in selection::inks(self.doc()?, n)? {
                                if v > 0.0 && !found.contains(&name) {
                                    found.push(name);
                                }
                            }
                        }
                    }
                } else {
                    for name in all {
                        let yes = if a.kind == "InvertSeparationSelection" {
                            !self.inks.contains(&name)
                        } else {
                            selection::matches(&name, required(p, "SeparationName")?, flag(p, "IgnoreCase")?)
                        };
                        if yes {
                            found.push(name);
                        }
                    }
                }
                selection::merge(&mut self.inks, found, p.value("Mode").unwrap_or("NewSelection"))?;
                Ok(0)
            }
            "RenameObjects" | "RenameLayers" | "RenameLayersWithWildcards" => {
                let targets = if a.kind == "RenameObjects" {
                    ids
                } else if a.kind == "RenameLayers" {
                    self.layers.clone()
                } else {
                    let old = required(p, "OldLayerName")?;
                    let ignore = flag(p, "CaseInsensitive")?;
                    selection::layer_ids(self.doc()?)
                        .into_iter()
                        .filter(|id| {
                            self.doc()
                                .ok()
                                .and_then(|d| d.node(*id))
                                .is_some_and(|n| selection::matches(n.name.as_deref().unwrap_or(""), old, ignore))
                        })
                        .collect()
                };
                let key = match a.kind.as_str() {
                    "RenameObjects" => "ObjectName",
                    "RenameLayers" => "LayerName",
                    _ => "NewLayerName",
                };
                let name = required(p, key)?.to_string();
                let n = targets.len();
                self.session.edit("Rename", |d, _| {
                    for id in targets {
                        if let Some(n) = d.node_mut(id) {
                            n.name = Some(name.clone());
                        }
                    }
                    Ok(())
                })?;
                Ok(n)
            }
            "CreateLayerOnTop" => {
                if p.value("LayerPosition").is_some_and(|v| v != "OnTopOfAllLayers") {
                    return Err(error("unsupported new-layer position"));
                }
                let name = required(p, "LayerName")?;
                let ignore = flag(p, "IgnoreCase")?;
                let existing = selection::layer_ids(self.doc()?).into_iter().find(|id| {
                    self.doc().ok().and_then(|d| d.node(*id)).is_some_and(|n| selection::matches(n.name.as_deref().unwrap_or(""), name, ignore))
                });
                let id = if let Some(id) = existing { id } else { self.session.edit("New Layer", |d, _| Ok(d.add_layer(Some(name))))? };
                self.layers = vec![id];
                self.session.doc_mut()?.active_layer = Some(id);
                Ok(0)
            }
            "DeleteLayers" | "DeleteObjects" => {
                let targets = roots(self.doc()?, if a.kind == "DeleteLayers" { &self.layers } else { &ids });
                let n = targets.len();
                self.session.edit("Delete", |d, sel| {
                    for id in &targets {
                        d.remove(*id)?;
                    }
                    sel.objects.retain(|id| d.node(*id).is_some());
                    Ok(())
                })?;
                self.layers.retain(|id| !targets.contains(id));
                Ok(n)
            }
            "MoveObjectsToLayer" => {
                if ids.is_empty() {
                    return Ok(0);
                }
                let target = self.active_layer()?;
                let targets = roots(self.doc()?, &ids);
                let n = targets.len();
                self.session.edit("Move", |d, _| {
                    for id in targets {
                        let node = d.remove(id)?;
                        d.insert(Some(target), usize::MAX, (*node).clone())?;
                    }
                    Ok(())
                })?;
                Ok(n)
            }
            "ChangeLayerOrder" => {
                let pos = required(p, "LayerPosition")?;
                let top = match pos {
                    "OnTopOfAllLayers" | "AboveAllLayers" => true,
                    "BelowAllLayers" => false,
                    v => return Err(error(format!("unsupported LayerPosition {v}"))),
                };
                let mut targets = roots(self.doc()?, &self.layers);
                if !top {
                    targets.reverse();
                }
                self.session.edit("Arrange", |d, _| {
                    for id in targets {
                        let parent = d.parent_of(id);
                        let node = d.remove(id)?;
                        d.insert(parent, if top { usize::MAX } else { 0 }, (*node).clone())?;
                    }
                    Ok(())
                })?;
                Ok(0)
            }
            "LockUnlockLayers" | "SetLayerPrintingState" | "SetLayerType" => {
                let locked = match p.value("ChangeLockState").unwrap_or("Unlock") {
                    "Lock" => true,
                    "Unlock" => false,
                    v => return Err(error(format!("unsupported lock state {v}"))),
                };
                if a.kind == "SetLayerType" && !matches!(required(p, "ProcessingStep")?, "None" | "White") {
                    return Err(error("only ProcessingStep=None or White is supported"));
                }
                let printable = match p.value("PrintingState").or_else(|| p.value("LayerPrintingState")).unwrap_or("Printing") {
                    "Printing" | "Print" | "1" => true,
                    "NonPrinting" | "Non-Printing" | "DoNotPrint" | "0" => false,
                    v => return Err(error(format!("unsupported printing state {v}"))),
                };
                let targets = self.layers.clone();
                self.session.edit("Layer Options", |d, _| {
                    for id in targets {
                        if let Some(n) = d.node_mut(id) {
                            match a.kind.as_str() {
                                "LockUnlockLayers" => n.locked = locked,
                                "SetLayerPrintingState" => {
                                    if let NodeKind::Layer { printable: p, .. } = &mut n.kind {
                                        *p = printable;
                                    }
                                }
                                _ => n.extras_mut().processing_white = p.value("ProcessingStep") == Some("White"),
                            }
                        }
                    }
                    Ok(())
                })?;
                Ok(0)
            }
            "GroupObjects" | "UngroupObjects" | "Copy" | "PasteInFront" | "ArrangeObjects" => {
                if a.kind == "Copy" {
                    self.clipboard_empty = ids.is_empty();
                }
                if (a.kind == "PasteInFront" && self.clipboard_empty) || (a.kind != "PasteInFront" && ids.is_empty()) {
                    return Ok(0);
                }
                let cmd = match a.kind.as_str() {
                    "GroupObjects" => "object.group",
                    "UngroupObjects" => "object.ungroup",
                    "Copy" => "edit.copy",
                    "PasteInFront" => "edit.pasteInFront",
                    _ => match required(p, "MoveDirection")? {
                        "Front" => "object.arrange.bringToFront",
                        "Back" => "object.arrange.sendToBack",
                        "Forward" => "object.arrange.bringForward",
                        "Backward" => "object.arrange.sendBackward",
                        v => return Err(error(format!("unsupported MoveDirection {v}"))),
                    },
                };
                self.call(cmd, json!({}))?;
                Ok(count)
            }
            "CreateSeparation" => {
                let name = required(p, "SeparationName")?.to_string();
                if process(&name).is_none() && !self.doc()?.swatches.iter().any(|s| s.spot && s.name == name) {
                    let rgb = [number(p, "PreviewRed")?, number(p, "PreviewGreen")?, number(p, "PreviewBlue")?];
                    if rgb.iter().any(|v| !(0.0..=1.0).contains(v)) {
                        return Err(error("ink preview components must be 0 to 1"));
                    }
                    let color = Color::rgb(rgb[0] as f32, rgb[1] as f32, rgb[2] as f32);
                    self.session.edit("New Swatch", |d, _| {
                        d.swatches.push(Swatch { name: name.clone(), paint: Paint::solid(color), global: true, spot: true });
                        Ok(())
                    })?;
                }
                self.inks = vec![name];
                Ok(0)
            }
            "DeleteInk" | "ConvertToCMYK" => {
                let inks = self.inks.clone();
                if inks.is_empty() {
                    return Ok(0);
                }
                let delete = a.kind == "DeleteInk";
                let custom = if !delete && required(p, "Option")? == "Custom" {
                    Some(Color::cmyk(
                        number(p, "CyanValue")? as f32 / 100.0,
                        number(p, "MagentaValue")? as f32 / 100.0,
                        number(p, "YellowValue")? as f32 / 100.0,
                        number(p, "BlackValue")? as f32 / 100.0,
                    ))
                } else {
                    None
                };
                if !delete && !matches!(required(p, "Option")?, "Custom" | "UseFromPDF") {
                    return Err(error("unsupported CMYK conversion method"));
                }
                let alternates: std::collections::BTreeMap<_, _> = self
                    .doc()?
                    .swatches
                    .iter()
                    .filter(|s| s.spot && inks.contains(&s.name))
                    .filter_map(|s| s.paint.color().map(|c| (s.name.clone(), c)))
                    .collect();
                let cms = profiles::document_cms(self.doc()?);
                let all = selection::all_objects(self.doc()?, false);
                // Prepare all replacements before changing paints or deleting swatches.
                // Empty spot channels can be pruned exactly. Positive image ink changes
                // require compositing/conversion semantics not yet implemented here.
                let mut images = std::collections::BTreeMap::new();
                for id in &all {
                    let Some(n) = self.doc()?.node(*id) else { continue };
                    let NodeKind::Image(image) = &n.kind else { continue };
                    let used = selection::inks(self.doc()?, n)?;
                    if used.iter().any(|(name, value)| inks.contains(name) && *value > 0.0 && (delete || process(name).is_none())) {
                        return Err(error(format!("{} cannot safely transform nonzero image ink channels on object {}", a.kind, id)));
                    }
                    let blob = self.doc()?.images.get(&image.key).ok_or_else(|| error("missing image data"))?;
                    if blob.spot_names().iter().any(|name| inks.contains(name)) {
                        let spots = blob.spot_inks().ok_or_else(|| error("undecodable image spot channels"))?;
                        let keep: Vec<_> = spots.names.iter().enumerate().filter(|(_, name)| !inks.contains(name)).map(|(i, _)| i).collect();
                        let process = blob.cmyk().ok_or_else(|| error("undecodable image process channels"))?;
                        let replacement = if keep.is_empty() {
                            vectorcraft_doc::ImageBlob::cmyk_tiff(&process)
                        } else {
                            let data = spots
                                .data
                                .chunks_exact(spots.names.len())
                                .flat_map(|pixel| keep.iter().filter_map(|i| pixel.get(*i).copied()))
                                .collect();
                            let names = keep.iter().filter_map(|i| spots.names.get(*i).cloned()).collect();
                            let spots = vectorcraft_doc::cmyk::SpotInks::new(spots.width, spots.height, names, data)
                                .ok_or_else(|| error("invalid retained image spot channels"))?;
                            vectorcraft_doc::ImageBlob::cmyk_spot_tiff(&process, &spots)
                        }
                        .ok_or_else(|| error("could not encode retained image ink channels"))?;
                        images.insert(image.key.clone(), replacement);
                    }
                }
                let affected = self.session.edit("Recolor Artwork", |d, _| {
                    let mut affected = 0;
                    for id in all {
                        if let Some(n) = d.node_mut(id) {
                            let before = n.appearance.clone();
                            for item in &mut n.appearance.items {
                                let paint = match item {
                                    AppearanceItem::Fill(f) => &mut f.paint,
                                    AppearanceItem::Stroke(s) => &mut s.paint,
                                };
                                convert_paint(paint, &inks, custom, delete, &alternates, cms.as_ref());
                            }
                            affected += usize::from(n.appearance != before);
                        }
                    }
                    d.images.extend(images);
                    d.swatches.retain(|s| !s.spot || !inks.contains(&s.name));
                    Ok(affected)
                })?;
                self.inks.retain(|n| process(n).is_some());
                Ok(affected)
            }
            "SetFillFlatPaint"
            | "UpdateFillFlatPaint"
            | "SetStrokeFlatPaint"
            | "UpdateStrokeFlatPaint"
            | "SetFillOverprint"
            | "SetStrokeOverprint"
            | "SetFillKnockout"
            | "SetStrokeKnockout"
            | "SetStrokeWidth"
            | "SetStrokeCorners" => self.paint(a),
            "AssignDocumentProfile" => {
                let name = required(p, "Profile")?.to_string();
                let resolved = self.resolve_profile(&name);
                let profile = match resolved {
                    Ok(Some(profile)) => profile,
                    other => {
                        let detail = other.err().map(|e| format!(" ({e})")).unwrap_or_default();
                        let message = format!("document profile unavailable: {name}; set VECTORCRAFT_PROFILES or profiles to an ICC folder{detail}");
                        if p.value("MessageSeverity") == Some("Error") {
                            return Err(error(message));
                        }
                        self.messages.push(message);
                        return Ok(0);
                    }
                };
                self.call("edit.assignProfile", json!({"cmyk":profile}))?;
                Ok(0)
            }
            "SetPageBoxSizeByContent" | "SetPageBoxSizeRelative" => {
                let name = required(p, "PageBox")?;
                if !self.boxes.contains_key(name) {
                    return Err(error("unknown page box"));
                }
                let rect = if a.kind == "SetPageBoxSizeByContent" {
                    if !matches!(p.value("Mode"), Some("FitAll" | "FitToSelection")) || p.value("AdjustType") != Some("Both") {
                        return Err(error("only FitAll or FitToSelection with Both content page boxes are supported"));
                    }
                    let targets = self.ids()?;
                    self.doc()?.bounds_of(&roots(self.doc()?, &targets), true).ok_or_else(|| error("cannot fit page box to empty artwork"))?
                } else {
                    let r = *self.boxes.get(required(p, "RefPageBox")?).ok_or_else(|| error("unknown reference page box"))?;
                    let mm = 72.0 / 25.4;
                    vectorcraft_geom::Rect::new(
                        r.x0 - number(p, "Left")? * mm,
                        r.y0 - number(p, "Top")? * mm,
                        r.x1 + number(p, "Right")? * mm,
                        r.y1 + number(p, "Bottom")? * mm,
                    )
                };
                if rect.width() <= 0.0 || rect.height() <= 0.0 {
                    return Err(error("page box has nonpositive size"));
                }
                self.boxes.insert(name.into(), rect);
                if name == "MediaBox" {
                    self.session.edit("Artboard Options", |d, _| {
                        if let Some(b) = d.artboards.first_mut() {
                            b.rect = rect;
                        }
                        d.setup.bleed = [0.0; 4];
                        Ok(())
                    })?;
                }
                Ok(count)
            }
            "PositionCrosshair" => {
                if required(p, "Mode")? != "RelativePosition" || required(p, "Anchor")? != "Center" {
                    return Err(error("PAL crosshair supports RelativePosition at a page box center"));
                }
                let reference = self.boxes.get(required(p, "Reference")?).ok_or_else(|| error("unknown crosshair reference page box"))?;
                let center = reference.center();
                self.crosshair = Some(vectorcraft_geom::Point::new(
                    center.x + number(p, "XOffset")? * 72.0 / 25.4,
                    center.y + number(p, "YOffset")? * 72.0 / 25.4,
                ));
                Ok(0)
            }
            "CreateShapeByParameters" => {
                if required(p, "Shape")? != "Rectangle"
                    || required(p, "Anchor")? != "Center"
                    || number(p, "CornerRadius")? != 0.0
                    || flag(p, "LinkEnabled")?
                {
                    return Err(error("PAL parameter shape supports unlinked, square-corner rectangles at the crosshair center"));
                }
                let center = self.crosshair.ok_or_else(|| error("position the crosshair before creating a parameter shape"))?;
                let width = number(p, "Width")? * 72.0 / 25.4;
                let height = number(p, "Height")? * 72.0 / 25.4;
                if width <= 0.0 || height <= 0.0 {
                    return Err(error("PAL shape size must be positive"));
                }
                let bounds =
                    vectorcraft_geom::Rect::new(center.x - width / 2.0, center.y - height / 2.0, center.x + width / 2.0, center.y + height / 2.0);
                let layer = Some(self.active_layer()?);
                self.session.edit("Rectangle", |d, sel| {
                    let mut appearance = vectorcraft_doc::Appearance::default();
                    appearance.set_fill(Paint::solid(Color::cmyk(0.0, 0.0, 0.0, 1.0)));
                    appearance.set_stroke(Paint::None);
                    let n = Node::path(d.alloc_id(), vectorcraft_geom::shapes::rectangle(bounds), appearance);
                    let id = n.id;
                    d.insert(layer, usize::MAX, n)?;
                    sel.set([id]);
                    Ok(())
                })?;
                Ok(1)
            }
            "CreateShapeOnSelection" => {
                if ids.is_empty() {
                    return Ok(0);
                }
                if required(p, "Shape")? != "Rectangle" || number(p, "CornerRadius")? != 0.0 {
                    return Err(error("PAL shape creation supports rectangles with zero corner radius"));
                }
                let bounds = self.doc()?.bounds_of(&roots(self.doc()?, &ids), true).ok_or_else(|| error("cannot fit page box to empty artwork"))?;
                let layer = Some(self.active_layer()?);
                self.session.edit("Rectangle", |d, sel| {
                    let mut appearance = vectorcraft_doc::Appearance::default();
                    appearance.set_fill(Paint::solid(Color::cmyk(0.0, 0.0, 0.0, 1.0)));
                    appearance.set_stroke(Paint::None);
                    let n = Node::path(d.alloc_id(), vectorcraft_geom::shapes::rectangle(bounds), appearance);
                    let id = n.id;
                    d.insert(layer, usize::MAX, n)?;
                    sel.set([id]);
                    Ok(())
                })?;
                Ok(1)
            }
            "SetObjectsSize" => {
                if ids.is_empty() {
                    return Ok(0);
                }
                // Relative chooses an anchor on the selection (as opposed to the crosshair).
                // Size is a physical length; this is Set Object Size, not Scale Object.
                if required(p, "Mode")? != "Relative" || required(p, "Anchor")? != "Center" || !flag(p, "IncludeOpacityMasks")? {
                    return Err(error("PAL object sizing supports the selection center with opacity masks included"));
                }
                let bounds = self.doc()?.bounds_of(&roots(self.doc()?, &ids), true).ok_or_else(|| error("cannot fit page box to empty artwork"))?;
                if bounds.width() <= 0.0 || bounds.height() <= 0.0 {
                    return Err(error("page box has nonpositive size"));
                }
                let sx = if flag(p, "UseWidth")? { number(p, "Width")? * 72.0 / 25.4 / bounds.width() * 100.0 } else { 100.0 };
                let sy = if flag(p, "UseHeight")? { number(p, "Height")? * 72.0 / 25.4 / bounds.height() * 100.0 } else { 100.0 };
                if sx <= 0.0 || sy <= 0.0 {
                    return Err(error("page box has nonpositive size"));
                }
                self.call("object.scale",json!({"sx":sx,"sy":sy,"origin":[bounds.center().x,bounds.center().y],"patterns":flag(p,"IncludeGradientsAndPatterns")?,"strokes":flag(p,"IncludeStrokeWeights")?,"corners":true}))?;
                Ok(count)
            }
            "CleanObjects" => {
                let opts = clean::Options::read(p)?;
                let brushes = vectorcraft_brush::library(self.doc()?);
                let selected = roots(self.doc()?, &ids);
                self.session.edit("Clean Up", |d, sel| {
                    let mut total = 0;
                    let mut fresh = Vec::new();
                    for id in selected {
                        if let Some(n) = d.node(id).cloned() {
                            let (n, changed) = clean::clean(d, n, &opts, &brushes)?;
                            total += changed;
                            if let Some(n) = n {
                                fresh.push(n.id);
                                *d.node_mut(id).ok_or_else(|| error("cleanup target disappeared"))? = n;
                            } else {
                                d.remove(id)?;
                            }
                        }
                    }
                    sel.set(fresh);
                    Ok(total)
                })
            }
            "AddWhiteUnderPrint" | "Choke" | "Spread" | "Shaper" => self.geometry(a),
            other => Err(error(format!("not implemented: {other}"))),
        }
    }
    fn paint(&mut self, a: &Action) -> Result<usize> {
        let p = &a.params;
        let targets = leaf_targets(self.doc()?, &self.ids()?);
        let count = targets.len();
        let stroke = a.kind.contains("Stroke");
        let flat = a.kind.ends_with("FlatPaint");
        let update = a.kind.starts_with("Update");
        let percentage = if flat { number(p, "Percentage")? as f32 / 100.0 } else { 0.0 };
        if !(0.0..=1.0).contains(&percentage) {
            return Err(error("paint percentage must be 0 to 100"));
        }
        let paint = if flat { Some(flat_paint(self.doc()?, &self.inks, percentage, flag(p, "MakeRegistrationColor")?)?) } else { None };
        let width = if a.kind == "SetStrokeWidth" { Some(number(p, "Width")? * 72.0 / 25.4) } else { None };
        if width.is_some_and(|w| w < 0.0) {
            return Err(error("stroke width must be nonnegative"));
        }
        let join = if a.kind == "SetStrokeCorners" {
            Some(match p.value("Corners").or_else(|| p.value("JoinType")).unwrap_or("Round") {
                "Round" => LineJoin::Round,
                "Miter" => LineJoin::Miter,
                "Bevel" => LineJoin::Bevel,
                v => return Err(error(format!("unsupported stroke corners {v}"))),
            })
        } else {
            None
        };
        let inks = self.inks.clone();
        self.session.edit("Appearance", |d, _| {
            for id in targets {
                let Some(n) = d.node_mut(id) else { continue };
                if let Some(new) = &paint {
                    let old = if stroke { n.appearance.stroke_paint() } else { n.appearance.fill_paint() };
                    let new = if update { updated_paint(&old, new, &inks, percentage)? } else { new.clone() };
                    if stroke {
                        n.appearance.set_stroke(new);
                    } else {
                        n.appearance.set_fill(new);
                    }
                }
                if stroke {
                    if n.appearance.stroke().is_none() {
                        n.appearance.items.push(AppearanceItem::Stroke(StrokeLayer::new(Paint::None, 0.0)));
                    }
                    if let Some(s) = n.appearance.stroke_mut() {
                        if let Some(w) = width {
                            s.width = w;
                        }
                        if let Some(j) = join {
                            s.join = j;
                        }
                        if a.kind == "SetStrokeOverprint" {
                            s.overprint = true;
                        } else if a.kind == "SetStrokeKnockout" || (flat && !update) {
                            s.overprint = false;
                        }
                    }
                } else if let Some(f) = n.appearance.fill_mut() {
                    if a.kind == "SetFillOverprint" {
                        f.overprint = true;
                    } else if a.kind == "SetFillKnockout" || (flat && !update) {
                        f.overprint = false;
                    }
                }
            }
            Ok(())
        })?;
        Ok(count)
    }
}
pub(super) fn flat_paint(d: &Document, inks: &[String], t: f32, registration: bool) -> Result<Paint> {
    if registration {
        return Ok(Paint::registration());
    }
    let spots: Vec<_> = inks.iter().filter(|n| process(n).is_none()).collect();
    if !spots.is_empty() {
        if inks.len() != 1 {
            return Err(error("mixed process/spot flat paint is not representable as one VectorCraft paint"));
        }
        let name = spots.first().ok_or_else(|| error("missing selected ink"))?;
        let color = d.swatch(name).and_then(|s| s.paint.color()).ok_or_else(|| error("selected ink has no preview color"))?;
        return Ok(Paint::Solid { color: color.tinted(t), swatch: Some((*name).clone()), tint: t });
    }
    let mut c = [0.0; 4];
    for n in inks {
        if let Some(i) = process(n)
            && let Some(v) = c.get_mut(i)
        {
            *v = t;
        }
    }
    Ok(Paint::solid(Color::cmyk(c[0], c[1], c[2], c[3])))
}
fn updated_paint(old: &Paint, new: &Paint, inks: &[String], t: f32) -> Result<Paint> {
    match (old, new) {
        (Paint::None, _) => Ok(new.clone()),
        (Paint::Solid { swatch: Some(a), .. }, Paint::Solid { swatch: Some(b), .. }) if a == b => Ok(new.clone()),
        (Paint::Solid { color, swatch: None, .. }, Paint::Solid { swatch: None, .. }) => {
            let mut c = color.to_cmyk();
            for n in inks {
                if let Some(i) = process(n)
                    && let Some(v) = c.get_mut(i)
                {
                    *v = t;
                }
            }
            Ok(Paint::solid(Color::cmyk(c[0], c[1], c[2], c[3])))
        }
        (Paint::Solid { swatch: Some(_), .. }, Paint::Solid { swatch: None, .. }) if t == 0.0 && inks.iter().all(|n| process(n).is_some()) => {
            Ok(old.clone())
        }
        (_, Paint::Solid { swatch: Some(_), .. }) if old.color().is_some_and(|c| c.to_cmyk().iter().all(|v| *v == 0.0)) => Ok(new.clone()),
        _ => Err(error("updating this paint would require a mixed-ink paint")),
    }
}
fn convert_paint(
    p: &mut Paint,
    inks: &[String],
    custom: Option<Color>,
    delete: bool,
    alternates: &std::collections::BTreeMap<String, Color>,
    cms: Option<&vectorcraft_color::cms::Cms>,
) {
    match p {
        Paint::Solid { color, swatch: Some(name), tint } if inks.contains(name) => {
            let c = custom.or_else(|| alternates.get(name).copied()).unwrap_or(*color);
            let [c, m, y, k] = profiles::cmyk(c, cms);
            *p = if delete {
                Paint::solid(Color::cmyk(0.0, 0.0, 0.0, 0.0))
            } else {
                Paint::solid(Color::cmyk(c * *tint, m * *tint, y * *tint, k * *tint))
            };
        }
        Paint::Solid { color, swatch: None, .. } if delete => {
            let mut c = profiles::cmyk(*color, cms);
            for n in inks {
                if let Some(i) = process(n)
                    && let Some(v) = c.get_mut(i)
                {
                    *v = 0.0;
                }
            }
            *color = Color::cmyk(c[0], c[1], c[2], c[3]);
        }
        Paint::Gradient(g) => {
            for s in &mut g.gradient.stops {
                let mut p = Paint::Solid { color: s.color, swatch: s.swatch.clone(), tint: s.tint };
                convert_paint(&mut p, inks, custom, delete, alternates, cms);
                if let Paint::Solid { color, swatch, tint } = p {
                    s.color = color;
                    s.swatch = swatch;
                    s.tint = tint;
                }
            }
        }
        _ => {}
    }
}
