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
fn light_recipe_ignores_persistent_no_trap_marks_and_preserves_plates_and_marks() {
    let f = Fixture::new();
    let (plain, marked) = no_trap_pdf_pair();
    std::fs::write(f.0.join("plain.pdf"), plain).unwrap();
    std::fs::write(f.0.join("marked.pdf"), marked).unwrap();
    f.pal(
        "WE Light",
        &[
            a("SelectAllObjects", ""),
            a("CreateLayerOnTop", "<LayerName>Artwork</LayerName>"),
            a("MoveObjectsToLayer", ""),
            a("AddWhiteUnderPrint", "<TrappingMode>Choke</TrappingMode><Distance>0.2</Distance><JoinType>Round</JoinType>"),
        ]
        .concat(),
    );
    for variant in ["plain", "marked"] {
        let mut s = Session::new();
        let output = f.0.join(format!("{variant}-light.pdf"));
        let result = s
            .execute("sep.runActionList", &json!({"root":f.0,"lists":["WE Light"],"input":f.0.join(format!("{variant}.pdf")),"output":output}))
            .unwrap();
        assert_eq!(result["status"], "completed", "{result}");
        assert!(!result["trace"].as_array().unwrap().iter().any(|step| step["action"] == "SelectNoTrap" || step["action"] == "TrapTask"));
        // Read the exported public PDF content, not its native editing attachment.
        s.execute("document.open", &json!({"path":output,"printProduction":true})).unwrap();
        let marks = s.execute("sep.mark.list", &json!({})).unwrap();
        let marks = marks["marks"].as_array().unwrap();
        if variant == "marked" {
            assert_eq!(marks.len(), 1);
            assert_eq!(marks[0]["mark"], json!({"type":"noTrap"}));
        } else {
            assert!(marks.is_empty());
        }
    }
    assert_same_plates(&std::fs::read(f.0.join("plain-light.pdf")).unwrap(), &std::fs::read(f.0.join("marked-light.pdf")).unwrap());
}

fn no_trap_pdf_pair() -> (Vec<u8>, Vec<u8>) {
    let mut s = Session::new();
    s.execute("file.new", &json!({"width":50,"height":40})).unwrap();
    let id = rect(&mut s, 5., 5., 20., 20.);
    s.execute("paint.setFill", &json!({"ids":[id],"color":{"c":0.2,"m":0.6,"y":0.1,"k":0.1}})).unwrap();
    let export = |s: &Session| {
        super::super::fileio::encode_with_warnings(&s.doc().unwrap().doc, "pdf", &json!({"preserveEditing":false,"output":{"conversion":"none"}}))
            .unwrap()
            .0
    };
    let plain = export(&s);
    s.execute("sep.mark.set", &json!({"ids":[id],"mark":{"type":"noTrap"}})).unwrap();
    (plain, export(&s))
}

#[test]
fn persistent_no_trap_marks_still_reject_malformed_trap_recipes() {
    let f = Fixture::new();
    std::fs::write(f.0.join("marked.pdf"), no_trap_pdf_pair().1).unwrap();
    for lists in [
        vec!["Trapping_Supacolour_Standard"],
        vec!["Trapping_Supacolour_Standard", "prepare", "TrapTask"],
        vec!["Trapping_Supacolour_Standard", "TrapTask", "Trapping_Supacolour_Standard (NEW)", "TrapTask"],
        vec!["TrapTask"],
    ] {
        let output = f.0.join("output.pdf");
        std::fs::write(&output, b"previous output").unwrap();
        let error = Session::new()
            .execute("sep.runActionList", &json!({"root":f.0,"lists":lists,"input":f.0.join("marked.pdf"),"output":output}))
            .unwrap_err();
        assert!(error.to_string().contains("NoTrap marks require exactly one trap ticket immediately followed by TrapTask"), "{lists:?}: {error}");
        assert_eq!(std::fs::read(output).unwrap(), b"previous output");
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
        "opaque_mesh",
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
        if variant == "opaque_mesh" {
            mask.art = Arc::new(vectorcraft_doc::Node::new(
                NodeId(999),
                vectorcraft_doc::NodeKind::Mesh(vectorcraft_doc::GradientMesh::from_surface(
                    1,
                    1,
                    &|u, v| vectorcraft_geom::Point::new(u * 10., v * 10.),
                    &|_, _| Color::WHITE,
                )),
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

fn image_document(blob: vectorcraft_doc::ImageBlob) -> (Document, NodeId) {
    use vectorcraft_doc::{ImageObject, Node, NodeKind};
    let mut d = Document::new(100., 100.);
    let id = d.alloc_id();
    d.images.insert("shared".into(), blob);
    let n = Node::new(
        id,
        NodeKind::Image(Box::new(ImageObject {
            key: "shared".into(),
            width: 2,
            height: 1,
            xf: vectorcraft_geom::Affine::new([10., 0., 0., 10., 20., 30.]),
            link: None,
            placement: Default::default(),
        })),
    );
    d.insert(d.default_layer(), 0, n).unwrap();
    (d, id)
}

#[test]
fn image_fill_replacement_is_instance_local_and_keeps_silver_and_masks() {
    use vectorcraft_color::{Color, Paint, Swatch};
    use vectorcraft_doc::{
        ImageBlob, NodeKind,
        cmyk::{Inks, SpotInks},
    };
    for action in ["RemoveFill", "SetFillFlatPaint"] {
        let blob = ImageBlob::cmyk_spot_tiff(
            &Inks::new(2, 1, vec![20, 30, 40, 50, 60, 70, 80, 90]).unwrap(),
            &SpotInks::new(2, 1, vec!["M-1".into()], vec![255, 128]).unwrap(),
        )
        .unwrap();
        let (mut d, id) = image_document(blob.clone());
        for name in ["M-1", "Silver"] {
            d.swatches.push(Swatch { name: name.into(), paint: Paint::solid(Color::gray(0.5)), spot: true, global: true });
        }
        let mask = vectorcraft_doc::Node::path(
            d.alloc_id(),
            vectorcraft_geom::shapes::rectangle(Rect::new(20., 30., 40., 40.)),
            vectorcraft_doc::Appearance::basic(Paint::solid(Color::WHITE), Paint::None, 0.),
        );
        d.node_mut(id).unwrap().extras_mut().mask = Some(Box::new(vectorcraft_doc::OpacityMask::new(mask, true)));
        let mut other = d.node(id).unwrap().clone();
        other.id = d.alloc_id();
        let other_id = other.id;
        d.insert(d.default_layer(), 1, other).unwrap();
        // Silver has already been generated from the named source outline when the
        // recipe clears its raster ink samples.
        let outline = vectorcraft_doc::Node::path(
            d.alloc_id(),
            vectorcraft_geom::shapes::rectangle(Rect::new(20., 30., 40., 40.)),
            vectorcraft_doc::Appearance::basic(Paint::solid(Color::WHITE), Paint::None, 0.),
        );
        let outline_id = outline.id;
        d.insert(d.default_layer(), 2, outline).unwrap();
        let mut s = Session::new();
        s.add_document(d, None);
        s.doc_mut().unwrap().selection.set([outline_id]);
        let mut p = player(&mut s);
        p.inks = vec!["Silver".into()];
        step(
            &mut p,
            "AddWhiteUnderPrint",
            "<TrappingMode>Spread</TrappingMode><Distance>0</Distance><JoinType>Round</JoinType><UseSelectedSeparation>1</UseSelectedSeparation>",
        )
        .unwrap();
        let silver = p.ids().unwrap();
        assert!(!silver.is_empty());
        let silver_nodes: Vec<_> = silver.iter().map(|id| p.doc().unwrap().node(*id).unwrap().clone()).collect();
        p.session.doc_mut().unwrap().selection.set([id]);
        p.inks = vec!["M-1".into()];
        let before = p.doc().unwrap().node(id).unwrap().clone();
        step(&mut p, action, "<Percentage>0</Percentage>").unwrap();
        let d = p.doc().unwrap();
        let NodeKind::Image(im) = &d.node(id).unwrap().kind else { panic!("lost image") };
        let NodeKind::Image(old) = &before.kind else { panic!() };
        assert_eq!(im.xf, old.xf);
        assert_eq!(im.placement, old.placement);
        assert_ne!(im.key, old.key);
        assert_eq!(d.node(id).unwrap().extras().mask, before.extras().mask);
        assert!(d.images[&im.key].cmyk().unwrap().data.iter().all(|v| *v == 0));
        assert!(d.images[&im.key].spot_names().is_empty());
        assert_eq!(d.images["shared"], blob);
        assert!(matches!(&d.node(other_id).unwrap().kind, NodeKind::Image(im) if im.key=="shared"));
        for n in &silver_nodes {
            assert_eq!(d.node(n.id).unwrap(), n);
        }
        // The selected instance's public PDF has Silver but no residual M-1 resource.
        let mut public = d.clone();
        public.remove(other_id).unwrap();
        let bytes = vectorcraft_pdf::export(&public, &vectorcraft_pdf::PdfOptions::default()).unwrap();
        let opened = vectorcraft_pdf::import(&bytes).unwrap();
        assert!(!opened.spot_names().contains(&"M-1".into()));
        assert!(opened.spot_names().contains(&"Silver".into()));
    }
}

#[test]
fn replacing_rgba_image_fill_keeps_intrinsic_alpha_and_outer_mask() {
    use vectorcraft_doc::{ImageBlob, Node, NodeKind, OpacityMask};
    let mut png = std::io::Cursor::new(Vec::new());
    let rgba = image::RgbaImage::from_raw(2, 1, vec![255, 0, 0, 64, 0, 0, 255, 0]).unwrap();
    rgba.write_to(&mut png, image::ImageFormat::Png).unwrap();
    let (mut d, id) = image_document(ImageBlob::png(png.into_inner()));
    let mask = OpacityMask::new(Node::group(d.alloc_id(), vec![]), false);
    d.node_mut(id).unwrap().extras_mut().mask = Some(Box::new(mask.clone()));
    let mut s = Session::new();
    s.add_document(d, None);
    s.doc_mut().unwrap().selection.set([id]);
    step(&mut player(&mut s), "RemoveFill", "").unwrap();
    let d = &s.doc().unwrap().doc;
    let n = d.node(id).unwrap();
    assert_eq!(n.extras().mask.as_deref(), Some(&mask));
    let child = &n.children().unwrap()[0];
    let NodeKind::Image(im) = &child.kind else { panic!() };
    assert!(d.images[&im.key].cmyk().unwrap().data.iter().all(|v| *v == 0));
    let NodeKind::Image(alpha) = &child.extras().mask.as_ref().unwrap().art.kind else { panic!() };
    assert_eq!(alpha.xf, im.xf);
    let pixels = d.images[&alpha.key].decoded().unwrap();
    assert_eq!(pixels.get_pixel(0, 0).0, [64, 64, 64, 255]);
    assert_eq!(pixels.get_pixel(1, 0).0, [0, 0, 0, 255]);
}

#[test]
fn rgb_jpeg_and_gray_images_pass_background_prep_and_process_queries() {
    use vectorcraft_color::{Color, Paint, Swatch};
    use vectorcraft_doc::{Appearance, ImageBlob, Node};
    for gray in [false, true] {
        for background in [false, true] {
            let mut bytes = std::io::Cursor::new(Vec::new());
            if gray {
                image::GrayImage::from_pixel(2, 1, image::Luma([90])).write_to(&mut bytes, image::ImageFormat::Png).unwrap();
            } else {
                image::RgbImage::from_pixel(2, 1, image::Rgb([180, 40, 90])).write_to(&mut bytes, image::ImageFormat::Jpeg).unwrap();
            }
            let blob = ImageBlob::new(if gray { "image/png" } else { "image/jpeg" }, bytes.into_inner());
            let (mut d, id) = image_document(blob);
            d.color_profiles.cmyk = Some(vectorcraft_color::cms::active_settings().cmyk);
            if background {
                d.swatches.push(Swatch { name: "SupaBG".into(), paint: Paint::solid(Color::BLACK), spot: true, global: true });
                let n = Node::path(
                    d.alloc_id(),
                    vectorcraft_geom::shapes::rectangle(Rect::new(0., 0., 100., 100.)),
                    Appearance::basic(Paint::Solid { color: Color::BLACK, swatch: Some("SupaBG".into()), tint: 1. }, Paint::None, 0.),
                );
                d.insert(d.default_layer(), 0, n).unwrap();
            }
            assert!(profiles::document_cms(&d).is_some(), "test must use the document CMS");
            let expected = {
                let px = d.images["shared"].decoded().unwrap();
                let p = px.get_pixel(0, 0).0;
                profiles::cmyk(Color::rgb8(p[0], p[1], p[2]), profiles::document_cms(&d).as_ref())
            };
            let inks = selection::inks(&d, d.node(id).unwrap()).unwrap();
            for (name, v) in ["Cyan", "Magenta", "Yellow", "Black"].iter().zip(expected) {
                assert_eq!(inks[*name], v);
            }
            assert!(!selection::spot_inks(&d, d.node(id).unwrap()).unwrap().contains_key("SupaBG"));
            let mut s = Session::new();
            s.add_document(d.clone(), None);
            let mut p = player(&mut s);
            p.remove_background().unwrap();
            assert!(p.doc().unwrap().node(id).is_some());
            let f = Fixture::new();
            std::fs::write(f.0.join("input.pdf"), vectorcraft_pdf::export(&d, &vectorcraft_pdf::PdfOptions::default()).unwrap()).unwrap();
            f.pal("main", &a("SelectAllObjects", ""));
            let result = f.run(&mut Session::new());
            assert_eq!(result["status"], "completed", "{gray}/{background}: {result}");
        }
    }
}

#[test]
fn mask_proofs_do_not_erase_a_short_strip_or_use_cleaned_backing() {
    use vectorcraft_color::{Color, Paint};
    use vectorcraft_doc::{Appearance, Node, OpacityMask};
    for short in [0.005, 1e-9] {
        for redundancy in [false, true] {
            let mut s = Session::new();
            s.execute("file.new", &json!({})).unwrap();
            let back = if redundancy { Some(rect(&mut s, 0., 0., 100. - short, 100.)) } else { None };
            let id = rect(&mut s, 0., 0., 100., 100.);
            s.execute("object.group", &json!({})).unwrap();
            let group = s.doc().unwrap().selection.objects[0];
            let d = std::sync::Arc::make_mut(&mut s.doc_mut().unwrap().doc);
            let art = Node::path(
                d.alloc_id(),
                vectorcraft_geom::shapes::rectangle(Rect::new(0., 0., 100. - short, 100.)),
                Appearance::basic(Paint::solid(if redundancy { Color::BLACK } else { Color::WHITE }), Paint::None, 0.),
            );
            d.node_mut(group).unwrap().extras_mut().mask = Some(Box::new(OpacityMask::new(art, true)));
            s.doc_mut().unwrap().selection.set(back.into_iter().chain([id]));
            let mut p = player(&mut s);
            let before = p.doc().unwrap().clone();
            let err = step(&mut p, "AddWhiteUnderPrint", "<TrappingMode>Spread</TrappingMode><Distance>0</Distance><JoinType>Round</JoinType>")
                .unwrap_err();
            assert!(err.to_string().contains("mask"));
            assert_eq!(p.doc().unwrap(), &before);
        }
    }
}

#[test]
fn raster_footprint_uses_ink_epsilon_holes_alpha_and_masks() {
    use vectorcraft_color::{Color, Paint};
    use vectorcraft_doc::{Appearance, ImageBlob, Node, NodeKind, OpacityMask};
    use vectorcraft_geom::{Affine, shapes};
    // Native samples land exactly on the 600 dpi grid: transparent red, paper white,
    // sub-epsilon ink, ink just above epsilon, and a solid ring with a real hole.
    let mut pixels = image::RgbaImage::from_pixel(8, 8, image::Rgba([255, 255, 255, 255]));
    for y in 1..7 {
        for x in 1..7 {
            pixels.put_pixel(x, y, image::Rgba([0, 0, 0, 255]));
        }
    }
    for y in 3..5 {
        for x in 3..5 {
            pixels.put_pixel(x, y, image::Rgba([0, 0, 0, 0]));
        }
    }
    pixels.put_pixel(0, 0, image::Rgba([255, 0, 0, 0]));
    pixels.put_pixel(1, 1, image::Rgba([0, 0, 0, 1]));
    pixels.put_pixel(2, 1, image::Rgba([0, 0, 0, 2]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    pixels.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    let (mut d, id) = image_document(ImageBlob::png(bytes.into_inner()));
    if let NodeKind::Image(im) = &mut d.node_mut(id).unwrap().kind {
        im.width = 8;
        im.height = 8;
        im.xf = Affine::scale(72. / 600.);
    }
    let path = images::footprint(&d, d.node(id).unwrap()).unwrap();
    let area = vectorcraft_pathops::area(&path, vectorcraft_geom::FillRule::NonZero);
    assert!((area - 31. * (72_f64 / 600.).powi(2)).abs() < 1e-6, "{area}");
    let mask = Node::path(
        d.alloc_id(),
        shapes::rectangle(Rect::new(0., 0., 4. * 72. / 600., 8. * 72. / 600.)),
        Appearance::basic(Paint::solid(Color::WHITE), Paint::None, 0.),
    );
    d.node_mut(id).unwrap().extras_mut().mask = Some(Box::new(OpacityMask::new(mask, true)));
    let clipped = images::footprint(&d, d.node(id).unwrap()).unwrap();
    assert!((vectorcraft_pathops::area(&clipped, vectorcraft_geom::FillRule::NonZero) - 15. * (72_f64 / 600.).powi(2)).abs() < 1e-6);
    d.node_mut(id).unwrap().extras_mut().mask.as_mut().unwrap().invert = true;
    let inverted = images::footprint(&d, d.node(id).unwrap()).unwrap();
    assert!((vectorcraft_pathops::area(&inverted, vectorcraft_geom::FillRule::NonZero) - 16. * (72_f64 / 600.).powi(2)).abs() < 1e-6);
}

#[test]
fn raster_footprints_drive_underprint_and_shaper() {
    use vectorcraft_doc::{
        ImageBlob,
        cmyk::{Inks, SpotInks},
    };
    // Empty process pixel then white spot ink: white spot is printed ink, paper is not.
    let blob =
        ImageBlob::cmyk_spot_tiff(&Inks::new(2, 1, vec![0; 8]).unwrap(), &SpotInks::new(2, 1, vec!["White".into()], vec![0, 255]).unwrap()).unwrap();
    for action in ["AddWhiteUnderPrint", "Shaper"] {
        let (d, id) = image_document(blob.clone());
        let mut s = Session::new();
        s.add_document(d, None);
        s.doc_mut().unwrap().selection.set([id]);
        let mut p = player(&mut s);
        if action == "AddWhiteUnderPrint" {
            under(&mut p, false, 0.);
        } else {
            step(&mut p, action, "<Operation>Unite</Operation><WorkOnCopy>1</WorkOnCopy>").unwrap();
        }
        let area = selected_area(&p);
        assert!((area - 100.).abs() < 2., "{action}: {area}");
    }
}

#[test]
fn pal_accepts_png_input_at_declared_size_and_exports_pdf() {
    let f = Fixture::new();
    let bytes = vectorcraft_doc::ImageBlob::cmyk_tiff(&vectorcraft_doc::cmyk::Inks::new(2, 1, vec![255; 8]).unwrap()).unwrap();
    let pixels = bytes.decoded().unwrap();
    pixels.save(f.0.join("input.png")).unwrap();
    f.pal("main", &a("SelectAllObjects", ""));
    let mut s = Session::new();
    let result =
        s.execute("sep.runActionList", &json!({"root":f.0,"lists":["main"],"input":f.0.join("input.png"),"output":f.0.join("output.pdf")})).unwrap();
    assert_eq!(result["status"], "completed", "{result}");
    assert!(f.0.join("output.pdf").exists());
}

#[test]
fn raster_trap_eligibility_keeps_epsilon_black_and_permissions() {
    use vectorcraft_doc::{ImageBlob, NodeKind, cmyk::Inks};
    let blob =
        ImageBlob::cmyk_tiff(&Inks::new(6, 1, vec![0, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 255, 0, 0, 178, 255, 0, 0, 179, 0, 0, 0, 255]).unwrap())
            .unwrap();
    let (d, id) = image_document(blob);
    let NodeKind::Image(im) = &d.node(id).unwrap().kind else { panic!() };
    let allowed = images::trap_eligibility(&d, im, 0.7, true).unwrap().decoded().unwrap();
    assert_eq!(allowed.pixels().map(|p| p[0]).collect::<Vec<_>>(), [0, 0, 255, 255, 0, 0]);
    let denied = images::trap_eligibility(&d, im, 0.7, false).unwrap().decoded().unwrap();
    assert!(denied.pixels().all(|p| p[0] == 0 && p[3] == 255));
}

#[test]
fn raster_smoothing_retains_small_islands_and_counters_within_one_pixel() {
    use vectorcraft_color::{Color, Paint};
    use vectorcraft_doc::{Appearance, ImageBlob, Node, NodeKind};
    use vectorcraft_geom::Affine;
    let mut pixels = image::RgbaImage::from_pixel(64, 64, image::Rgba([255, 255, 255, 255]));
    let mut original = vec![false; 64 * 64];
    for y in 0i32..64 {
        for x in 0i32..64 {
            let r = (x - 32) * (x - 32) + (y - 32) * (y - 32);
            let on = (400..625).contains(&r) || (x == 3 && y == 3);
            if on {
                pixels.put_pixel(x as u32, y as u32, image::Rgba([0, 0, 0, 255]));
            }
            original[(y * 64 + x) as usize] = on;
        }
    }
    let mut bytes = std::io::Cursor::new(Vec::new());
    pixels.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    let (mut d, id) = image_document(ImageBlob::png(bytes.into_inner()));
    if let NodeKind::Image(im) = &mut d.node_mut(id).unwrap().kind {
        im.width = 64;
        im.height = 64;
        im.xf = Affine::scale(72. / 600.);
    }
    let path = images::footprint(&d, d.node(id).unwrap()).unwrap();
    assert_eq!(path.subpaths.len(), 3, "ring, counter and isolated pixel");
    d.layers = vec![std::sync::Arc::new(Node::path(d.alloc_id(), path, Appearance::basic(Paint::solid(Color::BLACK), Paint::None, 0.)))];
    d.drop_lookup_cache();
    let rendered = vectorcraft_render::Renderer::new().render_region_with(
        &d,
        Rect::new(0., 0., 64. * 72. / 600., 64. * 72. / 600.),
        600. / 72.,
        &vectorcraft_render::RenderOptions { artboards: false, anti_alias: vectorcraft_render::AntiAlias::None, ..Default::default() },
    );
    let actual = rendered.pixels.as_chunks::<4>().0.iter().map(|p| p[3] > 127).collect::<Vec<_>>();
    assert!(actual[3 * 64 + 3]);
    assert!(!actual[32 * 64 + 32]);
    for (i, (&a, &b)) in original.iter().zip(&actual).enumerate() {
        if a == b {
            continue;
        }
        let (x, y) = ((i % 64) as i32, (i / 64) as i32);
        assert!(
            (-1..=1).any(|dy| (-1..=1).any(|dx| {
                let (xx, yy) = (x + dx, y + dy);
                (0..64).contains(&xx) && (0..64).contains(&yy) && original[(yy * 64 + xx) as usize] != a
            })),
            "fitting changed interior pixel {x},{y}"
        );
    }
}

#[test]
fn raster_spread_choke_refuses_without_losing_paint_or_publishing() {
    let f = Fixture::new();
    image::RgbaImage::from_pixel(8, 8, image::Rgba([255, 0, 0, 255])).save(f.0.join("red.png")).unwrap();
    for action in ["Spread", "Choke"] {
        for target in ["OriginalPath", "CopyInFront", "CopyBehind"] {
            let mut s = Session::new();
            s.execute("document.open", &json!({"path": f.0.join("red.png")})).unwrap();
            let id = selection::all_objects(&s.doc().unwrap().doc, false)[0];
            // A path processed before the image must roll back as well.
            let vector = rect(&mut s, 20., 20., 10., 10.);
            s.doc_mut().unwrap().selection.set([vector, id]);
            let before = s.doc().unwrap().doc.clone();
            let params = format!("<Distance>0.1</Distance><JoinType>Round</JoinType><SpreadTarget>{target}</SpreadTarget>");
            let err = step(&mut player(&mut s), action, &params).unwrap_err();
            assert!(err.to_string().contains("cannot preserve raster paint"), "{err}");
            assert_eq!(s.doc().unwrap().doc, before);
            assert_eq!(s.doc().unwrap().selection.objects, [vector, id]);
            f.pal("main", &[a("SelectAllObjects", ""), a(action, &params)].concat());
            let output = f.0.join("output.pdf");
            std::fs::write(&output, b"previous output").unwrap();
            let result =
                s.execute("sep.runActionList", &json!({"root": f.0, "lists": ["main"], "input": f.0.join("red.png"), "output": output})).unwrap();
            assert_eq!(result["status"], "stopped");
            assert_eq!(std::fs::read(&output).unwrap(), b"previous output");
        }
    }
}

#[test]
fn unselected_raster_knockout_uses_opacity_including_process_white() {
    use vectorcraft_color::{Color, Paint};
    use vectorcraft_doc::{Appearance, ImageBlob, Node, NodeKind, OpacityMask};
    for alpha in [0, 128, 255] {
        for masked in [false, true] {
            let pixels = image::RgbaImage::from_pixel(2, 1, image::Rgba([255, 255, 255, alpha]));
            let mut bytes = std::io::Cursor::new(Vec::new());
            pixels.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
            let (mut d, image) = image_document(ImageBlob::png(bytes.into_inner()));
            // Exactly aligned output pixels: the white cover spans 24 x 12 pt.
            if let NodeKind::Image(im) = &mut d.node_mut(image).unwrap().kind {
                im.xf = vectorcraft_geom::Affine::scale(12.);
            }
            if masked {
                let mask = Node::path(
                    d.alloc_id(),
                    vectorcraft_geom::shapes::rectangle(Rect::new(0., 0., 12., 12.)),
                    Appearance::basic(Paint::solid(Color::WHITE), Paint::None, 0.),
                );
                d.node_mut(image).unwrap().extras_mut().mask = Some(Box::new(OpacityMask::new(mask, true)));
            }
            let back = d.alloc_id();
            let node = Node::path(
                back,
                vectorcraft_geom::shapes::rectangle(Rect::new(0., 0., 24., 12.)),
                Appearance::basic(Paint::solid(Color::BLACK), Paint::None, 0.),
            );
            d.insert(d.default_layer(), 0, node).unwrap();
            let mut s = Session::new();
            s.add_document(d, None);
            s.doc_mut().unwrap().selection.set([back]);
            let mut p = player(&mut s);
            under(&mut p, true, 0.);
            let area = if p.ids().unwrap().is_empty() { 0. } else { selected_area(&p) };
            let expected = if alpha != 255 {
                288.
            } else if masked {
                144.
            } else {
                0.
            };
            assert!((area - expected).abs() < 0.01, "alpha={alpha}, masked={masked}: {area}");
        }
    }
}

#[test]
fn no_trap_marks_detect_trapping_in_wrappers_calls_and_both_branches() {
    let f = Fixture::new();
    std::fs::write(f.0.join("marked.pdf"), no_trap_pdf_pair().1).unwrap();
    f.pal("folder/inner", &a("Group", &format!("<Actions>{}</Actions>", a("TrapTask", ""))));
    let trap = a("CallActionList", "<Path>inner</Path>");
    for body in [
        a("TrapTask", ""),
        a("CallActionList", "<Path>folder/inner.pal</Path>"),
        a("Conditional", &format!("<Then>{trap}</Then>")),
        a("Conditional", &format!("<Else>{trap}</Else>")),
        a("CallActionList", "<Path>Trapping_Supacolour_Standard</Path>"),
        a("CallActionList", "<Path>Trapping_Supacolour_Standard.PAL</Path>"),
    ] {
        f.pal("folder/wrapper", &body);
        let output = f.0.join("output.pdf");
        std::fs::write(&output, b"previous output").unwrap();
        let err = Session::new()
            .execute(
                "sep.runActionList",
                &json!({"root": f.0, "lists": ["folder/wrapper"],
            "input": f.0.join("marked.pdf"), "output": output}),
            )
            .unwrap_err();
        assert!(err.to_string().contains("NoTrap marks require"), "{err}");
        assert_eq!(std::fs::read(&output).unwrap(), b"previous output");
    }
    f.pal("folder/inner", &a("SelectAllObjects", ""));
    f.pal("folder/wrapper", &a("CallActionList", "<Path>inner</Path>"));
    let result = Session::new()
        .execute(
            "sep.runActionList",
            &json!({"root": f.0, "lists": ["folder/wrapper"],
        "input": f.0.join("marked.pdf"), "output": f.0.join("light.pdf")}),
        )
        .unwrap();
    assert_eq!(result["status"], "completed", "{result}");
}

#[test]
fn raster_knockout_uses_cmyk_opacity_and_image_soft_masks_independent_of_ink() {
    use vectorcraft_doc::{ImageBlob, NodeKind, OpacityMask, cmyk::Inks};
    for samples in [[0, 0, 0, 0], [0, 255, 255, 0]] {
        for opacity in [1., 0.5] {
            let blob = ImageBlob::cmyk_tiff(&Inks::new(3, 1, samples.repeat(3)).unwrap()).unwrap();
            let (mut d, id) = image_document(blob);
            let mut mask_bytes = std::io::Cursor::new(Vec::new());
            image::GrayImage::from_raw(3, 1, vec![0, 128, 255]).unwrap().write_to(&mut mask_bytes, image::ImageFormat::Png).unwrap();
            d.images.insert("soft-mask".into(), ImageBlob::png(mask_bytes.into_inner()));
            let n = d.node_mut(id).unwrap();
            n.opacity = opacity;
            if let NodeKind::Image(im) = &mut n.kind {
                im.width = 3;
                im.xf = vectorcraft_geom::Affine::scale(12.);
            }
            let mut mask = n.clone();
            mask.id = d.alloc_id();
            mask.opacity = 1.;
            if let NodeKind::Image(im) = &mut mask.kind {
                im.key = "soft-mask".into();
            }
            d.node_mut(id).unwrap().extras_mut().mask = Some(Box::new(OpacityMask::new(mask, true)));
            let mut s = Session::new();
            s.add_document(d, None);
            let back = rect(&mut s, 0., 0., 36., 12.);
            let d = std::sync::Arc::make_mut(&mut s.doc_mut().unwrap().doc);
            let node = d.remove(back).unwrap();
            d.insert(d.default_layer(), 0, (*node).clone()).unwrap();
            s.doc_mut().unwrap().selection.set([back]);
            let mut p = player(&mut s);
            under(&mut p, true, 0.);
            let expected = if opacity == 1. { 288. } else { 432. };
            assert!((selected_area(&p) - expected).abs() < 0.01, "ink={samples:?}, opacity={opacity}");
        }
    }
}

#[test]
fn no_trap_inspection_rejects_extra_nested_traps_and_recursive_wrappers() {
    let f = Fixture::new();
    std::fs::write(f.0.join("marked.pdf"), no_trap_pdf_pair().1).unwrap();
    f.pal("Trapping_Supacolour_Standard", "");
    f.pal("wrapper", &a("TrapTask", ""));
    let output = f.0.join("output.pdf");
    std::fs::write(&output, b"previous output").unwrap();
    let mut params = json!({"root": f.0, "lists": ["Trapping_Supacolour_Standard", "TrapTask", "wrapper"],
        "input": f.0.join("marked.pdf"), "output": output});
    for explicit in [false, true] {
        if explicit {
            params["intents"] = json!({"noTrap":{"rectangles":[{"x":5.,"y":5.,"width":20.,"height":20.}]}});
        }
        let err = Session::new().execute("sep.runActionList", &params).unwrap_err();
        assert!(err.to_string().contains("nested trapping"), "{err}");
        assert_eq!(std::fs::read(&output).unwrap(), b"previous output");
    }
    f.pal("wrapper", &a("CallActionList", "<Path>wrapper</Path>"));
    params.as_object_mut().unwrap().remove("intents");
    params["lists"] = json!(["wrapper"]);
    let err = Session::new().execute("sep.runActionList", &params).unwrap_err();
    assert!(err.to_string().contains("recursive PAL call"), "{err}");
    assert_eq!(std::fs::read(&output).unwrap(), b"previous output");
}

#[test]
fn no_trap_inspects_every_intent_and_mark_pal_including_neon() {
    let f = Fixture::new();
    f.pal("prepare", &a("SelectAllObjects", ""));
    f.pal("Trapping_Supacolour_Standard", "");
    let output = f.0.join("output.pdf");
    std::fs::write(&output, b"previous output").unwrap();
    for name in ["metal", "Neon", "TrapTask"] {
        let mut s = Session::new();
        s.execute(
            "document.open",
            &json!({"name":"marked.pdf","dataBase64":vectorcraft_format::base64_encode(&no_trap_pdf_pair().1),"printProduction":true}),
        )
        .unwrap();
        let id = rect(&mut s, 30., 5., 10., 10.);
        s.execute("sep.mark.set", &json!({"ids":[id],"mark":{"type":"metallic","list":format!("folder/{name}")}})).unwrap();
        let bytes = super::super::fileio::encode_with_warnings(
            &s.doc().unwrap().doc,
            "pdf",
            &json!({"preserveEditing":false,"output":{"conversion":"none"}}),
        )
        .unwrap()
        .0;
        std::fs::write(f.0.join("marked.pdf"), bytes).unwrap();
        f.pal("folder/inner", &a("TrapTask", ""));
        for body in [
            a("TrapTask", ""),
            a("CallActionList", "<Path>inner</Path>"),
            a("Conditional", &format!("<Then>{}</Then>", a("CallActionList", "<Path>inner</Path>"))),
            a("Conditional", &format!("<Else>{}</Else>", a("CallActionList", "<Path>inner</Path>"))),
            a("CallActionList", "<Path>Trapping_Supacolour_Standard.PAL</Path>"),
        ] {
            f.pal(&format!("folder/{name}"), &body);
            for explicit in [false, true] {
                let mut params = json!({"root":f.0,"lists":["prepare"],"input":f.0.join("marked.pdf"),"output":output});
                if explicit {
                    params["lists"] = json!(["prepare", "Trapping_Supacolour_Standard", "TrapTask"]);
                    params["intents"] = json!({"noTrap":{},"metallic":[{"list":format!("folder/{name}")}]});
                }
                let err = Session::new().execute("sep.runActionList", &params).unwrap_err();
                assert!(err.to_string().contains("nested trapping"), "{name}, explicit={explicit}: {err}");
                assert_eq!(std::fs::read(&output).unwrap(), b"previous output");
            }
        }
        // Real metallic PALs set trapping parameters without executing TrapTask.
        // That alone must still work alongside NoTrap in a Light recipe.
        f.pal(&format!("folder/{name}"), &a("SetTrapTags", &trap_ticket("Always")));
        let result = Session::new()
            .execute("sep.runActionList", &json!({"root":f.0,"lists":["prepare"],"input":f.0.join("marked.pdf"),"output":f.0.join("allowed.pdf")}))
            .unwrap();
        assert_eq!(result["status"], "completed", "{result}");
        assert!(result["trace"].as_array().unwrap().iter().any(|step| step["action"] == "SetTrapTags"));
    }
}

#[test]
fn destructive_shaper_refuses_raster_paint_without_mutation_or_publication() {
    let f = Fixture::new();
    image::RgbaImage::from_pixel(8, 8, image::Rgba([255, 0, 0, 255])).save(f.0.join("red.png")).unwrap();
    for operation in ["Unite", "Intersect", "Exclude", "MinusFront", "MinusBack"] {
        for grouped in [false, true] {
            let mut s = Session::new();
            s.execute("document.open", &json!({"path": f.0.join("red.png")})).unwrap();
            let image = selection::all_objects(&s.doc().unwrap().doc, false)[0];
            let vector = rect(&mut s, 20., 20., 10., 10.);
            s.doc_mut().unwrap().selection.set([vector, image]);
            if grouped {
                step(&mut player(&mut s), "GroupObjects", "").unwrap();
            }
            let before = s.doc().unwrap().doc.clone();
            let selected = s.doc().unwrap().selection.objects.clone();
            let params = format!("<Operation>{operation}</Operation><WorkOnCopy>0</WorkOnCopy>");
            let err = step(&mut player(&mut s), "Shaper", &params).unwrap_err();
            assert!(err.to_string().contains("cannot preserve raster paint"), "{err}");
            assert_eq!(s.doc().unwrap().doc, before);
            assert_eq!(s.doc().unwrap().selection.objects, selected);
        }
        f.pal("main", &[a("SelectAllObjects", ""), a("Shaper", &format!("<Operation>{operation}</Operation><WorkOnCopy>0</WorkOnCopy>"))].concat());
        let output = f.0.join("output.pdf");
        for existing in [false, true] {
            if existing {
                std::fs::write(&output, b"previous output").unwrap();
            }
            let result = Session::new()
                .execute("sep.runActionList", &json!({"root": f.0, "lists": ["main"], "input": f.0.join("red.png"), "output": output}))
                .unwrap();
            assert_eq!(result["status"], "stopped", "{result}");
            assert!(result.to_string().contains("cannot preserve raster paint"), "{result}");
            if existing {
                assert_eq!(std::fs::read(&output).unwrap(), b"previous output");
                std::fs::remove_file(&output).unwrap();
            } else {
                assert!(!output.exists());
            }
        }
    }
    // The copy workflow keeps the pixels and selects geometry for subsequent PAL repainting.
    let mut s = Session::new();
    s.execute("document.open", &json!({"path": f.0.join("red.png")})).unwrap();
    let id = selection::all_objects(&s.doc().unwrap().doc, false)[0];
    let original = s.doc().unwrap().doc.node(id).unwrap().clone();
    s.doc_mut().unwrap().selection.set([id]);
    step(&mut player(&mut s), "Shaper", "<Operation>Unite</Operation><WorkOnCopy>1</WorkOnCopy>").unwrap();
    assert_eq!(s.doc().unwrap().doc.node(id), Some(&original));
    assert!(selected_area(&player(&mut s)) > 0.);
}

#[test]
fn rasterize_stops_on_spot_decode_limit_without_losing_ink() {
    let (d, id) = image_document(vectorcraft_testkit::tiff::oversized_spot_image());
    let mut s = Session::new();
    s.add_document(d, None);
    s.doc_mut().unwrap().selection.set([id]);
    let before = s.doc().unwrap().doc.clone();
    let err = super::super::sep_trap::rasterize(&mut s, &[id]).unwrap_err();
    assert!(err.to_string().contains("decoded ink bytes"), "{err}");
    assert_eq!(s.doc().unwrap().doc, before);
    assert_eq!(s.doc().unwrap().selection.objects, [id]);
}
