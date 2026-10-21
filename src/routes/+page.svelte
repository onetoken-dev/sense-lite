<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onMount } from "svelte";

  const QA = 5141, CC = 4343, DI = 4449;

  interface Status { port: number; service: string; connected: boolean; capabilities: string[] | null }
  interface Mode { Value: number; Text: string }
  interface Battery { ACMode: boolean; BatteryPercent: number; BatterySaverMode: boolean }

  let statuses = $state<Status[]>([]);
  let modes = $state<Mode[]>([]);
  let currentMode = $state<number | null>(null);
  let usbCharge = $state<number | null>(null);
  let usbSwitch = $state<boolean | null>(null);
  let bluelight = $state<number | null>(null);
  let hdr = $state<boolean | null>(null);
  let hdrUnsupported = $state(false);
  let battery = $state<Battery | null>(null);
  let chargeMode = $state<number | null>(null);
  let ccCaps = $state<string[]>([]);
  let sysinfo = $state<Record<string, unknown> | null>(null);
  let smbios = $state<Record<string, unknown> | null>(null);
  let busy = $state(false);
  let refreshing = $state(false);
  let error = $state("");

  const BLUELIGHT_LEVELS: [number, string][] = [
    [0, "关闭"], [1, "低"], [2, "中"], [3, "高"], [4, "长时间"], [5, "自定义"],
  ];

  async function get(port: number, command: string): Promise<any> {
    try {
      return await invoke("get_data", { port, command });
    } catch (e) {
      console.warn(`${command}:`, e);
      return null;
    }
  }

  async function apply(port: number, command: string, param: unknown, label: string) {
    busy = true; error = "";
    try {
      await invoke("set_data", { port, command, param });
    } catch (e) {
      error = `${label} 失败: ${e}`;
    } finally {
      busy = false;
    }
  }

  async function refreshAll() {
    if (refreshing) return;
    refreshing = true;
    try {
      statuses = await invoke("agent_status");
      const [modesR, modeR, usbR, usbSwR, blueR, hdrR, batR, batH, info, board] = await Promise.all([
        get(QA, "SystemUsageModes"), get(QA, "SystemUsageControl"),
        get(QA, "USBCharge"), get(QA, "USBChargeSwitch"),
        get(QA, "BluelightShield"), get(QA, "HDR"),
        get(CC, "BatteryStatus"), get(CC, "BatteryHealthy"),
        get(DI, "SystemInfo"), get(DI, "SMBIOS"),
      ]);
      modes = modesR ?? [];
      currentMode = num(modeR);
      usbCharge = num(usbR);
      usbSwitch = bool(usbSwR);
      bluelight = num(blueR);
      hdr = bool(hdrR);
      battery = batR;
      chargeMode = num(batH);
      ccCaps = statuses.find(s => s.port === CC)?.capabilities ?? [];
      sysinfo = info; smbios = board;
    } finally {
      refreshing = false;
    }
  }

  function num(v: any): number | null {
    if (v == null) return null;
    if (typeof v === "number") return v;
    return typeof v?.Value === "number" ? v.Value : null;
  }
  function bool(v: any): boolean | null {
    if (v == null) return null;
    if (typeof v === "boolean") return v;
    return typeof v?.Value === "boolean" ? v.Value : null;
  }

  async function setMode(m: number) {
    await apply(QA, "SystemUsageControl", m, "切换性能模式");
    currentMode = num(await get(QA, "SystemUsageControl"));
  }
  async function setUsbCharge(v: number) {
    await apply(QA, "USBCharge", v, "设置 USB 充电阈值");
    usbCharge = num(await get(QA, "USBCharge"));
  }
  async function setUsbSwitch(v: boolean) {
    await apply(QA, "USBChargeSwitch", v, "切换关机充电");
    usbSwitch = bool(await get(QA, "USBChargeSwitch"));
  }
  async function setBluelight(v: number) {
    await apply(QA, "BluelightShield", v, "设置护眼");
    bluelight = num(await get(QA, "BluelightShield"));
  }
  async function setHdr(v: boolean) {
    busy = true; error = "";
    try {
      await invoke("set_data", { port: QA, command: "HDR", param: v });
      hdr = bool(await get(QA, "HDR"));
    } catch (e) {
      // 本机面板不支持时,Agent 对 Set 静默丢弃(无响应→超时)。标记不支持并禁用开关。
      hdrUnsupported = true;
    } finally {
      busy = false;
    }
  }

  onMount(() => {
    refreshAll();
    const un1 = listen("agent-notify", (e: any) => {
      const { port, packet } = e.payload;
      if (port === CC && packet?.Command === "BatteryStatus") battery = packet.Result;
    });
    const un2 = listen("mode-changed", () => refreshAll());
    // 每 5s 刷新连接状态;若已连上但数据为空(启动时连接尚未就绪)则自动补拉
    const timer = setInterval(() => {
      invoke("agent_status").then(s => {
        statuses = s;
        const qa = s.find((x: Status) => x.port === QA);
        if (qa?.connected && modes.length === 0 && !busy && !refreshing) refreshAll();
      });
    }, 5000);
    return () => { un1.then(f => f()); un2.then(f => f()); clearInterval(timer); };
  });

  const CHARGE_MODES: { v: number; label: string }[] = [
    { v: 1, label: "充满 (100%)" },
    { v: 0, label: "优化充电 (养护限制)" },
    { v: 2, label: "AI 智能充电" },
  ];
  const showAICharge = $derived(ccCaps.includes("BatteryAdaptiveCharging"));
  const chargeModeOptions = $derived(CHARGE_MODES.filter(m => m.v !== 2 || showAICharge));

  async function setChargeMode(v: number) {
    await apply(CC, "BatteryHealthy", v, "切换充电模式");
    chargeMode = num(await get(CC, "BatteryHealthy"));
  }

  const has = (cap: string) => statuses.find(s => s.port === QA)?.capabilities?.includes(cap) ?? true;
  const pct = $derived(battery?.BatteryPercent ?? 0);
</script>

<main>
  <header>
    <h1>Sense Lite</h1>
    <div class="chips">
      {#each statuses as s (s.port)}
        <span class="chip" class:on={s.connected} title={s.capabilities?.join(', ') ?? ''}>
          {s.service} {s.connected ? "●" : "○"}
        </span>
      {/each}
      <button class="refresh" onclick={refreshAll} disabled={refreshing} title="重新加载数据">↻</button>
    </div>
  </header>

  {#if error}<div class="error">{error}</div>{/if}

  <div class="grid">
    {#if has("SystemUsageControl")}
      <section class="card">
        <h2>性能模式</h2>
        <div class="segmented">
          {#each modes as m (m.Value)}
            <button
              class:active={currentMode === m.Value}
              disabled={busy}
              onclick={() => setMode(m.Value)}
            >{m.Text.replace(" mode", "")}</button>
          {/each}
          {#if modes.length === 0}
            <span class="muted">等待连接…</span>
          {/if}
        </div>
      </section>
    {/if}

    {#if has("USBCharge")}
      <section class="card">
        <h2>USB 关机充电</h2>
        <label class="row">
          <span>关机供电开关</span>
          <input type="checkbox" checked={usbSwitch ?? false}
            onchange={(e) => setUsbSwitch(e.currentTarget.checked)} disabled={busy} />
        </label>
        <label class="row">
          <span>电量低于阈值时断电</span>
          <select value={usbCharge ?? 30} onchange={(e) => setUsbCharge(+e.currentTarget.value)} disabled={busy}>
            <option value={10}>10%</option>
            <option value={20}>20%</option>
            <option value={30}>30%</option>
          </select>
        </label>
      </section>
    {/if}

    {#if has("BluelightShield")}
      <section class="card">
        <h2>护眼 (蓝光过滤)</h2>
        <select value={bluelight ?? 0} onchange={(e) => setBluelight(+e.currentTarget.value)} disabled={busy}>
          {#each BLUELIGHT_LEVELS as [v, label] (v)}
            <option value={v}>{label}</option>
          {/each}
        </select>
      </section>
    {/if}

    {#if has("HDR")}
      <section class="card">
        <h2>HDR</h2>
        <label class="row">
          <span>启用 HDR</span>
          <input type="checkbox" checked={hdr ?? false} disabled={busy || hdrUnsupported}
            onchange={(e) => setHdr(e.currentTarget.checked)} />
        </label>
        {#if hdrUnsupported}
          <div class="muted" style="margin-top:6px">本机显示器不支持 HDR(指令被忽略)</div>
        {/if}
      </section>
    {/if}

    <section class="card">
      <h2>电池 {#if battery?.ACMode}<span class="badge">外接电源</span>{/if}</h2>
      <div class="batt">
        <div class="batt-bar"><div class="batt-fill" style={`width:${pct}%`}></div></div>
        <span class="batt-num">{battery ? `${battery.BatteryPercent}%` : "—"}</span>
      </div>
      <label class="row">
        <span>充电模式</span>
        <select value={chargeMode ?? 1} onchange={(e) => setChargeMode(+e.currentTarget.value)} disabled={busy}>
          {#each chargeModeOptions as m (m.v)}
            <option value={m.v}>{m.label}</option>
          {/each}
        </select>
      </label>
      <div class="kv">
        <span>省电模式</span>
        <span>{battery?.BatterySaverMode ? "开" : "关"}</span>
      </div>
    </section>

    <section class="card wide">
      <h2>设备信息</h2>
      <div class="kv">
        <span>型号</span><span>{smbios?.ProductName ?? sysinfo?.Model ?? "—"}</span>
        <span>CPU</span><span>{sysinfo?.CPUName ?? "—"}({sysinfo?.CPUCores ?? "?"} 核)</span>
        <span>内存</span><span>{sysinfo ? `${sysinfo.RAMType} ${(Number(sysinfo.RAMSizeMB) / 1024).toFixed(0)} GB` : "—"}</span>
        <span>BIOS</span><span>{smbios?.BaseboardVersion ?? "—"}</span>
        <span>序列号</span><span>{sysinfo?.SerialNumber ?? "—"}</span>
      </div>
    </section>
  </div>

  <footer>连接 Acer Sense 后台服务(5141/4343/4449)。关闭窗口 = 最小化到托盘。</footer>
</main>

<style>
  :global(html, body) { margin: 0; background: #f2f4f8; color: #1a1d23; }
  :global(*) { box-sizing: border-box; }
  main {
    font-family: "Segoe UI", system-ui, sans-serif;
    max-width: 860px; margin: 0 auto; padding: 20px 24px 40px;
  }
  header { display: flex; align-items: baseline; justify-content: space-between; margin-bottom: 16px; }
  h1 { font-size: 22px; margin: 0; letter-spacing: 0.5px; color: #111827; }
  h2 { font-size: 13px; margin: 0 0 12px; color: #6b7280; font-weight: 600; text-transform: uppercase; letter-spacing: 1px; }
  .chips { display: flex; gap: 8px; align-items: center; }
  .chip { font-size: 12px; color: #9aa1ad; border: 1px solid #dde1e8; border-radius: 999px; padding: 3px 10px; background: #fff; }
  .chip.on { color: #16a34a; border-color: #bbf7d0; background: #f0fdf4; }
  .refresh {
    background: #fff; color: #6b7280; border: 1px solid #dde1e8; border-radius: 999px;
    width: 28px; height: 24px; cursor: pointer; font-size: 14px; line-height: 1;
  }
  .refresh:hover { color: #1a1d23; border-color: #c3c9d4; }
  .error { background: #fef2f2; color: #b91c1c; border: 1px solid #fecaca; border-radius: 8px; padding: 8px 12px; margin-bottom: 12px; font-size: 13px; }
  .grid { display: grid; grid-template-columns: 1fr 1fr; gap: 14px; }
  .card { background: #ffffff; border: 1px solid #e5e8ee; border-radius: 14px; padding: 16px; box-shadow: 0 1px 2px rgba(16, 24, 40, 0.04); }
  .card.wide { grid-column: 1 / -1; }
  .segmented { display: flex; gap: 8px; }
  .segmented button {
    flex: 1; padding: 10px 0; border-radius: 9px; border: 1px solid #dde1e8;
    background: #f7f8fa; color: #4b5563; font-size: 13px; cursor: pointer; transition: all 0.15s;
  }
  .segmented button:hover { border-color: #c3c9d4; }
  .segmented button.active { background: #2563eb; border-color: #2563eb; color: #fff; font-weight: 600; }
  .segmented button:disabled { opacity: 0.5; cursor: wait; }
  .row { display: flex; align-items: center; justify-content: space-between; padding: 6px 0; font-size: 14px; color: #374151; }
  select {
    background: #f7f8fa; color: #1a1d23; border: 1px solid #dde1e8; border-radius: 8px;
    padding: 6px 10px; font-size: 13px;
  }
  input[type="checkbox"] { width: 38px; height: 20px; appearance: none; background: #d6dbe3; border-radius: 999px; position: relative; cursor: pointer; transition: background 0.15s; }
  input[type="checkbox"]:checked { background: #2563eb; }
  input[type="checkbox"]::after { content: ""; position: absolute; top: 2px; left: 2px; width: 16px; height: 16px; border-radius: 50%; background: #fff; box-shadow: 0 1px 2px rgba(0,0,0,0.2); transition: left 0.15s; }
  input[type="checkbox"]:checked::after { left: 20px; }
  .batt { display: flex; align-items: center; gap: 10px; margin-bottom: 10px; }
  .batt-bar { flex: 1; height: 10px; background: #e5e8ee; border-radius: 999px; overflow: hidden; }
  .batt-fill { height: 100%; background: linear-gradient(90deg, #22c55e, #4ade80); border-radius: 999px; transition: width 0.4s; }
  .batt-num { font-size: 18px; font-weight: 600; min-width: 48px; text-align: right; color: #111827; }
  .badge { font-size: 11px; background: #f0fdf4; color: #16a34a; border: 1px solid #bbf7d0; border-radius: 999px; padding: 2px 8px; vertical-align: middle; }
  .kv { display: grid; grid-template-columns: auto 1fr; gap: 4px 16px; font-size: 13px; }
  .kv span:nth-child(odd) { color: #6b7280; }
  .kv span:nth-child(even) { color: #1a1d23; }
  .muted { color: #9aa1ad; font-size: 13px; }
  footer { margin-top: 18px; color: #9aa1ad; font-size: 12px; text-align: center; }
</style>
