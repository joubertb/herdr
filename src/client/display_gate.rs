//! Holds host resizes while the display topology is transient.
//!
//! When an external monitor sleeps or wakes, macOS briefly parks the host
//! terminal window on the remaining display and then moves it back. Forwarding
//! that parked size resizes every pane, and agents with a live region taller
//! than the pane strand a redrawn copy of it in scrollback at each width.
//!
//! The gate keeps reporting suppressed while fewer displays are awake than
//! before the transition, and after they return until the window is back at
//! its last reported size. Measured on macOS 26 with iTerm2 (herdr pane
//! capture, 2026-10-18): the external display dropped off 2.4s before the
//! window shrank, and the window stayed narrow 1.2s after the display returned.

use std::time::{Duration, Instant};

/// A display missing this long, while another one is awake, is treated as a
/// real unplug, so the terminal follows the window onto the remaining screen.
pub(super) const UNPLUG_FALLBACK: Duration = Duration::from_secs(10);

/// ASSUMPTION: after the displays return, the host moves the window back within
/// this window (observed: 1.2s). If it takes longer, the parked size leaks to
/// panes once, which is the behavior without the gate.
pub(super) const RESTORE_SETTLE: Duration = Duration::from_secs(3);

#[derive(Debug)]
pub(super) struct DisplayTopologyGate {
    /// Awake displays while the topology was last stable.
    baseline: u32,
    hold: Option<Hold>,
}

#[derive(Debug)]
struct Hold {
    awake: u32,
    changed_at: Instant,
    restored_at: Option<Instant>,
}

impl DisplayTopologyGate {
    pub(super) fn new() -> Self {
        Self {
            baseline: 0,
            hold: None,
        }
    }

    /// Whether the resize watcher may report the current geometry.
    ///
    /// `awake` is `None` where the platform cannot observe displays, which
    /// disables the gate. `at_reported_size` is whether the sampled geometry
    /// equals the size the watcher last reported.
    pub(super) fn admit(
        &mut self,
        now: Instant,
        awake: Option<u32>,
        at_reported_size: bool,
    ) -> bool {
        let Some(awake) = awake else {
            self.hold = None;
            return true;
        };
        let Some(hold) = self.hold.as_mut() else {
            if awake >= self.baseline {
                self.baseline = awake;
                return true;
            }
            tracing::info!(
                awake,
                baseline = self.baseline,
                "holding host resizes: display topology changed"
            );
            self.hold = Some(Hold {
                awake,
                changed_at: now,
                restored_at: None,
            });
            return false;
        };

        if awake < self.baseline {
            if awake != hold.awake {
                hold.awake = awake;
                hold.changed_at = now;
            }
            hold.restored_at = None;
            // With every display asleep nobody sees the terminal, so holding
            // costs nothing and must not rebaseline to zero.
            if awake == 0 || now.duration_since(hold.changed_at) < UNPLUG_FALLBACK {
                return false;
            }
            tracing::info!(
                awake,
                baseline = self.baseline,
                "releasing host resizes: display stayed removed"
            );
            self.baseline = awake;
            self.hold = None;
            return true;
        }

        self.baseline = awake;
        hold.awake = awake;
        let restored_at = *hold.restored_at.get_or_insert(now);
        if !at_reported_size && now.duration_since(restored_at) < RESTORE_SETTLE {
            return false;
        }
        tracing::info!(
            awake,
            at_reported_size,
            "releasing host resizes: display topology restored"
        );
        self.hold = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    fn stable(awake: u32) -> (DisplayTopologyGate, Instant) {
        let mut gate = DisplayTopologyGate::new();
        let base = Instant::now();
        assert!(gate.admit(base, Some(awake), true));
        (gate, base)
    }

    #[test]
    fn unobservable_displays_never_hold() {
        let mut gate = DisplayTopologyGate::new();
        let base = Instant::now();
        assert!(gate.admit(base, None, false));
        assert!(gate.admit(at(base, 100), None, false));
    }

    #[test]
    fn sleep_wake_holds_the_parked_size_and_releases_when_the_window_returns() {
        // Replays the 2026-10-18 08:30–08:31 capture.
        let (mut gate, base) = stable(2);
        assert!(
            !gate.admit(at(base, 1_000), Some(0), true),
            "all displays asleep"
        );
        assert!(
            !gate.admit(at(base, 61_000), Some(0), true),
            "a long sleep never times out"
        );
        assert!(
            !gate.admit(at(base, 61_900), Some(1), true),
            "external still waking"
        );
        assert!(
            !gate.admit(at(base, 64_400), Some(1), false),
            "window parked on built-in"
        );
        assert!(
            !gate.admit(at(base, 64_500), Some(2), false),
            "display back, window still parked"
        );
        assert!(!gate.admit(at(base, 65_600), Some(2), false));
        assert!(
            gate.admit(at(base, 65_700), Some(2), true),
            "window back at its reported size"
        );
        assert!(
            gate.admit(at(base, 65_800), Some(2), false),
            "later resizes pass through"
        );
    }

    #[test]
    fn restored_topology_releases_a_size_that_does_not_return() {
        let (mut gate, base) = stable(2);
        assert!(!gate.admit(at(base, 100), Some(1), false));
        assert!(!gate.admit(at(base, 1_000), Some(2), false));
        assert!(!gate.admit(at(base, 3_999), Some(2), false));
        assert!(gate.admit(at(base, 4_000), Some(2), false));
    }

    #[test]
    fn genuine_unplug_releases_after_the_fallback_and_rebaselines() {
        let (mut gate, base) = stable(2);
        assert!(!gate.admit(at(base, 100), Some(1), false));
        assert!(!gate.admit(at(base, 10_099), Some(1), false));
        assert!(gate.admit(at(base, 10_100), Some(1), false));
        assert!(
            gate.admit(at(base, 10_200), Some(1), false),
            "one display is the new baseline"
        );
    }

    #[test]
    fn topology_changes_restart_the_unplug_fallback() {
        let (mut gate, base) = stable(3);
        assert!(!gate.admit(at(base, 0), Some(2), false));
        assert!(!gate.admit(at(base, 9_000), Some(1), false));
        assert!(!gate.admit(at(base, 18_999), Some(1), false));
        assert!(gate.admit(at(base, 19_000), Some(1), false));
    }

    #[test]
    fn plugging_in_a_display_rebaselines_without_holding() {
        let (mut gate, base) = stable(1);
        assert!(gate.admit(at(base, 100), Some(2), false));
        assert!(
            !gate.admit(at(base, 200), Some(1), false),
            "the new display is now expected"
        );
    }

    #[test]
    fn a_display_dropping_again_after_restore_resumes_the_hold() {
        let (mut gate, base) = stable(2);
        assert!(!gate.admit(at(base, 100), Some(1), false));
        assert!(!gate.admit(at(base, 200), Some(2), false));
        assert!(!gate.admit(at(base, 2_500), Some(1), false));
        assert!(
            !gate.admit(at(base, 6_000), Some(2), false),
            "settle restarts from the new restore"
        );
        assert!(gate.admit(at(base, 9_000), Some(2), false));
    }
}
