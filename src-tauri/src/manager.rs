//! 三个 Agent 客户端的聚合入口,对外提供按服务的统一读写。

use serde_json::Value;

use crate::agent::AgentClient;
use crate::protocol::Service;

#[derive(Clone)]
pub struct Manager {
    pub qa: AgentClient,
    pub cc: AgentClient,
    pub di: AgentClient,
}

impl Manager {
    pub fn spawn_all() -> Self {
        Self {
            qa: AgentClient::spawn(Service::QuickAccess),
            cc: AgentClient::spawn(Service::CareCenter),
            di: AgentClient::spawn(Service::DeviceInfo),
        }
    }

    pub fn client(&self, service: Service) -> &AgentClient {
        match service {
            Service::QuickAccess => &self.qa,
            Service::CareCenter => &self.cc,
            Service::DeviceInfo => &self.di,
        }
    }

    pub async fn get(&self, service: Service, command: &str) -> Result<Value, String> {
        self.client(service).get(command).await
    }

    pub async fn set(&self, service: Service, command: &str, param1: Value) -> Result<Value, String> {
        self.client(service).set(command, param1).await
    }
}

pub fn service_by_port(port: u16) -> Option<Service> {
    Service::ALL.into_iter().find(|s| s.port() == port)
}
