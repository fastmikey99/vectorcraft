//! Editable job intent shared by the checker and a future trap maker. No ticket/layer workaround.
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrapMode {
    Trap,
    NoTrap,
    Mixed,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrapRegion {
    /// Zero-based page, top-left origin, millimetres.
    pub page: usize,
    pub bounds_mm: [f64; 4],
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrappingIntent {
    pub mode: TrapMode,
    /// None means unknown legacy mixed exclusions, not an empty known selection.
    pub exclusions: Option<Vec<TrapRegion>>,
    /// Persistent native object IDs. Geometry is editable with the object; never a bounding-box surrogate.
    #[serde(default)]
    pub objects: Vec<u64>,
}
impl TrappingIntent {
    pub fn validate(&self) -> Result<(), String> {
        if self.objects.len() > 200 || self.objects.iter().any(|id| *id > 9_007_199_254_740_991) {
            return Err("No trap selection exceeds the object budget".into());
        }
        if let Some(regions) = &self.exclusions
            && (regions.len() > 100
                || regions.iter().any(|r| {
                    let [x0, y0, x1, y1] = r.bounds_mm;
                    r.page > 9999 || !r.bounds_mm.iter().all(|n| n.is_finite() && n.abs() <= 1e6) || x1 <= x0 || y1 <= y0
                }))
        {
            return Err("Invalid No trap region".into());
        }
        Ok(())
    }
}
