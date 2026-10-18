//! Thin command boundary over the shared checker; all state belongs to the active document.
use super::*;
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use vectorcraft_checks::{Report, Request, Status, Validity, digest, sha256};
use vectorcraft_doc::Document;

#[derive(Debug)]
struct Completion {
    request: Request,
    report: Report,
    document: Arc<Document>,
}
#[derive(Debug)]
struct Pending {
    cancelled: Arc<AtomicBool>,
    result: Arc<Mutex<Option<std::result::Result<Completion, String>>>>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}
#[derive(Clone, Debug)]
struct Remote {
    token: String,
    document: Arc<Document>,
    request: Request,
    diagnostics: Vec<String>,
    dispatched: bool,
}
static NEXT_REMOTE: AtomicUsize = AtomicUsize::new(1);
#[derive(Clone, Debug, Default)]
pub struct State {
    pub request: Option<Request>,
    pub report: Option<Report>,
    pub checked_document: Option<Arc<Document>>,
    pub selected: Option<String>,
    pending: Option<Arc<Pending>>,
    remote: Option<Remote>,
    origin_run_id: String,
    next_run: u64,
    pub error: Option<String>,
}
impl State {
    pub fn validity(&self, doc: &Arc<Document>) -> Validity {
        if (self.next_run == 0 && self.checked_document.is_none()) || self.checked_document.as_ref().is_some_and(|checked| Arc::ptr_eq(checked, doc))
        {
            Validity::Provisional
        } else {
            Validity::Stale
        }
    }
    /// Convert only a current result's page-local millimetres to the editor's document points.
    /// Re-imported object ids are deliberately not used to select native document objects.
    pub fn selected_rect(&self, doc: &Arc<Document>) -> Option<(usize, vectorcraft_geom::Rect)> {
        if self.validity(doc) == Validity::Stale {
            return None;
        }
        let key = self.selected.as_ref()?;
        let location = self.report.as_ref()?.findings.iter().find(|f| &f.obligation.key == key)?.location.as_ref()?;
        let [x0, y0, x1, y1] = location.bounds_mm?;
        if ![x0, y0, x1, y1].iter().all(|n| n.is_finite()) || x1 < x0 || y1 < y0 {
            return None;
        }
        let board = doc.artboards.get(location.page)?;
        let k = 72.0 / 25.4;
        Some((
            location.page,
            vectorcraft_geom::Rect::new(board.rect.x0 + x0 * k, board.rect.y0 + y0 * k, board.rect.x0 + x1 * k, board.rect.y0 + y1 * k),
        ))
    }
    pub fn running(&self) -> bool {
        self.pending.is_some() || self.remote.is_some()
    }
    pub fn remote_token(&self) -> Option<&str> {
        self.remote.as_ref().map(|r| r.token.as_str())
    }
    /// The exact document retained for the in-flight service export.
    pub fn remote_document(&self) -> Option<Arc<Document>> {
        self.remote.as_ref().map(|r| r.document.clone())
    }
    pub fn cancelling(&self) -> bool {
        self.pending.as_ref().is_some_and(|p| p.cancelled.load(Ordering::Acquire))
    }
}
static LIVE_WORKERS: AtomicUsize = AtomicUsize::new(0);
struct WorkerPermit;
impl WorkerPermit {
    fn acquire() -> std::result::Result<Self, String> {
        LIVE_WORKERS
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < 2).then_some(n + 1))
            .map_err(|_| "Two check workers are already finishing; retry after they finish".to_string())?;
        Ok(Self)
    }
}
impl Drop for WorkerPermit {
    fn drop(&mut self) {
        LIVE_WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}
pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "production.checks.configure", "Configure Separation Checks", [], None, "{request: versioned check request} immutable recipe/job/baseline inputs",has_doc,configure),
        cmd!(query "production.checks.run", "Check Separations", [], None, "{pdfOptions?: object,service?: bool} check a snapshot in the background; provisional until exact-byte service verification",has_doc,run),
        cmd!(query "production.checks.complete", "Receive Separation Check Results", [], None, "{token,report? | error?} accept only a matching service snapshot; never assert final validity",has_doc,complete),
        cmd!(query "production.checks.cancel", "Cancel Separation Checks", [], None, "{} discard the pending run without changing artwork",has_doc,cancel),
        cmd!(query "production.checks.list", "Separation Check Results", [], None, "{} full obligation coverage and separate validity",has_doc,list),
        cmd!(query "production.checks.select", "Inspect Separation Finding", [], None, "{key: obligation key} inspect evidenced PDF defect geometry",has_doc,select),
    ]
}
fn configure(s: &mut Session, p: &Value) -> Result<Value> {
    if p.get("request").is_some_and(Value::is_null) {
        s.doc_mut()?.production_checks = State::default();
        return list(s, &json!({}));
    }
    let request: Request =
        serde_json::from_value(p.get("request").cloned().unwrap_or(Value::Null)).map_err(|e| bad("production.checks.configure", e.to_string()))?;
    let mut report = vectorcraft_checks::begin(&request).map_err(|e| bad("production.checks.configure", e))?;
    vectorcraft_checks::preparation::check_inputs(&request, &mut report).map_err(|e| bad("production.checks.configure", e))?;
    let origin_run_id = request.run_id.clone();
    // Dropping a previous pending state cancels it; its completion has no route into this state.
    s.doc_mut()?.production_checks = State { request: Some(request), report: Some(report), origin_run_id, ..Default::default() };
    list(s, &json!({}))
}
fn work(
    mut request: Request,
    doc: Arc<Document>,
    options: Value,
    import_losses: Vec<String>,
    cancelled: &AtomicBool,
) -> std::result::Result<Completion, String> {
    let checkpoint = || if cancelled.load(Ordering::Acquire) { Err("Check cancelled".to_string()) } else { Ok(()) };
    checkpoint()?;
    let (bytes, warnings) = super::fileio::pdf::encode("production.checks.run", &doc, &options).map_err(|e| e.to_string())?;
    checkpoint()?;
    request.identity.document_sha256 = digest(&*doc)?;
    request.identity.pdf_sha256 = sha256(&bytes);
    request.identity.engine_version = vectorcraft_checks::ENGINE_VERSION.into();
    let mut report = vectorcraft_checks::pdf::check(&request, &bytes)?;
    checkpoint()?;
    if !warnings.is_empty() {
        report.record("global/export", Status::Incomplete, "PDF export emitted diagnostics", json!(warnings), vec![])?;
    }
    if !import_losses.is_empty() {
        report.record("global/integrity", Status::Incomplete, "Original import diagnostics remain unresolved", json!(import_losses), vec![])?;
    }
    Ok(Completion { request, report, document: doc })
}
fn run(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "production.checks.run";
    poll_pending(s);
    if s.doc()?.production_checks.running() {
        return Err(bad(C, "A check is already running; cancel it before starting another"));
    }
    let mut request =
        s.doc()?.production_checks.request.clone().ok_or_else(|| bad(C, "Choose a published recipe and preparation baseline through Reporter"))?;
    let doc = s.doc()?.doc.clone();
    let import_losses = s.doc()?.import_losses.clone();
    let options = p.get("pdfOptions").cloned().unwrap_or_else(|| json!({}));
    let state = &mut s.doc_mut()?.production_checks;
    state.checked_document = None;
    state.selected = None;
    state.error = None;
    state.next_run = state.next_run.checked_add(1).ok_or_else(|| bad(C, "Check sequence exhausted; reconfigure the check"))?;
    request.run_id = format!("{}:local:{}", state.origin_run_id, state.next_run);
    state.request = Some(request.clone());
    state.report = Some(vectorcraft_checks::begin(&request).map_err(|e| bad(C, e))?);
    // The browser host transfers one exact export to its service; editing can continue.
    if cfg!(target_arch = "wasm32") || p.get("service").and_then(Value::as_bool) == Some(true) {
        let sequence = NEXT_REMOTE
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
            .map_err(|_| bad(C, "Service check sequence exhausted"))?;
        state.remote = Some(Remote {
            token: format!("{}:service:{sequence}", state.origin_run_id),
            document: doc,
            request,
            diagnostics: import_losses,
            dispatched: false,
        });
        return list(s, &json!({}));
    }
    let permit = WorkerPermit::acquire().map_err(|e| bad(C, e))?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let result = Arc::new(Mutex::new(None));
    let pending = Arc::new(Pending { cancelled: cancelled.clone(), result: result.clone() });
    let scope = crate::file_access::ScopeHandle::capture();
    std::thread::Builder::new()
        .name("separation-check".into())
        .spawn(move || {
            let _permit = permit;
            let completion = scope
                .run(|| crate::guard::catch_panic(|| work(request, doc, options, import_losses, &cancelled)))
                .unwrap_or_else(|e| Err(format!("Internal checker failure: {e}")));
            if let Ok(mut result) = result.lock() {
                *result = Some(completion);
            }
        })
        .map_err(|e| bad(C, format!("Cannot start check worker: {e}")))?;
    s.doc_mut()?.production_checks.pending = Some(pending);
    list(s, &json!({}))
}
fn cancel(s: &mut Session, _: &Value) -> Result<Value> {
    let state = &mut s.doc_mut()?.production_checks;
    if let Some(p) = &state.pending {
        p.cancelled.store(true, Ordering::Release);
    }
    state.remote = None;
    state.checked_document = None;
    state.error = Some("Check cancelled; editing work retained".into());
    list(s, &json!({}))
}
/// Export the frozen service snapshot once. The parent receives raw bytes, never a base64 copy.
/// Source preservation and all pages are mandatory for this production check path.
pub fn take_remote_export(s: &mut Session) -> std::result::Result<Option<(String, Vec<u8>)>, String> {
    let Some(remote) = s.active().and_then(|st| st.production_checks.remote.as_ref()).filter(|r| !r.dispatched).cloned() else {
        return Ok(None);
    };
    let result = super::fileio::pdf::encode(
        "production.checks.run",
        &remote.document,
        &json!({"preset":"Separation Roundtrip","preserveEditing":false,"output":{"conversion":"preserveSource"}}),
    )
    .map_err(|e| e.to_string())
    .and_then(|(bytes, warnings)| {
        if bytes.len() > 64 * 1024 * 1024 { Err("Service check export exceeds the 64 MiB upload budget".into()) } else { Ok((bytes, warnings)) }
    });
    let state = &mut s.doc_mut().map_err(|e| e.to_string())?.production_checks;
    match result {
        Ok((bytes, warnings)) => {
            let Some(pending) = state.remote.as_mut().filter(|r| r.token == remote.token) else {
                return Ok(None);
            };
            pending.dispatched = true;
            pending.diagnostics.extend(warnings);
            let hash = sha256(&bytes);
            pending.request.identity.pdf_sha256 = hash.clone();
            // Reporter binds the submitted document to these exact bytes. The retained Arc
            // separately detects editor changes while the service is running.
            pending.request.identity.document_sha256 = hash;
            state.request = Some(pending.request.clone());
            state.report = Some(vectorcraft_checks::begin(&pending.request)?);
            Ok(Some((remote.token, bytes)))
        }
        Err(error) => {
            state.remote = None;
            state.error = Some(error.clone());
            Err(error)
        }
    }
}
fn complete(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "production.checks.complete";
    let token = p.get("token").and_then(Value::as_str).ok_or_else(|| bad(C, "Service token required"))?;
    let Some(remote) = s.doc()?.production_checks.remote.as_ref().filter(|r| r.token == token && r.dispatched).cloned() else {
        return Ok(json!({"accepted":false,"reason":"Cancelled, superseded or unopened service run"}));
    };
    let result = (|| -> std::result::Result<(Request, Report), String> {
        if let Some(error) = p.get("error").and_then(Value::as_str) {
            return Err(error.chars().take(4000).collect());
        }
        let mut report: Report = serde_json::from_value(p.get("report").cloned().unwrap_or(Value::Null)).map_err(|e| e.to_string())?;
        let mut request = remote.request.clone();
        // The service owns persistent run UUIDs. Correlation is this one-use token;
        // every immutable input and the entire denominator must still match.
        request.run_id = report.run_id.clone();
        vectorcraft_checks::validate_report(&request, &report)?;
        if !remote.diagnostics.is_empty() && report.findings.iter().any(|f| f.obligation.key == "global/export" && f.status != Status::Fail) {
            report.record(
                "global/export",
                Status::Incomplete,
                "Snapshot import/export diagnostics remain unresolved",
                json!(remote.diagnostics),
                vec![],
            )?;
        }
        Ok((request, report))
    })();
    let state = &mut s.doc_mut()?.production_checks;
    state.remote = None;
    match result {
        Ok((request, report)) => {
            state.request = Some(request);
            state.report = Some(report);
            state.checked_document = Some(remote.document);
            state.error = None;
        }
        Err(error) => {
            state.checked_document = None;
            state.error = Some(error);
        }
    }
    list(s, &json!({}))
}
/// Called by the UI frame and result-query command; completions remain attached to their document.
pub fn poll_pending(s: &mut Session) -> bool {
    let ids: Vec<_> = s.documents().iter().filter(|st| st.production_checks.running()).map(|st| st.uid).collect();
    let mut running = false;
    for id in ids {
        let Some(st) = s.document_mut(id) else {
            continue;
        };
        let state = &mut st.production_checks;
        let Some(p) = state.pending.as_ref() else {
            running |= state.remote.is_some();
            continue;
        };
        let cancelled = p.cancelled.load(Ordering::Acquire);
        let completion = match p.result.lock() {
            Ok(mut r) => r.take(),
            Err(_) => Some(Err("Check worker state unavailable".into())),
        };
        match completion {
            None => running = true,
            Some(result) => {
                state.pending = None;
                if cancelled {
                    state.error = Some("Check cancelled; editing work retained".into());
                    continue;
                }
                match result {
                    Ok(c) => {
                        state.request = Some(c.request);
                        state.report = Some(c.report);
                        state.checked_document = Some(c.document);
                        state.error = None;
                    }
                    Err(error) => {
                        state.error = Some(error);
                        state.checked_document = None;
                    }
                }
            }
        }
    }
    running
}
fn list(s: &mut Session, _: &Value) -> Result<Value> {
    poll_pending(s);
    let st = s.doc()?;
    let mut report = st.production_checks.report.clone();
    if let Some(r) = &mut report {
        r.validity = st.production_checks.validity(&st.doc);
    }
    Ok(
        json!({"report":report,"selected":st.production_checks.selected,"running":st.production_checks.running(),"cancelling":st.production_checks.cancelling(),"error":st.production_checks.error}),
    )
}
fn select(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "production.checks.select";
    let key = p.get("key").and_then(Value::as_str).ok_or_else(|| bad(C, "Finding key required"))?;
    let st = s.doc()?;
    let stale = st.production_checks.validity(&st.doc) == Validity::Stale;
    let finding = st
        .production_checks
        .report
        .as_ref()
        .and_then(|r| r.findings.iter().find(|f| f.obligation.key == key))
        .cloned()
        .ok_or_else(|| bad(C, "Unknown finding"))?;
    s.doc_mut()?.production_checks.selected = Some(key.into());
    Ok(json!({"location":if stale{None}else{finding.location.clone()},"finding":finding,"stale":stale}))
}
pub type CheckRequest = Request;
pub fn validate_request(request: &Request) -> std::result::Result<(), String> {
    vectorcraft_checks::enumerate(request).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session() -> Session {
        let mut s = Session::new();
        s.execute("file.new", &json!({"width":100,"height":100})).unwrap();
        let mut request:Request=serde_json::from_value(json!({
            "contract_version":1,"run_id":"native-test","garment":"dark","input_class":null,"baseline":null,"acceptance":{},
            "identity":{"recipe_sha256":"r","baseline_sha256":"b","original_sha256":"o","document_sha256":"d","pdf_sha256":"p","job_revision":"j","assignments_sha256":"a","measurement_sha256":"m","engine_version":vectorcraft_checks::ENGINE_VERSION},
            "recipe":{"id":1,"revision":1,"process":"synthetic","region":"test","version":1,"status":"draft","required_screens":[],"profile":"source","evidence":"synthetic test","rules":[],"file_links":[],"foundation":null,"approved_by":null,"approved_at":null}
        })).unwrap();
        request.identity.recipe_sha256 = digest(&request.recipe).unwrap();
        request.identity.assignments_sha256 = request.assignments_digest().unwrap();
        request.identity.measurement_sha256 = digest(&request.acceptance).unwrap();
        s.execute("production.checks.configure", &json!({"request":request})).unwrap();
        s
    }
    fn finish(s: &mut Session) {
        let start = std::time::Instant::now();
        while poll_pending(s) {
            assert!(start.elapsed() < std::time::Duration::from_secs(15));
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    #[test]
    fn background_check_keeps_editing_and_marks_snapshot_stale() {
        let mut s = session();
        s.execute("shape.rectangle", &json!({"x":1,"y":1,"width":10,"height":10})).unwrap();
        s.execute("production.checks.run", &json!({})).unwrap();
        s.execute("shape.rectangle", &json!({"x":20,"y":20,"width":10,"height":10})).unwrap();
        let current = s.doc().unwrap().doc.clone();
        finish(&mut s);
        let st = s.doc().unwrap();
        assert!(st.production_checks.error.is_none(), "{:?}", st.production_checks.error);
        assert!(Arc::ptr_eq(&current, &st.doc));
        assert_eq!(st.production_checks.validity(&st.doc), Validity::Stale);
        vectorcraft_checks::validate_report(st.production_checks.request.as_ref().unwrap(), st.production_checks.report.as_ref().unwrap()).unwrap();
        let result = s.execute("production.checks.list", &json!({})).unwrap();
        assert_eq!(result["report"]["validity"], "stale");
        assert_ne!(result["report"]["status"], "pass");
    }
    #[test]
    fn cancellation_and_reconfiguration_reject_late_completions() {
        let mut s = session();
        let doc = s.doc().unwrap().doc.clone();
        let request = s.doc().unwrap().production_checks.request.clone().unwrap();
        let result = Arc::new(Mutex::new(None));
        let cancelled = Arc::new(AtomicBool::new(false));
        s.doc_mut().unwrap().production_checks.pending = Some(Arc::new(Pending { result: result.clone(), cancelled: cancelled.clone() }));
        s.execute("production.checks.cancel", &json!({})).unwrap();
        *result.lock().unwrap() =
            Some(Ok(Completion { report: vectorcraft_checks::begin(&request).unwrap(), request: request.clone(), document: doc.clone() }));
        poll_pending(&mut s);
        assert!(cancelled.load(Ordering::Acquire));
        assert!(s.doc().unwrap().production_checks.checked_document.is_none());
        assert!(Arc::ptr_eq(&doc, &s.doc().unwrap().doc));
        let previous = Arc::new(Pending { result: Arc::new(Mutex::new(None)), cancelled: Arc::new(AtomicBool::new(false)) });
        let flag = previous.cancelled.clone();
        let output = previous.result.clone();
        s.doc_mut().unwrap().production_checks.pending = Some(previous);
        s.execute("production.checks.configure", &json!({"request":request})).unwrap();
        assert!(flag.load(Ordering::Acquire));
        *output.lock().unwrap() = Some(Err("old completion".into()));
        poll_pending(&mut s);
        assert!(s.doc().unwrap().production_checks.error.is_none());
    }
    #[test]
    fn preview_state_does_not_change_checked_document_identity() {
        let mut s = session();
        let doc = s.doc().unwrap().doc.clone();
        s.doc_mut().unwrap().production_checks.checked_document = Some(doc);
        s.execute("production.separations.preview", &json!({"reset":true})).unwrap();
        let st = s.doc().unwrap();
        assert_eq!(st.production_checks.validity(&st.doc), Validity::Provisional);
    }
    #[test]
    fn finding_highlight_uses_page_mm_and_disappears_after_edit() {
        let mut s = session();
        let st = s.doc_mut().unwrap();
        st.production_checks.checked_document = Some(st.doc.clone());
        st.production_checks.selected = Some("global/fidelity".into());
        st.production_checks.report.as_mut().unwrap().findings.iter_mut().find(|f| f.obligation.key == "global/fidelity").unwrap().location =
            Some(vectorcraft_checks::Location { page: 0, bounds_mm: Some([1.0, 2.0, 3.0, 4.0]), object_ids: vec![999] });
        let (_, rect) = st.production_checks.selected_rect(&st.doc).unwrap();
        assert!((rect.width() - 2.0 * 72.0 / 25.4).abs() < 1e-10);
        s.execute("shape.rectangle", &json!({"x":1,"y":1,"width":1,"height":1})).unwrap();
        let st = s.doc().unwrap();
        assert!(st.production_checks.selected_rect(&st.doc).is_none());
        let finding = s.execute("production.checks.select", &json!({"key":"global/fidelity"})).unwrap();
        assert_eq!(finding["stale"], true);
        assert!(finding["location"].is_null());
    }
    #[test]
    fn service_snapshot_returns_full_findings_without_losing_edits() {
        let mut s = session();
        s.execute("shape.rectangle", &json!({"x":1,"y":1,"width":10,"height":10})).unwrap();
        s.execute("production.checks.run", &json!({"service":true})).unwrap();
        let (token, bytes) = take_remote_export(&mut s).unwrap().unwrap();
        assert!(take_remote_export(&mut s).unwrap().is_none());
        let mut request = s.doc().unwrap().production_checks.request.clone().unwrap();
        request.run_id = "persisted-service-run".into();
        assert_eq!(request.identity.pdf_sha256, sha256(&bytes));
        let report = vectorcraft_checks::pdf::check(&request, &bytes).unwrap();
        s.execute("shape.rectangle", &json!({"x":20,"y":20,"width":10,"height":10})).unwrap();
        let edited = s.doc().unwrap().doc.clone();
        let result = s.execute("production.checks.complete", &json!({"token":token,"report":report})).unwrap();
        assert_eq!(result["report"]["validity"], "stale");
        assert_eq!(result["running"], false);
        assert!(Arc::ptr_eq(&edited, &s.doc().unwrap().doc));
        assert_eq!(result["report"]["coverage"]["total"], report.coverage.total);
    }
    #[test]
    fn service_tokens_fence_cancelled_reconfigured_and_forged_responses() {
        let mut s = session();
        s.execute("production.checks.run", &json!({"service":true})).unwrap();
        let (old, bytes) = take_remote_export(&mut s).unwrap().unwrap();
        let request = s.doc().unwrap().production_checks.request.clone().unwrap();
        let report = vectorcraft_checks::pdf::check(&request, &bytes).unwrap();
        s.execute("production.checks.cancel", &json!({})).unwrap();
        assert_eq!(s.execute("production.checks.complete", &json!({"token":old,"report":report})).unwrap()["accepted"], false);
        s.execute("production.checks.configure", &json!({"request":request})).unwrap();
        s.execute("production.checks.run", &json!({"service":true})).unwrap();
        let (next, _) = take_remote_export(&mut s).unwrap().unwrap();
        assert_ne!(old, next);
        assert_eq!(s.execute("production.checks.complete", &json!({"token":old,"report":report})).unwrap()["accepted"], false);
        let mut forged = report;
        forged.identity.pdf_sha256 = "wrong bytes".into();
        let result = s.execute("production.checks.complete", &json!({"token":next,"report":forged})).unwrap();
        assert!(result["error"].as_str().unwrap().contains("does not match"));
        assert_eq!(result["running"], false);
        assert!(s.doc().unwrap().production_checks.checked_document.is_none());
        let cleared = s.execute("production.checks.configure", &json!({"request":null})).unwrap();
        assert!(cleared["report"].is_null());
        assert!(!s.doc().unwrap().production_checks.running());
        assert!(s.doc().unwrap().production_checks.request.is_none());
    }
}
