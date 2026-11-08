//! Observability the controller keeps on the side: where the bytes went over
//! time, and who is pulling which subscription from where.
//!
//! Neither feeds a decision. Quotas run on [`crate::state::Ledger`] alone; this
//! is history for a person reading `xctrl stats`, so losing it costs insight,
//! never bytes. It lives on disk only because Nix cannot know it.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::config::Quota;
use crate::state::Traffic;

pub const HISTORY_FILE: &str = "history.json";
pub const FETCHES_FILE: &str = "fetches.json";

const HOUR: i64 = 3600;
/// How long hourly resolution is kept before an hour is folded into its day.
pub const HOURLY_KEEP: Duration = Duration::days(14);
/// How long daily totals are kept at all.
const DAILY_KEEP: Duration = Duration::days(400);
/// Distinct (address, client) pairs remembered per user. A token shared around
/// shows up as many pairs long before it hits this.
const MAX_CLIENTS: usize = 32;
/// User-Agents are attacker-controlled text; keep the file bounded.
const MAX_UA: usize = 200;

/// Traffic within one time slot, per user and per node.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Bucket {
    #[serde(default)]
    pub users: BTreeMap<String, Traffic>,
    #[serde(default)]
    pub nodes: BTreeMap<String, Traffic>,
}

impl Bucket {
    fn merge(&mut self, other: &Bucket) {
        for (user, t) in &other.users {
            self.users.entry(user.clone()).or_default().add(*t);
        }
        for (node, t) in &other.nodes {
            self.nodes.entry(node.clone()).or_default().add(*t);
        }
    }

    pub fn user(&self, name: &str) -> Traffic {
        self.users.get(name).copied().unwrap_or_default()
    }

    pub fn total(&self) -> Traffic {
        let mut sum = Traffic::default();
        for t in self.nodes.values() {
            sum.add(*t);
        }
        sum
    }
}

/// Traffic over time. Recent hours are kept as they are; older ones are folded
/// into days in the quota offset, so a day here is the same day the billing
/// period counts in.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct History {
    /// Keyed by the unix time the hour starts at.
    #[serde(default)]
    pub hourly: BTreeMap<i64, Bucket>,
    /// Days that have aged out of `hourly`, keyed by local date.
    #[serde(default)]
    pub daily: BTreeMap<NaiveDate, Bucket>,
}

fn hour_of(at: DateTime<Utc>) -> i64 {
    at.timestamp().div_euclid(HOUR) * HOUR
}

pub fn hour_start(key: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(key, 0).unwrap_or_default()
}

impl History {
    /// Attributes one node's rises to the hour they were collected in.
    ///
    /// Collection runs every minute, so a rise lands in the right hour unless
    /// the controller was down — then the backlog lands in the hour it came
    /// back, which is the honest thing to show.
    pub fn record(&mut self, node: &str, rises: &[(String, Traffic)], now: DateTime<Utc>) {
        if rises.is_empty() {
            return;
        }
        let bucket = self.hourly.entry(hour_of(now)).or_default();
        for (user, t) in rises {
            bucket.users.entry(user.clone()).or_default().add(*t);
            bucket.nodes.entry(node.to_string()).or_default().add(*t);
        }
    }

    /// Folds hours past [`HOURLY_KEEP`] into days and drops days past retention.
    pub fn compact(&mut self, quota: &Quota, now: DateTime<Utc>) {
        let cutoff = hour_of(now - HOURLY_KEEP);
        let keep = self.hourly.split_off(&cutoff);
        let old = std::mem::replace(&mut self.hourly, keep);
        for (key, bucket) in old {
            let day = quota.local_date(hour_start(key));
            self.daily.entry(day).or_default().merge(&bucket);
        }
        let oldest = quota.local_date(now - DAILY_KEEP);
        self.daily = self.daily.split_off(&oldest);
    }

    /// Everything since `from`, at hourly resolution. Only meaningful for
    /// windows within [`HOURLY_KEEP`].
    pub fn since(&self, from: DateTime<Utc>) -> Bucket {
        let mut sum = Bucket::default();
        for bucket in self.hourly.range(hour_of(from)..).map(|(_, b)| b) {
            sum.merge(bucket);
        }
        sum
    }

    /// Every day on record, merging folded days with the hours not yet folded.
    pub fn days(&self, quota: &Quota) -> BTreeMap<NaiveDate, Bucket> {
        let mut out = self.daily.clone();
        for (key, bucket) in &self.hourly {
            out.entry(quota.local_date(hour_start(*key)))
                .or_default()
                .merge(bucket);
        }
        out
    }
}

/// One address/client pair a subscription was fetched by.
#[derive(Clone, Serialize, Deserialize)]
pub struct Seen {
    pub ip: String,
    pub ua: String,
    /// What was served the last time: a format name or `dashboard`.
    pub format: String,
    pub first: DateTime<Utc>,
    pub last: DateTime<Utc>,
    pub count: u64,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct UserFetches {
    pub total: u64,
    /// Most recently seen first.
    #[serde(default)]
    pub clients: Vec<Seen>,
}

impl UserFetches {
    pub fn last(&self) -> Option<&Seen> {
        self.clients.first()
    }

    pub fn distinct_ips(&self) -> usize {
        let mut ips: Vec<&str> = self.clients.iter().map(|c| c.ip.as_str()).collect();
        ips.sort_unstable();
        ips.dedup();
        ips.len()
    }
}

/// Who fetched which subscription, from where, with what.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Fetches {
    #[serde(default)]
    pub users: BTreeMap<String, UserFetches>,
}

impl Fetches {
    pub fn record(&mut self, user: &str, ip: &str, ua: &str, format: &str, now: DateTime<Utc>) {
        let ua: String = ua.chars().take(MAX_UA).collect();
        let entry = self.users.entry(user.to_string()).or_default();
        entry.total += 1;
        let seen = match entry.clients.iter().position(|c| c.ip == ip && c.ua == ua) {
            Some(i) => {
                let mut seen = entry.clients.remove(i);
                seen.last = now;
                seen.count += 1;
                seen.format = format.to_string();
                seen
            }
            None => Seen {
                ip: ip.to_string(),
                ua,
                format: format.to_string(),
                first: now,
                last: now,
                count: 1,
            },
        };
        entry.clients.insert(0, seen);
        entry.clients.truncate(MAX_CLIENTS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quota() -> Quota {
        Quota {
            reset_day: 1,
            reset_hour: 1,
            reset_minute: 0,
            reset_utc_offset_hours: 3,
        }
    }

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn t(up: u64, down: u64) -> Traffic {
        Traffic { up, down }
    }

    fn rise(user: &str, up: u64, down: u64) -> (String, Traffic) {
        (user.to_string(), t(up, down))
    }

    #[test]
    fn rises_in_one_hour_share_a_bucket() {
        let mut h = History::default();
        h.record(
            "stockholm",
            &[rise("me", 1, 2)],
            utc("2026-03-10T12:05:00Z"),
        );
        h.record(
            "stockholm",
            &[rise("me", 3, 4)],
            utc("2026-03-10T12:59:00Z"),
        );
        h.record(
            "stockholm",
            &[rise("me", 5, 6)],
            utc("2026-03-10T13:00:00Z"),
        );
        assert_eq!(h.hourly.len(), 2);
        let first = &h.hourly[&utc("2026-03-10T12:00:00Z").timestamp()];
        assert_eq!(first.user("me").total(), 10);
    }

    #[test]
    fn nodes_and_users_are_both_summed() {
        let mut h = History::default();
        let now = utc("2026-03-10T12:00:00Z");
        h.record("stockholm", &[rise("a", 1, 1), rise("b", 2, 2)], now);
        h.record("asgard", &[rise("a", 10, 10)], now);
        let b = h.since(now);
        assert_eq!(b.user("a").total(), 22);
        assert_eq!(b.nodes["stockholm"].total(), 6);
        assert_eq!(b.total().total(), 26);
    }

    #[test]
    fn since_starts_at_the_hour_of_the_instant() {
        let mut h = History::default();
        h.record("n", &[rise("me", 0, 100)], utc("2026-03-10T10:30:00Z"));
        h.record("n", &[rise("me", 0, 1)], utc("2026-03-10T11:30:00Z"));
        assert_eq!(h.since(utc("2026-03-10T11:59:00Z")).user("me").down, 1);
        assert_eq!(h.since(utc("2026-03-10T10:59:00Z")).user("me").down, 101);
    }

    #[test]
    fn compaction_folds_old_hours_into_local_days() {
        let mut h = History::default();
        // 22:00 UTC on the 1st is 01:00 on the 2nd at +03:00.
        h.record("n", &[rise("me", 0, 5)], utc("2026-03-01T20:00:00Z"));
        h.record("n", &[rise("me", 0, 7)], utc("2026-03-01T22:00:00Z"));
        let now = utc("2026-03-01T22:00:00Z") + HOURLY_KEEP + Duration::hours(1);
        h.record("n", &[rise("me", 0, 1)], now);
        h.compact(&quota(), now);

        assert_eq!(h.hourly.len(), 1);
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        assert_eq!(h.daily[&d("2026-03-01")].user("me").down, 5);
        assert_eq!(h.daily[&d("2026-03-02")].user("me").down, 7);
        // The merged view still accounts for every byte.
        let all: u64 = h.days(&quota()).values().map(|b| b.user("me").down).sum();
        assert_eq!(all, 13);
    }

    #[test]
    fn compaction_drops_days_past_retention() {
        let mut h = History::default();
        h.record("n", &[rise("me", 1, 1)], utc("2025-01-01T12:00:00Z"));
        let now = utc("2026-06-01T12:00:00Z");
        h.compact(&quota(), now);
        assert!(h.hourly.is_empty());
        assert!(h.daily.is_empty());
    }

    #[test]
    fn history_survives_a_json_round_trip() {
        let mut h = History::default();
        h.record("n", &[rise("me", 1, 2)], utc("2026-03-01T20:00:00Z"));
        h.compact(&quota(), utc("2026-04-01T00:00:00Z"));
        h.record("n", &[rise("me", 3, 4)], utc("2026-04-01T00:00:00Z"));
        let back: History = serde_json::from_slice(&serde_json::to_vec(&h).unwrap()).unwrap();
        assert_eq!(back.daily.len(), 1);
        assert_eq!(back.hourly.len(), 1);
        assert_eq!(back.days(&quota()).len(), 2);
    }

    #[test]
    fn fetches_group_by_address_and_client() {
        let mut f = Fetches::default();
        let t0 = utc("2026-03-01T00:00:00Z");
        f.record("me", "192.0.2.1", "Happ/1.0", "happ", t0);
        f.record(
            "me",
            "192.0.2.1",
            "Happ/1.0",
            "happ",
            t0 + Duration::hours(1),
        );
        f.record(
            "me",
            "198.51.100.7",
            "mihomo/1.19",
            "clash-meta",
            t0 + Duration::hours(2),
        );
        let me = &f.users["me"];
        assert_eq!(me.total, 3);
        assert_eq!(me.clients.len(), 2);
        assert_eq!(me.distinct_ips(), 2);
        // Most recent first.
        assert_eq!(me.last().unwrap().ip, "198.51.100.7");
        let happ = &me.clients[1];
        assert_eq!(happ.count, 2);
        assert_eq!(happ.first, t0);
        assert_eq!(happ.last, t0 + Duration::hours(1));
    }

    #[test]
    fn fetches_forget_the_stalest_client_first() {
        let mut f = Fetches::default();
        let t0 = utc("2026-03-01T00:00:00Z");
        for i in 0..MAX_CLIENTS + 5 {
            f.record(
                "me",
                &format!("192.0.2.{i}"),
                "x",
                "base64",
                t0 + Duration::minutes(i as i64),
            );
        }
        let me = &f.users["me"];
        assert_eq!(me.clients.len(), MAX_CLIENTS);
        assert_eq!(me.total, (MAX_CLIENTS + 5) as u64);
        assert!(me.clients.iter().all(|c| c.ip != "192.0.2.0"));
    }

    #[test]
    fn user_agents_are_truncated() {
        let mut f = Fetches::default();
        f.record("me", "-", &"a".repeat(10_000), "base64", Utc::now());
        assert_eq!(f.users["me"].clients[0].ua.len(), MAX_UA);
    }
}
