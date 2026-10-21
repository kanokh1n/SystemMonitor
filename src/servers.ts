// Раздел «Серверы», v3: VPS по SSH без агента. Данные собирает Rust (src-tauri/src/servers/),
// здесь только карточки серверов и форма добавления.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { History, sparkline } from "./chart";
import { ask } from "./disk";
import { ago, bytes, duration, escape, percent } from "./format";
import type { Server, ServerStats, ServerTarget, SshSetup } from "./types";

type Toast = (text: string, error?: boolean) => void;

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;

const CPU_COLOR = "#14b8a6";
const MEM_COLOR = "#8b5cf6";

let toast: Toast = () => {};
let servers: Server[] | null = null;
let setup: SshSetup = { hosts: [], keys: [] };
/** Какой сервер сейчас редактируем: null — новый, undefined — форма закрыта. */
let editing: string | null | undefined;
const cpuHistory = new Map<string, History>();
const seenAt = new Map<string, number>(); // lastOk, по которому уже добавили точку на график
const dockerOpen = new Set<string>();

// ---------- Мелочи ----------

function bar(fraction: number, color?: string) {
  const pct = Math.max(0, Math.min(1, fraction)) * 100;
  const level = color ? "" : pct > 90 ? "crit" : pct > 75 ? "warn" : "";
  return `<div class="bar"><div class="fill ${level}" style="${color ? `background:${color};` : ""}width:${pct}%"></div></div>`;
}

const target = (s: ServerTarget) =>
  `${s.user ? `${s.user}@` : ""}${s.host}${s.port ? `:${s.port}` : ""}`;

const clock = (unix: number) => new Date(unix * 1000).toLocaleTimeString("ru-RU", { hour: "2-digit", minute: "2-digit" });

const STATUS_TEXT: Record<Server["status"], string> = {
  checking: "подключаюсь…",
  online: "на связи",
  down: "не отвечает",
  error: "ошибка подключения",
  noNetwork: "нет сети на Mac",
};

// ---------- Карточка сервера ----------

function metric(label: string, value: string, extra: string, body = "", aside = "") {
  return `<div class="metric"><span class="muted small">${label}</span>
    <div class="metric-top"><b>${value}</b>${aside}</div>${body}<span class="muted small">${extra}</span></div>`;
}

function metrics(s: Server, st: ServerStats) {
  const mem = st.memory;
  const memUsed = mem.total - mem.available;
  const root = st.disks.find((d) => d.mountPoint === "/") ?? st.disks[0];
  // Как в df: доля от места, доступного пользователям (без зарезервированного под root).
  const diskShare = (d: ServerStats["disks"][number]) => (d.used + d.available ? d.used / (d.used + d.available) : 0);
  const cpu = st.cpuPercent ?? 0;
  return `<div class="metrics">
    ${metric(
      "Процессор",
      st.cpuPercent == null ? "—" : percent(cpu),
      `нагрузка ${st.loadAvg.map((l) => l.toFixed(2)).join(" · ")} · ${st.cores} ${st.cores === 1 ? "ядро" : st.cores < 5 ? "ядра" : "ядер"}`,
      bar(cpu / 100, CPU_COLOR),
      `<canvas class="spark mini" id="srv-spark-${s.id}"></canvas>`,
    )}
    ${metric(
      "Память",
      percent(mem.total ? (memUsed / mem.total) * 100 : 0),
      `${bytes(memUsed)} из ${bytes(mem.total)}${mem.swapTotal ? ` · swap ${bytes(mem.swapUsed)}` : ""}`,
      bar(mem.total ? memUsed / mem.total : 0, MEM_COLOR),
    )}
    ${
      root
        ? metric(
            `Диск ${escape(root.mountPoint)}`,
            percent(diskShare(root) * 100),
            `свободно ${bytes(root.available)} из ${bytes(root.total)}`,
            bar(diskShare(root)),
          )
        : metric("Диск", "—", "")
    }
    ${metric("Без перезагрузки", duration(st.uptimeSecs), escape(st.kernel))}
  </div>
  ${st.disks
    .filter((d) => d !== root)
    .map(
      (d) => `<div class="row disk-row"><span class="mono small grow ellipsis">${escape(d.mountPoint)}
        <span class="muted">${escape(d.fileSystem)}</span></span>
        <span class="small"><b>${bytes(d.available)}</b> <span class="muted">свободно из ${bytes(d.total)}</span></span>
        <div class="disk-bar">${bar(diskShare(d))}</div></div>`,
    )
    .join("")}`;
}

function docker(s: Server, st: ServerStats) {
  const d = st.docker;
  if (!d.installed) return "";
  if (d.error) return `<div class="docker-line muted small">Docker: ${escape(d.error)}</div>`;
  if (d.containers.length === 0) return `<div class="docker-line muted small">Docker: контейнеров нет</div>`;
  const running = d.containers.filter((c) => c.state === "running").length;
  const stopped = d.containers.length - running;
  return `<details class="docker" data-docker="${s.id}" ${dockerOpen.has(s.id) ? "open" : ""}>
    <summary>Docker: <b>${running}</b> из ${d.containers.length} запущено${
      stopped ? ` · <span class="warn-text">${stopped} остановлено</span>` : ""
    }</summary>
    <table class="table">
      <thead><tr><th>Контейнер</th><th>Статус</th><th class="num">CPU</th><th class="num">Память</th></tr></thead>
      <tbody>${d.containers
        .map(
          (c) => `<tr>
          <td class="name"><span class="state ${escape(c.state)}"></span>${escape(c.name)}
            <div class="muted small">${escape(c.image)}</div></td>
          <td class="muted small">${escape(c.status)}</td>
          <td class="num">${c.cpuPercent != null ? percent(c.cpuPercent, 1) : "—"}</td>
          <td class="num">${c.memoryUsed != null ? bytes(c.memoryUsed) : "—"}</td>
        </tr>`,
        )
        .join("")}</tbody>
    </table>
  </details>`;
}

function subtitle(s: Server) {
  const st = s.stats;
  const parts = [escape(target(s))];
  if (st) parts.push(escape(st.os));
  if (s.status === "online" && s.latencyMs != null) parts.push(`ответ за ${s.latencyMs} мс`);
  return parts.join(" · ");
}

function problem(s: Server) {
  if (s.status === "online" && s.failures === 0) return "";
  if (s.status === "checking") return "";
  let title: string;
  if (s.status === "online") title = "Последний опрос не прошёл, пробую ещё раз";
  else if (s.status === "down" && s.downSince)
    title = `Не отвечает с ${clock(s.downSince)}${s.lastOk ? ` · последний ответ ${ago(s.lastOk)}` : ""}`;
  else title = STATUS_TEXT[s.status].replace(/^./, (c) => c.toUpperCase());
  const cls = s.status === "down" ? "crit" : "warn";
  return `<div class="server-problem ${cls}"><b>${escape(title)}</b>${s.error ? `<div class="small">${escape(s.error)}</div>` : ""}</div>`;
}

function card(s: Server) {
  const st = s.stats;
  const stale = s.status !== "online" && st;
  return `<div class="panel server ${s.status}" data-server="${s.id}">
    <div class="server-head">
      <span class="state ${s.busy && s.status === "checking" ? "checking" : s.status}" title="${STATUS_TEXT[s.status]}"></span>
      <div class="grow">
        <b>${escape(s.name)}</b>${st && st.hostname && st.hostname !== s.name ? ` <span class="muted small">${escape(st.hostname)}</span>` : ""}
        <div class="muted small ellipsis">${subtitle(s)}</div>
      </div>
      <button class="btn small" data-srv="refresh" ${s.busy ? "disabled" : ""}>${s.busy ? "Опрашиваю…" : "Обновить"}</button>
      <button class="btn small" data-srv="edit">Изменить</button>
      <button class="btn small" data-srv="remove">Удалить</button>
    </div>
    ${problem(s)}
    ${
      st
        ? `<div class="${stale ? "stale" : ""}">${stale && s.lastOk ? `<div class="muted small">Данные на ${clock(s.lastOk)}</div>` : ""}
           ${metrics(s, st)}${docker(s, st)}</div>`
        : s.status === "checking"
          ? `<p class="muted">Подключаюсь по SSH…</p>`
          : ""
    }
  </div>`;
}

function render() {
  const body = $("servers-body");
  if (!servers) {
    body.innerHTML = `<div class="panel muted">Загружаю список серверов…</div>`;
    return;
  }
  if (servers.length === 0 && editing === undefined) openForm(null);
  body.innerHTML =
    servers.length === 0
      ? `<div class="panel empty"><b>Серверов пока нет</b>
          <p class="muted">Добавь VPS: приложение будет заходить на него по SSH раз в 30 секунд
          и пришлёт уведомление, если сервер перестанет отвечать. Ставить на сервер ничего не нужно.</p></div>`
      : servers.map(card).join("");
  for (const s of servers) {
    const canvas = document.getElementById(`srv-spark-${s.id}`) as HTMLCanvasElement | null;
    const h = cpuHistory.get(s.id);
    if (canvas && h) sparkline(canvas, [{ data: h.values, color: CPU_COLOR }], 100, 40);
  }
  renderBadge();
}

function renderBadge() {
  const badge = document.getElementById("badge-servers");
  if (!badge || !servers) return;
  const down = servers.filter((s) => s.status === "down").length;
  const online = servers.filter((s) => s.status === "online").length;
  badge.textContent = !servers.length ? "" : down ? `⚠ ${down}` : `${online}/${servers.length}`;
  badge.classList.toggle("alert", down > 0);
}

function onServers(list: Server[]) {
  servers = list;
  for (const s of list) {
    if (s.stats?.cpuPercent != null && s.lastOk && seenAt.get(s.id) !== s.lastOk) {
      seenAt.set(s.id, s.lastOk);
      if (!cpuHistory.has(s.id)) cpuHistory.set(s.id, new History(40));
      cpuHistory.get(s.id)!.push(s.stats.cpuPercent);
    }
  }
  render();
}

// ---------- Форма добавления ----------

function openForm(id: string | null) {
  editing = id;
  const s = id ? servers?.find((x) => x.id === id) : null;
  const known = new Set(servers?.map((x) => x.host));
  const suggestions = setup.hosts.filter((h) => !known.has(h.alias));
  const keyName = (k: string) => k.split("/").pop();
  $("servers-form").hidden = false;
  $("servers-form").innerHTML = `<div class="panel form">
    <b>${s ? `Изменить «${escape(s.name)}»` : "Новый сервер"}</b>
    ${
      suggestions.length && !s
        ? `<div class="chips"><span class="muted small">Из ~/.ssh/config:</span>${suggestions
            .map(
              (h) => `<button class="btn small" data-alias="${escape(h.alias)}"
                title="${escape([h.user, h.hostName].filter(Boolean).join("@"))}${h.port ? `:${h.port}` : ""}">${escape(h.alias)}</button>`,
            )
            .join("")}</div>`
        : ""
    }
    <div class="fields">
      <label class="wide">Адрес<input id="srv-host" type="text" spellcheck="false" autocomplete="off"
        placeholder="алиас из ~/.ssh/config, 203.0.113.10 или root@203.0.113.10" value="${escape(s?.host ?? "")}" /></label>
      <label>Название<input id="srv-name" type="text" autocomplete="off" placeholder="как адрес" value="${escape(s?.name ?? "")}" /></label>
      <label>Пользователь<input id="srv-user" type="text" spellcheck="false" autocomplete="off" placeholder="из config или как на Mac" value="${escape(s?.user ?? "")}" /></label>
      <label>Порт<input id="srv-port" type="number" min="1" max="65535" placeholder="22" value="${s?.port ?? ""}" /></label>
      <label>Ключ<select id="srv-key">
        <option value="">как в Терминале (ssh выберет сам)</option>
        ${setup.keys.map((k) => `<option value="${escape(k)}" ${s?.identity === k ? "selected" : ""}>${escape(keyName(k) ?? k)}</option>`).join("")}
      </select></label>
    </div>
    <p class="muted small">Паролей приложение не хранит: подключается по ключам из ~/.ssh так же, как <code>ssh</code> в Терминале,
      и понимает алиасы из ~/.ssh/config. Ключ с паролем один раз добавь в связку:
      <code>ssh-add --apple-use-keychain ~/.ssh/id_ed25519</code>. Нужен Linux на сервере.</p>
    <div class="row">
      <span class="spacer"></span>
      ${servers?.length ? `<button class="btn" id="srv-cancel">Отмена</button>` : ""}
      <button class="btn primary" id="srv-save">${s ? "Сохранить" : "Добавить и проверить"}</button>
    </div>
  </div>`;
  if (!s) $<HTMLInputElement>("srv-host").focus();
}

function closeForm() {
  editing = undefined;
  $("servers-form").hidden = true;
  $("servers-form").innerHTML = "";
}

async function save() {
  const value = (id: string) => $<HTMLInputElement>(id).value.trim();
  const port = Number(value("srv-port"));
  const target: ServerTarget = {
    host: value("srv-host"),
    user: value("srv-user") || null,
    port: port > 0 ? port : null,
    identity: $<HTMLSelectElement>("srv-key").value || null,
  };
  const btn = $<HTMLButtonElement>("srv-save");
  btn.disabled = true;
  try {
    await invoke("server_save", { id: editing ?? null, name: value("srv-name"), target });
    closeForm();
    render();
  } catch (err) {
    toast(String(err), true);
    btn.disabled = false;
  }
}

// ---------- Клики ----------

async function onClick(e: MouseEvent) {
  const el = e.target as HTMLElement;
  if (el.closest("#servers-add")) return openForm(null);
  if (el.closest("#srv-cancel")) return closeForm();
  if (el.closest("#srv-save")) return save();
  const alias = el.closest<HTMLElement>("[data-alias]");
  if (alias) {
    $<HTMLInputElement>("srv-host").value = alias.dataset.alias!;
    $<HTMLInputElement>("srv-name").value ||= alias.dataset.alias!;
    return;
  }
  if (el.closest("#servers-refresh")) {
    const btn = $<HTMLButtonElement>("servers-refresh");
    btn.disabled = true;
    await invoke("server_refresh", { id: null }).catch((err) => toast(String(err), true));
    btn.disabled = false;
    return;
  }

  const action = el.closest<HTMLElement>("[data-srv]")?.dataset.srv;
  const id = el.closest<HTMLElement>("[data-server]")?.dataset.server;
  const s = servers?.find((x) => x.id === id);
  if (!action || !s) return;
  if (action === "refresh") invoke("server_refresh", { id: s.id }).catch((err) => toast(String(err), true));
  if (action === "edit") {
    openForm(s.id);
    $("servers-form").scrollIntoView({ behavior: "smooth", block: "nearest" });
  }
  if (action === "remove") {
    const ok = await ask({
      title: `Убрать «${s.name}» из списка?`,
      body: `<p>Приложение перестанет следить за ${escape(target(s))}. На самом сервере ничего не меняется.</p>`,
      ok: "Убрать",
      danger: true,
    });
    if (!ok) return;
    await invoke("server_remove", { id: s.id }).catch((err) => toast(String(err), true));
    cpuHistory.delete(s.id);
    if (editing === s.id) closeForm();
  }
}

export async function initServers(showToast: Toast) {
  toast = showToast;
  const section = $("servers");
  section.addEventListener("click", onClick);
  section.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && (e.target as HTMLElement).closest("#servers-form input")) save();
  });
  // Запоминаем, какие списки контейнеров раскрыты, чтобы перерисовка их не сворачивала.
  section.addEventListener(
    "toggle",
    (e) => {
      const d = e.target as HTMLDetailsElement;
      if (!d.dataset?.docker) return;
      if (d.open) dockerOpen.add(d.dataset.docker);
      else dockerOpen.delete(d.dataset.docker);
    },
    true,
  );
  listen<Server[]>("servers", ({ payload }) => onServers(payload));
  render();
  setup = await invoke<SshSetup>("ssh_setup").catch(() => setup);
  onServers(await invoke<Server[]>("servers_list").catch(() => []));
}
