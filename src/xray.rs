use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::{Context, Result};
use prost::Message;
use tonic::transport::{Channel, Endpoint};

mod pb {
    include!(concat!(env!("OUT_DIR"), "/xray.rs"));
}

use pb::xray::app::proxyman::command::handler_service_client::HandlerServiceClient;
use pb::xray::app::proxyman::command::{
    AddUserOperation, AlterInboundRequest, GetInboundUserRequest, RemoveUserOperation,
};
use pb::xray::app::stats::command::stats_service_client::StatsServiceClient;
use pb::xray::app::stats::command::{QueryStatsRequest, SysStatsRequest};
use pb::xray::common::protocol::User;
use pb::xray::common::serial::TypedMessage;
use pb::xray::proxy::vless::Account;

#[derive(Clone)]
pub struct Xray {
    stats: StatsServiceClient<Channel>,
    handler: HandlerServiceClient<Channel>,
}

fn typed<M: Message>(name: &str, msg: &M) -> TypedMessage {
    TypedMessage {
        r#type: name.into(),
        value: msg.encode_to_vec(),
    }
}

impl Xray {
    pub fn new(api: &str) -> Result<Self> {
        let channel = Endpoint::from_shared(format!("http://{api}"))?
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .connect_lazy();
        Ok(Self {
            stats: StatsServiceClient::new(channel.clone()),
            handler: HandlerServiceClient::new(channel),
        })
    }

    pub async fn alive(&self) -> bool {
        self.stats
            .clone()
            .get_sys_stats(SysStatsRequest {})
            .await
            .is_ok()
    }

    pub async fn traffic(&self, reset: bool) -> Result<Vec<(String, String, u64)>> {
        let r = self
            .stats
            .clone()
            .query_stats(QueryStatsRequest {
                pattern: "traffic>>>".into(),
                reset,
            })
            .await
            .context("query stats")?;
        Ok(r.into_inner()
            .stat
            .into_iter()
            .filter_map(|s| match s.name.split(">>>").collect::<Vec<_>>()[..] {
                ["user", email, "traffic", dir] => {
                    Some((email.to_string(), dir.to_string(), s.value.max(0) as u64))
                }
                _ => None,
            })
            .collect())
    }

    pub async fn users(&self, tag: &str) -> Result<BTreeSet<String>> {
        let r = self
            .handler
            .clone()
            .get_inbound_users(GetInboundUserRequest {
                tag: tag.into(),
                email: String::new(),
            })
            .await
            .with_context(|| format!("list users of {tag}"))?;
        Ok(r.into_inner().users.into_iter().map(|u| u.email).collect())
    }

    pub async fn add_user(&self, tag: &str, email: &str, id: &str, flow: &str) -> Result<()> {
        let account = Account {
            id: id.into(),
            flow: flow.into(),
            encryption: String::new(),
        };
        let op = AddUserOperation {
            user: Some(User {
                level: 0,
                email: email.into(),
                account: Some(typed("xray.proxy.vless.Account", &account)),
            }),
        };
        self.alter(
            tag,
            typed("xray.app.proxyman.command.AddUserOperation", &op),
        )
        .await
        .with_context(|| format!("add user {email}"))
    }

    pub async fn remove_user(&self, tag: &str, email: &str) -> Result<()> {
        let op = RemoveUserOperation {
            email: email.into(),
        };
        self.alter(
            tag,
            typed("xray.app.proxyman.command.RemoveUserOperation", &op),
        )
        .await
        .with_context(|| format!("remove user {email}"))
    }

    async fn alter(&self, tag: &str, operation: TypedMessage) -> Result<()> {
        self.handler
            .clone()
            .alter_inbound(AlterInboundRequest {
                tag: tag.into(),
                operation: Some(operation),
            })
            .await?;
        Ok(())
    }
}
