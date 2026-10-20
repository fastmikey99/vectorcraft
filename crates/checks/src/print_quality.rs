//! Operator print questions. No publication, baseline, receipt or renderer-certification prerequisites.
use crate::{Evidence, Finding, Obligation, Owner, Report, Request, Status, Validity};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use vectorcraft_pdf::{PrintColorSpace, PrintOrder};

pub fn enumerate(r: &Request) -> Result<Vec<Obligation>, String> {
    if r.contract_version != crate::CONTRACT_VERSION || r.run_id.is_empty() {
        return Err("Unsupported check request".into());
    }
    // Structural validation only; publication and approval are irrelevant to print measurement.
    let mut structure = vec![];
    r.recipe.enumerate(r.garment, &mut structure)?;
    let mut out = vec![];
    let mut add = |key: String, label: String, expected: Value, rule: Option<String>| {
        out.push(Obligation {
            key,
            area: "Print quality".into(),
            label,
            expected,
            rule,
            file_link: None,
            plate: None,
            owner: Owner::Operator,
            conditional: false,
        });
    };
    add(
        "print/job".into(),
        "Process and Dark/Light selection".into(),
        json!({"process":r.recipe.process,"region":r.recipe.region,"garment":r.garment}),
        None,
    );
    add("print/inks".into(), "Physical inks and Indigo/Screen grouping".into(), json!(r.recipe.file_links), None);
    if r.recipe.file_links.iter().any(|l| r.garment.matches(&l.garment) && l.name_mode == "source") {
        add(
            "print/source-inks".into(),
            "Customer PMS and custom ink names".into(),
            r.acceptance.get("source_ink_names").map(|f| f.value.clone()).unwrap_or(Value::Null),
            None,
        );
    }
    add(
        "print/dimensions".into(),
        "Ordered artwork dimensions".into(),
        r.acceptance.get("artwork_dimensions_mm").map(|f| f.value.clone()).unwrap_or(Value::Null),
        None,
    );
    add(
        "print/detail".into(),
        "Minimum printable detail and gaps".into(),
        json!({"minimum_detail_mm":r.acceptance.get("minimum_detail_mm"),"minimum_gap_mm":r.acceptance.get("minimum_gap_mm")}),
        None,
    );
    for l in r.recipe.file_links.iter().filter(|l| r.garment.matches(&l.garment)) {
        add(format!("print/file/{}/paint", l.key), format!("{}: ink and overprint", l.object_group_if_empty()), json!(l), None);
    }
    for rule in r.recipe.rules.iter().filter(|x| r.garment.matches(&x.garment) && x.operation != "preserve" && x.rule_key != "profile") {
        add(format!("observed/rule/{}/relationship", rule.rule_key), rule.title.clone(), json!(rule), Some(rule.rule_key.clone()));
    }
    if let Some(plates) = r.recipe.foundation.as_ref().and_then(|f| f.pointer("/verification/plates")).and_then(Value::as_array) {
        for p in plates.iter().filter(|p| p.get("garment").and_then(Value::as_str).is_some_and(|g| r.garment.matches(g))) {
            if let Some(key) = p.get("key").and_then(Value::as_str) {
                add(
                    format!("print/plate/{key}"),
                    format!("{}: physical plate", p.get("name").and_then(Value::as_str).unwrap_or("Indigo")),
                    p.clone(),
                    None,
                );
            }
        }
    }
    let declared: BTreeSet<_> = out
        .iter()
        .filter(|o| o.key.starts_with("print/plate/"))
        .filter_map(|o| o.expected.get("name").and_then(Value::as_str))
        .map(String::from)
        .collect();
    for name in r.recipe.required_screens.iter().flatten().filter(|n| !declared.contains(*n)) {
        out.push(Obligation {
            key: format!("print/plate/required-{name}"),
            area: "Print quality".into(),
            label: format!("{name}: required physical plate"),
            rule: None,
            file_link: None,
            plate: Some(name.clone()),
            owner: Owner::Operator,
            expected: json!({"name":name,"name_mode":"exact","destination":"Screen","required":true}),
            conditional: false,
        });
    }
    crate::trapping::enumerate(r, &mut out);
    let keys: BTreeSet<_> = out.iter().map(|o| &o.key).collect();
    if keys.len() != out.len() {
        return Err("Duplicate print question".into());
    }
    Ok(out)
}
impl crate::recipe::FileLink {
    fn object_group_if_empty(&self) -> &str {
        self.separation_name.as_deref().unwrap_or(&self.pdf_layer)
    }
}
pub fn begin(r: &Request) -> Result<Report, String> {
    let findings = enumerate(r)?
        .into_iter()
        .map(|obligation| Finding {
            obligation,
            status: Status::Incomplete,
            reason: "This print question has not been measured yet".into(),
            observed: Value::Null,
            evidence: vec![],
            location: None,
            uncertainty_mm: None,
        })
        .collect();
    let mut report = Report {
        contract_version: crate::CONTRACT_VERSION,
        run_id: r.run_id.clone(),
        identity: r.identity.clone(),
        status: Status::Incomplete,
        validity: Validity::Provisional,
        coverage: Default::default(),
        findings,
    };
    report.recount();
    Ok(report)
}
pub fn validate_report(r: &Request, report: &Report) -> Result<(), String> {
    crate::validate_report_against(r, report, enumerate(r)?)
}
fn record(report: &mut Report, key: &str, status: Status, reason: impl Into<String>, observed: Value) -> Result<(), String> {
    report.record(
        key,
        status,
        reason,
        observed,
        vec![Evidence {
            artifact_sha256: report.identity.pdf_sha256.clone(),
            producer: "checks/print-quality-1".into(),
            detail: "Measured PDF print question; recipe publication and original artwork approval are not prerequisites".into(),
        }],
    )
}
#[derive(Clone, Debug, serde::Serialize)]
struct Event {
    page: usize,
    order: usize,
    layer: String,
    inks: Vec<String>,
    process: bool,
    overprint: bool,
    image: bool,
    positive: Option<bool>,
    ink_tints: BTreeMap<String, f64>,
    attribution_known: bool,
}
fn events(order: &PrintOrder) -> Result<Vec<Event>, String> {
    let mut out = vec![];
    let mut name_bytes = 0usize;
    let mut charge = |n: usize| -> Result<(), String> {
        name_bytes = name_bytes.saturating_add(n);
        if name_bytes > 8 * 1024 * 1024 { Err("Paint ink/layer names exceed the 8 MiB observation budget".into()) } else { Ok(()) }
    };
    for (page, paths) in order.pages.iter().enumerate() {
        let images = order.images.get(page).map(Vec::as_slice).unwrap_or(&[]);
        let mut images = images.iter().peekable();
        for index in 0..=paths.len() {
            while images.peek().is_some_and(|im| im.before_path == index) {
                let Some(im) = images.next() else { break };
                charge(
                    im.description
                        .colourants_hex
                        .iter()
                        .map(String::len)
                        .sum::<usize>()
                        .saturating_add(im.layer.and_then(|i| order.layers.get(i)).map_or(0, String::len)),
                )?;
                let inks = im.description.colourants_hex.iter().filter_map(|s| decode_hex(s)).collect::<Vec<_>>();
                let process =
                    inks.is_empty() && ["DeviceCMYK", "DeviceRGB", "DeviceGray", "ICCBased"].iter().any(|s| im.description.colour_space.contains(s));
                let attribution_known =
                    !im.description.stencil && (process || (!inks.is_empty() && inks.len() == im.description.colourants_hex.len()));
                out.push(Event {
                    page,
                    order: out.len(),
                    layer: im.layer.and_then(|i| order.layers.get(i)).cloned().unwrap_or_default(),
                    inks,
                    process,
                    overprint: im.overprint,
                    image: true,
                    positive: None,
                    ink_tints: BTreeMap::new(),
                    attribution_known,
                });
            }
            let Some(layer) = paths.get(index) else { continue };
            let Some(source) = order.source_indices.get(page).and_then(|p| p.get(index)).and_then(|i| order.sources.get(*i)) else { continue };
            charge(
                source
                    .colorants
                    .iter()
                    .map(|n| n.len().saturating_mul(2))
                    .sum::<usize>()
                    .saturating_add(layer.and_then(|i| order.layers.get(i)).map_or(0, String::len)),
            )?;
            let positive = source.components.as_ref().map(|c| match source.color_space {
                PrintColorSpace::DeviceRgb => c.iter().any(|v| *v < 1.0),
                // PDF DeviceGray 1 is paper white.
                PrintColorSpace::DeviceGray => c.iter().any(|v| *v < 1.0),
                _ => c.iter().any(|v| *v > 0.0),
            });
            out.push(Event {
                page,
                order: out.len(),
                layer: layer.and_then(|i| order.layers.get(i)).cloned().unwrap_or_default(),
                inks: source.colorants.clone(),
                process: matches!(source.color_space, PrintColorSpace::DeviceGray | PrintColorSpace::DeviceRgb | PrintColorSpace::DeviceCmyk),
                overprint: source.overprint,
                image: false,
                positive,
                ink_tints: source
                    .components
                    .as_ref()
                    .filter(|c| c.len() == source.colorants.len())
                    .map(|c| source.colorants.iter().cloned().zip(c.iter().copied()).collect())
                    .unwrap_or_default(),
                attribution_known: source.color_space != PrintColorSpace::Other,
            });
        }
    }
    Ok(out)
}
fn decode_hex(s: &str) -> Option<String> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let bytes = s
        .as_bytes()
        .chunks_exact(2)
        .map(|c| {
            let a = char::from(*c.first()?).to_digit(16)?;
            let b = char::from(*c.get(1)?).to_digit(16)?;
            Some((a * 16 + b) as u8)
        })
        .collect::<Option<Vec<_>>>()?;
    String::from_utf8(bytes).ok()
}
fn matches(l: &crate::recipe::FileLink, p: &Event) -> bool {
    crate::routes::layer_matches(&l.pdf_layer, &p.layer)
        && match l.name_mode.as_str() {
            "exact" => l.separation_name.as_ref().is_some_and(|n| p.inks.contains(n)),
            "source" => true,
            _ => false,
        }
}
fn raw_checks(r: &Request, order: &PrintOrder, report: &mut Report) -> Result<bool, String> {
    let events = match events(order) {
        Ok(events) => events,
        Err(reason) => {
            for f in report.findings.iter_mut().filter(|f| {
                f.obligation.key.starts_with("print/file/")
                    || f.obligation.key.starts_with("print/plate/")
                    || f.obligation.key == "print/inks"
                    || f.obligation.key == "print/source-inks"
            }) {
                f.reason = reason.clone();
            }
            return Ok(false);
        }
    };
    let links: Vec<_> = r.recipe.file_links.iter().filter(|l| r.garment.matches(&l.garment)).collect();
    // These diagnostics concern coverage, not the names/state/order of recorded paint operators.
    // Other diagnostics may omit operators, so absence and whole-file successes remain unknown.
    let limitations: Vec<_> = order
        .issues
        .iter()
        .filter(|s| {
            !matches!(
                s.as_str(),
                "Image sample, mask and compositing coverage requires independent reconciliation"
                    | "Page transparency group colour space requires compositing reconciliation"
                    | "Form transparency group colour space requires compositing reconciliation"
                    | "Duplicate PDF layer names cannot establish unique routing"
            )
        })
        .collect();
    let unknown = !limitations.is_empty();
    let attribution_unknown = events.iter().any(|p| !p.attribution_known);
    if events.len().saturating_mul(links.len()).saturating_mul(links.len()).saturating_mul(order.pages.len().max(1)) > 20_000_000 {
        for f in report.findings.iter_mut().filter(|f| {
            f.obligation.key.starts_with("print/file/") || f.obligation.key.starts_with("print/plate/") || f.obligation.key == "print/inks"
        }) {
            f.reason = "Too many output/event combinations for this worker; ink and order observations could not finish".into();
        }
        return Ok(false);
    }
    let wrong: Vec<_> = events
        .iter()
        .filter(|p| {
            p.attribution_known
                && (!links.iter().any(|l| matches(l, p))
                    || p.inks.iter().any(|ink| {
                        !links.iter().any(|l| {
                            crate::routes::layer_matches(&l.pdf_layer, &p.layer)
                                && (l.name_mode == "source" || l.separation_name.as_ref() == Some(ink))
                        })
                    }))
        })
        .collect();
    let inks: BTreeSet<_> = events.iter().flat_map(|p| p.inks.iter()).collect();
    record(
        report,
        "print/inks",
        if !wrong.is_empty() && !unknown && !links.is_empty() {
            Status::Fail
        } else if unknown || attribution_unknown || events.is_empty() || links.is_empty() {
            Status::Incomplete
        } else {
            Status::Pass
        },
        if links.is_empty() {
            "The selected recipe has no ink/output routing facts; observed inks cannot yet be judged"
        } else if !wrong.is_empty() {
            "Paint uses an undeclared ink or layer route; check it against the selected process recipe"
        } else if attribution_unknown {
            "Some image/stencil paint inherits an unresolved ink; its exact ink name cannot yet be checked"
        } else if unknown {
            "Observed inks retained; some PDF operators could not be read, so unexpected inks cannot yet be excluded"
        } else {
            "Recorded paint uses the recipe's exact ink names and Indigo/Screen routes; numeric White Underprint suffixes are identification hints"
        },
        json!({"inks":inks,"paint_events":events.len(),"unexpected":wrong.iter().take(32).collect::<Vec<_>>(),"limitations":limitations,"routes":links.iter().map(|l|json!({"ink":l.separation_name,"layer":l.pdf_layer,"destination":l.destination})).collect::<Vec<_>>() }),
    )?;
    if report.findings.iter().any(|f| f.obligation.key == "print/source-inks") {
        let actual: BTreeSet<_> = events
            .iter()
            .filter(|p| links.iter().any(|l| l.name_mode == "source" && matches(l, p)))
            .flat_map(|p| p.inks.iter())
            .filter(|n| !["Cyan", "Magenta", "Yellow", "Black", "None", "All"].contains(&n.as_str()))
            .cloned()
            .collect();
        let expected = r
            .acceptance
            .get("source_ink_names")
            .and_then(|f| f.value.as_array())
            .filter(|a| a.iter().all(Value::is_string))
            .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect::<BTreeSet<_>>());
        record(
            report,
            "print/source-inks",
            match &expected {
                Some(e) if !unknown && !attribution_unknown => {
                    if e == &actual {
                        Status::Pass
                    } else {
                        Status::Fail
                    }
                }
                _ => Status::Incomplete,
            },
            if attribution_unknown {
                "Some image/stencil paint has unresolved ink attribution; observed names are partial"
            } else if expected.is_none() {
                "Observed PMS/custom names retained; the job's intended source ink list was not supplied"
            } else if unknown {
                "PMS/custom comparison is limited by unread PDF operators"
            } else {
                "PMS/custom names compared exactly with the job; no spelling or colour-name normalization"
            },
            json!({"observed_names":actual,"expected_names":expected}),
        )?;
    }
    for l in &links {
        let matched: Vec<_> = events.iter().filter(|p| matches(l, p)).collect();
        let unresolved_route = events.iter().any(|p| !p.attribution_known && crate::routes::layer_matches(&l.pdf_layer, &p.layer));
        let bad: Vec<_> = matched
            .iter()
            .filter(|p| p.overprint != l.overprint && !links.iter().any(|other| matches(other, p) && other.overprint != l.overprint))
            .collect();
        let ambiguous = matched
            .iter()
            .any(|p| links.iter().any(|other| matches(other, p) && (other.overprint != l.overprint || other.destination != l.destination)));
        // Recorded drawing positions are information, not a print requirement.
        // Layer-panel order and physical press sequence are separate concepts.
        let paint_sequence: Vec<_> = (0..order.pages.len())
            .filter_map(|page| {
                let positions: Vec<_> = matched.iter().filter(|p| p.page == page).map(|p| p.order).collect();
                (!positions.is_empty()).then(|| json!({"page":page,"first":positions.iter().min(),"last":positions.iter().max()}))
            })
            .collect();
        record(
            report,
            &format!("print/file/{}/paint", l.key),
            if !bad.is_empty() {
                Status::Fail
            } else if unknown || unresolved_route || matched.is_empty() || ambiguous {
                Status::Incomplete
            } else {
                Status::Pass
            },
            if !bad.is_empty() {
                "Overprint flag differs from the recipe; review this print setting"
            } else if matched.is_empty() {
                "This output was not observed; its recipe contribution may be conditional"
            } else if unresolved_route {
                "A stencil or image on this output has unresolved inherited ink; its overprint cannot yet be attributed"
            } else if ambiguous {
                "The recipe gives this observed ink conflicting output roles or overprint settings"
            } else if unknown {
                "Observed ink and flags retained; unread PDF operators prevent checking the entire output"
            } else {
                "Exact ink route and overprint flags match the recipe"
            },
            json!({"expected_ink":l.separation_name,"expected_overprint":l.overprint,"observed_events":matched.len(),"wrong_flags":bad.iter().take(32).collect::<Vec<_>>(),"image_events":matched.iter().filter(|p|p.image).count(),"paint_sequence":paint_sequence,"authoring_order_hint":l.layer_order,"limitations":limitations}),
        )?;
    }
    let plates: Vec<_> = report
        .findings
        .iter()
        .filter(|f| f.obligation.key.starts_with("print/plate/"))
        .map(|f| (f.obligation.key.clone(), f.obligation.expected.clone()))
        .collect();
    for (key, plate) in plates {
        let name = plate.get("name").and_then(Value::as_str);
        let members: Vec<_> = plate.get("links").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
        let objects: Vec<_> = events
            .iter()
            .filter(|p| {
                if plate.get("name_mode") == Some(&json!("exact")) {
                    name.is_some_and(|n| p.inks.iter().any(|i| i == n))
                } else {
                    links.iter().any(|l| members.contains(&l.key.as_str()) && matches(l, p))
                }
            })
            .collect();
        let required = plate.get("required") == Some(&json!(true));
        // A painted operator is evidence of a plate declaration, not a surviving pixel after knockout.
        record(
            report,
            &key,
            if objects.is_empty() && required && !unknown && !attribution_unknown { Status::Fail } else { Status::Incomplete },
            if objects.is_empty() && required && !unknown && !attribution_unknown {
                "Required physical plate has no recorded paint"
            } else if objects.is_empty() {
                "Conditional plate not observed; artwork may not require it"
            } else if objects.iter().all(|p| p.positive != Some(true)) {
                "Plate is declared; nonzero image/ink coverage could not be established"
            } else {
                "Nonzero paint is addressed to this physical plate; edge coverage is checked separately"
            },
            json!({"name":name,"destination":plate.get("destination"),"required":required,"paint_events":objects.len(),"limitations":limitations}),
        )?;
    }
    Ok(true)
}
pub fn check(r: &Request, bytes: &[u8]) -> Result<Report, String> {
    let mut report = begin(r)?;
    if bytes.len() > 256 * 1024 * 1024 {
        return Err("PDF exceeds the worker's 256 MiB input budget".into());
    }
    let job = r.acceptance.get("job_selection").map(|f| &f.value);
    let selection = json!({"process":r.recipe.process,"region":r.recipe.region,"garment":r.garment});
    record(
        &mut report,
        "print/job",
        if job == Some(&selection) {
            Status::Pass
        } else if job.is_some() {
            Status::Fail
        } else {
            Status::Incomplete
        },
        if job == Some(&selection) {
            "Recipe process, region and Dark/Light match the job"
        } else if job.is_some() {
            "Selected recipe variant differs from the job"
        } else {
            "Checking the selected recipe variant; the job's process, region and Dark/Light have not been supplied"
        },
        selection,
    )?;
    match vectorcraft_pdf::print_order(bytes) {
        Ok(order) => {
            if !raw_checks(r, &order, &mut report)? {
                for f in report.findings.iter_mut().filter(|f| f.reason == "This print question has not been measured yet") {
                    f.reason = "PDF observation resource budget exceeded before geometry could be decoded".into();
                }
                return Ok(report);
            }
        }
        Err(e) => {
            for f in report.findings.iter_mut().filter(|f| {
                f.obligation.key.starts_with("print/file/") || f.obligation.key.starts_with("print/plate/") || f.obligation.key == "print/inks"
            }) {
                f.reason = format!("Cannot read the PDF ink operators: {e}");
            }
            for f in report.findings.iter_mut().filter(|f| f.reason == "This print question has not been measured yet") {
                f.reason = format!("PDF interpretation stopped before this measurement: {e}");
            }
            return Ok(report);
        }
    }
    // Every rule has a concrete print question even when its source selection is unavailable.
    for rule in r.recipe.rules.iter().filter(|x| r.garment.matches(&x.garment) && x.operation != "preserve" && x.rule_key != "profile") {
        record(
            &mut report,
            &format!("observed/rule/{}/relationship", rule.rule_key),
            Status::Incomplete,
            format!(
                "{}: {}. This relationship needs the relevant artwork regions or separate contributors; the final sep does not identify them yet",
                rule.title, rule.instruction
            ),
            json!({"operation":rule.operation,"target_mm":rule.distance_mm,"sources":rule.sources,"protected_edges":rule.protection}),
        )?;
    }
    let imported = vectorcraft_pdf::import_with_report(
        bytes,
        &vectorcraft_pdf::ImportOptions {
            preserve_clip_paths: true,
            reject_depth_loss: true,
            text_as: vectorcraft_pdf::TextAs::Outlines,
            ..Default::default()
        },
    );
    match imported {
        Ok(imported) => {
            let mut census = crate::pdf::census(&imported.document);
            let (warnings, _) = crate::pdf::import_messages(imported.warnings);
            census.unresolved.extend(warnings);
            let _ = crate::ordering::try_reconcile(bytes, &mut census);
            crate::ordering::reconcile_practical_pages(bytes, &mut census);
            dimensions(r, &census, &mut report)?;
            crate::observed::practical_plate_coverage(r, &census, &mut report)?;
            let detail = crate::print_detail::measure(r, &census);
            record(&mut report, "print/detail", detail.0, detail.1, detail.2)?;
            crate::observed::practical_relationships(r, &census, &mut report)?;
            crate::trapping::check(r, &census, &mut report)?;
        }
        Err(e) => {
            for f in report.findings.iter_mut().filter(|f| {
                f.obligation.key == "print/dimensions" || f.obligation.key == "print/detail" || f.obligation.key.starts_with("observed/rule/")
            }) {
                f.reason = format!("Cannot measure {} because PDF geometry could not be decoded: {e}", f.obligation.label);
            }
        }
    }
    report.recount();
    Ok(report)
}
fn dimensions(r: &Request, c: &crate::pdf::Census, report: &mut Report) -> Result<(), String> {
    let mut bounds: Vec<Option<[f64; 4]>> = vec![None; c.pages];
    let artwork_links: Vec<_> = r
        .recipe
        .file_links
        .iter()
        .filter(|l| {
            r.garment.matches(&l.garment)
                && r.recipe.rules.iter().any(|rule| {
                    rule.rule_key == l.source_rule
                        && rule.operation == "preserve"
                        && rule.protection.is_empty()
                        && r.recipe
                            .foundation
                            .as_ref()
                            .and_then(|f| f.get("treatments"))
                            .and_then(|t| t.get(&rule.rule_key))
                            .is_some_and(|t| t.get("select") == Some(&json!([[]])) && t.get("result") == Some(&json!("full_shape")))
                })
        })
        .collect();
    for p in &c.paints {
        if !artwork_links.is_empty() && !artwork_links.iter().any(|l| crate::routes::paint_matches(l, p)) {
            continue;
        }
        if p.tint == Some(0.0) {
            continue;
        }
        if let (Some(page), Some(b)) = (
            p.page,
            p.geometry_mm.as_ref().filter(|g| !g.is_empty()).map(|g| {
                use vectorcraft_geom::Shape;
                let b = g.to_bezpath().bounding_box();
                [b.x0, b.y0, b.x1, b.y1]
            }),
        ) && let Some(slot) = bounds.get_mut(page)
        {
            *slot = Some(match *slot {
                None => b,
                Some(a) => [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])],
            });
        }
    }
    let measured: Vec<_> = bounds.iter().map(|b| b.map(|b| [b[2] - b[0], b[3] - b[1]])).collect();
    let target = r.acceptance.get("artwork_dimensions_mm").map(|f| &f.value);
    let tolerance = r.acceptance.get("geometry_tolerance_mm").and_then(|f| f.value.as_f64()).filter(|v| v.is_finite() && *v >= 0.0);
    let erasure_possible = c.paints.len() > 1000
        || c.paints.iter().any(|a| {
            artwork_links.iter().any(|l| crate::routes::paint_matches(l, a))
                && c.paints.iter().any(|b| {
                    b.page == a.page
                        && b.order > a.order
                        && (!b.overprint || (b.tint == Some(0.0) && b.ink == a.ink) || (b.process && b.overprint_zero))
                        && a.bounds_mm.zip(b.bounds_mm).is_some_and(|(a, b)| a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3])
                })
        });
    let complete = !artwork_links.is_empty()
        && !erasure_possible
        && c.paint_order_verified
        && c.image_objects == 0
        && c.unresolved.is_empty()
        && !c.budget_exhausted
        && c.paints.iter().all(|p| p.geometry_mm.is_some() && p.page.is_some());
    let target = target
        .and_then(Value::as_array)
        .filter(|a| a.len() == 2)
        .and_then(|a| Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?]))
        .filter(|a| a.iter().all(|n| n.is_finite() && *n > 0.0));
    let status = if complete && measured.len() == 1 {
        match (target, tolerance, measured.first().copied().flatten()) {
            (Some(t), Some(tol), Some(m)) => {
                if (t[0] - m[0]).abs() <= tol && (t[1] - m[1]).abs() <= tol {
                    Status::Pass
                } else {
                    Status::Fail
                }
            }
            _ => Status::Incomplete,
        }
    } else {
        Status::Incomplete
    };
    record(
        report,
        "print/dimensions",
        status,
        if c.paints.len() > 1000 {
            "Vector bounds measured; the final-extent comparison exceeds the 1000-object compositing budget"
        } else if erasure_possible {
            "Vector bounds measured, but later knockout or zero-tint paint may reduce the final artwork extent"
        } else if !complete {
            "Vector bounds measured; images or unresolved geometry prevent complete artwork dimensions"
        } else if target.is_none() {
            "Artwork bounds measured; ordered width and height were not supplied"
        } else if tolerance.is_none() {
            "Artwork dimensions measured against the order; dimensional tolerance was not supplied"
        } else {
            "Artwork width and height compared with the order"
        },
        json!({"artwork_dimensions_mm":measured,"page_sizes_mm":c.page_sizes_mm,"ordered_mm":target,"tolerance_mm":tolerance,"complete_artwork_bounds":complete,"scope":if artwork_links.is_empty(){"all separation vector bounds; artwork is not separately identified"}else{"declared whole-artwork output footprint"}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pdf(ink: &str) -> Vec<u8> {
        pdf_stream(ink, "/OC /L BDC /GS gs /Ink cs 1 scn 10 10 50 50 re f EMC")
    }
    fn pdf_stream(ink: &str, stream: &str) -> Vec<u8> {
        crate::pdf::tests::objects_pdf(&[
            "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [6 0 R] /D << /ON [6 0 R] >> >> >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /Properties << /L 6 0 R >> /ColorSpace << /Ink [/Separation /{ink} /DeviceCMYK 5 0 R] >> /ExtGState << /GS 7 0 R >> >> /Contents 4 0 R >>"
            ),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
            "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 1 0 0] /N 1 >>".into(),
            "<< /Type /OCG /Name (Layer) >>".into(),
            "<< /Type /ExtGState /op true /OP true /OPM 1 >>".into(),
        ])
    }
    fn request() -> Request {
        let mut r = crate::attribution::tests::request();
        r.baseline = None;
        r.geometry = None;
        r.recipe.status = "draft".into();
        r.recipe.approved_at = None;
        r.recipe.approved_by = None;
        r.identity.recipe_sha256 = "informational-old-recipe".into();
        r.identity.baseline_sha256 = "".into();
        r.identity.measurement_sha256 = "".into();
        r.input_class = None;
        r
    }
    #[test]
    fn sep_only_checks_find_wrong_ink_without_certification_or_approval() {
        let r = request();
        let good = check(&r, &pdf("Exact#20ink")).unwrap();
        let bad = check(&r, &pdf("Wrong")).unwrap();
        let status = |r: &Report| r.findings.iter().find(|f| f.obligation.key == "print/inks").unwrap().status;
        assert_eq!(status(&good), Status::Pass);
        assert_eq!(status(&bad), Status::Fail);
        let mut undeclared = r.clone();
        undeclared.recipe.file_links.clear();
        assert_eq!(status(&check(&undeclared, &pdf("Exact#20ink")).unwrap()), Status::Incomplete);
        assert!(good.findings.iter().all(|f| !f.obligation.key.starts_with("global/")));
        validate_report(&r, &good).unwrap();
        let mut omitted = good.clone();
        omitted.findings.pop();
        assert!(validate_report(&r, &omitted).is_err());
    }
    #[test]
    fn changing_publication_baseline_and_hashes_cannot_change_print_verdicts() {
        let r = request();
        let a = check(&r, &pdf("Exact#20ink")).unwrap();
        let mut other = r.clone();
        other.recipe.status = "approved".into();
        other.recipe.approved_by = Some("irrelevant".into());
        other.identity.recipe_sha256 = "other".into();
        other.identity.engine_version = "previous".into();
        let b = check(&other, &pdf("Exact#20ink")).unwrap();
        assert_eq!(a.findings.iter().map(|f| f.status).collect::<Vec<_>>(), b.findings.iter().map(|f| f.status).collect::<Vec<_>>());
    }
    #[test]
    fn dimensions_keep_values_without_inventing_tolerance() {
        let r = request();
        let report = check(&r, &pdf("Exact#20ink")).unwrap();
        let f = report.findings.iter().find(|f| f.obligation.key == "print/dimensions").unwrap();
        assert_eq!(f.status, Status::Incomplete);
        assert!(f.observed["artwork_dimensions_mm"][0][0].as_f64().unwrap() > 17.0);
        assert!(f.observed["tolerance_mm"].is_null());
    }
    #[test]
    fn wrong_choke_is_detected_and_unset_acceptance_keeps_the_measurement() {
        fn pair(inset_mm: f64, image_page: bool) -> Vec<u8> {
            let d = inset_mm * 72.0 / 25.4;
            let stream = format!(
                "/OC /L BDC /GS gs /A cs 1 scn 10 10 50 50 re f /B cs 1 scn {} {} {} {} re f EMC",
                10.0 + d,
                10.0 + d,
                50.0 - 2.0 * d,
                50.0 - 2.0 * d
            );
            let mut objects = vec![
                "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [6 0 R] /D << /ON [6 0 R] >> >> >>".into(),
                if image_page { "<< /Type /Pages /Kids [3 0 R 8 0 R] /Count 2 >>".into() } else { "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into() },
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /Properties << /L 6 0 R >> /ColorSpace << /A [/Separation /Exact#20ink /DeviceCMYK 5 0 R] /B [/Separation /Blocker /DeviceCMYK 5 0 R] >> /ExtGState << /GS 7 0 R >> >> /Contents 4 0 R >>".into(),
                format!("<< /Length {} >>\nstream\n{stream}\nendstream",stream.len()),
                "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 1 0 0] /N 1 >>".into(),
                "<< /Type /OCG /Name (Layer) >>".into(),"<< /Type /ExtGState /op true /OP true /OPM 1 >>".into(),
            ];
            if image_page {
                let image = "/OC /L BDC q 10 0 0 10 0 0 cm /Im Do Q EMC";
                objects.extend([
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /Properties << /L 6 0 R >> /XObject << /Im 10 0 R >> >> /Contents 9 0 R >>".into(),
                    format!("<< /Length {} >>\nstream\n{image}\nendstream",image.len()),
                    "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /ASCIIHexDecode /Length 3 >>\nstream\n80>\nendstream".into(),
                ]);
            }
            crate::pdf::tests::objects_pdf(&objects)
        }
        let mut r = request();
        r.recipe.rules[0].operation = "preserve".into();
        let mut target = r.recipe.rules[0].clone();
        target.rule_key = "blocker".into();
        target.operation = "choke".into();
        target.distance_mm = Some(0.2);
        target.join = Some("round".into());
        target.protection.clear();
        target.sources = vec![crate::recipe::Source { kind: "rule".into(), key: r.recipe.rules[0].rule_key.clone() }];
        r.recipe.rules.push(target);
        let mut link = r.recipe.file_links[0].clone();
        link.key = "blocker-output".into();
        link.source_rule = "blocker".into();
        link.separation_name = Some("Blocker".into());
        r.recipe.file_links.push(link);
        let f = r.recipe.foundation.as_mut().unwrap();
        f["treatments"]["blocker"] = f["treatments"]["arbitrary"].clone();
        f["outputs"]["blocker-output"] = json!({"select":"all","emit":"when_nonempty"});
        r.acceptance.insert(
            "geometry_tolerance_mm".into(),
            crate::preparation::Fact {
                value: json!(0.01),
                provenance: crate::preparation::Provenance::File { artifact_sha256: "fixture".into(), locator: "test-only tolerance".into() },
            },
        );
        let good = check(&r, &pair(0.2, false)).unwrap();
        let bad = check(&r, &pair(1.0, false)).unwrap();
        let find = |out: Report| out.findings.into_iter().find(|f| f.obligation.key == "observed/rule/blocker/relationship").unwrap();
        let good = find(good);
        let bad = find(bad);
        assert_eq!(good.status, Status::Pass, "{} {}", good.reason, good.observed);
        assert_eq!(bad.status, Status::Fail, "{} {}", bad.reason, bad.observed);
        let partial = find(check(&r, &pair(0.2, true)).unwrap());
        assert_eq!(partial.status, Status::Incomplete, "{} {}", partial.reason, partial.observed);
        assert_eq!(partial.observed["partial_pages"], true);
        assert!(partial.observed["measurements"][0]["target_boundary_to_source_gap"].is_object());
        let partial_bad = find(check(&r, &pair(1.0, true)).unwrap());
        assert_eq!(partial_bad.status, Status::Fail, "{} {}", partial_bad.reason, partial_bad.observed);
        r.acceptance.clear();
        let unset = find(check(&r, &pair(1.0, false)).unwrap());
        assert_eq!(unset.status, Status::Incomplete);
        assert!(unset.observed["measurements"][0]["target_boundary_to_source_gap"].is_object());
    }
    #[test]
    fn devicen_checks_every_ink_and_expansion_has_a_byte_limit() {
        let r = request();
        let mut order = vectorcraft_pdf::print_order(&pdf("Exact#20ink")).unwrap();
        order.sources[0].color_space = PrintColorSpace::DeviceN;
        order.sources[0].colorants = vec!["Exact ink".into(), "Unexpected".into()];
        order.sources[0].components = Some(vec![0.0, 1.0]);
        let mut report = begin(&r).unwrap();
        raw_checks(&r, &order, &mut report).unwrap();
        assert_eq!(report.findings.iter().find(|f| f.obligation.key == "print/inks").unwrap().status, Status::Fail);
        order.sources[0].colorants = vec!["x".repeat(65536)];
        order.pages[0] = vec![Some(0); 1000];
        order.source_indices[0] = vec![0; 1000];
        assert!(events(&order).unwrap_err().contains("8 MiB"));
    }
    #[test]
    fn required_screens_do_not_depend_on_foundation_migration() {
        let mut r = request();
        r.recipe.foundation = None;
        r.recipe.required_screens = Some(vec!["Missing".into()]);
        let report = check(&r, &pdf("Exact#20ink")).unwrap();
        assert_eq!(report.findings.iter().find(|f| f.obligation.key == "print/plate/required-Missing").unwrap().status, Status::Fail);
    }
    #[test]
    fn unresolved_stencil_ink_is_not_a_wrong_ink_defect() {
        let r = request();
        let bytes =
            pdf_stream("Exact#20ink", "/OC /L BDC /GS gs /Ink cs 1 scn q 10 0 0 10 0 0 cm BI /W 1 /H 1 /IM true /BPC 1 /F /AHx ID 80> EI Q EMC");
        let report = check(&r, &bytes).unwrap();
        let finding = report.findings.iter().find(|f| f.obligation.key == "print/inks").unwrap();
        assert_eq!(finding.status, Status::Incomplete, "{} {}", finding.reason, finding.observed);
        assert!(report.findings.iter().filter(|f| f.obligation.key.starts_with("print/plate/")).all(|f| f.status != Status::Fail));
    }
    #[test]
    fn dimensions_measure_clipped_artwork_not_the_unclipped_object() {
        let mut r = request();
        r.recipe.rules[0].operation = "preserve".into();
        let bytes = pdf_stream("Exact#20ink", "/OC /L BDC /GS gs 10 10 25 50 re W n /Ink cs 1 scn 10 10 50 50 re f EMC");
        let report = check(&r, &bytes).unwrap();
        let f = report.findings.iter().find(|f| f.obligation.key == "print/dimensions").unwrap();
        let width = f.observed["artwork_dimensions_mm"][0][0].as_f64().unwrap();
        assert!((width - 25.0 * 25.4 / 72.0).abs() < 0.0001, "{width}");
    }
    fn threshold(r: &mut Request, key: &str, n: f64) {
        r.acceptance.insert(
            key.into(),
            crate::preparation::Fact {
                value: json!(n),
                provenance: crate::preparation::Provenance::File { artifact_sha256: "synthetic".into(), locator: "test-only requirement".into() },
            },
        );
    }
    #[test]
    fn supported_detail_can_pass_and_unrelated_image_page_does_not_hide_a_defect() {
        let mut r = request();
        threshold(&mut r, "minimum_detail_mm", 1.0);
        threshold(&mut r, "minimum_gap_mm", 0.1);
        let report = check(&r, &pdf("Exact#20ink")).unwrap();
        let detail = |r: Report| r.findings.into_iter().find(|f| f.obligation.key == "print/detail").unwrap();
        let good = detail(report);
        assert_eq!(good.status, Status::Pass, "{} {}", good.reason, good.observed);
        let first = "/OC /L BDC /GS gs /Ink cs 1 scn 10 10 50 50 re f EMC";
        for image in [true, false] {
            let second = if image { "/OC /I BDC q 10 0 0 10 0 0 cm /Im Do Q EMC" } else { "/OC /I BDC /GS gs /Ink cs 0.5 scn 10 10 50 50 re f EMC" };
            let bytes=crate::pdf::tests::objects_pdf(&[
            "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [6 0 R 11 0 R] /D << /ON [6 0 R 11 0 R] >> >> >>".into(),
            "<< /Type /Pages /Kids [3 0 R 8 0 R] /Count 2 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /Properties << /L 6 0 R >> /ColorSpace << /Ink [/Separation /Exact#20ink /DeviceCMYK 5 0 R] >> /ExtGState << /GS 7 0 R >> >> /Contents 4 0 R >>".into(),
            format!("<< /Length {} >>\nstream\n{first}\nendstream",first.len()),
            "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 1 0 0] /N 1 >>".into(),
            "<< /Type /OCG /Name (Layer) >>".into(),"<< /Type /ExtGState /op true /OP true /OPM 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /Properties << /I 11 0 R >> /ColorSpace << /Ink [/Separation /Exact#20ink /DeviceCMYK 5 0 R] >> /ExtGState << /GS 7 0 R >> /XObject << /Im 10 0 R >> >> /Contents 9 0 R >>".into(),
            format!("<< /Length {} >>\nstream\n{second}\nendstream",second.len()),
            "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /ASCIIHexDecode /Length 3 >>\nstream\n80>\nendstream".into(),
            "<< /Type /OCG /Name (Unrelated image) >>".into(),
        ]);
            threshold(&mut r, "minimum_detail_mm", 20.0);
            let bad = detail(check(&r, &bytes).unwrap());
            assert_eq!(bad.status, Status::Fail, "{} {}", bad.reason, bad.observed);
        }
    }
    #[test]
    fn authoring_order_is_information_but_required_ink_loss_is_a_defect() {
        fn ordered(reverse: bool, panel_reverse: bool, erase: bool) -> Vec<u8> {
            let a = "/OC /L BDC /A cs 1 scn 10 10 50 50 re f EMC";
            let b = if erase { "/OC /R BDC /B cs 1 scn 10 10 50 50 re f EMC" } else { "/OC /R BDC /B cs 1 scn 70 70 5 5 re f EMC" };
            let stream = if reverse { format!("{b} {a}") } else { format!("{a} {b}") };
            let panel = if panel_reverse { "8 0 R 6 0 R" } else { "6 0 R 8 0 R" };
            crate::pdf::tests::objects_pdf(&[
                format!("<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [6 0 R 8 0 R] /D << /ON [6 0 R 8 0 R] /Order [{panel}] >> >> >>"),
                "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /Properties << /L 6 0 R /R 8 0 R >> /ColorSpace << /A [/Separation /Exact#20ink /DeviceCMYK 5 0 R] /B [/Separation /Other /DeviceCMYK 5 0 R] >> >> /Contents 4 0 R >>".into(),
                format!("<< /Length {} >>\nstream\n{stream}\nendstream",stream.len()),
                "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 1 0 0] /N 1 >>".into(),
                "<< /Type /OCG /Name (Layer) >>".into(),
                "null".into(),
                "<< /Type /OCG /Name (Other layer) >>".into(),
            ])
        }
        let mut r = request();
        r.recipe.file_links[0].overprint = false;
        let mut other = r.recipe.file_links[0].clone();
        other.key = "other-output".into();
        other.pdf_layer = "Other layer".into();
        other.separation_name = Some("Other".into());
        other.layer_order += 1;
        r.recipe.file_links.push(other);
        let plate = |out: &Report| out.findings.iter().find(|f| f.obligation.key == "print/plate/dark-output").unwrap().status;
        for reverse in [false, true] {
            for panel_reverse in [false, true] {
                let out = check(&r, &ordered(reverse, panel_reverse, false)).unwrap();
                assert!(out.findings.iter().all(|f| !f.obligation.key.ends_with("/order") && f.status != Status::Fail), "{:?}", out.findings);
                assert_eq!(plate(&out), Status::Pass);
                assert!(out.findings.iter().find(|f| f.obligation.key == "print/file/output/paint").unwrap().observed["paint_sequence"].is_array());
            }
        }
        let erased = check(&r, &ordered(false, false, true)).unwrap();
        assert_eq!(plate(&erased), Status::Fail, "{:?}", erased.findings);
        // Reversing actual overlapping knockout paint restores the required ink.
        assert_eq!(plate(&check(&r, &ordered(true, false, true)).unwrap()), Status::Pass);
    }
    #[test]
    fn later_erasures_prevent_a_false_dimension_verdict() {
        let mut r = request();
        r.recipe.rules[0].operation = "preserve".into();
        threshold(&mut r, "geometry_tolerance_mm", 0.01);
        let mut size = r.acceptance["geometry_tolerance_mm"].clone();
        size.value = json!([50.0 * 25.4 / 72.0, 50.0 * 25.4 / 72.0]);
        r.acceptance.insert("artwork_dimensions_mm".into(), size);
        let bytes = pdf_stream("Exact#20ink", "/OC /L BDC /GS gs /Ink cs 1 scn 10 10 50 50 re f 0 scn 35 10 25 50 re f EMC");
        let report = check(&r, &bytes).unwrap();
        let f = report.findings.iter().find(|f| f.obligation.key == "print/dimensions").unwrap();
        assert_eq!(f.status, Status::Incomplete);
        assert!(f.reason.contains("knockout"));
    }
    #[test]
    fn unresolved_stencil_does_not_pass_the_known_paths_output_flags() {
        let bytes = pdf_stream(
            "Exact#20ink",
            "/OC /L BDC q /GS gs /Ink cs 1 scn 10 10 50 50 re f Q /Ink cs 1 scn q 10 0 0 10 0 0 cm BI /W 1 /H 1 /IM true /BPC 1 /F /AHx ID 80> EI Q EMC",
        );
        let report = check(&request(), &bytes).unwrap();
        assert_eq!(report.findings.iter().find(|f| f.obligation.key == "print/file/output/paint").unwrap().status, Status::Incomplete);
    }
}
