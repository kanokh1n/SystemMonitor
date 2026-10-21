// Раздел «Диски», часть v2: что занимает место, программы и быстрая очистка.
// Всё удаляется только в Корзину (кроме образов Docker и очистки самой Корзины — там явно пишем «навсегда»).

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { homeDir } from "@tauri-apps/api/path";

import { bytes, escape } from "./format";
import { squarify } from "./treemap";
import type {
  AppInfo,
  CleanCategory,
  CleanItem,
  DirView,
  DiskEntry,
  DockerUsage,
  ScanProgress,
  TrashReport,
  Uninstall,
} from "./types";

type Toast = (text: string, error?: boolean) => void;

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;

let toast: Toast = () => {};
let home = "";

// ---------- Мелочи форматирования ----------

const count = (n: number) => n.toLocaleString("ru-RU");

function plural(n: number, [one, few, many]: [string, string, string]) {
  const m10 = n % 10;
  const m100 = n % 100;
  if (m10 === 1 && m100 !== 11) return one;
  if (m10 >= 2 && m10 <= 4 && (m100 < 12 || m100 > 14)) return few;
  return many;
}

/** «3 месяца назад» по unix‑времени в секундах. */
function ago(ts: number | null) {
  if (!ts) return "—";
  const days = Math.floor((Date.now() / 1000 - ts) / 86400);
  if (days < 1) return "сегодня";
  if (days < 31) return `${days} ${plural(days, ["день", "дня", "дней"])} назад`;
  const months = Math.floor(days / 30.4);
  if (months < 12) return `${months} мес назад`;
  const years = Math.floor(days / 365);
  return `${years} ${plural(years, ["год", "года", "лет"])} назад`;
}

const short = (p: string) => (home && p.startsWith(home) ? "~" + p.slice(home.length) : p);
const parent = (p: string) => p.slice(0, p.lastIndexOf("/")) || "/";

function bar(fraction: number, color = "var(--accent)") {
  return `<div class="bar"><div class="fill" style="background:${color};width:${Math.max(0, Math.min(1, fraction)) * 100}%"></div></div>`;
}

function clock(ms: number) {
  const s = Math.floor(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

// ---------- Диалог подтверждения ----------

export interface Ask {
  title: string;
  body: string;
  ok: string;
  danger?: boolean;
  /** Только кнопка «Понятно», без отмены. */
  info?: boolean;
}

/** Показывает диалог. Возвращает его (чтобы прочитать галочки) или null, если отменили. */
export function ask({ title, body, ok, danger, info }: Ask): Promise<HTMLDialogElement | null> {
  const dialog = $<HTMLDialogElement>("confirm");
  $("confirm-title").textContent = title;
  $("confirm-body").innerHTML = body;
  const okBtn = $<HTMLButtonElement>("confirm-ok");
  okBtn.textContent = ok;
  okBtn.className = `btn ${danger ? "danger" : "primary"}`;
  $("confirm-cancel").hidden = !!info;
  dialog.showModal();
  return new Promise((resolve) => {
    const done = (result: HTMLDialogElement | null) => {
      okBtn.onclick = null;
      $("confirm-cancel").onclick = null;
      dialog.onclose = null;
      if (dialog.open) dialog.close();
      resolve(result);
    };
    okBtn.onclick = () => done(dialog);
    $("confirm-cancel").onclick = () => done(null);
    dialog.onclose = () => done(null);
  });
}

/** Список путей для диалога: первые несколько и «и ещё N». */
function pathList(paths: string[]) {
  const shown = paths.slice(0, 8).map((p) => `<li class="mono small">${escape(short(p))}</li>`);
  if (paths.length > 8) shown.push(`<li class="muted">и ещё ${paths.length - 8}</li>`);
  return `<ul class="paths">${shown.join("")}</ul>`;
}

/**
 * Спрашивает и перемещает в Корзину. Возвращает пути, которые ушли в Корзину.
 * Про то, что не получилось, показывает отдельный диалог с причинами.
 */
async function trash(paths: string[], total: number, what = ""): Promise<string[]> {
  if (!paths.length) return [];
  const ok = await ask({
    title: `Переместить в Корзину${what ? ` ${what}` : ""}?`,
    body: `${pathList(paths)}
      <p>Освободится <b>${bytes(total)}</b> после очистки Корзины. До этого всё можно вернуть из Корзины в Finder.</p>`,
    ok: "В Корзину",
    danger: true,
  });
  if (!ok) return [];
  let report: TrashReport;
  try {
    report = await invoke<TrashReport>("disk_trash", { paths });
  } catch (err) {
    toast(String(err), true);
    return [];
  }
  if (report.trashed.length) toast(`В Корзине: ${report.trashed.length} ${plural(report.trashed.length, ["объект", "объекта", "объектов"])}`);
  if (report.failed.length) {
    await ask({
      title: "Часть не удалось переместить",
      body: `<ul class="paths">${report.failed
        .map(([p, why]) => `<li><span class="mono small">${escape(short(p))}</span><div class="muted small">${escape(why)}</div></li>`)
        .join("")}</ul>`,
      ok: "Понятно",
      info: true,
    });
  }
  return report.trashed;
}

// ---------- Полный доступ к диску ----------

let fullAccess = true;

function renderAccessHint(denied = 0) {
  const el = $("fda-hint");
  el.hidden = fullAccess && denied === 0;
  el.innerHTML = `<div>
      <b>${denied ? `Не удалось прочитать ${count(denied)} ${plural(denied, ["папку", "папки", "папок"])}` : "Нет полного доступа к диску"}</b>
      <div class="muted">Без него macOS прячет почту, Safari, данные других программ и спрашивает разрешение на каждую папку
        (Рабочий стол, Документы, Загрузки). Включи System Monitor в списке и перезапусти приложение.
        После каждой новой сборки галочку, возможно, придётся поставить заново.</div>
    </div>
    <button class="btn" data-fda>Открыть настройки</button>`;
}

// ---------- Что занимает место ----------

type Mode = "dirs" | "files" | "stale";

let progress: ScanProgress | null = null;
let view: DirView | null = null;
let mode: Mode = "dirs";
let staleDays = 180;
let list: DiskEntry[] = []; // для режимов «Крупные файлы» и «Давно не менялось»
const selected = new Map<string, number>(); // путь → размер

const MAP_COLORS = ["#3b82f6", "#8b5cf6", "#14b8a6", "#f59e0b", "#ec4899", "#22c55e", "#6366f1", "#ef4444", "#06b6d4", "#a855f7"];

async function startScan(path?: string) {
  selected.clear();
  view = null;
  try {
    await invoke("disk_scan_start", { path: path ?? null });
  } catch (err) {
    toast(String(err), true);
  }
}

async function loadView(path?: string) {
  try {
    view = await invoke<DirView>("disk_scan_dir", { path: path ?? null });
  } catch {
    view = await invoke<DirView>("disk_scan_dir", { path: null }).catch(() => null);
  }
  if (mode === "files") list = await invoke<DiskEntry[]>("disk_top_files", { limit: 200 }).catch(() => []);
  if (mode === "stale")
    list = await invoke<DiskEntry[]>("disk_stale", { days: staleDays, minSize: 50 * 1024 * 1024 }).catch(() => []);
  renderScan();
}

function entryRow(e: DiskEntry, max: number, showPath: boolean) {
  const checked = selected.has(e.path) ? "checked" : "";
  const icon = e.isDir ? "📁" : "📄";
  const name =
    e.isDir && e.hasChildren && mode === "dirs"
      ? `<button class="link" data-open="${escape(e.path)}">${escape(e.name)}</button>`
      : escape(e.name);
  const pills = [
    e.denied ? `<span class="pill warn" title="Нет прав на чтение">нет доступа</span>` : "",
    e.protected ? `<span class="pill" title="${escape(e.protected)}">защищено</span>` : "",
  ].join(" ");
  return `<tr class="${checked ? "sel" : ""}">
    <td class="check"><input type="checkbox" data-select="${escape(e.path)}" data-size="${e.size}" ${checked}
      ${e.protected ? `disabled title="${escape(e.protected)}"` : ""}></td>
    <td class="name" title="${escape(e.path)}">${icon} ${name} ${pills}
      ${showPath ? `<div class="muted small">${escape(short(parent(e.path)))}</div>` : ""}</td>
    <td class="wide-col">${bar(e.size / max, e.isDir ? "var(--accent)" : "#8b5cf6")}</td>
    <td class="num"><b>${bytes(e.size)}</b></td>
    <td class="num muted">${e.isDir ? count(e.files) : ""}</td>
    <td class="num muted">${ago(e.newest)}</td>
    <td class="actions"><button class="btn small" data-reveal="${escape(e.path)}">Finder</button></td>
  </tr>`;
}

const tableHead = `<thead><tr><th></th><th>Название</th><th class="wide-col"></th><th class="num">Размер</th>
  <th class="num">Файлов</th><th class="num">Изменено</th><th></th></tr></thead>`;

function breadcrumbs(v: DirView) {
  const rel = v.dir.path.slice(v.root.length).split("/").filter(Boolean);
  let path = v.root;
  const crumbs = [`<button class="link" data-open="${escape(v.root)}">${escape(short(v.root))}</button>`];
  for (const part of rel) {
    path += "/" + part;
    crumbs.push(`<button class="link" data-open="${escape(path)}">${escape(part)}</button>`);
  }
  return `<div class="crumbs">${crumbs.join('<span class="muted"> › </span>')}</div>`;
}

function renderTreemap(v: DirView) {
  const el = document.getElementById("treemap");
  if (!el) return;
  const items = v.children.filter((c) => c.size > 0).slice(0, 60);
  const restSize = v.restSize + v.children.slice(60).reduce((a, c) => a + c.size, 0);
  const values = [...items.map((c) => c.size), restSize].filter((x) => x > 0);
  const rects = squarify(values, el.clientWidth, el.clientHeight);
  el.innerHTML = rects
    .map((r) => {
      const e = items[r.i];
      const color = e ? (e.isDir ? MAP_COLORS[r.i % MAP_COLORS.length] : "#94a3b8") : "var(--track)";
      const label = e ? e.name : "остальное";
      const big = r.w > 70 && r.h > 34;
      const open = e?.isDir && e.hasChildren ? `data-open="${escape(e.path)}"` : "";
      return `<div class="map-tile ${open ? "clickable" : ""}" ${open}
        style="left:${r.x}px;top:${r.y}px;width:${r.w}px;height:${r.h}px;background:${color}"
        title="${escape(label)} — ${bytes(values[r.i])}">${big ? `<span>${escape(label)}</span><small>${bytes(values[r.i], 0)}</small>` : ""}</div>`;
    })
    .join("");
}

function renderScan() {
  const body = $("scan-body");
  const running = progress?.running ?? false;
  $("scan-stop").hidden = !running;

  if (running && progress) {
    body.innerHTML = `<div class="panel">
      <div class="row"><b>Сканирую ${escape(short(progress.root))}</b><span class="spacer"></span>
        <span class="muted">${clock(progress.elapsedMs)}</span></div>
      <div class="bar indeterminate"><div class="fill"></div></div>
      <div class="row"><span><b>${count(progress.files)}</b> файлов · <b>${bytes(progress.bytes)}</b></span></div>
      <div class="muted small ellipsis">${escape(short(progress.current))}</div>
    </div>`;
    return;
  }
  if (!view) {
    body.innerHTML = `<div class="panel empty">
      <b>${progress?.cancelled ? "Скан остановлен" : "Узнай, что занимает место"}</b>
      <p class="muted">Скан домашней папки на SSD обычно занимает меньше минуты. Ничего не удаляется без подтверждения,
        а удалённое попадает в Корзину.</p></div>`;
    return;
  }

  const v = view;
  const tabs: [Mode, string][] = [
    ["dirs", "Папки"],
    ["files", "Крупные файлы"],
    ["stale", "Давно не менялось"],
  ];
  const summary = `<div class="row scan-summary">
    <span><b>${bytes(v.dir.size)}</b> <span class="muted">в ${escape(short(v.root))} · ${count(v.dir.files)} файлов${
      progress ? ` · скан занял ${clock(progress.elapsedMs)}` : ""
    }</span></span>
    <span class="spacer"></span>
    <div class="segmented">${tabs
      .map(([m, t]) => `<button class="${m === mode ? "on" : ""}" data-mode="${m}">${t}</button>`)
      .join("")}</div>
  </div>`;

  let content = "";
  if (mode === "dirs") {
    const max = v.children[0]?.size || 1;
    const rest = v.restSize
      ? `<tr><td></td><td class="muted">Мелкие файлы и папки: ${count(v.restCount)}</td><td></td>
         <td class="num muted">${bytes(v.restSize)}</td><td colspan="3"></td></tr>`
      : "";
    content = `${breadcrumbs(v)}
      <div class="treemap" id="treemap"></div>
      <div class="panel flush scroll"><table class="table">${tableHead}
        <tbody>${v.children.map((e) => entryRow(e, max, false)).join("")}${rest}</tbody></table></div>`;
  } else {
    const max = list[0]?.size || 1;
    const stalePicker =
      mode === "stale"
        ? `<div class="row"><span class="muted">Крупнее 50 МБ, без изменений дольше</span>
           <select id="stale-days">${[
             [90, "3 месяцев"],
             [180, "полугода"],
             [365, "года"],
             [730, "2 лет"],
           ]
             .map(([d, t]) => `<option value="${d}" ${d === staleDays ? "selected" : ""}>${t}</option>`)
             .join("")}</select></div>`
        : "";
    content = `${stalePicker}
      <div class="panel flush scroll"><table class="table">${tableHead}
        <tbody>${
          list.length
            ? list.map((e) => entryRow(e, max, true)).join("")
            : `<tr><td colspan="7" class="muted">Ничего не нашлось</td></tr>`
        }</tbody></table></div>`;
  }
  body.innerHTML = summary + content;
  if (mode === "dirs") renderTreemap(v);
  renderAccessHint(v.denied.length);
  renderSelection();
}

/** Выбранные пути без вложенных (если выбрана папка, её содержимое отдельно не считаем). */
function topSelected(): [string, number][] {
  const all = [...selected.entries()];
  return all.filter(([p]) => !all.some(([q]) => q !== p && p.startsWith(q + "/")));
}

function renderSelection() {
  const items = topSelected();
  const el = $("selbar");
  el.hidden = items.length === 0;
  const total = items.reduce((a, [, s]) => a + s, 0);
  el.innerHTML = `<span>Выбрано: <b>${items.length}</b> · <b>${bytes(total)}</b></span>
    <button class="btn small" data-unselect>Снять</button>
    <button class="btn small danger" data-trash-selected>В Корзину</button>`;
}

listen<ScanProgress>("disk-scan", async ({ payload }) => {
  const finished = progress?.running && !payload.running;
  progress = payload;
  if (finished && !payload.cancelled) {
    mode = "dirs";
    await loadView();
  } else {
    renderScan();
  }
});

// ---------- Программы ----------

let apps: AppInfo[] | null = null;
let appsLoading = false;
let appsSort: "size" | "used" = "size";

async function loadApps() {
  appsLoading = true;
  renderApps();
  apps = await invoke<AppInfo[]>("disk_apps").catch((err) => {
    toast(String(err), true);
    return null;
  });
  appsLoading = false;
  renderApps();
}

function renderApps() {
  const body = $("apps-body");
  if (!apps) {
    body.innerHTML = `<div class="panel empty">
      ${
        appsLoading
          ? `<b>Считаю размеры программ…</b><p class="muted">Несколько секунд: в одном Xcode сотни тысяч файлов.</p>`
          : `<b>Удаление программ вместе с «хвостами»</b>
             <p class="muted">Как AppCleaner: программа и её кэши, настройки и данные в ~/Library уходят в Корзину.</p>
             <button class="btn primary" data-load-apps>Показать программы</button>`
      }</div>`;
    return;
  }
  const sorted = [...apps].sort((a, b) =>
    appsSort === "size" ? b.size - a.size : (a.lastUsed ?? 0) - (b.lastUsed ?? 0),
  );
  const max = Math.max(...apps.map((a) => a.size), 1);
  const yearAgo = Date.now() / 1000 - 180 * 86400;
  body.innerHTML = `<div class="row">
      <span class="muted">${apps.length} программ · ${bytes(apps.reduce((a, x) => a + x.size, 0))}</span>
      <span class="spacer"></span>
      <div class="segmented">
        <button class="${appsSort === "size" ? "on" : ""}" data-apps-sort="size">Самые большие</button>
        <button class="${appsSort === "used" ? "on" : ""}" data-apps-sort="used">Давно не открывали</button>
      </div>
      <button class="btn small" data-load-apps>Обновить</button>
    </div>
    <div class="panel flush scroll"><table class="table">
      <thead><tr><th>Программа</th><th class="wide-col"></th><th class="num">Размер</th><th class="num">Открывали</th><th></th></tr></thead>
      <tbody>${sorted
        .map(
          (a) => `<tr>
          <td class="name" title="${escape(a.path)}"><b>${escape(a.name)}</b> <span class="muted small">${escape(a.version ?? "")}</span>
            <div class="muted small">${escape(a.bundleId)}</div></td>
          <td class="wide-col">${bar(a.size / max)}</td>
          <td class="num"><b>${bytes(a.size)}</b></td>
          <td class="num">${
            a.lastUsed && a.lastUsed < yearAgo ? `<span class="pill warn">${ago(a.lastUsed)}</span>` : `<span class="muted">${ago(a.lastUsed)}</span>`
          }</td>
          <td class="actions">${
            a.protected
              ? `<span class="muted small" title="${escape(a.protected)}">системная</span>`
              : `<button class="btn small" data-uninstall="${escape(a.path)}">Удалить…</button>`
          }</td>
        </tr>`,
        )
        .join("")}</tbody></table></div>`;
}

function checkRow(item: CleanItem, checked: boolean, extra = "") {
  return `<label class="check-row ${item.protected ? "disabled" : ""}" title="${escape(item.protected ?? item.path)}">
    <input type="checkbox" data-path="${escape(item.path)}" data-size="${item.size}" ${checked && !item.protected ? "checked" : ""} ${item.protected ? "disabled" : ""} ${extra}>
    <span class="grow">${escape(item.label)}<div class="muted small mono ellipsis">${escape(short(item.path))}</div></span>
    <b>${bytes(item.size)}</b>
  </label>`;
}

function checkedPaths(root: HTMLElement): [string[], number] {
  const boxes = [...root.querySelectorAll<HTMLInputElement>("input[data-path]:checked")];
  return [boxes.map((b) => b.dataset.path!), boxes.reduce((a, b) => a + Number(b.dataset.size), 0)];
}

async function uninstall(path: string) {
  let info: Uninstall;
  try {
    info = await invoke<Uninstall>("disk_app_leftovers", { path });
  } catch (err) {
    return toast(String(err), true);
  }
  const all = [info.app, ...info.leftovers];
  const dialog = await ask({
    title: `Удалить ${info.app.label || path}?`,
    body: `<p class="muted">Программа и её данные уйдут в Корзину. Снять галочку можно с любых данных, если хочешь их оставить.</p>
      <div class="check-list">${checkRow(info.app, true)}${
        info.leftovers.length
          ? info.leftovers.map((i) => checkRow(i, true)).join("")
          : `<p class="muted">«Хвостов» в ~/Library не нашлось.</p>`
      }</div>
      <p>Всего: <b id="uninstall-total">${bytes(all.reduce((a, i) => a + (i.protected ? 0 : i.size), 0))}</b></p>`,
    ok: "В Корзину",
    danger: true,
  });
  if (!dialog) return;
  const [paths] = checkedPaths(dialog);
  if (!paths.length) return;
  const report = await invoke<TrashReport>("disk_trash", { paths }).catch((err) => {
    toast(String(err), true);
    return null;
  });
  if (!report) return;
  if (report.failed.length) {
    await ask({
      title: "Часть не удалось переместить",
      body: `<ul class="paths">${report.failed
        .map(([p, why]) => `<li><span class="mono small">${escape(short(p))}</span><div class="muted small">${escape(why)}</div></li>`)
        .join("")}</ul>`,
      ok: "Понятно",
      info: true,
    });
  }
  if (report.trashed.includes(path)) {
    toast(`${info.app.label} в Корзине`);
    apps = apps?.filter((a) => a.path !== path) ?? null;
    renderApps();
  }
}

// ---------- Быстрая очистка ----------

let categories: CleanCategory[] | null = null;
let dockerUsage: DockerUsage | null = null;
let trashSize: number | null = null;
let cleanupLoading = false;
const catSelected = new Map<string, Set<string>>(); // категория → выбранные пути
const expanded = new Set<string>();

async function loadCleanup() {
  cleanupLoading = true;
  renderCleanup();
  const [cats, docker, trashBytes] = await Promise.all([
    invoke<CleanCategory[]>("disk_cleanup_scan"),
    invoke<DockerUsage>("disk_docker_usage"),
    invoke<number | null>("disk_trash_size"),
  ]).catch((err) => {
    toast(String(err), true);
    return [null, null, null] as const;
  });
  categories = cats;
  dockerUsage = docker;
  trashSize = trashBytes;
  catSelected.clear();
  for (const c of categories ?? []) {
    catSelected.set(c.id, new Set(c.items.filter((i) => i.selected && !i.protected).map((i) => i.path)));
  }
  cleanupLoading = false;
  renderCleanup();
}

function categoryCard(c: CleanCategory) {
  const sel = catSelected.get(c.id)!;
  const total = c.items.reduce((a, i) => a + i.size, 0);
  const chosen = c.items.filter((i) => sel.has(i.path)).reduce((a, i) => a + i.size, 0);
  const open = expanded.has(c.id);
  return `<div class="panel clean">
    <div class="row">
      <div class="grow"><b>${c.title}</b> <span class="muted">· ${bytes(total)}</span>
        <div class="muted small">${c.hint}</div></div>
      ${c.items.length ? `<button class="btn small" data-expand="${c.id}">${open ? "Скрыть" : `Список (${c.items.length})`}</button>` : ""}
      <button class="btn small ${chosen ? "danger" : ""}" data-clean="${c.id}" ${chosen ? "" : "disabled"}>
        В Корзину ${chosen ? bytes(chosen) : ""}</button>
    </div>
    ${
      open
        ? `<div class="check-list">${c.items
            .map((i) =>
              checkRow(
                { ...i, label: c.id === "projects" ? `${i.label} · ${i.path.endsWith("/target") ? "target" : "node_modules"} · правки ${ago(i.newest)}` : i.label },
                sel.has(i.path),
                `data-cat="${c.id}"`,
              ),
            )
            .join("")}</div>`
        : ""
    }
  </div>`;
}

function dockerCard(d: DockerUsage) {
  if (!d.available)
    return `<div class="panel clean"><b>Docker</b> <span class="muted">· ${escape(d.error ?? "недоступен")}</span></div>`;
  const rows: [string, string, number, string][] = [
    ["containers", "Остановленные контейнеры", d.stoppedContainers[1], `${d.stoppedContainers[0]} шт.`],
    ["images", "Образы без контейнеров", d.unusedImages[1], `${d.unusedImages[0]} шт., общие слои посчитаны несколько раз`],
    ["buildCache", "Кэш сборки (docker build)", d.buildCache ?? 0, d.buildCache == null ? "размер неизвестен" : ""],
  ];
  return `<div class="panel clean">
    <div class="row"><div class="grow"><b>Docker</b>
      <div class="muted small">Чистит сам Docker, это навсегда. Тома (${d.volumes[0]} шт., ${bytes(d.volumes[1])}) с базами данных не трогаем.</div></div>
      <button class="btn small danger" data-docker-prune>Очистить</button></div>
    <div class="check-list">${rows
      .map(
        ([id, label, size, note]) => `<label class="check-row">
        <input type="checkbox" data-docker="${id}" ${size > 0 ? "checked" : ""}>
        <span class="grow">${label}<div class="muted small">${escape(note)}</div></span><b>${bytes(size)}</b></label>`,
      )
      .join("")}</div>
  </div>`;
}

function renderCleanup() {
  const body = $("cleanup-body");
  if (cleanupLoading || !categories) {
    body.innerHTML = `<div class="panel empty">${
      cleanupLoading
        ? `<b>Ищу, что можно почистить…</b><p class="muted">Кэши, node_modules в проектах, старые версии IDE, Docker.</p>`
        : `<b>Кэши, node_modules, старые IDE, Docker</b>
           <p class="muted">Найду то, что можно удалить почти не глядя, и покажу размер.</p>
           <button class="btn primary" data-load-cleanup>Найти</button>`
    }</div>`;
    return;
  }
  const trashCard = `<div class="panel clean"><div class="row">
    <div class="grow"><b>Корзина</b> <span class="muted">· ${trashSize == null ? "размер виден только с полным доступом к диску" : bytes(trashSize)}</span>
      <div class="muted small">Место освобождается только после очистки Корзины. Это навсегда.</div></div>
    <button class="btn small danger" data-empty-trash ${trashSize === 0 ? "disabled" : ""}>Очистить Корзину</button>
  </div></div>`;
  body.innerHTML = `<div class="row"><span class="spacer"></span><button class="btn small" data-load-cleanup>Пересчитать</button></div>
    ${categories.map(categoryCard).join("")}
    ${dockerUsage ? dockerCard(dockerUsage) : ""}
    ${trashCard}`;
}

async function cleanCategory(id: string) {
  const c = categories?.find((x) => x.id === id);
  if (!c) return;
  const sel = catSelected.get(id)!;
  const items = c.items.filter((i) => sel.has(i.path));
  const done = await trash(
    items.map((i) => i.path),
    items.reduce((a, i) => a + i.size, 0),
  );
  c.items = c.items.filter((i) => !done.includes(i.path));
  for (const p of done) sel.delete(p);
  renderCleanup();
}

async function dockerPrune() {
  const targets = [...document.querySelectorAll<HTMLInputElement>("input[data-docker]:checked")].map((b) => b.dataset.docker!);
  if (!targets.length) return;
  const ok = await ask({
    title: "Очистить Docker?",
    body: `<p>Docker удалит выбранное навсегда, из Корзины это не вернуть. Образы скачаются заново при следующем <code>docker run</code> или <code>docker pull</code>.</p>
      <p class="muted">Тома с данными не трогаются.</p>`,
    ok: "Очистить навсегда",
    danger: true,
  });
  if (!ok) return;
  try {
    const freed = await invoke<number>("disk_docker_prune", { targets });
    toast(`Docker освободил ${bytes(freed)}`);
    dockerUsage = await invoke<DockerUsage>("disk_docker_usage");
    renderCleanup();
  } catch (err) {
    toast(String(err), true);
  }
}

async function emptyTrash() {
  const ok = await ask({
    title: "Очистить Корзину?",
    body: `<p>Всё, что сейчас в Корзине${trashSize ? ` (${bytes(trashSize)})` : ""}, будет удалено навсегда.</p>`,
    ok: "Очистить навсегда",
    danger: true,
  });
  if (!ok) return;
  try {
    await invoke("disk_empty_trash");
    toast("Корзина очищена");
    trashSize = await invoke<number | null>("disk_trash_size");
    renderCleanup();
  } catch (err) {
    toast(String(err), true);
  }
}

// ---------- Клики и галочки ----------

function onClick(e: MouseEvent) {
  const el = e.target as HTMLElement;
  const data = (name: string) => el.closest<HTMLElement>(`[data-${name}]`)?.dataset;

  if (el.closest("#scan-home")) return startScan();
  if (el.closest("#scan-apps")) return startScan("/Applications");
  if (el.closest("#scan-stop")) return invoke("disk_scan_cancel");
  if (el.closest("#scan-pick"))
    return invoke<string | null>("pick_folder").then((p) => {
      if (p) startScan(p);
    });

  const d =
    data("open") ?? data("mode") ?? data("reveal") ?? data("fda") ?? data("unselect") ?? data("trash-selected") ??
    data("load-apps") ?? data("apps-sort") ?? data("uninstall") ?? data("load-cleanup") ?? data("expand") ??
    data("clean") ?? data("docker-prune") ?? data("empty-trash");
  if (!d) return;

  if (d.open) return loadView(d.open);
  if (d.mode) {
    mode = d.mode as Mode;
    return loadView(view?.dir.path);
  }
  if (d.reveal) return invoke("reveal_in_finder", { path: d.reveal });
  if (d.fda !== undefined) return invoke("open_full_disk_access");
  if (d.unselect !== undefined) {
    selected.clear();
    return renderScan();
  }
  if (d.trashSelected !== undefined) {
    const items = topSelected();
    return trash(
      items.map(([p]) => p),
      items.reduce((a, [, s]) => a + s, 0),
    ).then((done) => {
      for (const p of done) selected.delete(p);
      if (done.length) loadView(view?.dir.path);
    });
  }
  if (d.loadApps !== undefined) return loadApps();
  if (d.appsSort) {
    appsSort = d.appsSort as "size" | "used";
    return renderApps();
  }
  if (d.uninstall) return uninstall(d.uninstall);
  if (d.loadCleanup !== undefined) return loadCleanup();
  if (d.expand) {
    expanded.has(d.expand) ? expanded.delete(d.expand) : expanded.add(d.expand);
    return renderCleanup();
  }
  if (d.clean) return cleanCategory(d.clean);
  if (d.dockerPrune !== undefined) return dockerPrune();
  if (d.emptyTrash !== undefined) return emptyTrash();
}

function onChange(e: Event) {
  const el = e.target as HTMLInputElement | HTMLSelectElement;
  if (el.id === "stale-days") {
    staleDays = Number(el.value);
    return loadView(view?.dir.path);
  }
  if (!(el instanceof HTMLInputElement)) return;
  if (el.dataset.select) {
    if (el.checked) selected.set(el.dataset.select, Number(el.dataset.size));
    else selected.delete(el.dataset.select);
    el.closest("tr")?.classList.toggle("sel", el.checked);
    return renderSelection();
  }
  if (el.dataset.cat && el.dataset.path) {
    const sel = catSelected.get(el.dataset.cat)!;
    el.checked ? sel.add(el.dataset.path) : sel.delete(el.dataset.path);
    return renderCleanup();
  }
}

// Итог в диалоге удаления программы пересчитывается при каждой галочке.
function onDialogChange() {
  const total = document.getElementById("uninstall-total");
  if (total) total.textContent = bytes(checkedPaths($("confirm"))[1]);
}

export async function initDisk(showToast: Toast) {
  toast = showToast;
  home = (await homeDir().catch(() => "")).replace(/\/$/, "");
  $("disks").addEventListener("click", onClick);
  $("disks").addEventListener("change", onChange);
  $("selbar").addEventListener("click", onClick);
  $("confirm").addEventListener("change", onDialogChange);
  window.addEventListener("resize", () => mode === "dirs" && view && renderTreemap(view));
  fullAccess = await invoke<boolean>("has_full_disk_access").catch(() => true);
  renderAccessHint();
  renderScan();
  renderApps();
  renderCleanup();
}
