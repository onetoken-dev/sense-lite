//! 暴露给前端的 Tauri commands。业务包的 Result 字段已在此剥出。

use serde_json::Value;
use tauri::State;

use crate::manager::{service_by_port, Manager};

fn extract_result(packet: Value) -> Value {
    match packet.get("Result") {
        Some(r) => r.clone(),
        None => packet,
    }
}

fn service_or_err(port: u16) -> Result<crate::protocol::Service, String> {
    service_by_port(port).ok_or_else(|| format!("unknown port {port}"))
}

#[tauri::command]
pub async fn get_data(
    manager: State<'_, Manager>,
    port: u16,
    command: String,
) -> Result<Value, String> {
    let service = service_or_err(port)?;
    let packet = manager.get(service, &command).await?;
    Ok(extract_result(packet))
}

#[tauri::command]
pub async fn set_data(
    manager: State<'_, Manager>,
    port: u16,
    command: String,
    param: Value,
) -> Result<Value, String> {
    let service = service_or_err(port)?;
    let packet = manager.set(service, &command, param).await?;
    Ok(extract_result(packet))
}

#[derive(serde::Serialize)]
pub struct AgentStatus {
    pub port: u16,
    pub service: &'static str,
    pub connected: bool,
    pub capabilities: Option<Vec<String>>,
}

#[tauri::command]
pub async fn agent_status(manager: State<'_, Manager>) -> Result<Vec<AgentStatus>, String> {
    Ok(crate::protocol::Service::ALL
        .into_iter()
        .map(|s| {
            let c = manager.client(s);
            AgentStatus {
                port: s.port(),
                service: s.name(),
                connected: c.connected(),
                capabilities: c.capabilities(),
            }
        })
        .collect())
}
