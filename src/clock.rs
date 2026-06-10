//! Simulation clock — global "what time is it in the world we're
//! rendering" state.
//!
//! v1 is the stripped slice of plan 0010 that plan 0004's satellite
//! overlay depends on: a `SimClock` that advances with the monotonic
//! wall clock at a chosen `rate`. No play/pause UI, no scrubber, no
//! `LayerTick` trait yet — those land when the full plan 0010 ships.
//!
//! Convention: `sim_unix_s` is **UTC seconds** (UNIX time). sgp4
//! propagation tolerates UT1 ≈ UTC offset; for an orbital-trace
//! visualisation that's accurate to ~few-km, this is fine. Internal
//! advancement uses a monotonic delta (`Instant::now()` native,
//! `performance.now()` web) so NTP corrections and sleep/wake
//! don't inject jumps.

/// One source of truth for "what time is the simulation showing."
#[derive(Copy, Clone, Debug)]
pub struct SimClock {
    /// Simulation time in UNIX seconds. Always advances forward
    /// (under positive `rate`); never re-sampled from the wall
    /// clock after construction.
    sim_unix_s: f64,
    /// Last monotonic-time value the clock was stepped with.
    /// Subtracting the new value gives the wall-clock delta the
    /// clock applies (scaled by `rate`) to `sim_unix_s`.
    last_mono_s: f64,
    /// Playback rate. `1.0` = real time, `0.0` = paused, `60.0` =
    /// 60× real time. Negative values reverse — useful for
    /// satellite-orbit replay.
    rate: f64,
}

impl SimClock {
    /// Construct a clock starting at `sim_unix_s` with playback
    /// rate `rate`. `mono_now_s` is the caller's current monotonic
    /// time in seconds (e.g. `Instant::now()` converted, or
    /// `performance.now() / 1000.0`).
    pub fn new(sim_unix_s: f64, mono_now_s: f64, rate: f64) -> SimClock {
        SimClock {
            sim_unix_s,
            last_mono_s: mono_now_s,
            rate,
        }
    }

    /// Advance the simulation by the monotonic delta since the last
    /// `step` call, scaled by `rate`. Idempotent on the same
    /// `mono_now_s` value (delta is zero).
    pub fn step(&mut self, mono_now_s: f64) {
        let delta = mono_now_s - self.last_mono_s;
        self.last_mono_s = mono_now_s;
        self.sim_unix_s += delta * self.rate;
    }

    /// Current simulation time in UNIX seconds (UTC).
    pub fn sim_unix_s(&self) -> f64 {
        self.sim_unix_s
    }

    /// Playback rate. `1.0` = real time.
    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// Set the playback rate. Used by the not-yet-built time slider
    /// UI (plan 0010 M1) and by the native `--sim-rate` flag.
    pub fn set_rate(&mut self, rate: f64) {
        self.rate = rate;
    }

    /// Jump the simulation directly to `sim_unix_s` without
    /// advancing wall-clock state.
    pub fn set_sim(&mut self, sim_unix_s: f64) {
        self.sim_unix_s = sim_unix_s;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn step_at_rate_1_advances_sim_by_real_delta() {
        let mut c = SimClock::new(1_000_000.0, 0.0, 1.0);
        c.step(5.0);
        assert!(close(c.sim_unix_s(), 1_000_005.0, 1e-9));
    }

    #[test]
    fn step_at_rate_60_advances_sim_by_60x_real_delta() {
        let mut c = SimClock::new(0.0, 0.0, 60.0);
        c.step(1.0);
        assert!(close(c.sim_unix_s(), 60.0, 1e-9));
    }

    #[test]
    fn step_at_rate_zero_pauses_sim() {
        let mut c = SimClock::new(42.0, 0.0, 0.0);
        c.step(100.0);
        c.step(200.0);
        assert!(close(c.sim_unix_s(), 42.0, 1e-9));
    }

    #[test]
    fn step_is_idempotent_on_same_mono_time() {
        let mut c = SimClock::new(0.0, 10.0, 1.0);
        c.step(15.0);
        c.step(15.0);
        c.step(15.0);
        assert!(close(c.sim_unix_s(), 5.0, 1e-9));
    }

    #[test]
    fn set_sim_jumps_without_advancing_wallclock_state() {
        let mut c = SimClock::new(0.0, 10.0, 1.0);
        c.set_sim(123_456.0);
        c.step(15.0); // 5s of monotonic delta
        assert!(close(c.sim_unix_s(), 123_461.0, 1e-9));
    }

    #[test]
    fn negative_rate_runs_backwards() {
        let mut c = SimClock::new(1_000.0, 0.0, -1.0);
        c.step(10.0);
        assert!(close(c.sim_unix_s(), 990.0, 1e-9));
    }
}
