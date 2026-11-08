use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;
use reqwest::Client;
use tokio::sync::RwLock;

use crate::config::Config;
use crate::remote;
use crate::state::{self, Ledger, Traffic, Usage};
use crate::stats::{self, Fetches, History};

const LEDGER_FILE: &str = "ledger.json";
const POLL: Duration = Duration::from_secs(60);

pub struct Controller {
    pub cfg: Config,
    pub client: Client,
    pub ledger: RwLock<Ledger>,
    /// Last snapshot from each node, for display only.
    pub nodes: RwLock<BTreeMap<String, Usage>>,
    /// Where the ledger's bytes came from over time. Display only.
    pub history: RwLock<History>,
    /// Who fetched which subscription. Display only.
    pub fetches: RwLock<Fetches>,
}

impl Controller {
    pub fn new(cfg: Config) -> Result<Arc<Self>> {
        let ledger: Ledger = state::load(&cfg.state_dir, LEDGER_FILE);
        Ok(Arc::new(Self {
            client: remote::client()?,
            ledger: RwLock::new(ledger),
            nodes: RwLock::new(BTreeMap::new()),
            history: RwLock::new(state::load(&cfg.state_dir, stats::HISTORY_FILE)),
            fetches: RwLock::new(state::load(&cfg.state_dir, stats::FETCHES_FILE)),
            cfg,
        }))
    }

    /// One full cycle: collect, roll the period over if due, enforce, push.
    pub async fn cycle(&self) -> Result<()> {
        self.collect().await;
        let changed = self.settle().await?;
        if changed {
            self.push().await;
        }
        Ok(())
    }

    /// Pulls counters from every node. A node being down is not fatal — its
    /// numbers simply do not advance until it comes back.
    async fn collect(&self) {
        let now = Utc::now();
        let mut collected = Vec::new();
        for node in &self.cfg.nodes {
            match remote::fetch_usage(&self.client, &self.cfg, node).await {
                Ok(usage) => {
                    let rises = self.ledger.write().await.absorb(&node.name, &usage);
                    collected.push((node.name.as_str(), rises));
                    self.nodes.write().await.insert(node.name.clone(), usage);
                }
                Err(e) => eprintln!("traffic {}: {e:#}", node.name),
            }
        }
        // History is insight, not accounting: failing to persist it must not
        // stop the cycle that enforces quotas.
        let mut history = self.history.write().await;
        for (node, rises) in &collected {
            history.record(node, rises, now);
        }
        history.compact(&self.cfg.quota, now);
        if let Err(e) = state::save(&self.cfg.state_dir, stats::HISTORY_FILE, &*history) {
            eprintln!("save history: {e:#}");
        }
    }

    /// Notes a subscription fetch. Best effort, like the history.
    pub async fn record_fetch(&self, user: &str, ip: &str, ua: &str, format: &str) {
        let mut fetches = self.fetches.write().await;
        fetches.record(user, ip, ua, format, Utc::now());
        if let Err(e) = state::save(&self.cfg.state_dir, stats::FETCHES_FILE, &*fetches) {
            eprintln!("save fetches: {e:#}");
        }
    }

    /// Applies the period reset and the quota rules. Returns whether the
    /// cut-off set changed and therefore needs pushing.
    async fn settle(&self) -> Result<bool> {
        let now = Utc::now();
        let mut ledger = self.ledger.write().await;
        let before = ledger.cut_off();

        if ledger.reset_at.is_none() {
            // First run: adopt the current period rather than wiping counters.
            ledger.reset_at = Some(self.cfg.quota.period_start(now));
        } else if self.cfg.quota.is_due(ledger.reset_at, now) {
            eprintln!("quota period rolled over, clearing counters");
            ledger.spent.clear();
            ledger.limited.clear();
            ledger.reset_at = Some(self.cfg.quota.period_start(now));
        }

        for user in &self.cfg.users {
            // Admins are never cut off automatically: locking yourself out of
            // your own fleet is worse than overshooting a quota.
            if user.admin {
                continue;
            }

            let expired = user
                .expires
                .is_some_and(|date| self.cfg.quota.is_expired(date, now));
            if expired {
                if ledger.expired.insert(user.name.clone()) {
                    eprintln!("{} expired", user.name);
                }
            } else if ledger.expired.remove(&user.name) {
                eprintln!("{} is valid again", user.name);
            }

            let Some(limit) = self.cfg.limit_of(user) else {
                continue;
            };
            let used = ledger.spent_by(&user.name).total();
            if used >= limit {
                if ledger.limited.insert(user.name.clone()) {
                    eprintln!("{} exhausted quota ({used} of {limit} bytes)", user.name);
                }
            } else if ledger.limited.remove(&user.name) {
                eprintln!("{} is back under quota", user.name);
            }
        }

        ledger.updated_at = Some(now);
        state::save(&self.cfg.state_dir, LEDGER_FILE, &*ledger)?;
        Ok(ledger.cut_off() != before)
    }

    /// Pushes the cut-off set to every node. Declarative and idempotent, so a
    /// node that missed an update converges on the next cycle.
    pub async fn push(&self) {
        let cut_off = self.ledger.read().await.cut_off();
        for node in &self.cfg.nodes {
            if let Err(e) =
                remote::post_json(&self.client, &self.cfg, node, "/cutoff", &cut_off).await
            {
                eprintln!("cutoff {}: {e:#}", node.name);
            }
        }
    }

    /// Blocks or unblocks by hand. Admins are protected from blocking for the
    /// same reason they are exempt from quota enforcement.
    pub async fn set_blocked(&self, user: &str, blocked: bool) -> Result<()> {
        let u = self
            .cfg
            .user(user)
            .ok_or_else(|| anyhow::anyhow!("no such user: {user}"))?;
        anyhow::ensure!(!(blocked && u.admin), "cannot block admin: {user}");
        {
            let mut ledger = self.ledger.write().await;
            if blocked {
                ledger.blocked.insert(user.to_string());
            } else {
                ledger.blocked.remove(user);
            }
            state::save(&self.cfg.state_dir, LEDGER_FILE, &*ledger)?;
        }
        self.push().await;
        Ok(())
    }

    pub async fn spent_by(&self, user: &str) -> Traffic {
        self.ledger.read().await.spent_by(user)
    }
}

pub async fn run_poller(controller: Arc<Controller>) {
    // Converge the nodes once at startup before settling into the cycle.
    controller.push().await;
    let mut ticker = tokio::time::interval(POLL);
    loop {
        ticker.tick().await;
        if let Err(e) = controller.cycle().await {
            eprintln!("cycle: {e:#}");
        }
    }
}
