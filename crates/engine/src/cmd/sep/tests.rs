use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use vectorcraft_testkit::pdf::{PdfPage, pdf};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("vectorcraft-pal-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn pal(&self, name: &str, body: &str) {
        let p = self.0.join(format!("{name}.pal"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, format!("<ActionList Version=\"2.0\">{body}</ActionList>")).unwrap();
    }
    fn input(&self) {
        std::fs::write(self.0.join("input.pdf"), pdf(&[PdfPage::new(100.0, 100.0, "q 10 10 30 30 re W n 0 1 0 0 k 0 0 70 70 re f Q")], None))
            .unwrap();
    }
    fn run(&self, s: &mut Session) -> Value {
        s.execute("sep.runActionList", &json!({"root":self.0,"lists":["main"],"input":self.0.join("input.pdf"),"output":self.0.join("output.pdf")}))
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn a(kind: &str, params: &str) -> String {
    format!("<ActionListItem Type=\"{kind}\"><Parameters>{params}</Parameters></ActionListItem>")
}
fn player(s: &mut Session) -> Player<'_> {
    Player {
        session: s,
        root: PathBuf::new(),
        profiles: None,
        stack: vec![],
        layers: vec![],
        inks: vec![],
        boxes: BTreeMap::new(),
        trace: vec![],
        stopped: None,
        clipboard_empty: true,
        intent_scope: None,
        trap_footprints: BTreeMap::new(),
        messages: vec![],
        no_trap: None,
        crosshair: None,
        marked_intents: false,
    }
}
fn step(p: &mut Player<'_>, kind: &str, params: &str) -> Result<usize> {
    let actions = reader::parse(&format!("<ActionList Version=\"2.0\">{}</ActionList>", a(kind, params))).unwrap();
    p.action(&actions[0])
}

#[test]
fn command_clips_moves_creates_ink_and_stops_without_running_later_actions() {
    let f = Fixture::new();
    f.input();
    f.pal(
        "main",
        &[
            a("SelectAllObjects", ""),
            a("CleanObjects", "<ExecuteClip>1</ExecuteClip>"),
            a("CreateLayerOnTop", "<LayerName>Artwork</LayerName>"),
            a("GroupObjects", ""),
            a("MoveObjectsToLayer", ""),
            a("SelectEmptyLayers", ""),
            a("DeleteLayers", ""),
            a("SelectAllObjects", ""),
            a("UngroupObjects", ""),
            a("CallActionList", "<Path>nested\\prepare</Path>"),
            a("DeleteObjects", ""),
        ]
        .concat(),
    );
    f.pal(
        "nested/prepare",
        &[
            a(
                "CreateSeparation",
                "<SeparationName>Base ink</SeparationName><PreviewRed>1</PreviewRed><PreviewGreen>0</PreviewGreen><PreviewBlue>1</PreviewBlue>",
            ),
            a("FutureGeometry", ""),
            a("DeleteObjects", ""),
        ]
        .concat(),
    );
    let mut s = Session::new();
    let result = f.run(&mut s);
    assert_eq!(result["status"], "stopped");
    assert_eq!(result["stoppedAt"]["list"], "nested/prepare");
    assert_eq!(result["stoppedAt"]["position"], 2);
    assert_eq!(result["traceLength"], 13);
    assert_eq!(result["artwork"]["allArt"], true);
    assert_eq!(result["spots"], json!(["Base ink"]));
    let d = &s.doc().unwrap().doc;
    let b = d.bounds_of(&selection::all_objects(d, false), true).unwrap();
    assert!((b.width() - 30.0).abs() < 0.01 && (b.height() - 30.0).abs() < 0.01);
    assert!(!f.0.join("output.pdf").exists(), "stopped jobs must not publish partial plates");
}
#[test]
fn missing_and_recursive_calls_stop_at_the_call_without_executing_following_steps() {
    for target in ["missing", "main"] {
        let f = Fixture::new();
        f.input();
        f.pal("main", &[a("CallActionList", &format!("<Path>{target}</Path>")), a("SelectAllObjects", "")].concat());
        let v = f.run(&mut Session::new());
        assert_eq!(v["status"], "stopped");
        assert_eq!(v["traceLength"], 2);
        assert_eq!(v["stoppedAt"]["action"], "CallActionList");
        assert!(!f.0.join("output.pdf").exists());
    }
}
#[test]
fn selections_are_independent_and_locked_layers_are_excluded() {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    s.execute("shape.rectangle", &json!({"x":0,"y":0,"width":20,"height":30})).unwrap();
    let mut p = player(&mut s);
    step(&mut p, "SelectAllObjects", "").unwrap();
    step(&mut p, "RenameObjects", "<ObjectName>Red star</ObjectName>").unwrap();
    step(&mut p, "CreateLayerOnTop", "<LayerName>Destination</LayerName>").unwrap();
    assert_eq!(p.ids().unwrap().len(), 1);
    step(&mut p, "MoveObjectsToLayer", "").unwrap();
    step(&mut p, "LockUnlockLayers", "<ChangeLockState>Lock</ChangeLockState>").unwrap();
    step(&mut p, "SelectAllObjects", "").unwrap();
    assert!(p.ids().unwrap().is_empty());
    step(&mut p, "LockUnlockLayers", "<ChangeLockState>Unlock</ChangeLockState>").unwrap();
    step(&mut p,"SelectObjectsByAttributes","<SelectionMode>NewSelection</SelectionMode><Rule Type=\"NameRule\"><NameRelation>Contains</NameRelation><SearchString>star</SearchString></Rule>").unwrap();
    assert_eq!(p.ids().unwrap().len(), 1);
    step(&mut p, "SelectLayerByName", "<LayerName>Absent</LayerName>").unwrap();
    assert_eq!(p.ids().unwrap().len(), 1);
    assert!(p.layers.is_empty());
}
#[test]
fn empty_selection_is_a_noop_but_ink_creation_is_not() {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    let mut p = player(&mut s);
    for kind in ["GroupObjects", "UngroupObjects", "Copy", "PasteInFront", "MoveObjectsToLayer", "DeleteObjects"] {
        assert_eq!(step(&mut p, kind, "").unwrap(), 0);
    }
    step(
        &mut p,
        "CreateSeparation",
        "<SeparationName>New spot</SeparationName><PreviewRed>0</PreviewRed><PreviewGreen>1</PreviewGreen><PreviewBlue>0</PreviewBlue>",
    )
    .unwrap();
    step(
        &mut p,
        "ConvertToCMYK",
        "<Option>Custom</Option><CyanValue>0</CyanValue><MagentaValue>0</MagentaValue><YellowValue>0</YellowValue><BlackValue>50</BlackValue>",
    )
    .unwrap();
    assert!(!p.doc().unwrap().swatches.iter().any(|w| w.name == "New spot"));
}
#[test]
fn groups_and_conditional_branches_execute_in_order() {
    let f = Fixture::new();
    f.input();
    let rename = a("RenameObjects", "<ObjectName>Selected</ObjectName>");
    let condition = format!(
        "<Operator>All</Operator><Rule Type=\"ObjectSelectionRule\"><ObjectRelation>OneOrMoreObjectsSelection</ObjectRelation></Rule><Then>{rename}</Then><Else>{}</Else>",
        a("DeleteObjects", "")
    );
    f.pal("main", &a("Group", &format!("<Actions>{}{}</Actions>", a("SelectAllObjects", ""), a("Conditional", &condition))));
    let mut s = Session::new();
    let v = f.run(&mut s);
    assert_eq!(v["status"], "completed");
    assert_eq!(v["traceLength"], 5);
    assert!(s.doc().unwrap().selection.objects.iter().all(|id| s.doc().unwrap().doc.node(*id).unwrap().name.as_deref() == Some("Selected")));
}
#[test]
fn malformed_xml_and_unknown_actions_do_not_panic() {
    use proptest::prelude::*;
    proptest!(|(text in ".{0,4096}")| { let _result=reader::parse(&text); });
    let f = Fixture::new();
    f.input();
    f.pal("main", &a("FutureAction", ""));
    let v = f.run(&mut Session::new());
    assert_eq!(v["stoppedAt"]["action"], "FutureAction");
    assert_eq!(v["traceLength"], 2);
}

#[test]
fn ink_conversion_is_document_wide_and_applies_a_spot_tint_once() {
    use vectorcraft_color::Color;
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    s.execute("shape.rectangle", &json!({"x":0,"y":0,"width":20,"height":30})).unwrap();
    s.execute("shape.rectangle", &json!({"x":25,"y":0,"width":20,"height":30})).unwrap();
    let mut p = player(&mut s);
    step(&mut p, "SelectAllObjects", "").unwrap();
    let before = p.doc().unwrap().clone();
    assert_eq!(step(&mut p, "DeleteInk", "").unwrap(), 0);
    assert_eq!(*p.doc().unwrap(), before, "an empty ink selection must not convert RGB paint");
    step(
        &mut p,
        "CreateSeparation",
        "<SeparationName>Synthetic ink</SeparationName><PreviewRed>0</PreviewRed><PreviewGreen>0</PreviewGreen><PreviewBlue>0</PreviewBlue>",
    )
    .unwrap();
    step(&mut p, "SetFillFlatPaint", "<Percentage>50</Percentage>").unwrap();
    step(&mut p, "SetFillOverprint", "").unwrap();
    step(&mut p, "SetStrokeWidth", "<Width>0.4</Width>").unwrap();
    step(&mut p, "SetStrokeCorners", "<JoinType>Round</JoinType>").unwrap();
    let objects = p.ids().unwrap();
    p.session.doc_mut().unwrap().selection.clear();
    let affected = step(
        &mut p,
        "ConvertToCMYK",
        "<Option>Custom</Option><CyanValue>0</CyanValue><MagentaValue>0</MagentaValue><YellowValue>0</YellowValue><BlackValue>80</BlackValue>",
    )
    .unwrap();
    assert_eq!(affected, objects.len(), "conversion counts changed objects even without an object selection");
    for id in objects {
        let n = p.doc().unwrap().node(id).unwrap();
        assert_eq!(n.appearance.fill_paint().color(), Some(Color::cmyk(0.0, 0.0, 0.0, 0.4)));
        assert!(n.appearance.fill().unwrap().overprint);
        assert!((n.appearance.stroke_width() - 0.4 * 72.0 / 25.4).abs() < 1e-8);
        assert_eq!(n.appearance.stroke().unwrap().join, vectorcraft_doc::LineJoin::Round);
    }
}

#[test]
fn content_boxes_use_selection_and_relative_offsets_are_millimetres() {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    s.execute("shape.rectangle", &json!({"x":100,"y":100,"width":20,"height":30})).unwrap();
    let chosen = s.execute("shape.rectangle", &json!({"x":10,"y":20,"width":20,"height":30})).unwrap()["id"].as_u64().unwrap();
    s.execute("paint.setStroke", &json!({"ids":[chosen],"none":true})).unwrap();
    s.execute("select.set", &json!({"ids":[chosen]})).unwrap();
    let mut p = player(&mut s);
    p.boxes.insert("ArtBox".into(), Rect::new(0.0, 0.0, 100.0, 100.0));
    p.boxes.insert("MediaBox".into(), Rect::new(0.0, 0.0, 100.0, 100.0));
    step(&mut p, "SetPageBoxSizeByContent", "<PageBox>ArtBox</PageBox><Mode>FitAll</Mode><AdjustType>Both</AdjustType>").unwrap();
    assert_eq!(p.boxes["ArtBox"], Rect::new(10.0, 20.0, 30.0, 50.0));
    step(
        &mut p,
        "SetPageBoxSizeRelative",
        "<PageBox>MediaBox</PageBox><RefPageBox>ArtBox</RefPageBox><Top>3.2</Top><Left>3.2</Left><Right>3.2</Right><Bottom>3.2</Bottom>",
    )
    .unwrap();
    assert!((p.boxes["MediaBox"].width() - (20.0 + 6.4 * 72.0 / 25.4)).abs() < 1e-8);
}

#[test]
fn geometry_empty_selections_are_noops() {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    let mut p = player(&mut s);
    for kind in ["AddWhiteUnderPrint", "Choke", "Spread", "Shaper", "Rasterize", "SetTrapTags", "RemoveTrappingTags"] {
        assert_eq!(step(&mut p, kind, "").unwrap(), 0);
    }
}

fn rect(s: &mut Session, x: f64, y: f64, w: f64, h: f64) -> NodeId {
    let id = s.execute("shape.rectangle", &json!({"x":x,"y":y,"width":w,"height":h})).unwrap()["id"].as_u64().unwrap();
    s.execute("paint.setStroke", &json!({"ids":[id],"none":true})).unwrap();
    NodeId(id)
}
fn under(p: &mut Player<'_>, covered: bool, distance: f64) {
    step(p, "AddWhiteUnderPrint", &format!("<TrappingMode>Choke</TrappingMode><Distance>{distance}</Distance><JoinType>Round</JoinType><ExcludeCoveredAreas>{}</ExcludeCoveredAreas>", u8::from(covered))).unwrap();
}
fn selected_area(p: &Player<'_>) -> f64 {
    p.ids()
        .unwrap()
        .iter()
        .map(|id| {
            let n = p.doc().unwrap().node(*id).unwrap();
            let (path, rule) = super::super::pathops::node_path(n).unwrap();
            vectorcraft_pathops::area(&path, rule)
        })
        .sum()
}
#[test]
fn underprint_excludes_only_art_above_and_selected_objects_do_not_cover_each_other() {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    let back = rect(&mut s, 0., 0., 20., 20.);
    let front = rect(&mut s, 10., 0., 20., 20.);
    s.doc_mut().unwrap().selection.set([back]);
    let mut p = player(&mut s);
    under(&mut p, true, 0.);
    assert!((selected_area(&p) - 200.).abs() < 0.01);
    assert_eq!(
        p.doc().unwrap().node(p.ids().unwrap()[0]).unwrap().appearance.fill_paint(),
        vectorcraft_color::Paint::Solid { color: vectorcraft_color::Color::rgb(1., 1., 1.), swatch: Some("Opaque White".into()), tint: 1. }
    );
    step(&mut p, "DeleteObjects", "").unwrap();
    p.session.doc_mut().unwrap().selection.set([front]);
    under(&mut p, true, 0.);
    assert!((selected_area(&p) - 400.).abs() < 0.01);
    step(&mut p, "DeleteObjects", "").unwrap();
    p.session.doc_mut().unwrap().selection.set([back, front]);
    under(&mut p, true, 0.);
    assert!((selected_area(&p) - 600.).abs() < 0.01);
}
#[test]
fn underprint_chokes_union_once_and_selects_only_new_art() {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    let a = rect(&mut s, 0., 0., 10., 10.);
    let b = rect(&mut s, 10., 0., 10., 10.);
    s.doc_mut().unwrap().selection.set([a, b]);
    let mut p = player(&mut s);
    under(&mut p, false, 25.4 / 72.);
    assert!((selected_area(&p) - 144.).abs() < 0.01);
    assert_eq!(p.ids().unwrap().len(), 1);
    assert!(p.doc().unwrap().node(a).is_some());
    step(&mut p, "Choke", "<Distance>100</Distance><JoinType>Round</JoinType><SpreadTarget>OriginalPath</SpreadTarget>").unwrap();
    assert!(p.ids().unwrap().is_empty());
}

#[test]
fn underprint_small_stack_retains_intermediate_precision_cleanup() {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    let back = rect(&mut s, 0., 0., 100., 10.);
    rect(&mut s, 0.005, 0., 100., 10.);
    let front = rect(&mut s, 0.005, 0., 1., 10.);
    s.doc_mut().unwrap().selection.set([back, front]);
    let mut p = player(&mut s);
    under(&mut p, true, 0.);
    let n = p.doc().unwrap().node(p.ids().unwrap()[0]).unwrap();
    let (path, _) = super::super::pathops::node_path(n).unwrap();
    // The thin remainder of the back object is removed before the front joins it.
    assert!((path.bounds().unwrap().x0 - 0.005).abs() < 1e-8);
}

#[test]
fn underprint_ten_thousand_selected_shapes_finishes_within_budget() {
    use vectorcraft_color::{Color, Paint};
    use vectorcraft_doc::{Appearance, Node};
    use vectorcraft_geom::{PathData, Rect, Shape};
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    s.edit("synthetic art", |d, sel| {
        let layer = d.add_layer(Some("Artwork"));
        let mut ids = Vec::new();
        for i in 0..10_000 {
            let x = f64::from(i % 100) * 3.;
            let y = f64::from(i / 100) * 3.;
            let path = PathData::from_bezpath(&Rect::new(x, y, x + 2., y + 2.).to_path(0.01));
            let mut app = Appearance::default();
            app.set_fill(Paint::solid(Color::cmyk(1., 0., 0., 0.)));
            app.set_stroke(Paint::None);
            let id = d.alloc_id();
            d.insert(Some(layer), usize::MAX, Node::path(id, path, app))?;
            ids.push(id);
        }
        sel.set(ids);
        Ok(())
    })
    .unwrap();
    let start = std::time::Instant::now();
    let mut p = player(&mut s);
    under(&mut p, true, 0.);
    let elapsed = start.elapsed();
    let n = p.doc().unwrap().node(p.ids().unwrap()[0]).unwrap();
    let (path, _) = super::super::pathops::node_path(n).unwrap();
    assert_eq!(path.subpaths.len(), 10_000);
    assert!(elapsed.as_secs_f64() < 60., "10k underprint took {elapsed:?}");
}
#[test]
fn shaper_copy_and_empty_difference_are_valid() {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    let a = rect(&mut s, 0., 0., 10., 10.);
    let b = rect(&mut s, 0., 0., 10., 10.);
    s.doc_mut().unwrap().selection.set([a, b]);
    let mut p = player(&mut s);
    step(&mut p, "Shaper", "<Operation>Unite</Operation><WorkOnCopy>1</WorkOnCopy>").unwrap();
    assert!((selected_area(&p) - 100.).abs() < 0.01);
    assert!(p.doc().unwrap().node(a).is_some());
    p.session.doc_mut().unwrap().selection.set([a, b]);
    step(&mut p, "Shaper", "<Operation>MinusFront</Operation><WorkOnCopy>0</WorkOnCopy>").unwrap();
    assert!(p.ids().unwrap().is_empty());
    assert!(p.doc().unwrap().node(a).is_none());
}

#[test]
fn called_lists_can_contain_decimal_distances() {
    let f = Fixture::new();
    f.input();
    f.pal("main", &a("CallActionList", "<Path>Spread 0.4</Path>"));
    f.pal("Spread 0.4", &a("ShowMessage", "<Message>Done</Message>"));
    assert_eq!(f.run(&mut Session::new())["status"], "completed");
}

#[test]
fn clean_removes_fully_clipped_nonpath_content() {
    use vectorcraft_doc::Node;
    use vectorcraft_geom::PathData;
    let f = Fixture::new();
    f.input();
    let mut s = Session::new();
    s.execute("document.open", &json!({"path":f.0.join("input.pdf")})).unwrap();
    let d = std::sync::Arc::make_mut(&mut s.doc_mut().unwrap().doc);
    let mesh = vectorcraft_doc::GradientMesh::from_surface(1, 1, &|u, v| vectorcraft_geom::Point::new(60. + u * 10., 60. + v * 10.), &|_, _| {
        vectorcraft_color::Color::WHITE
    });
    let mut n = Node::path(d.alloc_id(), PathData::default(), Default::default());
    n.kind = vectorcraft_doc::NodeKind::Mesh(mesh);
    // The clip's real geometry is disjoint; an unsupported object fully outside it is harmless.
    let mask = Node::path(d.alloc_id(), vectorcraft_geom::shapes::rectangle(Rect::new(0., 0., 10., 10.)), Default::default());
    let mut group = Node::group(d.alloc_id(), vec![std::sync::Arc::new(mask), std::sync::Arc::new(n)]);
    group.set_clips(true);
    let actions = reader::parse(&format!("<ActionList Version=\"2.0\">{}</ActionList>", a("CleanObjects", "<ExecuteClip>1</ExecuteClip>"))).unwrap();
    let options = clean::Options::read(&actions[0].params).unwrap();
    let (out, _) = clean::clean(d, group, &options, &[]).unwrap();
    assert!(out.unwrap().children().unwrap().is_empty());
}

#[test]
fn background_prep_is_traced_and_selected_ink_tint_and_overprint_follow_pal() {
    let f = Fixture::new();
    let mut page = PdfPage::new(100., 100., "/BG cs 1 scn 0 0 100 100 re f 0 1 0 0 k 20 20 40 40 re f");
    page.resources =
        "/ColorSpace << /BG [/Separation /SupaBG /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 1 0 0] /N 1 >>] >>".into();
    std::fs::write(f.0.join("input.pdf"), pdf(&[page], None)).unwrap();
    f.pal("main", &[
        a("SelectAllObjects", ""),
        a("CreateSeparation", "<SeparationName>Base</SeparationName><PreviewRed>1</PreviewRed><PreviewGreen>0</PreviewGreen><PreviewBlue>1</PreviewBlue>"),
        a("AddWhiteUnderPrint", "<TrappingMode>Spread</TrappingMode><Distance>0.2</Distance><JoinType>Miter</JoinType><MiterLimit>6</MiterLimit><UseSelectedSeparation>1</UseSelectedSeparation>"),
        a("SetFillFlatPaint", "<Percentage>40</Percentage>"),
        a("SetFillOverprint", ""),
    ].concat());
    let mut s = Session::new();
    let result = f.run(&mut s);
    assert_eq!(result["status"], "completed");
    assert_eq!(result["trace"][0]["objectsAffected"], 1);
    assert_eq!(result["trace"][0]["action"], "DeleteBackground");
    let p = player(&mut s);
    let node = p.doc().unwrap().node(p.ids().unwrap()[0]).unwrap();
    assert!(matches!(node.appearance.fill_paint(),vectorcraft_color::Paint::Solid {swatch:Some(ref name),tint,..} if name=="Base" && tint==0.4));
    assert!(node.appearance.fill().unwrap().overprint);
    assert!((selected_area(&p) - (40. + 0.4 * 72. / 25.4_f64).powi(2)).abs() < 0.01);
}

#[test]
fn minimum_page_shape_uses_physical_size_about_its_center() {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    rect(&mut s, 10., 20., 100., 50.);
    let mut p = player(&mut s);
    step(&mut p, "CreateShapeOnSelection", "<Shape>Rectangle</Shape><CornerRadius>0</CornerRadius>").unwrap();
    step(&mut p,"SetObjectsSize","<Mode>Relative</Mode><Anchor>Center</Anchor><Width>19</Width><Height>19</Height><UseWidth>1</UseWidth><UseHeight>1</UseHeight><IncludeGradientsAndPatterns>1</IncludeGradientsAndPatterns><IncludeStrokeWeights>1</IncludeStrokeWeights><IncludeOpacityMasks>1</IncludeOpacityMasks>").unwrap();
    let b = p.doc().unwrap().bounds_of(&p.ids().unwrap(), true).unwrap();
    assert!((b.width() - 19. * 72. / 25.4).abs() < 1e-8);
    assert!((b.height() - 19. * 72. / 25.4).abs() < 1e-8);
    assert!((b.center() - vectorcraft_geom::Point::new(60., 45.)).hypot() < 1e-10);
}

fn trap_ticket(external: &str) -> String {
    format!(
        "<TrapType>Trapping</TrapType><ExternalTrap>{external}</ExternalTrap><ExternalPullback>Never</ExternalPullback><ExternalDirection>Outwards</ExternalDirection><ExternalDistance Mode=\"Set\">0.2</ExternalDistance><InternalTrap>Never</InternalTrap><InternalPullback>Never</InternalPullback><InternalDistance Mode=\"Set\">0</InternalDistance>"
    )
}
fn trap_art() -> Session {
    let mut s = Session::new();
    s.execute("file.new", &json!({"width":30,"height":30})).unwrap();
    let id = rect(&mut s, 5., 5., 20., 20.);
    let d = std::sync::Arc::make_mut(&mut s.doc_mut().unwrap().doc);
    std::sync::Arc::make_mut(&mut d.layers[0]).name = Some("Artwork".into());
    d.node_mut(id).unwrap().appearance.set_fill(vectorcraft_color::Paint::solid(vectorcraft_color::Color::cmyk(0.2, 0.8, 0., 0.)));
    s
}
#[test]
fn trap_task_pal_cleanup_equals_standalone() {
    let make = || {
        let mut s = trap_art();
        let d = std::sync::Arc::make_mut(&mut s.doc_mut().unwrap().doc);
        let color = vectorcraft_color::Color::cmyk(0.1, 0.6, 0.2, 0.0);
        d.swatches.push(vectorcraft_color::Swatch {
            name: "Synthetic spot".into(),
            paint: vectorcraft_color::Paint::solid(color),
            global: true,
            spot: true,
        });
        let paint = vectorcraft_color::Paint::Solid { color, swatch: Some("Synthetic spot".into()), tint: 0.73 };
        let n = vectorcraft_doc::Node::path(
            d.alloc_id(),
            vectorcraft_geom::shapes::ellipse(Rect::new(18., 3., 27., 12.)),
            vectorcraft_doc::Appearance::basic(paint, vectorcraft_color::Paint::None, 0.0),
        );
        d.insert(Some(d.layers[0].id), usize::MAX, n).unwrap();
        s
    };
    let mut direct = make();
    direct.execute("sep.trap", &json!({"blackThreshold":0.7})).unwrap();
    let mut s = make();
    let mut p = player(&mut s);
    step(&mut p, "SelectAllObjects", "").unwrap();
    step(&mut p, "SetTrapTags", &trap_ticket("Always")).unwrap();
    step(&mut p, "TrapTask", "").unwrap();
    assert_eq!(p.doc().unwrap().layers.last().unwrap().name.as_deref(), Some("Trap Layer 1"));
    step(&mut p, "SelectLayerByName", "<LayerName>Trap Layer 1</LayerName>").unwrap();
    step(&mut p, "RenameLayers", "<LayerName>Auto Trap-1</LayerName>").unwrap();
    step(&mut p, "SelectObjectsInLayer", "").unwrap();
    step(&mut p, "Rasterize", "<Resolution>600</Resolution><AntiAliasing>None</AntiAliasing><ConsiderBackground>0</ConsiderBackground>").unwrap();
    step(&mut p, "SelectObjectsByAttributes", "<Rule Type=\"ObjectTypeRule\"><ObjectType>Path</ObjectType><Option>Clipping</Option></Rule>").unwrap();
    assert_eq!(p.ids().unwrap().len(), 1);
    for action in ["Spread", "Choke"] {
        step(
            &mut p,
            action,
            "<Distance>0.01</Distance><JoinType>Miter</JoinType><MiterLimit>4</MiterLimit><SpreadTarget>OriginalPath</SpreadTarget>",
        )
        .unwrap();
    }
    assert!(matches!(p.doc().unwrap().node(p.ids().unwrap()[0]).unwrap().kind, vectorcraft_doc::NodeKind::Path { clipping: true, .. }));
    let layer = |d: &Document| {
        let mut d = d.clone();
        d.layers.retain(|n| n.name.as_deref() == Some("Auto Trap-1"));
        d.drop_lookup_cache();
        d
    };
    let a = layer(&direct.doc().unwrap().doc);
    let b = layer(p.doc().unwrap());
    for plate in ["Cyan", "Magenta", "Yellow", "Black", "Synthetic spot"] {
        let options = vectorcraft_render::RenderOptions {
            background: Some([255; 4]),
            artboards: false,
            anti_alias: vectorcraft_render::AntiAlias::None,
            smooth_images: false,
            image_mips: false,
            overprint_preview: true,
            proof: Some(vectorcraft_render::proof::ProofSetup { separations: Some(vec![plate.into()]), ..Default::default() }),
            ..Default::default()
        };
        let mut renderer = vectorcraft_render::Renderer::new();
        assert_eq!(
            renderer.render_region_with(&a, Rect::new(0., 0., 30., 30.), 600. / 72., &options).pixels,
            renderer.render_region_with(&b, Rect::new(0., 0., 30., 30.), 600. / 72., &options).pixels,
            "{plate}"
        );
    }
}
#[test]
fn trap_tags_inherit_remove_and_roundtrip_without_overriding_black() {
    let mut s = trap_art();
    let mut p = player(&mut s);
    step(&mut p, "SelectAllObjects", "").unwrap();
    step(&mut p, "GroupObjects", "").unwrap();
    step(&mut p, "SetTrapTags", &trap_ticket("Never")).unwrap();
    let wire = serde_json::to_vec(p.doc().unwrap()).unwrap();
    let read: Document = serde_json::from_slice(&wire).unwrap();
    assert_eq!(read.node(p.ids().unwrap()[0]).unwrap().extras().trap_external, Some(false));
    assert_eq!(step(&mut p, "TrapTask", "").unwrap(), 0);
    step(&mut p, "RemoveTrappingTags", "<AllLevels>1</AllLevels>").unwrap();
    assert_eq!(step(&mut p, "TrapTask", "").unwrap(), 1);
    let mut black = trap_art();
    let d = std::sync::Arc::make_mut(&mut black.doc_mut().unwrap().doc);
    vectorcraft_doc::inks::visit_node_colors(std::sync::Arc::make_mut(&mut d.layers[0]), &mut |c, _, _| {
        *c = vectorcraft_color::Color::cmyk(0.8, 0.7, 0.6, 0.7)
    });
    let mut p = player(&mut black);
    step(&mut p, "SelectAllObjects", "").unwrap();
    step(&mut p, "SetTrapTags", &trap_ticket("Always")).unwrap();
    assert_eq!(step(&mut p, "TrapTask", "").unwrap(), 0);
}

#[test]
fn no_trap_intent_selects_prepared_ids_and_splits_rectangles_without_repainting() {
    use vectorcraft_color::{Color, Paint};
    use vectorcraft_doc::{Appearance, Node};
    use vectorcraft_geom::shapes;
    let mut d = Document::new(100.0, 100.0);
    let layer = d.layers[0].id;
    d.node_mut(layer).unwrap().name = Some("Artwork".into());
    let mut ids = Vec::new();
    for x in [10.0, 50.0] {
        let n = Node::path(
            d.alloc_id(),
            shapes::rectangle(Rect::new(x, 10.0, x + 20.0, 30.0)),
            Appearance::basic(Paint::solid(Color::cmyk(0.4, 0.2, 0.0, 0.0)), Paint::None, 0.0),
        );
        ids.push(n.id);
        d.insert(Some(layer), usize::MAX, n).unwrap();
    }
    let mut s = Session::new();
    s.add_document(d, None);
    let mut p = player(&mut s);
    let mm = 25.4 / 72.0;
    let intent = intents::read(
        &json!({"intents":{"noTrap":{"objectIds":[ids[1]],"rectangles":[{"x":10.0*mm,"y":10.0*mm,"width":10.0*mm,"height":20.0*mm}]}}}),
    )
    .unwrap()
    .no_trap
    .unwrap();
    p.apply_no_trap(intent).unwrap();
    let selected = p.ids().unwrap();
    assert_eq!(selected.len(), 2);
    assert!(selected.contains(&ids[1]));
    let doc = p.doc().unwrap();
    let original = doc.node(ids[0]).unwrap();
    assert!((original.visual_bounds().unwrap().width() - 10.0).abs() < 1e-6);
    let split = doc.node(*selected.iter().find(|&&id| id != ids[1]).unwrap()).unwrap();
    assert_eq!(split.appearance, original.appearance);
    assert!((split.visual_bounds().unwrap().x1 - 20.0).abs() < 1e-6);
    assert_eq!(p.trace.last().unwrap()["action"], "SelectNoTrap");
}

#[test]
fn no_trap_intent_rejects_invalid_regions_and_missing_ticket() {
    for value in [json!(null), json!({"rectangles":[{"x":0,"y":0,"width":-1,"height":5}]}), json!({"objectIds":[-1]}), json!({"rectangle":[]})] {
        assert!(intents::read(&json!({"intents":{"noTrap":value}})).is_err());
    }
    let f = Fixture::new();
    f.input();
    f.pal("main", "");
    let error = Session::new()
        .execute(
            "sep.runActionList",
            &json!({"root":f.0,"lists":["main"],"input":f.0.join("input.pdf"),"output":f.0.join("output.pdf"),"intents":{"noTrap":{"objectIds":[]}}}),
        )
        .unwrap_err();
    assert!(error.to_string().contains("immediately followed by TrapTask"));
}

#[test]
fn mixed_mask_blocks_trap_and_zero_choke_whp_but_retains_light_underprint() {
    use vectorcraft_color::{Color, Paint};
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    let left = rect(&mut s, 10., 10., 20., 20.);
    let right = rect(&mut s, 50., 10., 20., 20.);
    s.edit("Trap", |d, _| {
        d.layers.iter_mut().for_each(|l| std::sync::Arc::make_mut(l).name = Some("Artwork".into()));
        for id in [left, right] {
            d.node_mut(id).unwrap().appearance.set_fill(Paint::solid(Color::cmyk(1., 0., 0., 0.)));
        }
        Ok(())
    })
    .unwrap();
    let mut p = player(&mut s);
    p.apply_no_trap(intents::read(&json!({"intents":{"noTrap":{"objectIds":[left]}}})).unwrap().no_trap.unwrap()).unwrap();
    step(&mut p, "RenameObjects", "<ObjectName>LIGHT</ObjectName>").unwrap();
    under(&mut p, true, 0.);
    let mask = p.ids().unwrap()[0];
    step(&mut p, "SetFillFlatPaint", "<Percentage>0</Percentage>").unwrap();
    step(&mut p, "RenameObjects", "<ObjectName>LIGHT NO BLEED</ObjectName>").unwrap();
    step(&mut p, "SelectLayerByName", "<LayerName>Artwork</LayerName><Mode>NewSelection</Mode>").unwrap();
    step(&mut p, "MoveObjectsToLayer", "").unwrap();
    p.trap_task().unwrap();
    let trap = p.doc().unwrap().layers.iter().find(|n| n.name.as_deref() == Some("Trap Layer 1")).unwrap();
    assert!(trap.visual_bounds().unwrap().x0 > 49., "only the right object's band exists");
    // WHP converts the temporary white to K50 before its Back/Front operations.
    p.session
        .edit("Fill", |d, _| {
            d.node_mut(mask).unwrap().appearance.set_fill(Paint::solid(Color::cmyk(0., 0., 0., 0.5)));
            Ok(())
        })
        .unwrap();
    p.session.doc_mut().unwrap().selection.set([left]);
    under(&mut p, true, 0.);
    assert!(p.ids().unwrap().is_empty(), "no zero-choke dark WHP under the front mask");
    p.session.doc_mut().unwrap().selection.set([mask]);
    step(&mut p, "ArrangeObjects", "<MoveDirection>Back</MoveDirection>").unwrap();
    p.session.doc_mut().unwrap().selection.set([left]);
    under(&mut p, true, 0.);
    assert!((selected_area(&p) - 400.).abs() < 0.01, "a back mask does not cover selected art");
    step(&mut p, "DeleteObjects", "").unwrap();
    p.session.doc_mut().unwrap().selection.set([mask]);
    step(&mut p, "ArrangeObjects", "<MoveDirection>Front</MoveDirection>").unwrap();
    p.session.doc_mut().unwrap().selection.set([left, mask]);
    under(&mut p, false, 0.2);
    let expected = (20. - 2. * 0.2 * 72. / 25.4_f64).powi(2);
    assert!((selected_area(&p) - expected).abs() < 0.01, "light-style choked WHP is retained");
}

#[test]
fn runner_applies_intent_after_preparation_immediately_before_explicit_ticket() {
    let f = Fixture::new();
    f.input();
    f.pal(
        "prepare",
        &[
            a("SelectAllObjects", ""),
            a("CleanObjects", "<ExecuteClip>1</ExecuteClip>"),
            a("CreateLayerOnTop", "<LayerName>Artwork</LayerName>"),
            a("MoveObjectsToLayer", ""),
            a("SelectAllObjects", ""),
            a("InvertObjectSelection", ""),
        ]
        .join(""),
    );
    f.pal("Trapping_Supacolour_Standard (NEW)", &a("RenameObjects", "<ObjectName>LIGHT</ObjectName>"));
    let mut s = Session::new();
    let r = s.execute("sep.runActionList", &json!({"root":f.0,"lists":["prepare","Trapping_Supacolour_Standard (NEW)","TrapTask"],"input":f.0.join("input.pdf"),"output":f.0.join("output.pdf"),"intents":{"noTrap":{"rectangles":[{"x":0,"y":0,"width":40,"height":40}]}}})).unwrap();
    assert_eq!(r["status"], "completed", "{r}");
    let trace = r["trace"].as_array().unwrap();
    let at = trace.iter().position(|t| t["action"] == "SelectNoTrap").unwrap();
    assert_eq!(trace[at]["objectsAffected"], 1);
    assert_eq!(trace[at + 1]["action"], "RenameObjects");
    assert_eq!(trace[at + 1]["objectsAffected"], 1);
    assert_eq!(trace[at + 2]["action"], "TrapTask");
}

#[test]
fn metallic_intents_are_scoped_and_restore_selection() {
    let f = Fixture::new();
    f.pal("metal", &[
        a("SelectObjectsByAttributes", "<SelectionMode>AddToSelection</SelectionMode><Rule Type=\"SeparationNameRule\"><SeparationName>M-2</SeparationName><SeparationRelation>Using</SeparationRelation></Rule>"),
        a("RemoveFill", ""),
        a("CreateSeparation", "<SeparationName>M-2</SeparationName><PreviewRed>0.8</PreviewRed><PreviewGreen>0.7</PreviewGreen><PreviewBlue>0.3</PreviewBlue>"),
        a("SetFillFlatPaint", "<Percentage>100</Percentage>"),
        a("RenameObjects", "<ObjectName>M-2</ObjectName>"),
    ].concat());
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    s.execute("shape.rectangle", &json!({"x":10,"y":10,"width":20,"height":20})).unwrap();
    let first = s.doc().unwrap().selection.objects[0];
    s.execute("shape.rectangle", &json!({"x":50,"y":10,"width":20,"height":20})).unwrap();
    let second = s.doc().unwrap().selection.objects[0];
    let mut p = player(&mut s);
    p.root = f.0.canonicalize().unwrap();
    step(
        &mut p,
        "CreateSeparation",
        "<SeparationName>M-2</SeparationName><PreviewRed>0.8</PreviewRed><PreviewGreen>0.7</PreviewGreen><PreviewBlue>0.3</PreviewBlue>",
    )
    .unwrap();
    step(&mut p, "SetFillFlatPaint", "<Percentage>50</Percentage>").unwrap();
    let intents = intents::read(&json!({"intents":{"metallic":[{"objects":{"ids":[first.0]},"list":"metal"}]}})).unwrap();
    p.apply_intents(&intents).unwrap();
    assert_eq!(p.ids().unwrap(), vec![second]);
    assert_eq!(p.doc().unwrap().node(first).unwrap().name.as_deref(), Some("M-2"));
    assert_ne!(p.doc().unwrap().node(second).unwrap().name.as_deref(), Some("M-2"));
    assert!(matches!(p.doc().unwrap().node(second).unwrap().appearance.fill_paint(),vectorcraft_color::Paint::Solid{tint,..} if tint == 0.5));
    let by_spot = intents::read(&json!({"intents":{"metallic":[{"objects":{"spot":"M-2"},"list":"metal"}]}})).unwrap();
    p.apply_intents(&by_spot).unwrap();
    assert_eq!(p.doc().unwrap().node(second).unwrap().name.as_deref(), Some("M-2"));
    let by_rect = intents::read(&json!({"intents":{"metallic":[{"objects":{"rectMm":[0,0,12,12]},"list":"metal"}]}})).unwrap();
    p.apply_intents(&by_rect).unwrap();
    let bad = intents::read(&json!({"intents":{"metallic":[{"objects":{"ids":[999999]},"list":"metal"}]}})).unwrap();
    assert!(p.apply_intents(&bad).is_err());
    assert!(intents::read(&json!({"intents":{"metallic":[{"objects":{"spot":"ink","typo":true},"list":"metal"}]}})).is_err());
}

#[test]
fn clip_on_shape_preserves_layers_unselected_siblings_and_overprint() {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    let mut ids = vec![];
    for x in [0, 10, 20] {
        s.execute("shape.rectangle", &json!({"x":x,"y":0,"width":10,"height":10})).unwrap();
        ids.push(s.doc().unwrap().selection.objects[0]);
    }
    let mut p = player(&mut s);
    step(&mut p, "CreateLayerOnTop", "<LayerName>Upper</LayerName>").unwrap();
    p.call("shape.rectangle", json!({"x":0,"y":0,"width":25,"height":5})).unwrap();
    let top = p.ids().unwrap()[0];
    step(&mut p, "MoveObjectsToLayer", "").unwrap();
    p.session.doc_mut().unwrap().selection.set([ids[0], ids[2]]);
    step(&mut p, "SetFillOverprint", "").unwrap();
    p.session.doc_mut().unwrap().selection.set([ids[0], ids[2], top]);
    step(&mut p, "ClipOnShape", "").unwrap();
    assert_eq!(p.ids().unwrap().len(), 2);
    assert!(p.doc().unwrap().node(top).is_none());
    assert_eq!(p.doc().unwrap().layers.len(), 2);
    for id in p.ids().unwrap() {
        let group = p.doc().unwrap().node(id).unwrap();
        assert!(group.clips());
        let cs = group.children().unwrap();
        assert!(cs[0].appearance.fill_paint().is_none());
        assert!(cs[1].appearance.fill().unwrap().overprint);
    }
    let parent = p.doc().unwrap().parent_of(ids[1]).unwrap();
    assert_eq!(p.doc().unwrap().node(parent).unwrap().children().unwrap()[1].id, ids[1]);
}

#[test]
fn remove_fill_preserves_stroke_and_silver_underprint() {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    s.execute("shape.rectangle", &json!({"x":0,"y":0,"width":20,"height":20})).unwrap();
    let mut p = player(&mut s);
    step(&mut p, "SelectSeparationByName", "<SeparationName>Black</SeparationName>").unwrap();
    step(&mut p, "SetStrokeFlatPaint", "<Percentage>100</Percentage>").unwrap();
    step(&mut p, "SetStrokeWidth", "<Width>0.1</Width>").unwrap();
    step(&mut p, "RemoveFill", "").unwrap();
    let id = p.ids().unwrap()[0];
    let n = p.doc().unwrap().node(id).unwrap();
    assert!(n.appearance.fill().is_none());
    assert!(!n.appearance.stroke_paint().is_none());
    step(
        &mut p,
        "CreateSeparation",
        "<SeparationName>M-2</SeparationName><PreviewRed>0.8</PreviewRed><PreviewGreen>0.7</PreviewGreen><PreviewBlue>0.3</PreviewBlue>",
    )
    .unwrap();
    step(&mut p, "SetFillFlatPaint", "<Percentage>100</Percentage>").unwrap();
    step(&mut p,"AddWhiteUnderPrint","<TrappingMode>Spread</TrappingMode><Distance>0</Distance><JoinType>Round</JoinType><ExcludeCoveredAreas>1</ExcludeCoveredAreas><ExcludeWhiteObjects>1</ExcludeWhiteObjects>").unwrap();
    step(
        &mut p,
        "CreateSeparation",
        "<SeparationName>Silver</SeparationName><PreviewRed>0.6</PreviewRed><PreviewGreen>0.6</PreviewGreen><PreviewBlue>0.6</PreviewBlue>",
    )
    .unwrap();
    step(&mut p, "SetFillFlatPaint", "<Percentage>100</Percentage>").unwrap();
    step(&mut p, "SetFillOverprint", "").unwrap();
    let n = p.doc().unwrap().node(p.ids().unwrap()[0]).unwrap();
    assert!(n.appearance.fill().unwrap().overprint);
    assert!(matches!(n.appearance.fill_paint(),vectorcraft_color::Paint::Solid{swatch:Some(ref name),..} if name=="Silver"));
}

#[test]
fn silver_underprint_is_not_erased_by_overprinting_white_layers() {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    let back = rect(&mut s, 0., 0., 20., 20.);
    let front = rect(&mut s, 0., 0., 20., 20.);
    let mut p = player(&mut s);
    p.session.doc_mut().unwrap().selection.set([front]);
    step(
        &mut p,
        "CreateSeparation",
        "<SeparationName>WHP</SeparationName><PreviewRed>1</PreviewRed><PreviewGreen>0.5</PreviewGreen><PreviewBlue>1</PreviewBlue>",
    )
    .unwrap();
    step(&mut p, "SetFillFlatPaint", "<Percentage>100</Percentage>").unwrap();
    step(&mut p, "SetFillOverprint", "").unwrap();
    p.session.doc_mut().unwrap().selection.set([back]);
    under(&mut p, true, 0.);
    assert!((selected_area(&p) - 400.).abs() < 0.01);
    step(&mut p, "DeleteObjects", "").unwrap();
    p.session.doc_mut().unwrap().selection.set([front]);
    step(&mut p, "SetFillKnockout", "").unwrap();
    p.session.doc_mut().unwrap().selection.set([back]);
    under(&mut p, true, 0.);
    assert!(p.ids().unwrap().is_empty());
}

#[test]
fn move_consolidates_duplicate_named_layers_into_the_topmost_selected_layer() {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    let first = rect(&mut s, 0., 0., 10., 10.);
    let mut p = player(&mut s);
    step(&mut p, "CreateLayerOnTop", "<LayerName>Lower</LayerName>").unwrap();
    step(&mut p, "MoveObjectsToLayer", "").unwrap();
    let lower = p.layers[0];
    step(&mut p, "CreateLayerOnTop", "<LayerName>Upper</LayerName>").unwrap();
    let upper = p.layers[0];
    p.layers = vec![upper, lower];
    step(&mut p, "RenameLayers", "<LayerName>Silver layer</LayerName>").unwrap();
    step(&mut p, "MoveObjectsToLayer", "").unwrap();
    assert_eq!(p.doc().unwrap().parent_of(first), Some(upper));
    p.layers.clear();
    assert_eq!(p.active_layer().unwrap(), upper);
}

#[test]
fn combined_intents_apply_metallic_after_preparation_before_no_trap_ticket() {
    for ticket in [
        "Trapping_Supacolour_Standard",
        "(2b)/Trapping_Supacolour_Standard.pal",
        "Trapping_Supacolour_Standard (NEW)",
        "(2b)/Trapping_Supacolour_Standard (NEW).pal",
    ] {
        let f = Fixture::new();
        f.input();
        f.pal(
            "prepare",
            &[
                a("SelectAllObjects", ""),
                a("CleanObjects", "<ExecuteClip>1</ExecuteClip>"),
                a("CreateLayerOnTop", "<LayerName>Artwork</LayerName>"),
                a("MoveObjectsToLayer", ""),
                a("SelectAllObjects", ""),
                a("InvertObjectSelection", ""),
            ]
            .concat(),
        );
        f.pal(
            "metal",
            &[
                a("RemoveFill", ""),
                a(
                    "CreateSeparation",
                    "<SeparationName>M-2</SeparationName><PreviewRed>0.8</PreviewRed><PreviewGreen>0.7</PreviewGreen><PreviewBlue>0.3</PreviewBlue>",
                ),
                a("SetFillFlatPaint", "<Percentage>100</Percentage>"),
                a("RenameObjects", "<ObjectName>M-2</ObjectName>"),
            ]
            .concat(),
        );
        f.pal(ticket.trim_end_matches(".pal"), &a("RenameObjects", "<ObjectName>LIGHT</ObjectName>"));
        let mut s = Session::new();
        let result = s
            .execute(
                "sep.runActionList",
                &json!({
                    "root": f.0, "lists": ["prepare", ticket, "TrapTask"],
                    "input": f.0.join("input.pdf"), "output": f.0.join("output.pdf"),
                    "intents": {
                        "metallic": [{"objects": {"rectMm": [0, 0, 40, 40]}, "list": "metal"}],
                        "noTrap": {"rectangles": [{"x": 0, "y": 0, "width": 40, "height": 40}]}
                    }
                }),
            )
            .unwrap();
        assert_eq!(result["status"], "completed", "{result}");
        let trace = result["trace"].as_array().unwrap();
        let metal = trace.iter().position(|t| t["action"] == "SelectIntent").unwrap();
        let no_trap = trace.iter().position(|t| t["action"] == "SelectNoTrap").unwrap();
        assert_eq!(trace[metal - 1]["list"], "prepare");
        assert_eq!(trace[metal]["objects"].as_array().unwrap().len(), 1);
        assert_eq!(trace[no_trap - 1]["list"], "metal");
        assert_eq!(trace[no_trap]["objectsAffected"], 1);
        assert_eq!(trace[no_trap + 1]["action"], "RenameObjects");
        assert_eq!(trace[no_trap + 2]["action"], "TrapTask");
        let d = &s.doc().unwrap().doc;
        let light = selection::all_objects(d, false).into_iter().filter_map(|id| d.node(id)).find(|n| n.name.as_deref() == Some("LIGHT")).unwrap();
        assert!(matches!(light.appearance.fill_paint(), vectorcraft_color::Paint::Solid {swatch: Some(ref name), ..} if name == "M-2"));
    }
}

#[test]
fn shared_intent_parser_rejects_unknown_keys_and_each_invalid_payload() {
    assert!(intents::read(&json!({})).unwrap().no_trap.is_none());
    assert!(intents::read(&json!({"intents": {}})).unwrap().no_trap.is_none());
    for intents in [
        json!(null),
        json!({"noTrap": {}, "metallic": [], "unknown": true}),
        json!({"noTrap": null, "metallic": []}),
        json!({"noTrap": {}, "metallic": null}),
        json!({"noTrap": {}, "metallic": [{"objects": {"objectIds": [1]}, "list": "metal"}]}),
        json!({"noTrap": {"ids": [1]}, "metallic": []}),
    ] {
        assert!(intents::read(&json!({"intents": intents})).is_err(), "{intents}");
    }
}

#[test]
fn crosshair_parameter_rectangle_uses_artbox_mm_and_rejects_unsupported_variants() {
    let mut s = Session::new();
    s.add_document(Document::new(100.0, 100.0), None);
    let mut p = player(&mut s);
    p.boxes.insert("ArtBox".into(), Rect::new(30.0, 40.0, 90.0, 80.0));
    let shape = "<Shape>Rectangle</Shape><Anchor>Center</Anchor><Width>19</Width><Height>19</Height><CornerRadius>0</CornerRadius><LinkEnabled>0</LinkEnabled>";
    assert!(step(&mut p, "CreateShapeByParameters", shape).is_err());
    step(
        &mut p,
        "PositionCrosshair",
        "<Mode>RelativePosition</Mode><Reference>ArtBox</Reference><Anchor>Center</Anchor><XOffset>2</XOffset><YOffset>-3</YOffset>",
    )
    .unwrap();
    step(&mut p, "CreateShapeByParameters", shape).unwrap();
    let ids = p.ids().unwrap();
    let bounds = p.doc().unwrap().bounds_of(&ids, true).unwrap();
    let mm = 72.0 / 25.4;
    assert!((bounds.width() - 19.0 * mm).abs() < 1e-9);
    assert!((bounds.height() - 19.0 * mm).abs() < 1e-9);
    assert!((bounds.center().x - (60.0 + 2.0 * mm)).abs() < 1e-9);
    assert!((bounds.center().y - (60.0 - 3.0 * mm)).abs() < 1e-9);
    for invalid in [
        shape.replace("<Width>19", "<Width>0"),
        shape.replace("<LinkEnabled>0", "<LinkEnabled>1"),
        shape.replace("<Anchor>Center", "<Anchor>TopLeft"),
    ] {
        assert!(step(&mut p, "CreateShapeByParameters", &invalid).is_err());
        assert_eq!(p.ids().unwrap(), ids);
    }
}

#[test]
fn conditional_error_message_stops_both_detector_branches() {
    for (select, relation, stops) in [
        (false, "OneOrMoreObjectsSelection", true),
        (true, "OneOrMoreObjectsSelection", false),
        (false, "NoObjectSelection", false),
        (true, "NoObjectSelection", true),
    ] {
        let f = Fixture::new();
        f.input();
        f.pal("main", &[
            a("SelectAllObjects", ""),
            if select { String::new() } else { a("InvertObjectSelection", "") },
            a("Conditional", &format!("<Operator>None</Operator><Rule Type=\"ObjectSelectionRule\"><ObjectRelation>{relation}</ObjectRelation></Rule><Then>{}</Then>", a("ShowMessage", "<Message>synthetic guard failed</Message><Severity>Error</Severity>"))),
            a("CreateSeparation", "<SeparationName>Later</SeparationName><PreviewRed>0</PreviewRed><PreviewGreen>0</PreviewGreen><PreviewBlue>0</PreviewBlue>"),
        ].concat());
        let mut s = Session::new();
        let r = f.run(&mut s);
        assert_eq!(r["status"], if stops { "stopped" } else { "completed" });
        assert_eq!(r["spots"].as_array().unwrap().iter().any(|n| n == "Later"), !stops);
        if stops {
            assert_eq!(r["stoppedAt"]["reason"], "synthetic guard failed");
        }
    }
}

#[test]
fn white_processing_step_survives_native_and_pdf_export_and_none_clears_it() {
    let f = Fixture::new();
    f.input();
    f.pal(
        "main",
        &[
            a("SelectAllObjects", ""),
            a("CreateLayerOnTop", "<LayerName>Specialty</LayerName>"),
            a("MoveObjectsToLayer", ""),
            a("SetLayerType", "<ProcessingStep>White</ProcessingStep>"),
        ]
        .concat(),
    );
    let mut s = Session::new();
    assert_eq!(f.run(&mut s)["status"], "completed");
    let layer = selection::layer_ids(&s.doc().unwrap().doc)
        .into_iter()
        .find(|id| s.doc().unwrap().doc.node(*id).unwrap().name.as_deref() == Some("Specialty"))
        .unwrap();
    let n = s.doc().unwrap().doc.node(layer).unwrap();
    assert!(n.extras().processing_white);
    let copy: vectorcraft_doc::Node = serde_json::from_value(serde_json::to_value(n).unwrap()).unwrap();
    assert!(copy.extras().processing_white);
    let bytes = std::fs::read(f.0.join("output.pdf")).unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("/GTS_Metadata<</GTS_ProcStepsGroup/White>>"));
    let mut p = player(&mut s);
    p.layers = vec![layer];
    step(&mut p, "SetLayerType", "<ProcessingStep>None</ProcessingStep>").unwrap();
    assert!(!p.doc().unwrap().node(layer).unwrap().extras().processing_white);
    assert!(step(&mut p, "SetLayerType", "<ProcessingStep>Unknown</ProcessingStep>").is_err());
}

/// Print rendering always reads public PDF drawing, with native editing data absent.
fn assert_same_plates(a: &[u8], b: &[u8]) {
    let a = vectorcraft_pdf::import(a).unwrap();
    let b = vectorcraft_pdf::import(b).unwrap();
    let mut plates = vec!["Cyan".into(), "Magenta".into(), "Yellow".into(), "Black".into()];
    plates.extend(a.swatches.iter().chain(&b.swatches).filter(|w| w.spot).map(|w| w.name.clone()));
    plates.sort();
    plates.dedup();
    for plate in plates {
        let options = vectorcraft_render::RenderOptions {
            background: Some([255; 4]),
            artboards: false,
            anti_alias: vectorcraft_render::AntiAlias::None,
            smooth_images: false,
            image_mips: false,
            overprint_preview: true,
            proof: Some(vectorcraft_render::proof::ProofSetup { separations: Some(vec![plate.clone()]), ..Default::default() }),
            ..Default::default()
        };
        let mut renderer = vectorcraft_render::Renderer::new();
        let bounds = a.artboards[0].rect;
        let aa = renderer.render_region_with(&a, bounds, 600. / 72., &options).pixels;
        let bb = vectorcraft_render::Renderer::new().render_region_with(&b, bounds, 600. / 72., &options).pixels;
        let changed = aa.iter().zip(&bb).filter(|(a, b)| a != b).count();
        assert!(aa.len() == bb.len() && changed == 0, "plate {plate}: {changed} changed channels");
    }
}

#[test]
fn separation_marks_public_pdf_plates_and_builder_match_explicit_intents() {
    let f = Fixture::new();
    f.pal(
        "prepare",
        &[
            a("SelectAllObjects", ""),
            a(
                "CleanObjects",
                "<ExecuteClip>1</ExecuteClip><CleanUpGroupsAndClippingMasks>1</CleanUpGroupsAndClippingMasks><OutlineStrokes>1</OutlineStrokes>",
            ),
            a("CreateLayerOnTop", "<LayerName>Artwork</LayerName>"),
            a("MoveObjectsToLayer", ""),
        ]
        .concat(),
    );
    f.pal("metal", &[
        a("SelectObjectsByAttributes", "<SelectionMode>NewSelection</SelectionMode><Rule Type=\"SeparationNameRule\"><SeparationName>Test Metal</SeparationName><SeparationRelation>Using</SeparationRelation></Rule>"),
        a("CreateSeparation", "<SeparationName>Test Metal</SeparationName><PreviewRed>0.7</PreviewRed><PreviewGreen>0.5</PreviewGreen><PreviewBlue>0.3</PreviewBlue>"),
        a("SetFillFlatPaint", "<Percentage>100</Percentage>"),
    ].concat());
    f.pal("Trapping_Supacolour_Standard", &a("RenameObjects", "<ObjectName>LIGHT</ObjectName>"));
    let mut s = Session::new();
    s.execute("file.new", &json!({"width":50,"height":40})).unwrap();
    let left = rect(&mut s, 5.12, 5.13, 12., 12.);
    let right = rect(&mut s, 28.23, 5.17, 12., 12.);
    s.execute("paint.setFill", &json!({"ids":[left,right],"color":{"c":0.2,"m":0.6,"y":0.1,"k":0.1}})).unwrap();
    let export = |s: &Session| {
        super::super::fileio::encode_with_warnings(&s.doc().unwrap().doc, "pdf", &json!({"preserveEditing":false,"output":{"conversion":"none"}}))
            .unwrap()
            .0
    };
    let plain = export(&s);
    // The artist opens an existing original, then saves again with marks. This exercises
    // the page-coordinate round trip as well as the metadata carrier.
    s.execute("document.open", &json!({"name":"original.pdf","dataBase64":vectorcraft_format::base64_encode(&plain),"printProduction":true}))
        .unwrap();
    let paths = actions::leaf_targets(&s.doc().unwrap().doc, &selection::layer_ids(&s.doc().unwrap().doc));
    let left = *paths.iter().find(|id| s.doc().unwrap().doc.bounds_of(&[**id], true).is_some_and(|b| b.x0 < 20.)).unwrap();
    let right = *paths.iter().find(|id| s.doc().unwrap().doc.bounds_of(&[**id], true).is_some_and(|b| b.x0 > 20.)).unwrap();
    s.execute("sep.mark.set", &json!({"ids":[left],"mark":{"type":"metallic","list":"metal"}})).unwrap();
    s.execute("sep.mark.set", &json!({"ids":[right],"mark":{"type":"noTrap"}})).unwrap();
    let marked = export(&s);
    assert_same_plates(&plain, &marked);
    std::fs::write(f.0.join("marked.pdf"), &marked).unwrap();
    std::fs::write(f.0.join("plain.pdf"), &plain).unwrap();
    let mut params = json!({"root":f.0,"lists":["prepare","Trapping_Supacolour_Standard","TrapTask"],"input":f.0.join("marked.pdf"),"output":f.0.join("auto.pdf")});
    let auto = s.execute("sep.runActionList", &params).unwrap();
    assert_eq!(auto["status"], "completed", "{auto}");
    let mm = 25.4 / 72.;
    params["input"] = json!(f.0.join("plain.pdf"));
    params["output"] = json!(f.0.join("explicit.pdf"));
    params["intents"] = json!({"metallic":[{"list":"metal","objects":{"rectMm":[0.,0.,22.*mm,30.*mm]}}],"noTrap":{"rectangles":[{"x":25.*mm,"y":0.,"width":25.*mm,"height":30.*mm}]}});
    let explicit = s.execute("sep.runActionList", &params).unwrap();
    assert_eq!(explicit["status"], "completed", "{explicit}");
    assert_same_plates(&std::fs::read(f.0.join("auto.pdf")).unwrap(), &std::fs::read(f.0.join("explicit.pdf")).unwrap());
    // An explicit empty intents object suppresses both kinds of persisted mark.
    params["input"] = json!(f.0.join("marked.pdf"));
    params["output"] = json!(f.0.join("override.pdf"));
    params["intents"] = json!({});
    let overridden = s.execute("sep.runActionList", &params).unwrap();
    assert_eq!(overridden["status"], "completed", "{overridden}");
    assert!(!overridden["trace"].as_array().unwrap().iter().any(|step| step["action"] == "SelectIntent" || step["action"] == "SelectNoTrap"));
}

#[test]
fn lossy_images_stop_before_actions_and_preserve_existing_output() {
    use vectorcraft_testkit::pdf::pdf_with;
    for (dict, data, reason) in [
        ("/ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /JPXDecode", "bad", "skipped"),
        (
            "/ColorSpace [/Separation /SyntheticSpot /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 1 0 0] /N 1 >>] /BitsPerComponent 1",
            "x",
            "spot inks",
        ),
        (
            "/ColorSpace [/Separation /All /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [1 1 1 1] /N 1 >>] /BitsPerComponent 8",
            "x",
            "/All",
        ),
    ] {
        let f = Fixture::new();
        let mut page = PdfPage::new(100., 100., "0 1 0 0 k 10 10 20 20 re f q 10 0 0 10 40 40 cm /Im Do Q");
        page.resources = "/XObject << /Im 5 0 R >>".into();
        let stream = format!("<< /Type /XObject /Subtype /Image /Width 1 /Height 1 {dict} /Length {} >>\nstream\n{data}\nendstream", data.len());
        std::fs::write(f.0.join("input.pdf"), pdf_with(&[page], &[&stream], None)).unwrap();
        f.pal("main", &a("SelectAllObjects", ""));
        std::fs::write(f.0.join("output.pdf"), b"previous valid output").unwrap();
        let result = f.run(&mut Session::new());
        assert_eq!(result["status"], "stopped", "{result}");
        assert_eq!(result["stoppedAt"]["list"], "Import", "{result}");
        assert!(result["stoppedAt"]["reason"].as_str().unwrap().contains(reason), "{result}");
        assert_eq!(result["traceLength"], 0);
        assert_eq!(std::fs::read(f.0.join("output.pdf")).unwrap(), b"previous valid output");
    }
}

#[test]
fn devicen_selection_and_ink_mutations_are_atomic_for_image_only_and_mixed_art() {
    use vectorcraft_color::{Color, Paint, Swatch};
    use vectorcraft_doc::{
        Appearance, ImageBlob, ImageObject, Node, NodeKind,
        cmyk::{Inks, SpotInks},
    };
    for mixed in [false, true] {
        for action in ["ConvertToCMYK", "DeleteInk"] {
            let mut d = Document::new(100., 100.);
            let layer = d.default_layer().unwrap();
            d.swatches.push(Swatch { name: "SyntheticSpot".into(), paint: Paint::solid(Color::cmyk(0., 1., 0., 0.)), spot: true, global: true });
            let blob = ImageBlob::cmyk_spot_tiff(
                &Inks::new(1, 1, vec![0; 4]).unwrap(),
                &SpotInks::new(1, 1, vec!["SyntheticSpot".into()], vec![128]).unwrap(),
            )
            .unwrap();
            d.images.insert("test".into(), blob);
            let image = Node::new(
                d.alloc_id(),
                NodeKind::Image(Box::new(ImageObject {
                    key: "test".into(),
                    width: 1,
                    height: 1,
                    xf: vectorcraft_geom::Affine::IDENTITY,
                    link: None,
                    placement: Default::default(),
                })),
            );
            let image_id = image.id;
            d.insert(Some(layer), 0, image).unwrap();
            if mixed {
                let node = Node::path(
                    d.alloc_id(),
                    vectorcraft_geom::shapes::rectangle(Rect::new(10., 10., 20., 20.)),
                    Appearance::basic(Paint::Solid { color: Color::BLACK, swatch: Some("SyntheticSpot".into()), tint: 0.5 }, Paint::None, 0.),
                );
                d.insert(Some(layer), 0, node).unwrap();
            }
            let mut session = Session::new();
            session.add_document(d, None);
            let mut p = player(&mut session);
            step(&mut p,"SelectObjectsByAttributes",r#"<Rule Type="SeparationNameRule"><SeparationName>SyntheticSpot</SeparationName><SeparationRelation>Using</SeparationRelation></Rule>"#).unwrap();
            assert!(p.ids().unwrap().contains(&image_id));
            step(&mut p, "SelectSeparationsInPrintingLayers", "").unwrap();
            assert_eq!(p.inks, vec!["SyntheticSpot"]);
            let before = p.doc().unwrap().clone();
            let err = step(&mut p, action, "<Option>UseFromPDF</Option>").unwrap_err();
            assert!(err.to_string().contains("image ink channels"), "{err}");
            assert_eq!(p.doc().unwrap(), &before);
            // Removing an empty channel is exact and must not leave a dangling spot resource.
            let d = std::sync::Arc::make_mut(&mut p.session.doc_mut().unwrap().doc);
            d.images.insert(
                "test".into(),
                ImageBlob::cmyk_spot_tiff(
                    &Inks::new(1, 1, vec![17, 0, 0, 0]).unwrap(),
                    &SpotInks::new(1, 1, vec!["SyntheticSpot".into()], vec![0]).unwrap(),
                )
                .unwrap(),
            );
            step(&mut p, action, "<Option>UseFromPDF</Option>").unwrap();
            let d = p.doc().unwrap();
            assert!(d.images["test"].spot_names().is_empty());
            assert_eq!(d.images["test"].cmyk().unwrap().data, vec![17, 0, 0, 0]);
            assert!(d.swatch("SyntheticSpot").is_none());
        }
    }
}

#[test]
fn underprint_checks_effective_ancestor_composition_before_footprints() {
    use std::sync::Arc;
    use vectorcraft_color::BlendMode;
    use vectorcraft_doc::{Node, OpacityMask};
    for variant in ["zero", "opacity", "blend", "mask", "effects"] {
        let mut s = Session::new();
        s.execute("file.new", &json!({})).unwrap();
        let id = rect(&mut s, 0., 0., 10., 10.);
        s.execute("object.group", &json!({})).unwrap();
        let group = s.doc().unwrap().selection.objects[0];
        let d = Arc::make_mut(&mut s.doc_mut().unwrap().doc);
        let parent = d.node_mut(group).unwrap();
        match variant {
            "zero" => parent.opacity = 0.,
            "opacity" => parent.opacity = 0.5,
            "blend" => parent.blend = BlendMode::Multiply,
            "mask" => parent.extras_mut().mask = Some(Box::new(OpacityMask::new(Node::group(NodeId(999), vec![]), true))),
            _ => {
                parent.appearance.effects.push(vectorcraft_doc::Effect { id: "blur.gaussian".into(), params: json!({"radius":1}), visible: true });
            }
        }
        s.doc_mut().unwrap().selection.set([id]);
        let mut p = player(&mut s);
        let before = p.doc().unwrap().clone();
        let result = step(&mut p, "AddWhiteUnderPrint", "<TrappingMode>Spread</TrappingMode><Distance>0</Distance><JoinType>Round</JoinType>");
        if variant == "zero" {
            assert_eq!(result.unwrap(), 0);
            assert!(p.ids().unwrap().is_empty());
        } else {
            assert!(result.unwrap_err().to_string().contains("ancestor"));
            assert_eq!(p.doc().unwrap(), &before);
        }
    }
}

#[test]
fn attribute_operators_and_ink_threshold_use_stored_precision() {
    use vectorcraft_color::{Color, Paint};
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    let a_id = rect(&mut s, 0., 0., 10., 10.);
    let b_id = rect(&mut s, 20., 0., 10., 10.);
    let d = std::sync::Arc::make_mut(&mut s.doc_mut().unwrap().doc);
    d.node_mut(a_id).unwrap().name = Some("A".into());
    d.node_mut(b_id).unwrap().name = Some("B".into());
    let mut p = player(&mut s);
    for (operator, count) in [("All", 0), ("Any", 2), ("None", 0)] {
        let rules = ["A", "B"]
            .map(|name| format!(r#"<Rule Type="NameRule"><NameRelation>Equals</NameRelation><SearchString>{name}</SearchString></Rule>"#))
            .concat();
        step(&mut p, "SelectObjectsByAttributes", &format!("<Operator>{operator}</Operator>{rules}")).unwrap();
        assert_eq!(p.ids().unwrap().len(), count);
    }
    assert!(
        step(
            &mut p,
            "SelectObjectsByAttributes",
            r#"<Operator>Typo</Operator><Rule Type="ObjectSelectionRule"><ObjectRelation>NoObjectsSelection</ObjectRelation></Rule>"#
        )
        .unwrap_err()
        .to_string()
        .contains("operator")
    );
    let epsilon = vectorcraft_doc::inks::PRESENCE_EPSILON;
    for value in [f32::from_bits(epsilon.to_bits() - 1), epsilon, f32::from_bits(epsilon.to_bits() + 1)] {
        let d = std::sync::Arc::make_mut(&mut p.session.doc_mut().unwrap().doc);
        d.node_mut(a_id).unwrap().appearance =
            vectorcraft_doc::Appearance::basic(Paint::solid(Color::cmyk(value, 0., 0., 0.)), Paint::None, 0.).into();
        let rule = r#"<Rule Type="SeparationTypeRule"><SeparationType>Any</SeparationType><SeparationRelation>Using</SeparationRelation><Epsilon>0.005</Epsilon></Rule>"#;
        step(&mut p, "SelectObjectsByAttributes", rule).unwrap();
        assert_eq!(p.ids().unwrap().contains(&a_id), vectorcraft_doc::inks::present(value, epsilon));
    }
}

#[test]
fn underprint_accepts_only_masks_proven_opaque_over_printed_content() {
    use std::sync::Arc;
    use vectorcraft_color::{Color, Paint};
    use vectorcraft_doc::{Appearance, Effect, OpacityMask};
    for variant in [
        "opaque",
        "gray",
        "zero",
        "partial_extent",
        "disabled",
        "hidden_effect",
        "zero_blur",
        "clipped_away",
        "opaque_bitmap",
        "partial_bitmap",
        "zero_bitmap",
        "inverted_outside",
    ] {
        let mut s = Session::new();
        s.execute("file.new", &json!({})).unwrap();
        let id = rect(&mut s, 0., 0., 10., 10.);
        s.execute("object.group", &json!({})).unwrap();
        let group = s.doc().unwrap().selection.objects[0];
        let d = Arc::make_mut(&mut s.doc_mut().unwrap().doc);
        let mut art = d.node(id).unwrap().clone();
        art.id = NodeId(999);
        art.appearance = Appearance::basic(
            Paint::solid(Color::gray(match variant {
                "gray" => 0.5,
                "zero" => 1.,
                _ => 0.,
            })),
            Paint::None,
            0.,
        )
        .into();
        if variant == "inverted_outside" {
            art.transform(vectorcraft_geom::Affine::translate((20., 0.)), false);
        }
        if variant == "partial_extent" {
            art.transform(vectorcraft_geom::Affine::scale(0.5), false);
        }
        let mut clip = art.clone();
        clip.id = NodeId(1000);
        clip.transform(vectorcraft_geom::Affine::translate((20., 0.)), false);
        if let vectorcraft_doc::NodeKind::Path { clipping, .. } = &mut clip.kind {
            *clipping = true;
        }
        clip.appearance = Appearance::default().into();
        let mut mask = OpacityMask::new(art, true);
        mask.invert = variant == "inverted_outside";
        mask.disabled = variant == "disabled";
        if mask.disabled {
            mask.art = Arc::new(vectorcraft_doc::Node::group(NodeId(999), vec![]));
        }
        if variant.ends_with("_bitmap") {
            let alpha = match variant {
                "partial_bitmap" => 128,
                "zero_bitmap" => 0,
                _ => 255,
            };
            let mut png = std::io::Cursor::new(Vec::new());
            image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 255, 255, alpha])).write_to(&mut png, image::ImageFormat::Png).unwrap();
            d.images.insert("mask".into(), vectorcraft_doc::ImageBlob::png(png.into_inner()));
            mask.art = Arc::new(vectorcraft_doc::Node::new(
                NodeId(999),
                vectorcraft_doc::NodeKind::Image(
                    vectorcraft_doc::ImageObject {
                        key: "mask".into(),
                        width: 2,
                        height: 2,
                        xf: vectorcraft_geom::Affine::scale(5.),
                        link: None,
                        placement: Default::default(),
                    }
                    .into(),
                ),
            ));
        }
        let parent = d.node_mut(group).unwrap();
        if variant == "clipped_away" {
            parent.children_mut().unwrap().insert(0, Arc::new(clip));
            if let vectorcraft_doc::NodeKind::Group { clip, .. } = &mut parent.kind {
                *clip = true;
            }
        }
        parent.extras_mut().mask = Some(Box::new(mask));
        if variant == "hidden_effect" || variant == "zero_blur" {
            parent.appearance.effects.push(Effect {
                id: "blur.gaussian".into(),
                params: json!({"radius":if variant == "zero_blur" {0} else {5}}),
                visible: variant == "zero_blur",
            });
        }
        s.doc_mut().unwrap().selection.set([id]);
        let mut p = player(&mut s);
        let before = p.doc().unwrap().clone();
        let result = step(&mut p, "AddWhiteUnderPrint", "<TrappingMode>Spread</TrappingMode><Distance>0</Distance><JoinType>Round</JoinType>");
        if ["gray", "zero", "partial_extent", "partial_bitmap", "zero_bitmap"].contains(&variant) {
            assert!(result.unwrap_err().to_string().contains("mask"), "{variant}");
            assert_eq!(p.doc().unwrap(), &before);
        } else if variant == "clipped_away" {
            assert_eq!(result.unwrap(), 0);
        } else {
            assert!(result.unwrap() > 0, "{variant}");
        }
    }
}

#[test]
fn unsupported_page_blend_space_stops_only_when_printed_transparency_uses_it() {
    use vectorcraft_testkit::pdf::pdf_with;
    for transparent in [false, true] {
        let f = Fixture::new();
        let mut page = PdfPage::new(100., 100., if transparent { "/Gs gs 0 1 0 0 k 10 10 20 20 re f" } else { "0 1 0 0 k 10 10 20 20 re f" });
        page.entries = "/Group << /S /Transparency /CS [/CalRGB << /WhitePoint [0.9505 1 1.089] >>] >>".into();
        page.resources = "/ExtGState << /Gs << /ca 0.5 >> >>".into();
        std::fs::write(f.0.join("input.pdf"), pdf_with(&[page], &[], None)).unwrap();
        f.pal("main", &a("SelectAllObjects", ""));
        let result = f.run(&mut Session::new());
        assert_eq!(result["status"], if transparent { "stopped" } else { "completed" }, "{result}");
        if transparent {
            assert!(result["stoppedAt"]["reason"].as_str().unwrap().contains("opacity"), "{result}");
            assert!(!f.0.join("output.pdf").exists());
        } else {
            assert!(result["importWarnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().starts_with("Info:")), "{result}");
        }
    }
}

#[test]
fn a_mask_cannot_change_an_underprint_already_covered_by_selected_art() {
    use std::sync::Arc;
    use vectorcraft_color::{Color, Paint};
    use vectorcraft_doc::{Appearance, OpacityMask};
    for (extends, chosen, passes) in [(false, true, true), (true, true, false), (false, false, false)] {
        let mut s = Session::new();
        s.execute("file.new", &json!({})).unwrap();
        let back = rect(&mut s, 0., 0., 20., 20.);
        let front = rect(&mut s, if extends { 18. } else { 2. }, 2., 10., 10.);
        s.execute("object.group", &json!({})).unwrap();
        let group = s.doc().unwrap().selection.objects[0];
        let d = Arc::make_mut(&mut s.doc_mut().unwrap().doc);
        let mut art = d.node(front).unwrap().clone();
        art.id = NodeId(999);
        art.appearance = Appearance::basic(Paint::solid(Color::gray(0.5)), Paint::None, 0.).into();
        d.node_mut(group).unwrap().extras_mut().mask = Some(Box::new(OpacityMask::new(art, true)));
        s.doc_mut().unwrap().selection.set(if chosen { vec![back, front] } else { vec![back] });
        let mut p = player(&mut s);
        let before = p.doc().unwrap().clone();
        let result = step(
            &mut p,
            "AddWhiteUnderPrint",
            "<TrappingMode>Spread</TrappingMode><Distance>0</Distance><JoinType>Round</JoinType><ExcludeCoveredAreas>1</ExcludeCoveredAreas>",
        );
        if passes {
            assert_eq!(result.unwrap(), 1);
            let d = p.doc().unwrap();
            let bounds = d.node(p.ids().unwrap()[0]).unwrap().geometric_bounds().unwrap();
            assert_eq!(bounds, vectorcraft_geom::Rect::new(0., 0., 20., 20.));
        } else {
            assert!(result.unwrap_err().to_string().contains("mask"));
            assert_eq!(p.doc().unwrap(), &before);
        }
    }
}
