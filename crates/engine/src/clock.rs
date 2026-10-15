//! A monotonic clock for time budgets (`lineweight.check`'s `timeLimit`). Native builds read the
//! system's; the web build has none of its own, so its shell gives one at start-up
//! ([`set_web_clock`]): without it, budgets there aren't enforced.

#[cfg(target_arch = "wasm32")]
static WEB_CLOCK: std::sync::OnceLock<fn() -> f64> = std::sync::OnceLock::new();

/// The web shell's clock: milliseconds from any fixed point (`Date.now()`, `performance.now()`).
/// The first one given stays.
pub fn set_web_clock(clock: fn() -> f64) {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = WEB_CLOCK.set(clock);
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = clock;
}

/// Milliseconds from a fixed point, or `None` where there is no clock.
pub fn now_ms() -> Option<f64> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        Some(START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64() * 1000.0)
    }
    #[cfg(target_arch = "wasm32")]
    {
        WEB_CLOCK.get().map(|c| c())
    }
}
