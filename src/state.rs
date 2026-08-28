use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub struct Traffic {
    pub up: u64,
    pub down: u64,
}

impl Traffic {
    pub fn total(&self) -> u64 {
        self.up + self.down
    }

    fn since(&self, previous: &Traffic) -> Traffic {
        Traffic {
            up: self.up.saturating_sub(previous.up),
            down: self.down.saturating_sub(previous.down),
        }
    }

    fn add(&mut self, other: Traffic) {
        self.up += other.up;
        self.down += other.down;
    }
}

/// One node's view, maintained by its agent. Counters are read from xray with
/// reset, so this file is the only place the running total survives a restart.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub users: BTreeMap<String, Traffic>,
    /// Last time each user moved any bytes, used to derive `online`.
    #[serde(default)]
    pub seen: BTreeMap<String, u64>,
    #[serde(default)]
    pub online: Vec<String>,
    pub collected_at: u64,
}

/// The controller's view: traffic summed across every node, plus enforcement
/// decisions. This is what quota checks run against.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Ledger {
    /// Bytes spent in the current billing period, per user, across all nodes.
    /// Kept split by direction because that is what clients display; quota
    /// checks run against the total.
    #[serde(default)]
    pub spent: BTreeMap<String, Traffic>,
    #[serde(default)]
    pub baseline: BTreeMap<String, Traffic>,
    /// Cut off by hand.
    #[serde(default)]
    pub blocked: BTreeSet<String>,
    /// Cut off automatically for exhausting the quota; cleared on reset.
    #[serde(default)]
    pub limited: BTreeSet<String>,
    /// Cut off automatically for passing their expiry date. Unlike `limited`
    /// this survives a period reset — a new month does not renew an account.
    #[serde(default)]
    pub expired: BTreeSet<String>,
    /// Start of the period the counters belong to.
    #[serde(default)]
    pub reset_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
}

impl Ledger {
    /// Whether the user must not be present in any inbound right now.
    pub fn is_cut_off(&self, user: &str) -> bool {
        self.blocked.contains(user) || self.limited.contains(user) || self.expired.contains(user)
    }

    /// Everyone currently cut off, in the shape pushed to agents.
    pub fn cut_off(&self) -> BTreeSet<String> {
        self.blocked
            .iter()
            .chain(&self.limited)
            .chain(&self.expired)
            .cloned()
            .collect()
    }

    /// Folds one node's cumulative counters into the period totals, returning
    /// the bytes added.
    pub fn absorb(&mut self, node: &str, usage: &Usage) -> u64 {
        let mut added = 0;
        for (user, traffic) in &usage.users {
            let key = format!("{node}/{user}");
            let Some(previous) = self.baseline.insert(key, *traffic) else {
                continue;
            };
            let rise = traffic.since(&previous);
            if rise.total() > 0 {
                self.spent.entry(user.clone()).or_default().add(rise);
                added += rise.total();
            }
        }
        added
    }

    /// Bytes spent this period by a user, per direction.
    pub fn spent_by(&self, user: &str) -> Traffic {
        self.spent.get(user).copied().unwrap_or_default()
    }
}

pub fn load<T: DeserializeOwned + Default>(dir: &Path, name: &str) -> T {
    std::fs::read(dir.join(name))
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default()
}

pub fn save<T: Serialize>(dir: &Path, name: &str, value: &T) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{name}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec(value)?)?;
    std::fs::rename(tmp, dir.join(name))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(pairs: &[(&str, u64, u64)]) -> Usage {
        Usage {
            users: pairs
                .iter()
                .map(|(user, up, down)| {
                    (
                        user.to_string(),
                        Traffic {
                            up: *up,
                            down: *down,
                        },
                    )
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn first_sight_pins_the_baseline_without_counting() {
        let mut ledger = Ledger::default();
        // The agent has been up far longer than this ledger; its history is
        // not traffic spent in the current period.
        assert_eq!(ledger.absorb("stockholm", &usage(&[("me", 40, 60)])), 0);
        assert_eq!(ledger.spent_by("me").total(), 0);
    }

    #[test]
    fn absorbs_only_the_rise() {
        let mut ledger = Ledger::default();
        ledger.absorb("stockholm", &usage(&[("me", 40, 60)]));
        // The node's counters are cumulative, so only the rise is period spend.
        assert_eq!(ledger.absorb("stockholm", &usage(&[("me", 50, 80)])), 30);
        assert_eq!(ledger.spent_by("me").up, 10);
        assert_eq!(ledger.spent_by("me").down, 20);
    }

    #[test]
    fn a_restarted_agent_does_not_double_count() {
        let mut ledger = Ledger::default();
        ledger.absorb("stockholm", &usage(&[("me", 0, 0)]));
        ledger.absorb("stockholm", &usage(&[("me", 100, 400)]));
        // The agent lost its state file and starts counting from zero again.
        assert_eq!(ledger.absorb("stockholm", &usage(&[("me", 5, 15)])), 0);
        assert_eq!(ledger.spent_by("me").total(), 500);
        // The baseline followed it down, so the next rise is counted normally.
        assert_eq!(ledger.absorb("stockholm", &usage(&[("me", 10, 35)])), 25);
        assert_eq!(ledger.spent_by("me").total(), 525);
    }

    #[test]
    fn a_period_reset_does_not_recount_the_node_history() {
        let mut ledger = Ledger::default();
        ledger.absorb("stockholm", &usage(&[("me", 0, 0)]));
        ledger.absorb("stockholm", &usage(&[("me", 100, 900)]));
        assert_eq!(ledger.spent_by("me").total(), 1000);
        // The period rolls over: the totals are cleared, the baseline is not,
        // because the node keeps counting from where it was.
        ledger.spent.clear();
        ledger.absorb("stockholm", &usage(&[("me", 110, 940)]));
        assert_eq!(ledger.spent_by("me").total(), 50);
    }

    #[test]
    fn nodes_are_counted_separately_but_summed_per_user() {
        let mut ledger = Ledger::default();
        ledger.absorb("stockholm", &usage(&[("me", 0, 0)]));
        ledger.absorb("asgard", &usage(&[("me", 0, 0)]));
        ledger.absorb("stockholm", &usage(&[("me", 40, 60)]));
        ledger.absorb("asgard", &usage(&[("me", 30, 40)]));
        assert_eq!(ledger.spent_by("me").total(), 170);
    }

    #[test]
    fn cut_off_merges_manual_quota_and_expiry() {
        let mut ledger = Ledger::default();
        ledger.blocked.insert("a".into());
        ledger.limited.insert("b".into());
        ledger.limited.insert("a".into());
        ledger.expired.insert("c".into());
        assert!(ledger.is_cut_off("a"));
        assert!(ledger.is_cut_off("b"));
        assert!(ledger.is_cut_off("c"));
        assert!(!ledger.is_cut_off("d"));
        assert_eq!(ledger.cut_off().len(), 3);
    }
}
