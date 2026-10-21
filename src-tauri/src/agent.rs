//! 单个 Agent 服务的长连接客户端:握手、请求/响应配对、Notify 广播、断线重连。
//!
//! 协议要点(见 acer-sense-protocol.md §2):
//! - 客户端必须先发 AuthPacket,服务端才回质询(应答式);
//! - 握手完成后所有业务包为明文 JSON;
//! - Agent 对高频反复建连会节流(约 15-20s),因此必须单连接复用,重连退避较长。

use std::collections::HashMap;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{Connector, WebSocketStream};

use crate::protocol::{self, Service};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const RECONNECT_MIN: Duration = Duration::from_secs(5);
const RECONNECT_MAX: Duration = Duration::from_secs(30);
/// Agent 节流观察值 ~15-20s,从未连上过(或被节流)时至少等这么久。
const THROTTLE_GUARD: Duration = Duration::from_secs(15);

#[derive(Clone)]
pub struct AgentClient {
    service: Service,
    cmd_tx: mpsc::Sender<Cmd>,
    status_rx: watch::Receiver<bool>,
    caps_rx: watch::Receiver<Option<Vec<String>>>,
    notify_tx: broadcast::Sender<Value>,
}

struct Cmd {
    packet: Value,
    session: String,
    reply: oneshot::Sender<Result<Value, String>>,
}

impl AgentClient {
    /// 启动连接任务。需在 tokio 运行时上下文中调用(tauri::async_runtime::spawn)。
    pub fn spawn(service: Service) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>(32);
        let (notify_tx, _) = broadcast::channel(64);
        let (status_tx, status_rx) = watch::channel(false);
        let (caps_tx, caps_rx) = watch::channel(None);
        // 用 tauri::async_runtime 而非 tokio::spawn:setup 闭包运行在主线程,
        // 无 tokio 运行时上下文;tauri 的全局运行时可在任意线程提交任务。
        tauri::async_runtime::spawn(run_loop(
            service,
            cmd_rx,
            notify_tx.clone(),
            status_tx,
            caps_tx,
        ));
        Self {
            service,
            cmd_tx,
            status_rx,
            caps_rx,
            notify_tx,
        }
    }

    pub fn service(&self) -> Service {
        self.service
    }

    pub fn connected(&self) -> bool {
        *self.status_rx.borrow()
    }

    pub fn capabilities(&self) -> Option<Vec<String>> {
        self.caps_rx.borrow().clone()
    }

    pub fn subscribe_notify(&self) -> broadcast::Receiver<Value> {
        self.notify_tx.subscribe()
    }

    pub async fn get(&self, command: &str) -> Result<Value, String> {
        self.request(command, "Get", None).await
    }

    pub async fn set(&self, command: &str, param1: Value) -> Result<Value, String> {
        self.request(command, "Set", Some(param1)).await
    }

    pub async fn request(
        &self,
        command: &str,
        action: &str,
        param1: Option<Value>,
    ) -> Result<Value, String> {
        // 超时自动重试一次:节流窗口期可能吞包;重试用新 session。
        let mut last_err = String::new();
        for attempt in 0..2 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(800)).await;
            }
            match self.request_once(command, action, param1.clone()).await {
                Ok(v) => return Ok(v),
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }

    async fn request_once(
        &self,
        command: &str,
        action: &str,
        param1: Option<Value>,
    ) -> Result<Value, String> {
        if !self.connected() {
            return Err(format!("{}: not connected", self.service.name()));
        }
        let session = uuid::Uuid::new_v4().to_string();
        let packet = protocol::request_packet(self.service, &session, command, action, param1);
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Cmd {
                packet,
                session,
                reply: tx,
            })
            .await
            .map_err(|_| "connection task died".to_string())?;
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => Err("no reply (connection closed?)".into()),
            Err(_) => Err("request timeout".into()),
        }
    }
}

async fn run_loop(
    service: Service,
    mut cmd_rx: mpsc::Receiver<Cmd>,
    notify_tx: broadcast::Sender<Value>,
    status_tx: watch::Sender<bool>,
    caps_tx: watch::Sender<Option<Vec<String>>>,
) {
    let mut backoff = RECONNECT_MIN;
    loop {
        let result =
            connect_and_serve(service, &mut cmd_rx, &notify_tx, &status_tx, &caps_tx).await;
        // serve 退出后 status 仍为握手成功与否的值,借此判断是否连上过
        let was_connected = *status_tx.borrow();
        status_tx.send_replace(false);
        caps_tx.send_replace(None);

        let err = match result {
            Ok(()) => "stream ended".to_string(), // 不会发生,防御性处理
            Err(e) => e,
        };
        let _ = err;

        // 从未握手成功 → 可能被节流,至少等 THROTTLE_GUARD;连上过则重置退避
        let wait = if was_connected {
            backoff = RECONNECT_MIN;
            backoff
        } else {
            backoff = (backoff * 2).min(RECONNECT_MAX);
            backoff.max(THROTTLE_GUARD)
        };
        tokio::time::sleep(wait).await;
    }
}

async fn connect_and_serve(
    service: Service,
    cmd_rx: &mut mpsc::Receiver<Cmd>,
    notify_tx: &broadcast::Sender<Value>,
    status_tx: &watch::Sender<bool>,
    caps_tx: &watch::Sender<Option<Vec<String>>>,
) -> Result<(), String> {
    use tokio_tungstenite::connect_async_tls_with_config;

    let url = format!("wss://localhost:{}", service.port());
    let request = url
        .into_client_request()
        .map_err(|e| format!("connect: bad request url: {e}"))?;
    let tls = native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .build()
        .map_err(|e| format!("connect: tls builder: {e}"))?;

    let (ws, _) = connect_async_tls_with_config(request, None, false, Some(Connector::NativeTls(tls)))
        .await
        .map_err(|e| format!("connect: {e}"))?;

    serve(service, ws, cmd_rx, notify_tx, status_tx, caps_tx).await
}

async fn serve(
    service: Service,
    ws: WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    cmd_rx: &mut mpsc::Receiver<Cmd>,
    notify_tx: &broadcast::Sender<Value>,
    status_tx: &watch::Sender<bool>,
    caps_tx: &watch::Sender<Option<Vec<String>>>,
) -> Result<(), String> {
    let (mut sink, mut stream) = ws.split();
    let mut pending: HashMap<String, Pending> = HashMap::new();
    let mut handshaken = false;

    // 应用层保活:每 30s 发一次 FunctionQuery(协议保证有响应)。
    // 若连续两次无响应 → 连接假死,主动断开让 run_loop 重连。
    let mut keepalive = tokio::time::interval(Duration::from_secs(30));
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut keepalive_pending = false;

    // 协议要求:客户端先发 auth
    let auth_session = uuid::Uuid::new_v4().to_string();
    sink.send(Message::text(
        protocol::auth_packet_for(service, &auth_session).to_string(),
    ))
    .await
    .map_err(|e| format!("send auth: {e}"))?;

    loop {
        tokio::select! {
            _ = keepalive.tick() => {
                if !handshaken { continue; }
                if keepalive_pending {
                    return Err("keepalive timeout (zombie connection)".into());
                }
                let s = uuid::Uuid::new_v4().to_string();
                if sink
                    .send(Message::text(
                        protocol::request_packet(service, &s, "FunctionQuery", "Get", None)
                            .to_string(),
                    ))
                    .await
                    .is_err()
                {
                    return Err("keepalive send failed".into());
                }
                keepalive_pending = true;
            }
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { return Err("command channel closed".into()) };
                if !handshaken {
                    let _ = cmd.reply.send(Err(format!("{}: not handshaken", service.name())));
                    continue;
                }
                pending.insert(cmd.session, Pending { tx: cmd.reply });
                if sink.send(Message::text(cmd.packet.to_string())).await.is_err() {
                    return Err("send failed".into());
                }
            }
            msg = stream.next() => {
                let Some(msg) = msg else { return Err("stream ended".into()) };
                let msg = msg.map_err(|e| format!("recv: {e}"))?;
                let text = match msg {
                    Message::Text(t) => t,
                    Message::Close(_) => return Err("closed by peer".into()),
                    Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_) => continue,
                };
                let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                match protocol::classify(v) {
                    protocol::Inbound::AuthChallenge => {
                        handshaken = true;
                        status_tx.send_replace(true);
                        // 惯例:握手后立即查询能力表(响应只用于更新 caps,无需挂起)
                        let s = uuid::Uuid::new_v4().to_string();
                        let _ = sink
                            .send(Message::text(
                                protocol::request_packet(service, &s, "FunctionQuery", "Get", None)
                                    .to_string(),
                            ))
                            .await;
                    }
                    protocol::Inbound::Response(v) => {
                        let session = protocol::field_str(&v, "Session").unwrap_or("").to_string();
                        if v.get("Command").and_then(|c| c.as_str()) == Some("FunctionQuery") {
                            keepalive_pending = false;
                            let caps: Vec<String> = v
                                .get("Data")
                                .and_then(|d| d.as_array())
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|x| x.as_str().map(str::to_string))
                                        .collect()
                                })
                                .unwrap_or_default();
                            let _ = caps_tx.send(Some(caps));
                        } else if let Some(p) = pending.remove(&session) {
                            let _ = p.tx.send(Ok(v));
                        }
                    }
                    protocol::Inbound::Notify(v) => {
                        let _ = notify_tx.send(v);
                    }
                    protocol::Inbound::Error(v) => {
                        let session = protocol::field_str(&v, "Session").unwrap_or("").to_string();
                        let code = v.get("ErrorCode").and_then(|c| c.as_i64()).unwrap_or(0);
                        let msg = v
                            .get("Message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("error")
                            .to_string();
                        if let Some(p) = pending.remove(&session) {
                            let _ = p.tx.send(Err(format!("error {code}: {msg}")));
                        }
                    }
                    protocol::Inbound::Other => {}
                }
            }
        }
    }
}

struct Pending {
    tx: oneshot::Sender<Result<Value, String>>,
}
