use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, NaiveTime, TimeZone, Utc};

use crate::config::Quota;

impl Quota {
    fn tz(&self) -> FixedOffset {
        FixedOffset::east_opt(self.reset_utc_offset_hours * 3600)
            .unwrap_or_else(|| FixedOffset::east_opt(0).expect("utc is a valid offset"))
    }

    fn reset_at(&self, year: i32, month: u32) -> DateTime<Utc> {
        let day = self.reset_day.clamp(1, days_in(year, month));
        let hour = self.reset_hour.min(23);
        let minute = self.reset_minute.min(59);
        self.tz()
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .expect("fixed offset has no ambiguous local times")
            .with_timezone(&Utc)
    }

    pub fn period_start(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        let local = now.with_timezone(&self.tz());
        let this = self.reset_at(local.year(), local.month());
        if now < this {
            let (year, month) = prev_month(local.year(), local.month());
            self.reset_at(year, month)
        } else {
            this
        }
    }

    pub fn is_due(&self, last: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
        match last {
            // Never reset: adopt the current period rather than wiping on first boot.
            None => false,
            Some(last) => last < self.period_start(now),
        }
    }

    pub fn render_local(&self, at: DateTime<Utc>) -> String {
        at.with_timezone(&self.tz())
            .format("%d.%m.%Y %H:%M (UTC%:z)")
            .to_string()
    }

    pub fn expiry(&self, date: NaiveDate) -> DateTime<Utc> {
        let next = date.succ_opt().unwrap_or(date);
        self.tz()
            .from_local_datetime(&next.and_time(NaiveTime::MIN))
            .single()
            .expect("fixed offset has no ambiguous local times")
            .with_timezone(&Utc)
    }

    pub fn is_expired(&self, date: NaiveDate, now: DateTime<Utc>) -> bool {
        now >= self.expiry(date)
    }

    pub fn next_reset(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        let local = now.with_timezone(&self.tz());
        let this = self.reset_at(local.year(), local.month());
        if now < this {
            this
        } else {
            let (year, month) = next_month(local.year(), local.month());
            self.reset_at(year, month)
        }
    }
}

fn days_in(year: i32, month: u32) -> u32 {
    let (y, m) = next_month(year, month);
    NaiveDate::from_ymd_opt(y, m, 1)
        .and_then(|d| d.pred_opt())
        .map(|d| d.day())
        .unwrap_or(28)
}

fn prev_month(year: i32, month: u32) -> (i32, u32) {
    if month == 1 {
        (year - 1, 12)
    } else {
        (year, month - 1)
    }
}

fn next_month(year: i32, month: u32) -> (i32, u32) {
    if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quota(day: u32) -> Quota {
        Quota {
            reset_day: day,
            reset_hour: 1,
            reset_minute: 0,
            reset_utc_offset_hours: 3,
        }
    }

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn boundary_is_one_am_at_plus_three() {
        // 01:00+03:00 is 22:00 UTC on the previous day.
        let q = quota(1);
        assert_eq!(
            q.period_start(utc("2026-03-15T00:00:00Z")),
            utc("2026-02-28T22:00:00Z")
        );
    }

    #[test]
    fn before_boundary_falls_back_to_previous_month() {
        let q = quota(10);
        // 10 Mar 00:00 MSK is still before the 10 Mar 01:00 MSK reset.
        assert_eq!(
            q.period_start(utc("2026-03-09T21:00:00Z")),
            utc("2026-02-09T22:00:00Z")
        );
        // One hour later the new period has begun.
        assert_eq!(
            q.period_start(utc("2026-03-09T22:30:00Z")),
            utc("2026-03-09T22:00:00Z")
        );
    }

    #[test]
    fn day_31_clamps_to_february() {
        let q = quota(31);
        // 2026 is not a leap year: the February boundary lands on the 28th.
        assert_eq!(
            q.period_start(utc("2026-03-01T00:00:00Z")),
            utc("2026-02-27T22:00:00Z")
        );
        // 2028 is a leap year: it lands on the 29th.
        assert_eq!(
            q.period_start(utc("2028-03-01T00:00:00Z")),
            utc("2028-02-28T22:00:00Z")
        );
    }

    #[test]
    fn year_rolls_over_backwards() {
        let q = quota(15);
        assert_eq!(
            q.period_start(utc("2026-01-05T00:00:00Z")),
            utc("2025-12-14T22:00:00Z")
        );
    }

    #[test]
    fn due_only_after_the_boundary_passes() {
        let q = quota(1);
        let now = utc("2026-03-15T00:00:00Z");
        assert!(q.is_due(Some(utc("2026-01-20T00:00:00Z")), now));
        assert!(!q.is_due(Some(utc("2026-03-05T00:00:00Z")), now));
        assert!(!q.is_due(None, now));
    }

    #[test]
    fn renders_in_the_configured_offset_not_utc() {
        let q = quota(1);
        // 22:00 UTC on the 31st is 01:00 on the 1st at +03:00, which is the
        // wording the schedule was written in and what the user must see.
        assert_eq!(
            q.render_local(utc("2026-08-31T22:00:00Z")),
            "01.09.2026 01:00 (UTC+03:00)"
        );
    }

    #[test]
    fn expiry_includes_the_whole_final_day() {
        let q = quota(1);
        let day = NaiveDate::from_ymd_opt(2027, 3, 24).unwrap();
        // Midnight opening the 24th at +03:00 is 21:00 UTC on the 23rd.
        assert!(!q.is_expired(day, utc("2027-03-23T21:00:00Z")));
        // Still valid a minute before the day ends locally.
        assert!(!q.is_expired(day, utc("2027-03-24T20:59:00Z")));
        // The 25th has begun at +03:00.
        assert!(q.is_expired(day, utc("2027-03-24T21:00:00Z")));
    }

    #[test]
    fn next_reset_moves_forward() {
        let q = quota(1);
        assert_eq!(
            q.next_reset(utc("2026-03-15T00:00:00Z")),
            utc("2026-03-31T22:00:00Z")
        );
    }
}
