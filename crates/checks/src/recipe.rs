//! Reporter snapshot adapter. Identifiers never select a geometry algorithm.
use crate::{Obligation, Owner};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Garment {
    Dark,
    Light,
}
impl Garment {
    pub fn matches(self, value: &str) -> bool {
        value == "any"
            || value
                == match self {
                    Self::Dark => "dark",
                    Self::Light => "light",
                }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub kind: String,
    pub key: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub rule_key: String,
    pub title: String,
    pub garment: String,
    pub scope: String,
    pub operation: String,
    pub sources: Vec<Source>,
    pub protection: Vec<Source>,
    pub distance_mm: Option<f64>,
    pub join: Option<String>,
    pub instruction: String,
    pub purpose: String,
    pub evidence: String,
    pub confirmation: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileLink {
    pub key: String,
    pub garment: String,
    pub source_rule: String,
    pub name_mode: String,
    pub separation_name: Option<String>,
    pub pdf_layer: String,
    pub destination: String,
    pub overprint: bool,
    pub layer_order: u32,
    pub object_group: String,
    pub alternate_colour: String,
    pub confirmation: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub id: u64,
    pub revision: u64,
    pub process: String,
    pub region: String,
    pub version: u32,
    pub status: String,
    pub required_screens: Option<Vec<String>>,
    pub profile: String,
    pub evidence: String,
    pub rules: Vec<Rule>,
    pub file_links: Vec<FileLink>,
    pub foundation: Option<Value>,
    pub approved_by: Option<String>,
    pub approved_at: Option<String>,
}
impl Recipe {
    pub fn enumerate(&self, garment: Garment, out: &mut Vec<Obligation>) -> Result<(), String> {
        if self.rules.len() > 100 || self.file_links.len() > 100 {
            return Err("Recipe exceeds contract bounds".into());
        }
        // Validate duplicate identities before filtering: an inactive duplicate still corrupts a snapshot.
        let rules: BTreeSet<_> = self.rules.iter().map(|r| &r.rule_key).collect();
        let links: BTreeSet<_> = self.file_links.iter().map(|l| &l.key).collect();
        if rules.len() != self.rules.len() || links.len() != self.file_links.len() {
            return Err("Duplicate recipe key".into());
        }
        if self.rules.iter().any(|r| !["any", "dark", "light"].contains(&r.garment.as_str()))
            || self.file_links.iter().any(|l| !["any", "dark", "light"].contains(&l.garment.as_str()))
        {
            return Err("Unknown garment must not erase obligations".into());
        }
        for r in self.rules.iter().filter(|r| garment.matches(&r.garment)) {
            let facets: &[&str] =
                if r.operation == "policy" { &["policy"] } else { &["applicability", "sources", "geometry", "paint", "topology", "protection"] };
            for facet in facets {
                out.push(Obligation {
                    key: format!("rule/{}/{facet}", r.rule_key),
                    area: if r.operation == "policy" { "Assignments" } else { "Recipe relationships" }.into(),
                    label: format!("{}: {facet}", r.title),
                    rule: Some(r.rule_key.clone()),
                    file_link: None,
                    plate: None,
                    owner: if *facet == "applicability" || *facet == "policy" { Owner::Operator } else { Owner::Engineering },
                    expected: json!({"rule":r,"treatment":self.foundation.as_ref().and_then(|f| f.get("treatments")).and_then(|t| t.get(&r.rule_key)),
                        "policy":self.foundation.as_ref().and_then(|f| f.get("policies")).and_then(|t| t.get(&r.rule_key))}),
                    conditional: true,
                });
            }
        }
        for l in self.file_links.iter().filter(|l| garment.matches(&l.garment)) {
            for facet in ["contribution", "identity", "layer", "order", "overprint", "coverage"] {
                out.push(Obligation {
                    key: format!("file/{}/{facet}", l.key),
                    area: "File construction".into(),
                    label: format!("{}: {facet}", l.separation_name.as_deref().unwrap_or(&l.pdf_layer)),
                    rule: Some(l.source_rule.clone()),
                    file_link: Some(l.key.clone()),
                    plate: l.separation_name.clone(),
                    owner: Owner::Engineering,
                    expected: json!(l),
                    conditional: self
                        .foundation
                        .as_ref()
                        .and_then(|f| f.pointer("/verification/contributions"))
                        .and_then(|c| c.get(&l.key))
                        .and_then(|c| c.get("emptiness"))
                        .and_then(Value::as_str)
                        == Some("conditional_on_proven_source"),
                });
            }
        }
        // Legacy inventory is never a second editable source. Until migrated, it still creates
        // obligations (and the recipe contract remains Incomplete), rather than disappearing.
        let plates = self.foundation.as_ref().and_then(|f| f.pointer("/verification/plates")).and_then(Value::as_array);
        if let Some(plates) = plates {
            for p in plates {
                let g = p.get("garment").and_then(Value::as_str).ok_or("Plate garment missing")?;
                if !["any", "dark", "light"].contains(&g) {
                    return Err("Invalid physical plate garment".into());
                }
                if !garment.matches(g) {
                    continue;
                }
                let key = p.get("key").and_then(Value::as_str).ok_or("Physical plate key missing")?;
                let required = p.get("required").and_then(Value::as_bool).ok_or("Physical plate requiredness missing")?;
                out.push(Obligation {
                    key: format!("plate/{key}"),
                    area: "Physical plates".into(),
                    label: format!("{} physical output", p.get("name").and_then(Value::as_str).unwrap_or(key)),
                    rule: None,
                    file_link: None,
                    plate: p.get("name").and_then(Value::as_str).map(String::from),
                    owner: Owner::Engineering,
                    expected: p.clone(),
                    conditional: !required,
                });
            }
        }
        // An incomplete or contradictory migration cannot erase a legacy mandatory output.
        // Once a matching unconditional plate exists for this garment there is just one slot.
        for name in self.required_screens.iter().flatten() {
            // A fully validated migration can intentionally require a plate for only one
            // garment. Until then the old mandatory declaration remains in the denominator.
            let represented = crate::contract::physical_contract_valid(self);
            if !represented {
                out.push(Obligation {
                    key: format!("legacy-plate/{name}"),
                    area: "Physical plates".into(),
                    label: format!("{name} required physical plate"),
                    rule: None,
                    file_link: None,
                    plate: Some(name.clone()),
                    owner: Owner::RecipeOwner,
                    expected: json!({"name":name,"destination":"Screen","required":true,"migration_required":true}),
                    conditional: false,
                });
            }
        }
        Ok(())
    }

    /// Contract failures never remove slots. Unknown operations and unmet implementation support
    /// are reported separately from production truth; this audit does not pretend to measure art.
    pub fn contract_issues(&self, garment: Garment) -> Vec<String> {
        let mut issues = vec![];
        if self.status != "approved"
            || self.approved_by.as_ref().is_none_or(String::is_empty)
            || self.approved_at.as_ref().is_none_or(String::is_empty)
        {
            issues.push("Published recipe with approval provenance required".into());
        }
        let missing = Value::Null;
        let f = self.foundation.as_ref().unwrap_or(&missing);
        if f.is_null() {
            issues.push("Executable foundation missing".into());
        }
        if f.get("version").and_then(Value::as_u64) != Some(1) {
            issues.push("Unqualified foundation version".into());
        }
        if f.pointer("/verification/version").and_then(Value::as_u64) != Some(1) {
            issues.push("Physical-plate and contribution requiredness contract missing".into());
        }
        issues.extend(crate::contract::issues(self));
        let active: BTreeMap<_, _> = self.rules.iter().filter(|r| garment.matches(&r.garment)).map(|r| (r.rule_key.as_str(), r)).collect();
        for r in active.values() {
            if r.confirmation.is_some() {
                issues.push(format!("{}: unresolved confirmation", r.rule_key));
            }
            if !["preserve", "combine", "spread", "choke", "policy"].contains(&r.operation.as_str()) {
                issues.push(format!("{}: unknown operation {}", r.rule_key, r.operation));
            }
            if ["spread", "choke"].contains(&r.operation.as_str())
                && (r.distance_mm.is_none_or(|n| !n.is_finite() || n < 0.0) || r.join.as_deref() != Some("round"))
            {
                issues.push(format!("{}: confirmed distance and round join required", r.rule_key));
            }
            let section = if r.operation == "policy" { "policies" } else { "treatments" };
            if f.get(section).and_then(|v| v.get(&r.rule_key)).is_none() {
                issues.push(format!("{}: executable definition missing", r.rule_key));
            }
            for s in r.sources.iter().chain(&r.protection) {
                if s.kind == "rule" && !active.contains_key(s.key.as_str()) {
                    issues.push(format!("{}: missing active dependency {}", r.rule_key, s.key));
                }
                if s.kind == "artwork" && f.get("selections").and_then(|s2| s2.get(&s.key)).is_none() {
                    issues.push(format!("{}: missing selection {}", r.rule_key, s.key));
                }
            }
        }
        for l in self.file_links.iter().filter(|l| garment.matches(&l.garment)) {
            if !active.contains_key(l.source_rule.as_str()) {
                issues.push(format!("{}: source rule missing", l.key));
            }
            if l.confirmation.is_some() {
                issues.push(format!("{}: unresolved file requirement", l.key));
            }
        }
        issues
    }
}
