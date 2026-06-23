// Copyright (c) 2026, Salesforce, Inc.
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Compute the next fire time for each [`Schedule`] variant.
//!
//! The scheduler is intentionally minimal — the daemon polls every minute,
//! asks each loop "is `next_fire(now)` <= now?" and fires if so. No
//! sub-minute precision; that's plenty for our schedules ("every 10m",
//! "daily 09:00").

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc, Weekday};
use chrono_tz::Tz;

use crate::config::Schedule;

/// Returns the next fire time strictly **after** `now`, or `None` for
/// `Schedule::Never`. `last_fired` lets `EveryMinutes` compute "Nm after
/// the previous fire" rather than "Nm from now"; pass `None` if the loop
/// has never fired (we still return `now + interval`, so the daemon does
/// not paint everything as "due now" the moment it starts).
pub fn next_fire(
    schedule: &Schedule,
    now: DateTime<Utc>,
    last_fired: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    match schedule {
        Schedule::Never => None,
        Schedule::EveryMinutes(n) => {
            let interval = Duration::minutes(*n as i64);
            let base = last_fired.unwrap_or(now);
            // Always strictly in the future relative to `now`.
            let mut t = base + interval;
            while t <= now {
                t += interval;
            }
            Some(t)
        }
        Schedule::DailyAt { hh, mm, tz } => Some(next_daily(*tz, *hh, *mm, now)),
        Schedule::WeeklyAt {
            weekday,
            hh,
            mm,
            tz,
        } => Some(next_weekly(*tz, *weekday, *hh, *mm, now)),
    }
}

fn next_daily(tz: Tz, hh: u8, mm: u8, now: DateTime<Utc>) -> DateTime<Utc> {
    let local_now = now.with_timezone(&tz);
    // Today's candidate at HH:MM in tz.
    let today_candidate = build_local(tz, local_now.date_naive(), hh, mm);
    if today_candidate > now {
        today_candidate
    } else {
        // Tomorrow.
        let tomorrow = local_now.date_naive() + Duration::days(1);
        build_local(tz, tomorrow, hh, mm)
    }
}

fn next_weekly(tz: Tz, weekday: Weekday, hh: u8, mm: u8, now: DateTime<Utc>) -> DateTime<Utc> {
    let local_now = now.with_timezone(&tz);
    let today = local_now.date_naive();
    let cur_dow = today.weekday();
    // Days until target weekday (0..=6).
    let days_ahead = (weekday.num_days_from_monday() as i64
        - cur_dow.num_days_from_monday() as i64)
        .rem_euclid(7);
    let candidate_date = today + Duration::days(days_ahead);
    let candidate = build_local(tz, candidate_date, hh, mm);
    if candidate > now {
        candidate
    } else {
        // Same weekday but earlier today / past — go a full week ahead.
        let next_week = candidate_date + Duration::days(7);
        build_local(tz, next_week, hh, mm)
    }
}

/// Resolve a (date, hh, mm) in `tz` to a UTC instant. If the local
/// time falls inside a DST gap (skipped hour), pick the post-gap instant.
/// If the time is ambiguous (DST fold), pick the earliest one — so a
/// scheduled "01:30" in fall-back fires once, before the clocks roll back.
fn build_local(tz: Tz, date: NaiveDate, hh: u8, mm: u8) -> DateTime<Utc> {
    let naive = date
        .and_hms_opt(hh as u32, mm as u32, 0)
        .expect("HH/MM validated upstream");
    match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => dt.with_timezone(&Utc),
        chrono::LocalResult::Ambiguous(earliest, _later) => earliest.with_timezone(&Utc),
        chrono::LocalResult::None => {
            // Spring-forward gap: pick the next valid instant by adding
            // one minute until from_local_datetime succeeds.
            let mut probe = naive;
            for _ in 0..120 {
                probe += Duration::minutes(1);
                if let chrono::LocalResult::Single(dt) = tz.from_local_datetime(&probe) {
                    return dt.with_timezone(&Utc);
                }
            }
            // Fall back: best-effort UTC.
            Utc.from_utc_datetime(&naive)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, m: u32, d: u32, hh: u32, mm: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, hh, mm, 0).unwrap()
    }

    #[test]
    fn never_fires() {
        assert!(next_fire(&Schedule::Never, utc(2026, 5, 13, 12, 0), None).is_none());
    }

    #[test]
    fn every_10m_first_fire_is_in_future_when_no_last() {
        let now = utc(2026, 5, 13, 12, 0);
        let t = next_fire(&Schedule::EveryMinutes(10), now, None).unwrap();
        assert_eq!(t, utc(2026, 5, 13, 12, 10));
    }

    #[test]
    fn every_10m_with_last_fired_advances() {
        let now = utc(2026, 5, 13, 12, 0);
        let last = utc(2026, 5, 13, 11, 55);
        let t = next_fire(&Schedule::EveryMinutes(10), now, Some(last)).unwrap();
        assert_eq!(t, utc(2026, 5, 13, 12, 5));
    }

    #[test]
    fn every_10m_skips_past_now_if_last_was_long_ago() {
        let now = utc(2026, 5, 13, 12, 0);
        let last = utc(2026, 5, 13, 11, 0);
        let t = next_fire(&Schedule::EveryMinutes(10), now, Some(last)).unwrap();
        // Should advance past now, not return a stale 11:10.
        assert_eq!(t, utc(2026, 5, 13, 12, 10));
    }

    #[test]
    fn daily_today_if_in_future() {
        // PT 8:00am on 2026-05-13 = UTC 15:00 (PDT, -07:00)
        let now = utc(2026, 5, 13, 14, 0);
        let s = Schedule::DailyAt {
            hh: 9,
            mm: 0,
            tz: chrono_tz::America::Los_Angeles,
        };
        let t = next_fire(&s, now, None).unwrap();
        // 09:00 PDT = 16:00 UTC same day
        assert_eq!(t, utc(2026, 5, 13, 16, 0));
    }

    #[test]
    fn daily_rolls_to_tomorrow_if_passed() {
        // 12:00 PT on 2026-05-13 = 19:00 UTC. 09:00 PT today is gone.
        let now = utc(2026, 5, 13, 19, 0);
        let s = Schedule::DailyAt {
            hh: 9,
            mm: 0,
            tz: chrono_tz::America::Los_Angeles,
        };
        let t = next_fire(&s, now, None).unwrap();
        // 09:00 PT tomorrow = 16:00 UTC 2026-05-14
        assert_eq!(t, utc(2026, 5, 14, 16, 0));
    }

    #[test]
    fn weekly_picks_next_target_weekday() {
        // 2026-05-13 is a Wednesday. Schedule for Monday 08:00 PT.
        let now = utc(2026, 5, 13, 19, 0); // Wed
        let s = Schedule::WeeklyAt {
            weekday: Weekday::Mon,
            hh: 8,
            mm: 0,
            tz: chrono_tz::America::Los_Angeles,
        };
        let t = next_fire(&s, now, None).unwrap();
        // Next Monday is 2026-05-18; 08:00 PDT = 15:00 UTC.
        assert_eq!(t, utc(2026, 5, 18, 15, 0));
    }

    #[test]
    fn weekly_today_after_target_time_rolls_a_full_week() {
        // 2026-05-18 Mon at 16:00 UTC = 09:00 PDT — already past 08:00 PDT.
        let now = utc(2026, 5, 18, 16, 0);
        let s = Schedule::WeeklyAt {
            weekday: Weekday::Mon,
            hh: 8,
            mm: 0,
            tz: chrono_tz::America::Los_Angeles,
        };
        let t = next_fire(&s, now, None).unwrap();
        assert_eq!(t, utc(2026, 5, 25, 15, 0));
    }

    #[test]
    fn daily_handles_dst_spring_forward() {
        // 2026-03-08 is the US spring-forward day in PDT.
        // 02:30 LA local does not exist; we should fire at 03:00 LA (10:00 UTC).
        let now = utc(2026, 3, 8, 9, 0); // before any candidate
        let s = Schedule::DailyAt {
            hh: 2,
            mm: 30,
            tz: chrono_tz::America::Los_Angeles,
        };
        let t = next_fire(&s, now, None).unwrap();
        // The build_local fallback advances probe minutes until valid.
        // 02:30 PST (=10:30 UTC) doesn't exist; next valid local is 03:00 PDT
        // (=10:00 UTC). Since probe iterates +1 minute starting from 02:30,
        // it'll hit 03:00 LA after exactly 30 probes.
        assert_eq!(t, utc(2026, 3, 8, 10, 0));
    }

    #[test]
    fn daily_handles_dst_fall_back() {
        // 2026-11-01 is fall-back; 01:30 LA is ambiguous.
        // We pick the EARLIEST instance — 01:30 PDT = 08:30 UTC (not 09:30 UTC).
        let now = utc(2026, 11, 1, 7, 0); // before any candidate
        let s = Schedule::DailyAt {
            hh: 1,
            mm: 30,
            tz: chrono_tz::America::Los_Angeles,
        };
        let t = next_fire(&s, now, None).unwrap();
        assert_eq!(t, utc(2026, 11, 1, 8, 30));
    }
}