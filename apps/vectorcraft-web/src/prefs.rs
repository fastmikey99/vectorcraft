//! Preference snapshots shared by the web shell and native bridge regression tests.
use vectorcraft_ui_egui::VectorcraftApp;

const EVERY_MS: f64 = 2000.0;

pub fn json(app: &VectorcraftApp) -> Option<String> {
    // Dialog drafts and transient status messages aren't saved preferences. Use
    // the same normalization as restoration so they don't trigger spurious posts.
    let mut ui = app.ui.clone().sanitized();
    ui.engine_prefs = app.session.prefs.to_json();
    serde_json::to_string(&ui).ok()
}

#[derive(Default)]
pub struct Sync {
    pub saved: Option<String>,
    checked: f64,
}

impl Sync {
    /// Called after the UI frame. OK bypasses polling; workspace changes still poll.
    /// The caller records `saved` only after storage/postMessage succeeds.
    pub fn pending(&mut self, app: &mut VectorcraftApp, now: f64) -> Option<String> {
        let committed = app.take_prefs_changed();
        if !committed && now - self.checked < EVERY_MS {
            return None;
        }
        self.checked = now;
        let text = json(app)?;
        (self.saved.as_deref() != Some(text.as_str())).then_some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use vectorcraft_ui_egui::{Services, prefs_dialog};

    #[test]
    fn ok_posts_committed_units_immediately_without_dirtying_content() {
        let mut app = VectorcraftApp::new(vectorcraft_engine::Session::new(), Services::default());
        app.run("file.new", json!({"width": 100, "height": 100})).unwrap();
        app.session.active_mut().unwrap().mark_saved();
        let mut sync = Sync { saved: json(&app), checked: 2000.0 };
        prefs_dialog::open(&mut app, Some("Units"));
        app.ui.dialog.as_mut().unwrap().fields.insert("unitsGeneral".into(), json!("millimeters"));
        assert!(sync.pending(&mut app, 2100.0).is_none(), "uncommitted dialog fields aren't posted");
        prefs_dialog::confirm(&mut app).unwrap();
        let posted = sync.pending(&mut app, 2101.0).expect("OK bypasses the two-second poll");
        let prefs: Value = serde_json::from_str(&posted).unwrap();
        assert_eq!(prefs["engine_prefs"]["unitsGeneral"], "millimeters");
        assert!(!app.session.active().unwrap().is_dirty());
        sync.saved = Some(posted);
        assert!(sync.pending(&mut app, 4200.0).is_none(), "no duplicate post");
        let mut restored = VectorcraftApp::new(vectorcraft_engine::Session::new(), Services::default());
        restored.ui = serde_json::from_str(sync.saved.as_ref().unwrap()).unwrap();
        prefs_dialog::restore(&mut restored);
        assert_eq!(restored.session.prefs.units_general, "millimeters");
    }

    #[test]
    fn cancel_and_invalid_preferences_do_not_post_changes() {
        let mut app = VectorcraftApp::new(vectorcraft_engine::Session::new(), Services::default());
        app.run("file.new", json!({"width": 100, "height": 100})).unwrap();
        app.session.active_mut().unwrap().mark_saved();
        let mut sync = Sync { saved: json(&app), checked: 0.0 };
        prefs_dialog::open(&mut app, None);
        app.ui.dialog.as_mut().unwrap().fields.insert("unitsGeneral".into(), json!("invalid"));
        assert!(prefs_dialog::confirm(&mut app).is_err());
        assert!(sync.pending(&mut app, 2100.0).is_none());
        app.ui.dialog = None; // Cancel discards the pending fields.
        assert!(sync.pending(&mut app, 4200.0).is_none());
        assert!(!app.session.active().unwrap().is_dirty());
    }
}
