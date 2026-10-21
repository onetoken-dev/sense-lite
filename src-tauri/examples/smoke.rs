//! 真机冒烟测试:连接三个 Agent,验证握手 + FunctionQuery + 读取。
//! 运行:cargo run --example smoke

#[tokio::main]
async fn main() {
    let qa = sense_lite_lib::AgentClient::spawn(sense_lite_lib::Service::QuickAccess);
    let cc = sense_lite_lib::AgentClient::spawn(sense_lite_lib::Service::CareCenter);
    let di = sense_lite_lib::AgentClient::spawn(sense_lite_lib::Service::DeviceInfo);

    // 等握手 + 能力查询
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;

    for c in [&qa, &cc, &di] {
        println!(
            "[{}] connected={} caps={:?}",
            c.service().name(),
            c.connected(),
            c.capabilities()
        );
    }

    match qa.get("SystemUsageControl").await {
        Ok(v) => println!("SystemUsageControl = {}", v.get("Result").unwrap_or(&v)),
        Err(e) => println!("SystemUsageControl ERR: {e}"),
    }
    match cc.get("BatteryStatus").await {
        Ok(v) => println!("BatteryStatus = {}", v.get("Result").unwrap_or(&v)),
        Err(e) => println!("BatteryStatus ERR: {e}"),
    }
    match di.get("SystemInfo").await {
        Ok(v) => println!("SystemInfo = {}", v.get("Result").unwrap_or(&v)),
        Err(e) => println!("SystemInfo ERR: {e}"),
    }

    // 订阅 Notify 5 秒(插拔电源可触发 BatteryStatus 推送)
    let mut rx = cc.subscribe_notify();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await;
    println!("smoke done");
}
