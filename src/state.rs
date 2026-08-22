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
    #[serde(default)]
    pub used: BTreeMap<String, u64>,
    /// Last cumulative total observed per `node/user`, so each poll can add only
    /// the delta. An agent that lost its state reports a smaller number than
    /// before; the delta saturates to zero and the baseline follows it down.
    #[serde(default)]
    pub seen: BTreeMap<String, u64>,
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
    /// the bytes added. Counters going backwards means the agent restarted with
    /// lost state, so the baseline is simply re-pinned.
    pub fn absorb(&mut self, node: &str, usage: &Usage) -> u64 {
        let mut added = 0;
        for (user, traffic) in &usage.users {
            let key = format!("{node}/{user}");
            let total = traffic.total();
            let previous = self.seen.insert(key, total).unwrap_or(0);
            let delta = total.saturating_sub(previous);
            if delta > 0 {
                *self.used.entry(user.clone()).or_default() += delta;
                added += delta;
            }
        }
        added
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

    fn usage(pairs: &[(&str, u64)]) -> Usage {
        Usage {
            users: pairs
                .iter()
                .map(|(user, total)| {
                    (
                        user.to_string(),
                        Traffic {
                            up: *total,
                            down: 0,
                        },
                    )
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn absorbs_only_the_delta() {
        let mut ledger = Ledger::default();
        assert_eq!(ledger.absorb("stockholm", &usage(&[("me", 100)])), 100);
        // The node's counter is cumulative, so a second poll adds only the rise.
        assert_eq!(ledger.absorb("stockholm", &usage(&[("me", 130)])), 30);
        assert_eq!(ledger.used["me"], 130);
    }

    #[test]
    fn a_restarted_agent_does_not_double_count() {
        let mut ledger = Ledger::default();
        ledger.absorb("stockholm", &usage(&[("me", 500)]));
        // The agent lost its state file and starts counting from zero again.
        assert_eq!(ledger.absorb("stockholm", &usage(&[("me", 20)])), 0);
        assert_eq!(ledger.used["me"], 500);
        // The baseline followed it down, so the next rise is counted normally.
        assert_eq!(ledger.absorb("stockholm", &usage(&[("me", 45)])), 25);
        assert_eq!(ledger.used["me"], 525);
    }

    #[test]
    fn nodes_are_counted_separately_but_summed_per_user() {
        let mut ledger = Ledger::default();
        ledger.absorb("stockholm", &usage(&[("me", 100)]));
        ledger.absorb("asgard", &usage(&[("me", 70)]));
        assert_eq!(ledger.used["me"], 170);
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
