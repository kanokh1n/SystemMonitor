import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { History, sparkline } from "./chart";
import { ago, bytes, duration, escape, minutes, percent, rate } from "./format";
import { initDisk } from "./disk";
import { describe } from "./known";
import type { Container, Device, DockerSnapshot, NetDetails, Process, Snapshot } from "./types";

const COLORS = {
  cpu: "#14b8a6",
  mem: "#8b5cf6",
  down: "#3b82f6",
  up: "#f59e0b",
  wired: "#f43f5e",
  compressed: "#f59e0b",
  cached: "#3b82f6",
};

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;

const history = {
  cpu: new History(),
  mem: new History(),
  rx: new History(),
  tx: new History(),
  diskRead: new History(),
  diskWrite: new History(),
};

let snapshot: Snapshot | null = null;
let docker: DockerSnapshot | null = null;
let net: NetDetails | null = null;
let devices: Device[] | null = null;
let publicIp: string | null = null;

// ---------- Боковое меню и прокрутка ----------

const SECTIONS: [id: string, title: string, icon: string][] = [
  ["overview", "Обзор", '<path d="M3 13h4l3-8 4 14 3-6h4"/>'],
  ["cpu", "Процессор", '<rect x="6" y="6" width="12" height="12" rx="2"/><path d="M9 2v4M15 2v4M9 18v4M15 18v4M2 9h4M2 15h4M18 9h4M18 15h4"/>'],
  ["memory", "Память", '<rect x="3" y="7" width="18" height="10" rx="1.5"/><path d="M7 7v10M11 7v10M15 7v10M6 20v-3M18 20v-3"/>'],
  ["network", "Сеть", '<path d="M2 9a15 15 0 0 1 20 0M5 12.5a10 10 0 0 1 14 0M8.5 16a5 5 0 0 1 7 0"/><circle cx="12" cy="19" r="1"/>'],
  ["disks", "Диски", '<ellipse cx="12" cy="6" rx="8" ry="3"/><path d="M4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3"/>'],
  ["devices", "Устройства", '<path d="M4 18V7a2 2 0 0 1 2-2h11M2 18h12"/><rect x="16" y="9" width="6" height="11" rx="1.5"/>'],
  ["processes", "Процессы", '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18M8 14h8"/>'],
  ["docker", "Docker", '<path d="M3 12h16a4 4 0 0 1-4 5H8a5 5 0 0 1-5-5zM5 9h3v3H5zM8 9h3v3H8zM11 9h3v3h-3zM8 6h3v3H8z"/>'],
];

$("nav").innerHTML = SECTIONS.map(
  ([id, title, icon]) => `<button class="tab" data-goto="${id}">
    <svg viewBox="0 0 24 24" aria-hidden="true">${icon}</svg><span>${title}</span><small id="badge-${id}"></small>
  </button>`,
).join("");

const content = $("content");

function scrollToSection(id: string) {
  const target = $(id);
  content.scrollTo({ top: id === "overview" ? 0 : target.offsetTop - 64, behavior: "smooth" });
}

// Подсвечиваем в меню раздел, который сейчас наверху экрана.
function updateActiveSection() {
  const atBottom = content.scrollTop + content.clientHeight >= content.scrollHeight - 4;
  let current = SECTIONS[0][0];
  for (const [id] of SECTIONS) if ($(id).offsetTop - 120 <= content.scrollTop) current = id;
  if (atBottom) current = SECTIONS[SECTIONS.length - 1][0];
  document.querySelectorAll<HTMLElement>(".tab").forEach((t) => t.classList.toggle("active", t.dataset.goto === current));
  $("section-title").textContent = SECTIONS.find(([id]) => id === current)![1];
}
content.addEventListener("scroll", updateActiveSection, { passive: true });

document.addEventListener("click", (e) => {
  const goto = (e.target as HTMLElement).closest<HTMLElement>("[data-goto]");
  if (goto) scrollToSection(goto.dataset.goto!);
});

// ---------- Подписка на данные из Rust ----------

listen<Snapshot>("metrics", ({ payload }) => {
  snapshot = payload;
  const mem = payload.memory;
  history.cpu.push(payload.cpu.usage);
  history.mem.push(mem.total ? (mem.used / mem.total) * 100 : 0);
  history.rx.push(payload.network.rxPerSec);
  history.tx.push(payload.network.txPerSec);
  history.diskRead.push(payload.diskIo.readPerSec);
  history.diskWrite.push(payload.diskIo.writePerSec);
  render();
});

listen<DockerSnapshot>("docker", ({ payload }) => {
  docker = payload;
  renderDocker();
});

listen<NetDetails>("netinfo", ({ payload }) => {
  net = payload;
});

listen<Device[]>("devices", ({ payload }) => {
  devices = payload;
  renderDevices();
});

function render() {
  const s = snapshot;
  if (!s) return;
  $("host").textContent = `${s.system.hostName} · ${s.cpu.brand}`;
  renderOverview(s);
  renderCpu(s);
  renderMemory(s);
  renderNetwork(s);
  renderDisks(s);
  renderBattery(s);
  renderProcesses(s.processes);
  updateBadges(s);
  updateActiveSection();
}

// ---------- Общие кусочки разметки ----------

function bar(fraction: number, color?: string) {
  const pct = Math.max(0, Math.min(1, fraction)) * 100;
  // Без явного цвета полоса желтеет после 75% и краснеет после 90%.
  const level = color ? "" : pct > 90 ? "crit" : pct > 75 ? "warn" : "";
  const background = color ? `background:${color};` : "";
  return `<div class="bar"><div class="fill ${level}" style="${background}width:${pct}%"></div></div>`;
}

/** Строки «название — значение», как в Системных настройках. */
function rows(items: [string, string][]) {
  return `<dl class="rows">${items.map(([k, v]) => `<dt>${k}</dt><dd>${v}</dd>`).join("")}</dl>`;
}

function hero(value: string, label: string, color?: string) {
  return `<div class="hero"><div class="big" ${color ? `style="color:${color}"` : ""}>${value}</div>
    <div class="muted">${label}</div></div>`;
}

const legend = (color: string, label: string, value: string) =>
  `<span class="legend"><span class="dot" style="background:${color}"></span>${label} <b>${value}</b></span>`;

function killButton(pid: number) {
  const armed = armedPid === pid;
  return `<button class="btn small ${armed ? "danger" : ""}" data-kill="${pid}">${armed ? "Точно?" : "Завершить"}</button>`;
}

/** Имя процесса, PID и, если знаем, пояснение, что это такое. */
function procName(name: string, pid: number, app?: string | null) {
  const about = describe(name, app);
  return `<td class="name" title="${escape(name)}${about ? ` — ${escape(about)}` : ""}">${escape(name)}
    <span class="muted small">${pid}</span>${about ? `<div class="about">${escape(about)}</div>` : ""}</td>`;
}

const memPercent = (s: Snapshot) => (s.memory.total ? (s.memory.used / s.memory.total) * 100 : 0);

// ---------- Обзор ----------

function renderOverview(s: Snapshot) {
  const root = s.disks.find((d) => d.mountPoint === "/");
  const running = docker?.available ? docker.containers.filter((c) => c.state === "running").length : null;
  const tiles: [string, string, string, string?][] = [
    ["cpu", "Процессор", percent(s.cpu.usage), COLORS.cpu],
    ["memory", "Память", percent(memPercent(s)), COLORS.mem],
    ["network", "Сеть", `↓ ${rate(s.network.rxPerSec)}`, COLORS.down],
    ["disks", "Свободно", root ? bytes(root.available, 0) : "—"],
    ["devices", "Батарея", s.battery ? percent(s.battery.percent) : "—"],
    ["docker", "Контейнеры", running == null ? "—" : String(running)],
  ];
  $("overview-strip").innerHTML = tiles
    .map(
      ([id, label, value, color]) => `<button class="tile" data-goto="${id}">
        <span class="muted">${label}</span><b ${color ? `style="color:${color}"` : ""}>${value}</b></button>`,
    )
    .join("");
}

// ---------- Процессор ----------

function renderCpu(s: Snapshot) {
  const { cpu } = s;
  const temps = [...s.temperatures].sort((a, b) => b.celsius - a.celsius).slice(0, 4);
  $("cpu-body").innerHTML = `
    <div class="panel-head">
      ${hero(percent(cpu.usage), "общая загрузка", COLORS.cpu)}
      <canvas class="spark" id="spark-cpu"></canvas>
    </div>
    <div class="cores">${cpu.perCore
      .map((c, i) => `<span title="Ядро ${i + 1}: ${percent(c)}"><i style="height:${Math.min(100, c)}%"></i></span>`)
      .join("")}</div>
    ${rows([
      ["Модель", escape(cpu.brand)],
      ["Ядра", `${cpu.physicalCores ?? "?"} физических · ${cpu.cores} потоков`],
      ["Средняя нагрузка за 1 / 5 / 15 мин", cpu.loadAvg.map((l) => l.toFixed(2)).join(" · ")],
      ...temps.map((t): [string, string] => [`Датчик ${escape(t.label)}`, `${t.celsius.toFixed(0)} °C`]),
      ["Без перезагрузки", duration(s.system.uptimeSecs)],
      ["Система", `${escape(s.system.os)} · ${escape(s.system.arch)}`],
    ])}`;
  sparkline($("spark-cpu"), [{ data: history.cpu.values, color: COLORS.cpu }], 100);
}

// ---------- Память ----------

const PRESSURE: Record<string, [string, string]> = {
  normal: ["в норме", "var(--ok)"],
  warning: ["повышенное", "var(--warn)"],
  critical: ["критическое", "var(--crit)"],
};

function renderMemory(s: Snapshot) {
  const mem = s.memory;
  const b = mem.breakdown;
  const pressure = b?.pressureLevel ? PRESSURE[b.pressureLevel] : null;

  // Разбивка на полосе: программы, wired, сжатая, кэш, остальное свободно.
  const parts: [string, string, number][] = b
    ? [
        ["Программы", COLORS.mem, b.app],
        ["Wired (ядро)", COLORS.wired, b.wired],
        ["Сжатая", COLORS.compressed, b.compressed],
        ["Кэш файлов", COLORS.cached, b.cached],
      ]
    : [["Занято", COLORS.mem, mem.used]];
  const stack = parts
    .map(([label, color, v]) => `<i title="${label}: ${bytes(v)}" style="background:${color};width:${(v / mem.total) * 100}%"></i>`)
    .join("");

  $("memory-body").innerHTML = `
    <div class="panel-head">
      ${hero(percent(memPercent(s)), `занято ${bytes(mem.used)} из ${bytes(mem.total)}`, COLORS.mem)}
      <canvas class="spark" id="spark-mem"></canvas>
    </div>
    <div class="stack">${stack}</div>
    <div class="legends">${parts.map(([l, c, v]) => legend(c, l, bytes(v))).join("")}</div>
    ${rows([
      ...(pressure
        ? [["Давление памяти", `<span style="color:${pressure[1]}">${pressure[0]}</span>${b?.pressure != null ? ` · ${b.pressure}%` : ""}`] as [string, string]]
        : []),
      ["Доступно программам", bytes(mem.available)],
      ["Swap (файл подкачки)", `${bytes(mem.swapUsed)} из ${bytes(mem.swapTotal)}`],
    ])}`;
  sparkline($("spark-mem"), [{ data: history.mem.values, color: COLORS.mem }], 100);

  const top = [...s.processes].sort((a, b) => b.memory - a.memory).slice(0, 25);
  const max = top[0]?.memory || 1;
  $("memory-procs").innerHTML = `
    <thead><tr><th>Процесс</th><th class="wide-col"></th><th class="num">Память</th><th class="num">% RAM</th><th class="num">CPU</th><th></th></tr></thead>
    <tbody>${top
      .map(
        (p) => `<tr>
        ${procName(p.name, p.pid, p.app)}
        <td class="wide-col">${bar(p.memory / max, COLORS.mem)}</td>
        <td class="num"><b>${bytes(p.memory)}</b></td>
        <td class="num">${percent((p.memory / mem.total) * 100, 1)}</td>
        <td class="num">${percent(p.cpu, 1)}</td>
        <td class="actions">${killButton(p.pid)}</td>
      </tr>`,
      )
      .join("")}</tbody>`;
}

// ---------- Сеть ----------

// Приложение, которому принадлежит процесс, знает только список процессов.
const appByPid = (pid: number) => snapshot?.processes.find((p) => p.pid === pid)?.app ?? null;

function renderNetwork(s: Snapshot) {
  const n = s.network;
  $("network-body").innerHTML = `
    <div class="panel-head">
      <div class="hero">
        <div class="big" style="color:${COLORS.down}">↓ ${rate(n.rxPerSec)}</div>
        <div class="big small-big" style="color:${COLORS.up}">↑ ${rate(n.txPerSec)}</div>
      </div>
      <canvas class="spark" id="spark-net"></canvas>
    </div>
    ${rows([
      ["Основной интерфейс", escape(net?.defaultInterface ?? "—")],
      ["Шлюз (роутер)", escape(net?.gateway ?? "—")],
      ["DNS‑серверы", net?.dns.length ? net.dns.map(escape).join(", ") : "—"],
      ["Внешний IP", publicIp ? escape(publicIp) : `<button class="link" id="public-ip">узнать</button>`],
      ["Получено с загрузки", bytes(n.rxTotal)],
      ["Отправлено с загрузки", bytes(n.txTotal)],
    ])}`;
  sparkline($("spark-net"), [
    { data: history.rx.values, color: COLORS.down },
    { data: history.tx.values, color: COLORS.up },
  ]);

  $("network-ifaces").innerHTML = `
    <thead><tr><th>Интерфейс</th><th>IPv4</th><th>MAC</th><th class="num">↓</th><th class="num">↑</th><th class="num">Всего ↓ / ↑</th></tr></thead>
    <tbody>${n.interfaces
      .map(
        (i) => `<tr>
        <td class="name"><b>${escape(i.name)}</b>${i.name === net?.defaultInterface ? ' <span class="pill">основной</span>' : ""}
          ${i.ipv6.length ? `<div class="muted small" title="${i.ipv6.map(escape).join("\n")}">IPv6: ${i.ipv6.length}</div>` : ""}</td>
        <td>${i.ipv4.map(escape).join("<br>") || '<span class="muted">—</span>'}</td>
        <td class="mono small">${escape(i.mac)}</td>
        <td class="num">${rate(i.rxPerSec)}</td>
        <td class="num">${rate(i.txPerSec)}</td>
        <td class="num muted">${bytes(i.rxTotal)} / ${bytes(i.txTotal)}</td>
      </tr>`,
      )
      .join("")}</tbody>`;

  const apps = net?.connections ?? [];
  $("network-apps").innerHTML = apps.length
    ? `<thead><tr><th>Программа</th><th class="num">Соединений</th><th class="num">Разных серверов</th></tr></thead>
       <tbody>${apps
         .map(
           (a) => `<tr>${procName(a.process, a.pid, appByPid(a.pid))}
           <td class="num">${a.connections}</td><td class="num">${a.remoteHosts}</td></tr>`,
         )
         .join("")}</tbody>`
    : `<tbody><tr><td class="muted">${net ? "Нет активных соединений" : "Собираю данные…"}</td></tr></tbody>`;

  const ports = net?.listening ?? [];
  $("network-ports").innerHTML = ports.length
    ? `<thead><tr><th class="num">Порт</th><th>Адрес</th><th>Программа</th><th></th></tr></thead>
       <tbody>${ports
         .map(
           (p) => `<tr><td class="num"><b>${p.port}</b></td>
           <td class="mono small">${escape(p.address)}${p.address === "*" ? ' <span class="pill warn">доступен из сети</span>' : ""}</td>
           ${procName(p.process, p.pid, appByPid(p.pid))}
           <td class="actions">${killButton(p.pid)}</td></tr>`,
         )
         .join("")}</tbody>`
    : `<tbody><tr><td class="muted">${net ? "Ни одна программа не слушает порты" : "Собираю данные…"}</td></tr></tbody>`;
}

content.addEventListener("click", async (e) => {
  if ((e.target as HTMLElement).id !== "public-ip") return;
  try {
    publicIp = await invoke<string>("public_ip");
  } catch (err) {
    toast(String(err), true);
  }
  render();
});

// ---------- Диски ----------

function renderDisks(s: Snapshot) {
  const volumes = s.disks
    .map((d) => {
      const used = d.total - d.available;
      return `<div class="volume">
        <div class="row"><b>${escape(d.mountPoint === "/" ? "Macintosh HD" : d.name)}</b>
          <span class="muted small">${d.removable ? "внешний" : "встроенный"} · ${escape(d.fileSystem)} · ${escape(d.mountPoint)}</span>
          <span class="spacer"></span><span><b>${bytes(d.available)}</b> <span class="muted">свободно из ${bytes(d.total)}</span></span></div>
        ${bar(d.total ? used / d.total : 0)}
      </div>`;
    })
    .join("");
  $("disks-body").innerHTML = `
    ${volumes}
    <div class="panel-head">
      <div class="hero">
        ${legend(COLORS.down, "Чтение", rate(s.diskIo.readPerSec))}
        ${legend(COLORS.up, "Запись", rate(s.diskIo.writePerSec))}
      </div>
      <canvas class="spark" id="spark-disk"></canvas>
    </div>`;
  sparkline($("spark-disk"), [
    { data: history.diskRead.values, color: COLORS.down },
    { data: history.diskWrite.values, color: COLORS.up },
  ]);
}

// ---------- Устройства: батарея Mac и Bluetooth ----------

const BATTERY_STATE: Record<string, string> = {
  charging: "Заряжается",
  discharging: "Работает от батареи",
  full: "Заряжена",
  empty: "Разряжена",
  paused: "Зарядка на паузе",
  unknown: "Подключено к сети",
};

function renderBattery(s: Snapshot) {
  const b = s.battery;
  if (!b) {
    $("battery-body").innerHTML = `<p class="muted">Батарея не найдена</p>`;
    return;
  }
  const color = chargeColor(b.percent);
  const time: [string, string][] =
    b.state === "discharging" && b.minutesToEmpty
      ? [["Осталось", minutes(b.minutesToEmpty)]]
      : b.state === "charging" && b.minutesToFull
        ? [["До полной зарядки", minutes(b.minutesToFull)]]
        : [];
  $("battery-body").innerHTML = `
    ${hero(percent(b.percent), BATTERY_STATE[b.state], color)}
    ${bar(b.percent / 100, color)}
    ${rows([
      ...time,
      ["Потребление", b.powerWatts > 0.1 ? `${b.powerWatts.toFixed(1)} Вт` : "—"],
      ["Здоровье батареи", percent(b.healthPercent)],
      ["Циклов зарядки", String(b.cycleCount ?? "?")],
    ])}`;
}

const chargeColor = (pct: number) => (pct < 15 ? "var(--crit)" : pct < 30 ? "var(--warn)" : "var(--ok)");

const DEVICE_ICONS: Record<Device["kind"], string> = {
  headphones: '<path d="M4 15v-3a8 8 0 0 1 16 0v3"/><rect x="3" y="14" width="4" height="6" rx="1.5"/><rect x="17" y="14" width="4" height="6" rx="1.5"/>',
  mouse: '<rect x="6" y="3" width="12" height="18" rx="6"/><path d="M12 7v4"/>',
  keyboard: '<rect x="2" y="6" width="20" height="12" rx="2"/><path d="M6 10h.01M10 10h.01M14 10h.01M18 10h.01M7 14h10"/>',
  trackpad: '<rect x="3" y="5" width="18" height="14" rx="2"/>',
  gamepad: '<path d="M6 9h12a4 4 0 0 1 0 8c-2 0-3-2-6-2s-4 2-6 2a4 4 0 0 1 0-8z"/><path d="M8 11v3M6.5 12.5h3M16 12h.01M18 13h.01"/>',
  speaker: '<rect x="6" y="2" width="12" height="20" rx="2"/><circle cx="12" cy="14" r="3"/><path d="M12 6h.01"/>',
  phone: '<rect x="7" y="2" width="10" height="20" rx="2"/><path d="M11 18h2"/>',
  watch: '<rect x="7" y="6" width="10" height="12" rx="3"/><path d="M9 6l1-4h4l1 4M9 18l1 4h4l1-4"/>',
  other: '<path d="M7 7l10 10-5 5V2l5 5L7 17"/>',
};

const PART_LABEL: Record<Device["levels"][number]["part"], string> = {
  main: "",
  left: "Левый",
  right: "Правый",
  case: "Кейс",
};

function deviceStatus(d: Device) {
  if (d.connected) {
    if (d.live) return "подключено";
    return d.levels.length && d.levelsAt ? `подключено · заряд ${ago(d.levelsAt)}` : "подключено · заряд не сообщает";
  }
  const when = d.levelsAt ?? d.lastSeen;
  return when ? `не подключено · заряд ${ago(when)}` : "не подключено";
}

function deviceRow(d: Device) {
  const levels = d.levels
    .map((l) => {
      const label = PART_LABEL[l.part];
      return `<div class="charge ${d.live ? "" : "stale"}">
        <span class="muted small">${label}</span>
        ${bar(l.percent / 100, chargeColor(l.percent))}
        <b>${percent(l.percent)}</b>
      </div>`;
    })
    .join("");
  return `<div class="device ${d.connected ? "" : "off"}">
    <svg viewBox="0 0 24 24" aria-hidden="true">${DEVICE_ICONS[d.kind] ?? DEVICE_ICONS.other}</svg>
    <div class="device-name"><b>${escape(d.name)}</b><div class="muted small">${deviceStatus(d)}</div></div>
    <div class="charges">${levels || '<span class="muted small">—</span>'}</div>
  </div>`;
}

function renderDevices() {
  const body = $("devices-body");
  if (!devices) return (body.innerHTML = `<div class="device muted">Ищу устройства…</div>`);
  if (devices.length === 0)
    return (body.innerHTML = `<div class="device muted">Нет сопряжённых Bluetooth‑устройств</div>`);
  body.innerHTML = devices.map(deviceRow).join("");
}

$("devices-refresh").addEventListener("click", async (e) => {
  const btn = e.currentTarget as HTMLButtonElement;
  btn.disabled = true;
  try {
    devices = await invoke<Device[]>("refresh_devices");
    renderDevices();
  } catch (err) {
    toast(String(err), true);
  } finally {
    btn.disabled = false;
  }
});

// ---------- Процессы ----------

type SortKey = "name" | "cpu" | "memory" | "readPerSec" | "writePerSec";
let sortKey: SortKey = "cpu";
let sortDesc = true;
let armedPid: number | null = null; // PID, для которого нажали «Завершить» один раз
const search = $<HTMLInputElement>("proc-search");
search.addEventListener("input", () => snapshot && renderProcesses(snapshot.processes));

const COLUMNS: [SortKey, string][] = [
  ["name", "Процесс"],
  ["cpu", "CPU"],
  ["memory", "Память"],
  ["readPerSec", "Чтение"],
  ["writePerSec", "Запись"],
];

function renderProcesses(all: Process[]) {
  const q = search.value.trim().toLowerCase();
  const list = all
    .filter((p) => !q || p.name.toLowerCase().includes(q) || String(p.pid).includes(q))
    .sort((a, b) => {
      const av = a[sortKey];
      const bv = b[sortKey];
      const cmp = typeof av === "string" ? av.localeCompare(bv as string) : (av as number) - (bv as number);
      return sortDesc ? -cmp : cmp;
    });
  $("proc-count").textContent = `${list.length} из ${all.length}`;

  const head = COLUMNS.map(
    ([key, title]) =>
      `<th data-sort="${key}" class="${key === "name" ? "" : "num"} ${key === sortKey ? "sorted" : ""}">${title}${
        key === sortKey ? (sortDesc ? " ↓" : " ↑") : ""
      }</th>`,
  ).join("");

  const body = list
    .slice(0, 200)
    .map(
      (p) => `<tr>
        ${procName(p.name, p.pid, p.app)}
        <td class="num">${percent(p.cpu, 1)}</td>
        <td class="num">${bytes(p.memory)}</td>
        <td class="num">${p.readPerSec ? rate(p.readPerSec) : "—"}</td>
        <td class="num">${p.writePerSec ? rate(p.writePerSec) : "—"}</td>
        <td class="actions">${killButton(p.pid)}</td>
      </tr>`,
    )
    .join("");

  $("proc-table").innerHTML = `<thead><tr>${head}<th></th></tr></thead><tbody>${body}</tbody>`;
}

$("proc-table").addEventListener("click", (e) => {
  const th = (e.target as HTMLElement).closest<HTMLElement>("[data-sort]");
  if (!th) return;
  const key = th.dataset.sort as SortKey;
  sortDesc = key === sortKey ? !sortDesc : key !== "name";
  sortKey = key;
  if (snapshot) renderProcesses(snapshot.processes);
});

// Кнопка «Завершить» есть в нескольких таблицах, поэтому слушаем клики на всей странице.
document.addEventListener("click", async (e) => {
  const kill = (e.target as HTMLElement).closest<HTMLElement>("[data-kill]");
  if (!kill) return;
  const pid = Number(kill.dataset.kill);
  if (armedPid !== pid) {
    // Первое нажатие только «взводит» кнопку, второе завершает процесс.
    armedPid = pid;
    setTimeout(() => armedPid === pid && (armedPid = null), 4000);
  } else {
    armedPid = null;
    try {
      await invoke("kill_process", { pid });
      toast("Процесс завершён");
    } catch (err) {
      toast(String(err), true);
    }
  }
  render();
});

// ---------- Docker ----------

const busy = new Set<string>(); // контейнеры, над которыми сейчас идёт действие

function containerRow(c: Container) {
  const running = c.state === "running";
  const disabled = busy.has(c.id) ? "disabled" : "";
  const mem =
    c.memoryUsed != null
      ? `${bytes(c.memoryUsed)}${c.memoryLimit ? ` <span class="muted small">/ ${bytes(c.memoryLimit, 0)}</span>` : ""}`
      : "—";
  return `<tr>
    <td class="name"><span class="state ${escape(c.state)}"></span>${escape(c.name)}
      <div class="muted small">${escape(c.image)}</div></td>
    <td><span class="muted small">${escape(c.status)}</span></td>
    <td class="small">${c.ports.map(escape).join(", ") || "—"}</td>
    <td class="num">${c.cpuPercent != null ? percent(c.cpuPercent, 1) : "—"}</td>
    <td class="num">${mem}</td>
    <td class="actions">
      ${
        running
          ? `<button class="btn small" data-action="restart" data-id="${c.id}" ${disabled}>Перезапуск</button>
             <button class="btn small" data-action="stop" data-id="${c.id}" ${disabled}>Стоп</button>`
          : `<button class="btn small primary" data-action="start" data-id="${c.id}" ${disabled}>Старт</button>`
      }
      <button class="btn small" data-logs="${c.id}" data-name="${escape(c.name)}">Логи</button>
    </td>
  </tr>`;
}

function renderDocker() {
  const body = $("docker-body");
  if (!docker) return (body.innerHTML = `<div class="panel muted">Подключаюсь к Docker…</div>`);
  if (!docker.available)
    return (body.innerHTML = `<div class="panel empty"><b>Docker недоступен</b>
      <p class="muted">${escape(docker.error ?? "")}</p></div>`);
  if (docker.containers.length === 0)
    return (body.innerHTML = `<div class="panel empty"><b>Контейнеров нет</b>
      <p class="muted">Запусти любой, например <code>docker run -d nginx</code>, и он появится здесь.</p></div>`);
  const running = docker.containers.filter((c) => c.state === "running").length;
  body.innerHTML = `<p class="muted">${running} из ${docker.containers.length} контейнеров запущено</p>
  <div class="panel flush"><table class="table">
    <thead><tr><th>Контейнер</th><th>Статус</th><th>Порты</th><th class="num">CPU</th><th class="num">Память</th><th></th></tr></thead>
    <tbody>${docker.containers.map(containerRow).join("")}</tbody>
  </table></div>`;
}

$("docker-body").addEventListener("click", async (e) => {
  const el = e.target as HTMLElement;
  const action = el.closest<HTMLElement>("[data-action]");
  if (action) {
    const id = action.dataset.id!;
    busy.add(id);
    renderDocker();
    try {
      await invoke("container_action", { id, action: action.dataset.action });
    } catch (err) {
      toast(String(err), true);
    } finally {
      busy.delete(id);
      renderDocker();
    }
  }
  const logs = el.closest<HTMLElement>("[data-logs]");
  if (logs) openLogs(logs.dataset.logs!, logs.dataset.name!);
});

async function openLogs(id: string, name: string) {
  const dialog = $<HTMLDialogElement>("logs");
  $("logs-title").textContent = `Логи · ${name}`;
  $("logs-body").textContent = "Загружаю…";
  dialog.showModal();
  try {
    const text = await invoke<string>("container_logs", { id });
    $("logs-body").textContent = text || "Логи пустые";
    $("logs-body").scrollTop = $("logs-body").scrollHeight;
  } catch (err) {
    $("logs-body").textContent = String(err);
  }
}
$("logs-close").addEventListener("click", () => $<HTMLDialogElement>("logs").close());

// ---------- Подписи в боковом меню ----------

function updateBadges(s: Snapshot) {
  const root = s.disks.find((d) => d.mountPoint === "/");
  const badges: Record<string, string> = {
    cpu: percent(s.cpu.usage),
    memory: percent(memPercent(s)),
    network: `↓ ${bytes(s.network.rxPerSec, 0)}`,
    disks: root ? bytes(root.available, 0) : "",
    devices: s.battery ? percent(s.battery.percent) : "",
    processes: String(s.processes.length),
    docker: docker?.available ? String(docker.containers.filter((c) => c.state === "running").length) : "",
  };
  for (const [id, text] of Object.entries(badges)) $(`badge-${id}`).textContent = text;
}

// ---------- Уведомления внизу экрана ----------

let toastTimer = 0;
function toast(text: string, error = false) {
  const el = $("toast");
  el.textContent = text;
  el.classList.toggle("error", error);
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => (el.hidden = true), 3500);
}

window.addEventListener("resize", render);

initDisk(toast);
