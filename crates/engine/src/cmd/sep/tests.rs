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
        trap_footprints: BTreeMap::new(),
        messages: vec![],
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
    let bytes = std::fs::read(f.0.join("output.pdf")).unwrap();
    assert!(vectorcraft_pdf::import(&bytes).is_ok());
    assert!(vectorcraft_pdf::editing(&bytes).unwrap().intact, "preparation output must retain editable object names");
    let reopened = s.execute("document.open", &json!({"path":f.0.join("output.pdf")})).unwrap();
    assert_eq!(reopened["restored"], true);
    assert!(s.doc().unwrap().doc.swatches.iter().any(|w| w.spot && w.name == "Base ink"));
    assert!(String::from_utf8_lossy(&bytes).contains("/PALInk0 [/Separation /#42#61#73#65#20#69#6E#6B"));
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
        assert!(f.0.join("output.pdf").exists());
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
