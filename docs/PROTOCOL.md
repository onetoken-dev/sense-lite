# Acer Sense 底层协议逆向记录(v2 完整版)

| 项目 | 内容 |
|---|---|
| 目标软件 | Acer Sense 5.0.2079.0(Store 签名 `ULICTekInc.AcerSense5.0`,开发商 ULIC Tek) |
| 后台服务版本 | QA / CC / DI Agent `ServiceVersion 1.4.50` |
| 测试机型 | Acer Swift 3 SF314-512(主板 Shark_ADP,BIOS V1.14,i5-1240P,Win11 24H2 26100) |
| 逆向方法 | 解包 `app.asar` → 静态分析(主进程 `dist/main/main.js`、渲染层 `dist/widgets/main/main.*.js`)→ 本机实探验证 |
| 实测工具 | Node 24(全局 WebSocket + crypto),探针脚本 `%TEMP%\asarsense\probe.js` |
| 状态标注 | ✅=已实测通过;📄=静态分析得出,未实测;⚠️=行为异常/未决 |

---

## 1. 架构总览

```
┌────────────────────────────────────────────────────────────────────┐
│ 界面层  AcerSense.exe (Electron/Chromium, MSIX)                     │
│  ├─ 渲染层(调用 QA/CC/DI 的 WebSocket 客户端都编译在前端 JS 里)      │
│  ├─ xsense_native.node(自研原生模块:直连 WMI、显示器控制)           │
│  ├─ NodeRT(WinRT:应用管理/设备枚举/包部署)                          │
│  └─ 内部服务:15150 MQTT 总线 + 6888 inApp 内容服务器(均为自身)      │
└──────────────┬─────────────────────────────────────────────────────┘
               │ wss / https (127.0.0.1)
┌──────────────▼─────────────────────────────────────────────────────┐
│ 服务层(常驻,各监听一个本地端口)                                     │
│  5141 AcerQAAgent    wss  Quick Access(性能/USB充电/护眼/HDR…)      │
│  4343 AcerCCAgent    wss  Care Center(电池/清理)                    │
│  4449 AcerDIAgent    wss  Device Info(系统信息)                     │
│ 15152 AcerServiceSvc https SSDK(驱动更新/提权执行/质保/FPS)          │
│ 19443 AcerEZService  https exzone 首页内容服务器                     │
│ 46753/46760 ASMSvc   tcp/ws 二进制帧 平台监控(温度/风扇/GPU)         │
│ 51779 ADESv2Svc      tcp    二进制帧 设备使能(X-Sense_*)            │
└──────────────┬─────────────────────────────────────────────────────┘
               │ WMI (root\WMI)
┌──────────────▼─────────────────────────────────────────────────────┐
│ 硬件层  ACPI(ACPI\VEN_1025&DEV_165F)→ AML 字节码 → EC              │
│  AcerGenericMethod / AcerGamingFunction / AcerBiosConfigurationTool │
└────────────────────────────────────────────────────────────────────┘
```

服务二进制部署位置(随驱动包,`DriverStore\FileRepository\`):
`acerservicecomponent.inf`(15152)、`sysmonitorservice.inf`(ASMSvc)、`acerdeviceenablingservice*.inf`(ADES)、`acerartaimmxdrivercomponent.inf`(AI:ARTAIMMX 服务 + AcerPixyService)、`acergaicameracomponent.inf`(AI 摄像头)、`ezservicecomponent.inf`(19443)。
常驻 Agent 在 `C:\Windows\System32\`(AcerQAAgent.exe 等)或 `C:\Program Files\Acer*Agent\`。

---

## 2. WebSocket 服务协议(QA 5141 / CC 4343 / DI 4449)✅

三个服务是**同一套协议**的三个实例,仅端口、ServiceName、握手问句不同。

### 2.1 常量(三服务通用,硬编码)

```
KEY_A = "A6052DC8A6E44210"      # 16 字节 UTF-8 → AES-128 密钥
KEY_B = "AB252AB73BED1CDB"      # 16 字节,握手校验用
算法  = AES-128-ECB,输出 base64,无 IV(encodeAES(text, key))
TLS   = 服务端自签证书,客户端 rejectUnauthorized:false(证书校验关闭)
```

每个服务的 `QUESTION_STRING`:
| 端口 | ServiceName | 握手问句 |
|---|---|---|
| 5141 | QuickAccess | `AcerQuickAccessFunctionalityTests_HandshakingQuestion` |
| 4343 | CareCenter | `AcerCareCenterFunctionalityTests_HandshakingQuestion` |
| 4449 | DeviceInfo | `AcerDeviceInfoFunctionalityTests_HandshakingQuestion` |

### 2.2 握手时序 ✅(顺序关键:客户端先发)

```
Client                                   Agent(wss://localhost:PORT)
  │  ws 连接建立(open)                      │
  │ ── AuthPacket ─────────────────────────▶│   客户端必须先发!
  │    {PacketType:1, Session:uuid,         │
  │     Version:1,                          │
  │     Data:B64(AES-ECB(KEY_A,             │
  │       JSON({"Question":"<问句>",        │
  │             "Key":"AB252AB73BED1CDB"})))}│
  │                                         │
  │ ◀──────────────── AuthPacket ───────────│   服务端应答式质询
  │    {PacketType:1,                       │
  │     Data:B64(AES-ECB(KEY_B, "<问句>"))}  │   客户端比对后即完成
  │                                         │
  │  此后所有消息为【明文 JSON】               │
  │ ── {PacketType:2, ..., Command:         │
  │     "FunctionQuery", Action:"Get"} ────▶│   惯例:连上第一包查能力
```

> ⚠️ 实测教训:若等服务端先发质询再发 auth 会死锁——服务端是应答式,必须客户端先发。
> ⚠️ 实测观察:短时间内反复建连,Agent 会停止应答(连接成功但不回包),约 15-20 秒后恢复。客户端应做连接复用。

### 2.3 包格式

```jsonc
// 请求(Get)
{"PacketType":2, "Version":1, "Session":"<uuid>", "Command":"<命令>", "Action":"Get"}
// 请求(Set,单一参数 Param1;部分命令带扩展字段,见 2.6)
{"PacketType":2, "Version":1, "Session":"<uuid>", "Command":"<命令>", "Action":"Set", "Param1":<值>}
// 响应
{"PacketType":3, "Version":1, "Session":"<uuid>", "Command":"<命令>", "Action":"Get",
 "ServiceVersion":"1.4.50", "Result":{...}, "Param1":""}
// 服务端主动通知(无 Session,Action 固定 "Notify")
{"Action":"Notify", "Command":"BatteryStatus", "Result":{...}}
// 错误
{"PacketType":9, "Version":1, "Session":"<uuid>", "ErrorCode":1000, "Message":"General error"}
```

- PacketType:**1**=Auth,**2**=Request,**3**=Response,**9**=Error
- 会话:请求带随机 UUID,响应原样带回,客户端按此配对;`Action:"Notify"` 不带 Session
- ErrorCode 实测/静态:**1000**=General error(如本机不支持该命令);**1006**=SystemUsageControl 不支持;**1007**=DustDefender 相关事件
- `FunctionQuery` 响应附加字段:`ServiceVersion`、`PanelID`(QA 返回,如 "06afa20b63")、`Features`、以及每个命令的 Actions 元数据(DI 返回 `{"SystemInfo":{"Actions":["Get"],...}}`)

### 2.4 能力发现 ✅

连接后发 `FunctionQuery`,响应 `Data` 为字符串数组=本机真实支持的命令集。

| 端口 | 本机实测能力数组(逐字) |
|---|---|
| 5141 QA | `["USBCharge","USBChargeSwitch","SystemUsageControl","SystemUsageModes","SystemUsageModeCapability","BluelightShield","DarkMode","TransparentEffect","AutoHideTaskbar","ScreenSaver","ScreenSaverTimeout","HighlightFeature","HDR","KeyEvent"]` |
| 4449 DI | `["SystemInfo","SMBIOS","DeviceManager","MotherBoard","OS","Storage","PrivacyPolicy","Network","Memory"]` |
| 4343 CC | `["CheckMemory","CheckStorage","QuickCleanup","BatteryStatus","CheckBattery","CleanDisk","BatteryHealthy","BatteryCalibration","BatteryInformation"]` |

> 注意:代码里还有 `FunctionArrayUsageFake=["USBCharge","CoolBoost","SIMControl","DustDefender","BluelightShield","SystemUsageControl"]` 默认值,**CoolBoost / DustDefender / SIMControl 不在本机实测数组中**——它们是其他机型(风扇机型/蜂窝机型)的能力。静态代码全集见 §8 Ue 枚举。**复刻时以 FunctionQuery 返回为准,不要按机型硬编码。**

### 2.5 QA 命令参数语义(渲染层 setter 逐一对应)

| Command | Action | Param1 取值 | 实测 Get 响应 |
|---|---|---|---|
| `USBCharge` | Get/Set | 10 / 20 / 30(电量百分比阈值) | ✅ `{"Value":30,"Text":"Disable USB Charge when battery level under 30%"}` |
| `USBChargeSwitch` | Get/Set | bool | 关机充电总开关 |
| `SystemUsageControl` | Get/Set | 0=Silent / 1=Normal / 2=Performance | 模式查询用 `SystemUsageModes`:✅ `[{"Value":0,"Text":"Silent mode"},{"Value":1,"Text":"Normal mode"},{"Value":2,"Text":"Performance mode"}]` |
| `SystemUsageModeCapability` | Get | — | 模式支持位 |
| `BluelightShield` | Get/Set | 0-5:`Low/Medium/High reduction`、`Long Hour Mode`、5=`Customized` | ✅ `{"Value":5,"Text":"Customized"}` |
| `DarkMode` | Get/Set | bool | |
| `TransparentEffect` | Get/Set | bool | |
| `AutoHideTaskbar` | Get/Set | bool | |
| `ScreenSaver` | Get/Set | bool | |
| `ScreenSaverTimeout` | Get/Set | 秒数(代码中实测值 600) | |
| `AdaptiveBrightness` | Get/Set | bool | |
| `HDR` | Get/Set | bool | ✅ `{"Value":false,"Description":"HDR is disabled."}` |
| `HighlightFeature` | Get | — | AI 功能高亮(Ln 枚举,§8) |
| `KeyEvent` | — | — | 按键事件通道(本机支持,细节未逆) |
| `Whisper` | Get/Set | bool | 静音键盘/低噪模式(非本机能力) |
| `DustDefender` | Get/Set | bool | 除尘(风扇反转清灰,非本机能力) |
| `SIMControl` | Get/Set | 0=不支持 / 1=eSIM / 2=uSIM | ✅ 本机返回 Error 1000(无蜂窝) |
| `VeroEcoPlusUSBCharge`、`VeroModeQuery` | Get/Set | — | Vero 环保机型专有 |
| `DisplayModeCapability/Switch`、`ColorProfileSwitch/Active` | Get/Set | — | 显示模式/色彩配置切换 |
| `SenseAppStatus`、`SystemUsageOSD` | Set | — | 应用状态上报/屏显提示 |

### 2.6 CC(CareCenter)命令细节

| Command | 说明 |
|---|---|
| `BatteryStatus` | Get;✅ `{"ACMode":true,"BatteryPercent":100,"BatterySaverMode":false,"BatteryFlag":1,"RemainingSeconds":-1,"FullLifeTimeSeconds":0}`;**服务端会主动 Notify 推送**(AC 插拔触发) |
| `BatteryHealthy` | Get;✅ `{"Value":1}`(健康度) |
| `CheckBattery` | Get;✅ `{"HealthStatus":-1}` |
| `BatteryInformation` / `BatteryCalibration` | Get / Set(校准启动) |
| `BatteryAdaptiveCharging` | 特殊包:`{"PacketType":2,...,"Command":"BatteryAdaptiveCharging","Action":"Set","AdaptiveCharging":bool}`(不走 Param1);服务端 Notify 带 `Data.EventId`;停止用 `sendStopBAC` |
| `BatteryBoost` | Get/Notify(电池增强,响应 `{"Status":0|1}`,影响 OC 模式可选性) |
| `CheckMemory` / `CheckStorage` | Get(内存/存储体检) |
| `QuickCleanup` | `Action:"Clean"`,扩展字段 `{BrowserCache:bool, TemporaryFiles:bool, RecycleBin:bool}`;进度通过响应 `Result.progress`(100=完成)轮询 |
| `CleanDisk` | 两段式:`Action:"Search"` + `{Directories:[], SearchConditions:{FileSizeMB, LastAccessedDay}}` → 返回 SearchSession;`Action:"Delete"` + `{SearchSession, FileIndex, ForceDelete}` |

充电模式枚举(Fr):`0=OptimizedBatteryCharging, 1=FullBatteryCharging, 2=AISmartCharging`。

### 2.7 DI(DeviceInfo)命令细节

Get 即可:`SystemInfo`(✅ 返回 SerialNumber/Model/RAMType/RAMSizeMB/SNID/CPUName/CPUCores)、`SMBIOS`(✅ Manufacturer/Family/ProductName/Baseboard*/ChassisType)、`DeviceManager`、`MotherBoard`、`OS`、`Storage`(含 StorageVolumes,媒体类型枚举 y1 见 §8)、`Network`、`Memory`。
另:`PrivacyPolicy` Set(bool),服务端会主动 Notify 当前值 `Param1` 与本地存储不一致时要求重设。

---

## 3. SSDK REST(https://localhost:15152,AcerServiceSvc)✅

### 3.1 认证:HS256 JWT,密钥硬编码 ✅

```js
header = {"alg":"HS256","typ":"JWT"}
payload = {"iss":"PWA","iat":<unix秒>}        // 实测带不带 iat 均可
secret  = "18F826BC492E6B8B3C6B5E69B6F73"      // 32 字符,来自渲染层 Upe() 调用
// 请求头:Authorization: Bearer <jwt>
```

### 3.2 接口 ✅

- **能力发现**:`GET /getAllFuncs?q=all` → `{"data":{<类目>:[<函数名>...]}}`
- **调用**:`POST /{类目}/{函数名}`,JSON body 为参数数组,响应 `{"data":...,"result":0}`

### 3.3 实测函数目录 ✅

```json
{"info":   ["getAcerAnswer","getAcerCommunity","getAvailableSize",
            "getCountryFromPublicIP","getVersion","getWarranty"],
 "driver": ["beginInstall","downloadItem","endInstall","getUpdateList",
            "installItem","setCancelStatus"],
 "admin":  ["elevatedRun","elevatedRun2"],
 "etw":    ["IsFPSMonitor","StartFPSMonitor","getFps"]}
```

- `admin.elevatedRun2("", "<命令行>")`:**以管理员权限执行任意命令**(UI 用它卸载/重装 QuickAccess 组件)。替代应用可借它免写 UAC 逻辑;⚠️ 也是无鉴权的本地提权面。
- `driver.*`:宏碁驱动更新流水(getUpdateList → downloadItem → installItem → endInstall),配合 `info.getWarranty`(质保,查询宏碁云端)。
- `etw.*`:基于 ETW 的游戏 FPS 监控。

---

## 4. 内容服务

- **exzone(19443)** ✅:AcerEZService 提供的本地内容服务器,`GET https://localhost:19443/exzone/oem/sensehome/index.html` 返回首页 SPA(301 规范化)。渲染层通过 URL 查询参数控制:`Planet9, Enable3d, EnableAvatarVideo, EnableAvatarApng, useEmbedded, devTool, showWhenReady, reCreateHome, wssport, enableEXZone2, verosku, showTitleBarOverlay` 等。内容缓存于 `C:\ProgramData\OEM...`,可被云端更新。
- **inApp(6888)** 📄:主进程自建 HTTPS 内容服务,`https://localhost:6888/<路径>?<composeParameters>`,加载本地缓存的 OEM 页面。
- **15150 MQTT** 📄:`wss://localhost:15150/mqtt`,**Electron 主进程自己开的 MQTT broker**(mqtt npm 包),主进程↔渲染进程的内部 RPC 桥(`MqttFunctionCall.export("ddsc",...)`、`export("getAllFuncs",...)`)。与硬件服务无关,复刻时直接无视。

---

## 5. 二进制帧协议(PA 46753/46760、ADES 51779)📄⚠️

监控服务(AcerSystemCentralService/ASMSvc)与设备使能服务(ADESv2Svc)共用同一种协议(PSSdkAdapter / ades 类):

```
请求帧 = "ACER"(4字节魔数) + uint32LE(PacketID) + JSON 文本
PacketID: INITIALIZATION=0 | GET_MONITOR_DATA=10 | GET_UPDATED_DATA=20 | SET_DEVICE_DATA=100
响应   = 花括号配平切分的 JSON 对象流(PSSdkStreamReader),按 {"request":"<函数名>"} 匹配回调
可选   = 整帧 base64+cipher 包装(客户端 decipher 存在时启用)
传输   = 46753/46760:ws:// 与 tcp 二选一(客户端断线后交替重试);51779:裸 TCP
```

- `INITIALIZATION` 函数:`VERSION`、`SUPPORT_SYSTEM_CAPABILITY`、`SUPPORT_GPU_INFO`、`SUPPORT_DISPLAY_POWER_SAVER`
- **系统能力位图**(11 位,`Number(v).toString(2)` 右对齐):`[0]GPU2温度 [1]GPU1温度 [2]GPU2风扇 [3]系统2风扇 [4]系统2温度 [5]GPU风扇 [6]外置风扇 [7]系统风扇 [8]系统温度 [9]CPU风扇 [10]CPU温度`
- `GET_UPDATED_DATA` / `SET_DEVICE_DATA` 的数据项(UPDATED_DATA 枚举全集):`LIGHTING, LIGHTING_MODE, FAN_CONTROL, OPERATING_MODE, OVERCLOCKING_MODE, AC_STATUS, TURBO_BUTTON_STATUS, SOUND_MODE, STICKY_KEY, WIN_KEY, LCD_OVERDRIVE, BOOT_SOUND, SUPPORT_OPERATING_MODE, SUPPORT_AUDIO_MODE, SUPPORT_CPU_INFO, SUPPORT_GPU_INFO, BACKLIGHT_30S, GPU_MODE, CUSTOM_BOOT_LOGO, BATTERY_BOOST, ADAPTOR_STATUS, AUTO_REFRESH_RATE`
- 风扇结构(Predator 级):`{"mode":"Auto|Custom","custom_fan_data":[{"fan_custom_auto":1,"fan_custom_speed":30,"fan_index":0,"fan_name":"CPU|GPU|GPU2"}]}`(风扇名 FanName 枚举:CPU/GPU/GPU2)
- ADES 函数命名空间 `X-Sense_*`(实测捕获 `X-Sense_PrivacyPolicyConsensus_ADESV2`,`{Function:..., Parameter:{status:0|1}}`)
- ⚠️ **未决**:两服务端口均接受 TCP 连接,但对裸帧(`ACER`+ID+JSON)不回包;客户端代码显示存在可选 cipher,且服务端可能校验连接进程身份。要使用这两条通道需再逆向服务端二进制(`DriverStore\...\sysmonitorservice.inf_...\x64\AcerSystemCentralService.exe`、`acerdeviceenablingservicecomponent.inf_...\ADESv2Svc.exe`)。
- 对本机影响有限:Swift 3 无独显/多风扇,主用 QA WebSocket 通道。

---

## 6. 最底层:WMI / ACPI(root\WMI)

### 6.1 宏碁专有类(随驱动包 MOF 注册;类可见,实例枚举需管理员进一步验证)

**AcerGenericMethod —— 万能直通**
```
SetFunction(uiInput:UInt64) → uiOutput:UInt32      # 功能号+参数打包进 UInt64
GetFunction(uiInput:UInt32) → uiOutput:UInt64      # 充电上限/USB充电/风扇模式等开关型功能走此通道
GetStorageInformation(...)                          # BIOS 级硬盘信息
```

**AcerGamingFunction —— 高级硬件控制(游戏本;本机有类无实例,未激活)**
```
Set/GetGamingProfile            Set/GetGamingProfileSetting     GetGamingSysInfo
Set/GetGamingFanBehavior        Set/GetGamingFanSpeed           Set/GetGamingFanTable   ★温度-转速曲线
Set/GetCPUOverclockingProfile   (OCProfile:UInt8 + 结构体)
Set/GetGamingLED  Set/GetGamingRgbKb  Set/GetGamingLEDBehavior  Set/GetGamingLEDColor  Set/GetGamingKBBacklight
Set/GetGamingMiscSetting
```

**AcerBiosConfigurationTool —— BIOS 设置直改**
```
Set/GetBiosOptions(Data:UInt8Array, Password:UInt8Array, PasswordLen)   ★高危,只读为宜
GetUSBDeviceFilterStatus → UDFStatus:UInt8                              # BIOS 级 USB 过滤
```

**AcerGenericEvent** —— BIOS→OS 事件源(EventDetail:UInt8Array),WMI 事件订阅即收。

### 6.2 OS 标准类(与宏碁无关,直接可用)✅部分实测

| 类 | 用途 | 本机状态 |
|---|---|---|
| `BatteryStatus` | 充放电状态/电压/速率 | ✅ 非管理员可读(Charging/Discharging/Voltage/DischargeRate) |
| `BatteryStaticData` / `BatteryFullChargedCapacity` / `BatteryCycleCount` / `BatteryTemperature` / `BatteryRuntime` / `BatteryControl` | 电池静态/健康/控制 | 类已注册 |
| `MSAcpi_ThermalZoneTemperature` | ACPI 热区温度 | 本机无实例(轻薄本常见) |
| `WmiMonitorBrightness` / `Methods` / `Event` | 内建屏亮度 | xsense_native 调用的即此层(日志串 "Physical Monitors/LCD Screen") |

### 6.3 链路与平台对应

```
应用 → WMI(root\WMI) → acpi.sys 解释 BIOS 里的 AML → 写 EC 寄存器 → 硬件(EC 常驻,关机也工作)
```
- ACPI 设备:`ACPI\VEN_1025&DEV_165F`(1025=ACER Vendor ID);`assets/SupportedModel.txt` 按机型 `SUBSYS_XXXX1025` 白名单控制功能启用
- Linux:`acer_wmi` 内核驱动对接同一套 AML 方法(charge_behaviour 等),风扇走 nbfc-linux(EC);跨平台后端=同一 BIOS API 的两条 OS 绑定

---

## 7. 云端依赖(替代应用可全部丢弃)📄

- 账号:`acer-id.auth.eu-central-1.amazoncognito.com`(AWS Cognito)、`account.acer.com`、`uat-account.acer.com`
- 后端:`backend-prd-imub2p4wyq-uc.a.run.app`(GCP Cloud Run,UAT/dev 同型)
- 推送/配置:Firebase(FCM + Remote Config,`google-services` 配置内嵌)
- 其他:`planet9.gg`(游戏平台合作)、`www.acer.com`(帮助页)

---

## 8. 枚举总表(渲染层提取,📄)

```js
Ue   // QA/共享命令(27):FunctionQuery, USBChargeSwitch, USBCharge, CoolBoost, DustDefender,
     //   SIMControl, BluelightShield, SystemUsageControl, SystemUsageModes, AdaptiveBrightness,
     //   DarkMode, TransparentEffect, AutoHideTaskbar, ScreenSaver, ScreenSaverTimeout,
     //   HighlightFeature, Features, SystemUsageOSD, SenseAppStatus, VeroEcoPlusUSBCharge,
     //   VeroModeQuery, DisplayModeCapability, DisplayModeSwitch, ColorProfileSwitch,
     //   ColorProfileActive, HDR, Whisper, SystemUsageModeCapability
Ga   // DI 命令(8):SystemInfo, SMBIOS, DeviceManager, MotherBoard, OS, Storage,
     //   StorageVolumes, Network
Ui   // CC 命令(11):CheckBattery, CheckStorage, CheckMemory, QuickCleanup, BatteryStatus,
     //   BatteryHealthy, BatteryCalibration, CleanDisk, BatteryAdaptiveCharging, BatteryBoost,
     //   BatteryInformation
Qi   // 设置页分组:SystemUsageMode, "Battery_&_Charge", Screen, Keyboard, SystemBootEffect,
     //   CellularData, AcerDustDefender, GPUOperating, ColorProfile, TouchPad,
     //   TouchPad_AILighting, CopilotKeyboard
Fr   // 充电模式:0=OptimizedBatteryCharging, 1=FullBatteryCharging, 2=AISmartCharging
Ln   // AI 高亮特性:None, AIZoneIndex, LiveArt, AlterView, GIMP, PurifiedView, PurifiedVoice,
     //   OLED, AISmartCharging, MediaTouchPad, AIPC, NVIDIA
y1   // 存储介质类型:Unknown=0, HDD=3, SSD=4, SCM=5, NVME=6, EMMC=7, UFS=8, OPTANE=9, OPTANE_SSD=10
其他 // GPUOperatingModes: MUI_Discrete_GPU_Optimus / MUI_Discrete_GPU / MUI_Discrete_GPU_Automatic_Select
     // SIMControlArray: "NOT Support" / "eSIM mode" / "uSIM mode"
     // BluelightShieldArray: Customized / Low reduction / Medium reduction / High reduction / Long Hour Mode
     // USBCharge 阈值档:10% / 20% / 30%
```

---

## 9. 复刻最小客户端(Node 20+,已验证)✅

```js
const crypto = require('crypto');
const KEY_A = Buffer.from('A6052DC8A6E44210'), KEY_B = 'AB252AB73BED1CDB';
const enc = t => { const c = crypto.createCipheriv('aes-128-ecb', KEY_A, null);
  return Buffer.concat([c.update(t,'utf8'), c.final()]).toString('base64'); };
const QUESTIONS = { 5141:'AcerQuickAccessFunctionalityTests_HandshakingQuestion',
                    4343:'AcerCareCenterFunctionalityTests_HandshakingQuestion',
                    4449:'AcerDeviceInfoFunctionalityTests_HandshakingQuestion' };

function connect(port){ process.env.NODE_TLS_REJECT_UNAUTHORIZED='0';
  const ws = new WebSocket('wss://localhost:'+port);
  const req = (Command,Action,extra={}) =>
    ws.send(JSON.stringify({PacketType:2,Version:1,Session:crypto.randomUUID(),Command,Action,...extra}));
  ws.onopen = () => ws.send(JSON.stringify({PacketType:1,Session:crypto.randomUUID(),Version:1,
    Data:enc(JSON.stringify({Question:QUESTIONS[port],Key:KEY_B}))}));   // 客户端必须先发
  ws.onmessage = async ev => { const p = JSON.parse(await new Response(ev.data).text());
    if (p.PacketType===1) req('FunctionQuery','Get');      // 握手完成 → 查能力
    else console.log(p.Command, JSON.stringify(p.Result ?? p)); };
  return ws;
}
// connect(5141)  → 性能/充电/护眼
// connect(4343)  → 电池养护
// connect(4449)  → 设备信息
```

工程化建议:Tauri(Rust)壳 + 单条长连接每服务(注意 §2.2 的节流观察);`SystemUsageControl` Set 0/1/2 即性能切换;所有 Set 先 Get 确认取值域;错误包 PacketType=9 要按 ErrorCode 分流(1000→不支持,提示置灰)。

---

## 10. 未决问题与风险

| # | 事项 | 状态 |
|---|---|---|
| 1 | PA(46753/46760)/ ADES(51779)裸帧无响应 | ⚠️ 疑服务端校验客户端进程或需前置协商;需逆向服务端 exe |
| 2 | `AcerGenericMethod.SetFunction` 功能号表 | 📄 未逆;方法:对 QA Agent 的 Set 调用做 WMI 跟踪(WPP/ETW 或 ReactOS WMI 分析)对照提取 |
| 3 | QA Agent 快速反复连接后停止应答 | ⚠️ 观察到 ~15-20s 恢复;复刻客户端必须长连接复用 |
| 4 | Acer WMI 类实例枚举为空 | ⚠️ 未提权验证;需在管理员进程复查(或确认本机未实例化 Gaming 类) |
| 5 | `KeyEvent` 命令语义 | 📄 未逆 |
| 6 | 安全 | 明文 JSON + 硬编码密钥 + 证书校验关闭 + 无客户端鉴权;`admin.elevatedRun2` 即未授权本地提权面;`C:\Program Files\Acer*Agent\server_key.pem` 全局可读 |

---

## 附录:证据与工件

- asar 解包:`%TEMP%\asarsense\pkg\`(main.js=主进程,widgets/main=渲染层,assets/i18n=多语言,SupportedModel.txt=机型白名单)
- 探针:`%TEMP%\asarsense\probe.js`(握手+Get)、`probe3.js`、`pa.js`(二进制帧)
- 实测记录:`%TEMP%\asarsense\probe_out.txt`(230 行完整会话)
- TLS 证书:`C:\Program Files\Acer{QA,DI}Agent\server_crt.pem / server_key.pem`(服务端身份,自签)
- 代码常量出处:加密密钥与协议类 `dist/widgets/main/main.*.js`(`class Ok` 基类、`jt`=CC、`Pi`=DI、QA 匿名类);JWT 密钥在 SSDK 客户端 `getJWT()`;二进制帧在 `dist/main/main.js` 的 `PSSdkAdapter.sendRequest` 与 `class ades`
