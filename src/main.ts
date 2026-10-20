import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { History, sparkline } from "./chart";
import { bytes, duration, escape, minutes, percent, rate } from "./format";
import type { Container, DockerSnapshot, Process, Snapshot } from "./types";

const COLORS = { cpu: "#14b8a6", mem: "#8b5cf6", down: "#3b82f6", up: "#f59e0b" };

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;

const history = {
  cpu: new History(),
  mem: new History(),
  rx: new History(),
  tx: new History(),
};

let snapshot: Snapshot | null = null;
let docker: DockerSnapshot | null = null;
let activeTab = "overview";

// ---------- Вкладки ----------

document.querySelectorAll<HTMLButtonElement>(".tab").forEach((tab) => {
  tab.addEventListener("click", () => {
    activeTab = tab.dataset.tab!;
    document.querySelectorAll(".tab").forEach((t) => t.classList.toggle("active", t === tab));
    document.querySelectorAll(".view").forEach((v) => v.classList.toggle("active", v.id === activeTab));
    render();
  });
});

// ---------- Подписка на данные из Rust ----------

listen<Snapshot>("metrics", ({ payload }) => {
  snapshot = payload;
  const mem = payload.memory;
  history.cpu.push(payload.cpu.usage);
  history.mem.push(mem.total ? (mem.used / mem.total) * 100 : 0);
  history.rx.push(payload.network.rxPerSec);
  history.tx.push(payload.network.txPerSec);
  render();
});

listen<DockerSnapshot>("docker", ({ payload }) => {
  docker = payload;
  if (activeTab !== "processes") render();
});

function render() {
  if (!snapshot) return;
  $("host").textContent = `${snapshot.system.hostName} · ${snapshot.cpu.brand}`;
  if (activeTab === "overview") renderOverview(snapshot);
  if (activeTab === "processes") renderProcesses(snapshot.processes);
  if (activeTab === "docker") renderDocker();
}

// ---------- Обзор ----------

function bar(fraction: number, color?: string) {
  const pct = Math.max(0, Math.min(1, fraction)) * 100;
  // Без явного цвета полоса желтеет после 75% и краснеет после 90%.
  const level = color ? "" : pct > 90 ? "crit" : pct > 75 ? "warn" : "";
  const background = color ? `background:${color};` : "";
  return `<div class="bar"><div class="fill ${level}" style="${background}width:${pct}%"></div></div>`;
}

function renderOverview(s: Snapshot) {
  const { cpu, memory: mem, network: net } = s;

  $("card-cpu").innerHTML = `
    <h2>Процессор</h2>
    <div class="big" style="color:${COLORS.cpu}">${percent(cpu.usage)}</div>
    <canvas class="spark" id="spark-cpu"></canvas>
    <div class="cores">${cpu.perCore
      .map((c) => `<span title="${percent(c)}"><i style="height:${Math.min(100, c)}%"></i></span>`)
      .join("")}</div>
    <dl>
      <dt>Ядра</dt><dd>${cpu.physicalCores ?? "?"} физ. · ${cpu.cores} лог.</dd>
      <dt>Нагрузка (1/5/15 мин)</dt><dd>${cpu.loadAvg.map((l) => l.toFixed(2)).join(" · ")}</dd>
      ${hottest(s)}
    </dl>`;
  sparkline($("spark-cpu"), [{ data: history.cpu.values, color: COLORS.cpu }], 100);

  const memFrac = mem.total ? mem.used / mem.total : 0;
  $("card-mem").innerHTML = `
    <h2>Память</h2>
    <div class="big" style="color:${COLORS.mem}">${percent(memFrac * 100)}</div>
    <canvas class="spark" id="spark-mem"></canvas>
    ${bar(memFrac, COLORS.mem)}
    <dl>
      <dt>Занято</dt><dd>${bytes(mem.used)} из ${bytes(mem.total)}</dd>
      <dt>Доступно</dt><dd>${bytes(mem.available)}</dd>
      <dt>Swap</dt><dd>${bytes(mem.swapUsed)} из ${bytes(mem.swapTotal)}</dd>
    </dl>`;
  sparkline($("spark-mem"), [{ data: history.mem.values, color: COLORS.mem }], 100);

  $("card-net").innerHTML = `
    <h2>Сеть</h2>
    <div class="pair">
      <div><span class="dot" style="background:${COLORS.down}"></span>↓ <b>${rate(net.rxPerSec)}</b></div>
      <div><span class="dot" style="background:${COLORS.up}"></span>↑ <b>${rate(net.txPerSec)}</b></div>
    </div>
    <canvas class="spark" id="spark-net"></canvas>
    <dl>
      ${net.interfaces
        .map((i) => `<dt>${escape(i.name)}</dt><dd>${i.ips.map(escape).join(", ")}</dd>`)
        .join("")}
      <dt>Всего с загрузки</dt><dd>↓ ${bytes(net.rxTotal)} · ↑ ${bytes(net.txTotal)}</dd>
    </dl>`;
  sparkline($("spark-net"), [
    { data: history.rx.values, color: COLORS.down },
    { data: history.tx.values, color: COLORS.up },
  ]);

  $("card-disk").innerHTML = `
    <h2>Диски</h2>
    ${s.disks
      .map((d) => {
        const used = d.total - d.available;
        return `<div class="disk">
          <div class="row"><b>${escape(d.mountPoint === "/" ? "Macintosh HD" : d.name)}</b>
          <span class="muted">${bytes(d.available)} свободно</span></div>
          ${bar(d.total ? used / d.total : 0)}
          <div class="muted small">${bytes(used)} из ${bytes(d.total)} · ${escape(d.fileSystem)}</div>
        </div>`;
      })
      .join("")}
    <dl>
      <dt>Чтение</dt><dd>${rate(s.diskIo.readPerSec)}</dd>
      <dt>Запись</dt><dd>${rate(s.diskIo.writePerSec)}</dd>
    </dl>`;

  $("card-battery").innerHTML = renderBattery(s);

  $("card-system").innerHTML = `
    <h2>Система</h2>
    <dl>
      <dt>Компьютер</dt><dd>${escape(s.system.hostName)}</dd>
      <dt>ОС</dt><dd>${escape(s.system.os)}</dd>
      <dt>Ядро</dt><dd>Darwin ${escape(s.system.kernel)}</dd>
      <dt>Архитектура</dt><dd>${escape(s.system.arch)}</dd>
      <dt>Без перезагрузки</dt><dd>${duration(s.system.uptimeSecs)}</dd>
      <dt>Процессов</dt><dd>${s.processes.length}</dd>
    </dl>`;

  const top = [...s.processes].sort((a, b) => b.cpu - a.cpu).slice(0, 6);
  $("card-top").innerHTML = `
    <h2>Больше всего грузят CPU <button class="link" data-goto="processes">все процессы →</button></h2>
    <table class="table compact">
      ${top
        .map(
          (p) => `<tr><td class="name">${escape(p.name)}</td>
          <td class="num">${percent(p.cpu, 1)}</td><td class="num">${bytes(p.memory)}</td></tr>`,
        )
        .join("")}
    </table>`;

  $("card-docker").innerHTML = renderDockerSummary();
}

function hottest(s: Snapshot) {
  if (s.temperatures.length === 0) return "";
  const max = s.temperatures.reduce((a, b) => (b.celsius > a.celsius ? b : a));
  return `<dt>Самый горячий датчик</dt><dd title="${escape(max.label)}">${max.celsius.toFixed(0)} °C</dd>`;
}

const BATTERY_STATE: Record<string, string> = {
  charging: "Заряжается",
  discharging: "От батареи",
  full: "Заряжена",
  empty: "Разряжена",
  paused: "Зарядка на паузе",
  unknown: "Подключено к сети",
};

function renderBattery(s: Snapshot) {
  const b = s.battery;
  if (!b) return `<h2>Батарея</h2><p class="muted">Батарея не найдена</p>`;
  const time =
    b.state === "discharging" && b.minutesToEmpty
      ? `<dt>Осталось</dt><dd>${minutes(b.minutesToEmpty)}</dd>`
      : b.state === "charging" && b.minutesToFull
        ? `<dt>До полной</dt><dd>${minutes(b.minutesToFull)}</dd>`
        : "";
  const color = b.percent < 15 ? "var(--crit)" : b.percent < 30 ? "var(--warn)" : "var(--ok)";
  return `
    <h2>Батарея</h2>
    <div class="big" style="color:${color}">${percent(b.percent)}</div>
    <div class="muted">${BATTERY_STATE[b.state]}${b.powerWatts > 0.1 ? ` · ${b.powerWatts.toFixed(1)} Вт` : ""}</div>
    ${bar(b.percent / 100, color)}
    <dl>
      ${time}
      <dt>Здоровье</dt><dd>${percent(b.healthPercent)}</dd>
      <dt>Циклов</dt><dd>${b.cycleCount ?? "?"}</dd>
    </dl>`;
}

document.addEventListener("click", (e) => {
  const goto = (e.target as HTMLElement).closest<HTMLElement>("[data-goto]");
  if (goto) document.querySelector<HTMLButtonElement>(`.tab[data-tab="${goto.dataset.goto}"]`)?.click();
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

  const rows = list
    .slice(0, 250)
    .map(
      (p) => `<tr>
        <td class="name">${escape(p.name)} <span class="muted small">${p.pid}</span></td>
        <td class="num">${percent(p.cpu, 1)}</td>
        <td class="num">${bytes(p.memory)}</td>
        <td class="num">${p.readPerSec ? rate(p.readPerSec) : "—"}</td>
        <td class="num">${p.writePerSec ? rate(p.writePerSec) : "—"}</td>
        <td class="actions"><button class="btn small ${armedPid === p.pid ? "danger" : ""}" data-kill="${p.pid}">
          ${armedPid === p.pid ? "Точно?" : "Завершить"}</button></td>
      </tr>`,
    )
    .join("");

  $("proc-table").innerHTML = `<thead><tr>${head}<th></th></tr></thead><tbody>${rows}</tbody>`;
}

$("proc-table").addEventListener("click", async (e) => {
  const el = e.target as HTMLElement;
  const th = el.closest<HTMLElement>("[data-sort]");
  if (th) {
    const key = th.dataset.sort as SortKey;
    sortDesc = key === sortKey ? !sortDesc : key !== "name";
    sortKey = key;
  }
  const kill = el.closest<HTMLElement>("[data-kill]");
  if (kill) {
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
  }
  if (snapshot) renderProcesses(snapshot.processes);
});

// ---------- Docker ----------

function dockerUnavailable() {
  return `<div class="empty">
    <b>Docker недоступен</b>
    <p class="muted">${escape(docker?.error ?? "Подключаюсь…")}</p>
  </div>`;
}

function renderDockerSummary() {
  if (!docker) return `<h2>Docker</h2><p class="muted">Подключаюсь…</p>`;
  if (!docker.available) return `<h2>Docker</h2>${dockerUnavailable()}`;
  const running = docker.containers.filter((c) => c.state === "running");
  return `
    <h2>Docker · ${running.length} из ${docker.containers.length} запущено
      <button class="link" data-goto="docker">управление →</button></h2>
    <table class="table compact">
      ${running
        .slice(0, 6)
        .map(
          (c) => `<tr><td class="name">${escape(c.name)} <span class="muted small">${escape(c.image)}</span></td>
          <td class="num">${c.cpuPercent != null ? percent(c.cpuPercent, 1) : "—"}</td>
          <td class="num">${c.memoryUsed != null ? bytes(c.memoryUsed) : "—"}</td></tr>`,
        )
        .join("") || `<tr><td class="muted">Нет запущенных контейнеров</td></tr>`}
    </table>`;
}

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
  if (!docker) return (body.innerHTML = `<p class="muted">Подключаюсь к Docker…</p>`);
  if (!docker.available) return (body.innerHTML = dockerUnavailable());
  if (docker.containers.length === 0)
    return (body.innerHTML = `<div class="empty"><b>Контейнеров нет</b></div>`);
  body.innerHTML = `<div class="table-wrap"><table class="table">
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
