//! Desk schedule router — research isolation by UTC hour.
//!
//! Flag `DESK_SCHEDULE=1` gates **new EnterLong** only. Open positions stay on
//! their opening `strategy_id`. No live multi-strat claim; edge unproven.

use crate::sessions::{hour_in_windows, utc_datetime, HourWindow, DEFAULT_ENTRY_WINDOWS};
use chrono::Timelike;
use std::env;

/// S4 open windows (same default as `STRATEGY4_ENTRY_HOURS` / `DEFAULT_ENTRY_HOURS`).
pub const S4_OPEN_WINDOWS: [HourWindow; 3] = DEFAULT_ENTRY_WINDOWS;

/// S3 arm: pre/around 1d close — 22:00–00:00 UTC (hours 22, 23).
pub const S3_ARM_WINDOW: HourWindow = (22, 24);

/// Strategy id that may open new longs at `utc_hour` (0–23).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum DeskSid {
    Scalp = 2,
    Trend = 3,
    Continuation = 4,
}

impl DeskSid {
    pub fn as_i32(self) -> i32 {
        self as i32
    }
}

pub fn desk_schedule_enabled() -> bool {
    matches!(
        env::var("DESK_SCHEDULE").ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

pub fn hour_in_s4_open(hour: u8) -> bool {
    hour_in_windows(hour as u32, &S4_OPEN_WINDOWS)
}

pub fn hour_in_s3_arm(hour: u8) -> bool {
    hour_in_windows(hour as u32, &[S3_ARM_WINDOW])
}

/// Owner for **new opens** at UTC hour.
///
/// - S4 windows win (incl. 0–2 over any wrap).
/// - Else S3 arm 22–24.
/// - Else S2 fills the gaps.
pub fn desk_owner(utc_hour: u8) -> DeskSid {
    let h = utc_hour % 24;
    if hour_in_s4_open(h) {
        return DeskSid::Continuation;
    }
    if hour_in_s3_arm(h) {
        return DeskSid::Trend;
    }
    DeskSid::Scalp
}

pub fn desk_owner_at(now: f64) -> DeskSid {
    desk_owner(utc_datetime(now).hour() as u8)
}

/// When flag off → always allow. When on → only owner strategy may EnterLong.
pub fn may_enter_long(strategy_id: i32, utc_hour: u8) -> bool {
    if !desk_schedule_enabled() {
        return true;
    }
    desk_owner(utc_hour).as_i32() == strategy_id
}

pub fn may_enter_long_at(strategy_id: i32, now: f64) -> bool {
    may_enter_long(strategy_id, utc_datetime(now).hour() as u8)
}

/// Hour → owner table for docs / tests (0..=23).
pub fn hour_owner_table() -> [(u8, DeskSid); 24] {
    let mut out = [(0u8, DeskSid::Scalp); 24];
    for h in 0u8..24 {
        out[h as usize] = (h, desk_owner(h));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s4_windows_match_default_entry_hours() {
        assert!(hour_in_s4_open(0));
        assert!(hour_in_s4_open(1));
        assert!(!hour_in_s4_open(2));
        assert!(hour_in_s4_open(7));
        assert!(hour_in_s4_open(9));
        assert!(!hour_in_s4_open(10));
        assert!(hour_in_s4_open(13));
        assert!(hour_in_s4_open(15));
        assert!(!hour_in_s4_open(16));
    }

    #[test]
    fn s3_arm_22_23_only() {
        assert!(!hour_in_s3_arm(21));
        assert!(hour_in_s3_arm(22));
        assert!(hour_in_s3_arm(23));
        assert!(!hour_in_s3_arm(0)); // hour 0 is S4, not S3 wrap into open
    }

    #[test]
    fn desk_owner_s4_wins_0_2() {
        assert_eq!(desk_owner(0), DeskSid::Continuation);
        assert_eq!(desk_owner(1), DeskSid::Continuation);
    }

    #[test]
    fn desk_owner_s3_arm_and_s2_gaps() {
        assert_eq!(desk_owner(22), DeskSid::Trend);
        assert_eq!(desk_owner(23), DeskSid::Trend);
        assert_eq!(desk_owner(3), DeskSid::Scalp); // 02–07 gap
        assert_eq!(desk_owner(11), DeskSid::Scalp); // 10–13
        assert_eq!(desk_owner(17), DeskSid::Scalp); // 16–22
        assert_eq!(desk_owner(8), DeskSid::Continuation);
        assert_eq!(desk_owner(14), DeskSid::Continuation);
    }

    #[test]
    fn full_day_table_no_overlap_ambiguity() {
        let t = hour_owner_table();
        assert_eq!(t[0].1, DeskSid::Continuation);
        assert_eq!(t[2].1, DeskSid::Scalp);
        assert_eq!(t[7].1, DeskSid::Continuation);
        assert_eq!(t[10].1, DeskSid::Scalp);
        assert_eq!(t[13].1, DeskSid::Continuation);
        assert_eq!(t[16].1, DeskSid::Scalp);
        assert_eq!(t[22].1, DeskSid::Trend);
        // Exactly one owner per hour
        for h in 0..24u8 {
            let o = desk_owner(h);
            assert!(matches!(
                o,
                DeskSid::Scalp | DeskSid::Trend | DeskSid::Continuation
            ));
        }
    }

    // Env flag tests must not race under --test-threads > 1.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn may_enter_flag_off_and_on() {
        let _g = ENV_LOCK.lock().unwrap();
        env::remove_var("DESK_SCHEDULE");
        assert!(may_enter_long(1, 3));
        assert!(may_enter_long(2, 0));
        assert!(may_enter_long(4, 22));

        env::set_var("DESK_SCHEDULE", "1");
        assert!(may_enter_long(4, 0));
        assert!(!may_enter_long(2, 0));
        assert!(!may_enter_long(3, 0));
        assert!(may_enter_long(2, 3));
        assert!(!may_enter_long(4, 3));
        assert!(may_enter_long(3, 22));
        assert!(!may_enter_long(2, 22));
        env::remove_var("DESK_SCHEDULE");
    }
}
