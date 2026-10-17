//! Reporter recipe names, shown with native panel widgets. Eyes affect preview only.
use crate::{VectorcraftApp, theme::Tokens, widgets};
use egui::{Sense, Ui, vec2};
use serde_json::{Value, json};
use vectorcraft_engine::cmd::production_separations::{self as production, Destination, Row};
use vectorcraft_render::proof;

fn run(app: &mut VectorcraftApp, command: &str, params: Value) {
    if let Err(e) = app.run(command, params) {
        app.ui.status = e;
    }
}
fn rows(app: &VectorcraftApp, ctx: &egui::Context) -> Vec<Row> {
    let Some(st) = app.session.active() else { return vec![] };
    let key = (st.uid, st.revision);
    let id = egui::Id::new("production-separations-rows");
    if let Some((old, rows)) = ctx.data(|d| d.get_temp::<((u64, u64), Vec<Row>)>(id))
        && old == key
    {
        return rows;
    }
    let rows = production::rows(&app.session);
    ctx.data_mut(|d| d.insert_temp(id, (key, rows.clone())));
    rows
}
pub fn show(app: &mut VectorcraftApp, ui: &mut Ui) {
    let Some(st) = app.session.active() else {
        widgets::dim_label(ui, tl!("No document open."));
        return;
    };
    let hidden = st.production_separations.hidden.clone();
    let selected = st.selection.objects.clone();
    let visible_plates = st.production_separations.plates.clone();
    let overprint = st.production_separations.overprint;
    let rows = rows(app, ui.ctx());
    let t = Tokens::get(ui.ctx());
    let view = proof::view();
    let all: Vec<String> = rows.iter().flat_map(|r| r.plates.clone()).collect();
    let shown = visible_plates.as_ref().or(view.separations.as_ref()).unwrap_or(&all);
    ui.horizontal(|ui| {
        widgets::dim_label(ui, tl!("Preview"));
        if widgets::flat_button(ui, tl!("Show all"), 76.0).clicked() {
            run(app, "production.separations.preview", json!({"reset":true}));
        }
    });
    if widgets::check(ui, "Overprint Preview", overprint, true) {
        run(app, "production.separations.preview", json!({"overprint": !overprint}));
    }
    for destination in [Some(Destination::Indigo), Some(Destination::Screen), None] {
        let group: Vec<_> = rows.iter().filter(|r| r.destination == destination).collect();
        if group.is_empty() {
            continue;
        }
        widgets::section_header(
            ui,
            match destination {
                Some(Destination::Indigo) => "Indigo",
                Some(Destination::Screen) => "Screen",
                None => tl!("Unassigned"),
            },
        );
        widgets::list_box(ui, |ui| {
            for row in group {
                ui.push_id(&row.name, |ui| {
                    let visible = row.plates.iter().all(|p| shown.contains(p));
                    ui.horizontal(|ui| {
                        if widgets::toggle_icon(ui, "eye", visible, 18.0, tl!("Show/hide ink (Alt-click: isolate)")) {
                            let plates: Vec<_> = if ui.input(|i| i.modifiers.alt) {
                                row.plates.clone()
                            } else if visible {
                                shown.iter().filter(|p| !row.plates.contains(p)).cloned().collect()
                            } else {
                                all.iter().filter(|p| shown.contains(p) || row.plates.contains(p)).cloned().collect()
                            };
                            run(app, "production.separations.preview", json!({"plates":plates}));
                        }
                        let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                        let q = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
                        ui.painter().rect_filled(rect, 2.0, egui::Color32::from_rgb(q(row.rgb[0]), q(row.rgb[1]), q(row.rgb[2])));
                        let is_selected = !row.ids.is_empty() && row.ids.iter().all(|id| selected.contains(id));
                        if ui
                            .selectable_label(is_selected, egui::RichText::new(&row.name).size(12.0).color(t.text))
                            .on_hover_text(tl!("Select whole objects containing this ink."))
                            .clicked()
                        {
                            run(app, "production.separations.select", json!({"name":row.name}));
                        }
                    });
                    for (name, ids) in &row.components {
                        ui.horizontal(|ui| {
                            ui.add_space(22.0);
                            let visible = ids.iter().any(|id| !hidden.contains(id));
                            if widgets::toggle_icon(ui, "eye", visible, 18.0, tl!("Show/hide group in preview")) {
                                run(app, "production.separations.preview", json!({"component":{"ink":row.name,"name":name}}));
                            }
                            if ui
                                .selectable_label(ids.iter().all(|id| selected.contains(id)), name)
                                .on_hover_text(tl!("Show all components and select this group"))
                                .clicked()
                            {
                                run(app, "production.separations.select", json!({"name":row.name,"component":name}));
                            }
                        });
                    }
                });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn production_panel_uses_native_layout_and_composite_process_row() {
        let mut app = VectorcraftApp::new(vectorcraft_engine::Session::new(), Default::default());
        app.session.execute("file.new", &json!({})).unwrap();
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| show(&mut app, ui));
        fn text(s: &egui::Shape, out: &mut Vec<String>) {
            match s {
                egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| text(s, out)),
                _ => {}
            }
        }
        output.textures_delta.clear();
        let mut labels = vec![];
        for s in &output.shapes {
            text(&s.shape, &mut labels);
        }
        assert!(labels.iter().any(|s| s == "Indigo"));
        assert!(labels.iter().any(|s| s == "CMYK"));
        assert!(!labels.iter().any(|s| ["Cyan", "Magenta", "Yellow", "Black"].contains(&s.as_str())));
    }
}
