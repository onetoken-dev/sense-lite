//! Acer Sense Agent(QA 5141 / CC 4343 / DI 4449)WebSocket 协议编解码。
//! 协议细节见 ZCodeProject/acer-sense-protocol.md §2。

use aes::Aes128;
use base64::Engine;
use ecb::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyInit};
use serde_json::{json, Value};

/// 服务端与客户端共享的固定密钥(逆向自官方前端,三服务通用)。
pub const KEY_A: [u8; 16] = *b"A6052DC8A6E44210";
pub const KEY_B: &str = "AB252AB73BED1CDB";

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Service {
    QuickAccess,
    CareCenter,
    DeviceInfo,
}

impl Service {
    pub const ALL: [Service; 3] = [
        Service::QuickAccess,
        Service::CareCenter,
        Service::DeviceInfo,
    ];

    pub const fn port(self) -> u16 {
        match self {
            Service::QuickAccess => 5141,
            Service::CareCenter => 4343,
            Service::DeviceInfo => 4449,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Service::QuickAccess => "QuickAccess",
            Service::CareCenter => "CareCenter",
            Service::DeviceInfo => "DeviceInfo",
        }
    }

    /// 握手问句,每个服务不同。
    pub const fn question(self) -> &'static str {
        match self {
            Service::QuickAccess => "AcerQuickAccessFunctionalityTests_HandshakingQuestion",
            Service::CareCenter => "AcerCareCenterFunctionalityTests_HandshakingQuestion",
            Service::DeviceInfo => "AcerDeviceInfoFunctionalityTests_HandshakingQuestion",
        }
    }
}

type Aes128EcbEnc = ecb::Encryptor<Aes128>;

pub fn aes_ecb_encrypt(key: &[u8; 16], plaintext: &[u8]) -> Vec<u8> {
    let enc = <Aes128EcbEnc as KeyInit>::new(key.into());
    enc.encrypt_padded_vec_mut::<Pkcs7>(plaintext)
}

/// 构造 AuthPacket 的 Data 字段:B64(AES-ECB(KEY_A, {"Question":…,"Key":KEY_B}))
/// 字段顺序必须与官方一致(serde_json 按结构体声明顺序序列化)。
pub fn encode_auth_data(service: Service) -> String {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "PascalCase")]
    struct AuthPayload<'a> {
        question: &'a str,
        key: &'a str,
    }
    let text = serde_json::to_string(&AuthPayload {
        question: service.question(),
        key: KEY_B,
    })
    .expect("static payload serializes");
    base64::engine::general_purpose::STANDARD.encode(aes_ecb_encrypt(&KEY_A, text.as_bytes()))
}

pub fn auth_packet_for(service: Service, session: &str) -> Value {
    json!({
        "PacketType": 1,
        "Session": session,
        "Version": 1,
        "Data": encode_auth_data(service),
    })
}

pub fn request_packet(
    service: Service,
    session: &str,
    command: &str,
    action: &str,
    param1: Option<Value>,
) -> Value {
    let mut pkt = json!({
        "PacketType": 2,
        "Version": 1,
        "Session": session,
        "Command": command,
        "Action": action,
    });
    if let Some(p) = param1 {
        pkt["Param1"] = p;
    }
    let _ = service; // 命令包与服务无关
    pkt
}

/// 服务端入站包分类。
#[derive(Debug)]
pub enum Inbound {
    /// PacketType=1,服务端握手质询(内容为 AES-ECB(KEY_B, 问句),本实现不校验内容)
    AuthChallenge,
    /// PacketType=3,响应(command/session/result)
    Response(Value),
    /// Action="Notify",服务端主动推送
    Notify(Value),
    /// PacketType=9
    Error(Value),
    Other,
}

pub fn classify(v: Value) -> Inbound {
    if v.get("Action").and_then(|a| a.as_str()) == Some("Notify") {
        return Inbound::Notify(v);
    }
    match v.get("PacketType").and_then(|p| p.as_i64()) {
        Some(1) => Inbound::AuthChallenge,
        Some(3) => Inbound::Response(v),
        Some(9) => Inbound::Error(v),
        _ => Inbound::Other,
    }
}

pub fn field_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|x| x.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 与官方前端 JS(CryptoJS AES-128-ECB-PKCS7 + base64)输出逐字节一致,
    /// 期望值取自 probe.js 实测抓包。
    #[test]
    fn auth_data_matches_official_client() {
        let data = encode_auth_data(Service::QuickAccess);
        // probe.js 对同一明文 {"Question":"…QuickAccess…","Key":"AB252AB73BED1CDB"} 的输出前缀
        assert!(
            data.starts_with("OO24T5iPpUKxfxjPZW4dd"),
            "unexpected ciphertext prefix: {data}"
        );
        // 同一明文必须可复现
        assert_eq!(data, encode_auth_data(Service::QuickAccess));
        // 三个服务的问句不同,密文必须不同
        assert_ne!(
            encode_auth_data(Service::QuickAccess),
            encode_auth_data(Service::CareCenter)
        );
    }

    #[test]
    fn classify_routes_packets() {
        assert!(matches!(
            classify(json!({"PacketType":1,"Data":"x"})),
            Inbound::AuthChallenge
        ));
        assert!(matches!(
            classify(json!({"PacketType":3,"Command":"USBCharge"})),
            Inbound::Response(_)
        ));
        assert!(matches!(
            classify(json!({"Action":"Notify","Command":"BatteryStatus"})),
            Inbound::Notify(_)
        ));
        assert!(matches!(
            classify(json!({"PacketType":9,"ErrorCode":1000})),
            Inbound::Error(_)
        ));
    }
}
