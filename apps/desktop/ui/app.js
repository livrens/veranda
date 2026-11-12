/* Veranda UI - vanilla JS, no build step. Talks to the Rust side through Tauri IPC. */
// Apply the saved theme as early as possible (no inline script: keeps the CSP strict).
try { const saved = localStorage.getItem("veranda.theme"); if (saved === "light" || saved === "dark") document.documentElement.dataset.theme = saved; } catch (e) {}
try { const skin = localStorage.getItem("veranda.skin"); if (["glass", "paper", "charcoal"].includes(skin)) document.documentElement.dataset.skin = skin; } catch (e) {}
try { const l = localStorage.getItem("veranda.lang"); if (l === "id" || l === "en") document.documentElement.lang = l; } catch (e) {}
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const ICONS = window.VERANDA_ICONS;
const BRANDS = window.VERANDA_BRANDS || {};
// Vendor logo when we have one (brands.js), else Veranda's own line icon.
const kindIcon = (kind) => BRANDS[kind] || ICONS[KIND_META[kind].icon];

// Every downloadable kind. `svc` links a kind to the runtime service it provides (web/php/db/pg/mongodb);
// tools have no service. The sidebar and the service pages are generated from this table.
const KIND_META = {
  nginx:      { label: "Nginx",          svc: "web",   group: "web",   icon: "web" },
  apache:     { label: "Apache",         svc: "web",   group: "web",   icon: "web" },
  php:        { label: "PHP",            svc: "php",   group: "lang",  icon: "php" },
  python:     { label: "Python",         group: "lang", icon: "lang" },
  mariadb:    { label: "MariaDB",        svc: "db",    group: "db",    icon: "db" },
  mysql:      { label: "MySQL",          svc: "db",    group: "db",    icon: "db" },
  postgres:   { label: "PostgreSQL",     svc: "pg",    group: "db",    icon: "db" },
  mongodb:    { label: "MongoDB",        svc: "mongodb", group: "db",  icon: "db" },
  meilisearch:{ label: "Meilisearch",    svc: "meilisearch", group: "search", icon: "search", open: "meili" },
  node:       { label: "Node.js",        group: "tools", icon: "node" },
  adminer:    { label: "Adminer",        group: "tools", icon: "tool", open: "adminer" },
  phpmyadmin: { label: "phpMyAdmin",     group: "tools", icon: "tool", open: "phpmyadmin" },
};
const GROUPS = [["web", "group.web"], ["lang", "group.lang"], ["db", "group.db"], ["tools", "group.tools"]];
const SVC_SECTION = { web: "web", php: "php", db: "db", pg: "pg", mongodb: "mongodb" };
const SVC_ROLE = { web: "role.web", db: "role.db" };

let state = null, lang = document.documentElement.lang === "id" ? "id" : "en", upstream = null, builtin = null, upstreamLoading = false;
let installing = null; // { kind, downloaded, total, phase: "download"|"extract" }
let removing = null;   // { kind, version }
let busy = {};         // service id -> "start"|"stop"|"restart"|"reload" while a request runs
let busyAll = null;    // "start"|"stop" while Start All / Stop All runs
let currentKind = null, svcTab = "service", currentTab = "dashboard";

const $ = (s, el = document) => el.querySelector(s);
const $$ = (s, el = document) => [...el.querySelectorAll(s)];
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
const t = (key, vars = {}) => {
  const dict = window.VERANDA_I18N[lang] || window.VERANDA_I18N.en;
  let s = dict[key] ?? window.VERANDA_I18N.en[key] ?? key;
  for (const [k, v] of Object.entries(vars)) s = s.replace(`{${k}}`, v);
  return s;
};
const cmpVer = (a, b) => {
  const pa = String(a).match(/\d+/g)?.map(Number) || [], pb = String(b).match(/\d+/g)?.map(Number) || [];
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) { const d = (pa[i] ?? -1) - (pb[i] ?? -1); if (d) return d; }
  return 0;
};
const clone = (o) => JSON.parse(JSON.stringify(o));

/* ---------------- activity + toasts ---------------- */
const activity = [];
function logActivity(msg, cls) {
  const d = new Date();
  activity.unshift({ t: `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}:${String(d.getSeconds()).padStart(2, "0")}`, msg, cls });
  if (activity.length > 100) activity.pop();
  renderActivity();
}
function renderActivity() {
  const ul = $("#activity-list");
  if (!ul) return;
  ul.innerHTML = activity.map((a) => `<li class="${a.cls}"><time>${a.t}</time><i></i><span>${esc(a.msg)}</span></li>`).join("") || `<div class="activity-empty">—</div>`;
}
const TOAST_ICON = { ok: ICONS.toastOk, bad: ICONS.toastBad, info: ICONS.toastInfo };
// Static markup (index.html) declares icons as <i class="ic" data-icon="name"></i>; fill them once.
const applyIcons = () => { $$("i.ic[data-icon]").forEach((el) => { if (!el.firstChild) el.innerHTML = ICONS[el.dataset.icon] || ""; }); };
applyIcons();
function toast(msg, cls = "") {
  const box = $("#toasts");
  while (box.children.length >= 3) box.firstChild.remove();
  const el = document.createElement("div");
  el.className = `toast ${cls}`;
  el.innerHTML = `${TOAST_ICON[cls] || TOAST_ICON.info}<div class="body"></div>`;
  $(".body", el).textContent = msg;
  const close = () => { if (!el.isConnected) return; el.classList.add("out"); setTimeout(() => el.remove(), 180); };
  el.onclick = close;
  box.appendChild(el);
  setTimeout(close, cls === "bad" ? 6000 : 2800);
  logActivity(msg, cls === "bad" ? "bad" : cls === "ok" ? "ok" : "");
}
const fail = (e) => { const m = typeof e === "string" ? e : e?.message || String(e); toast(m, "bad"); console.error(e); };
window.addEventListener("error", (ev) => toast(`UI error: ${ev.message} (${(ev.filename || "").split("/").pop()}:${ev.lineno})`, "bad"));
window.addEventListener("unhandledrejection", (ev) => toast(`UI error: ${ev.reason?.message || ev.reason}`, "bad"));
function askConfirm(text, okLabel) {
  return new Promise((resolve) => {
    const dlg = $("#dlg-confirm");
    $("#confirm-text").textContent = text;
    $("#btn-confirm-ok").textContent = okLabel || t("btn.remove");
    const done = (v) => { dlg.close(); resolve(v); };
    dlg.querySelector("form").onsubmit = (ev) => { ev.preventDefault(); done(true); };
    $("#btn-confirm-cancel").onclick = () => done(false);
    dlg.oncancel = (ev) => { ev.preventDefault(); done(false); };
    dlg.showModal();
  });
}
const applyI18n = () => { $$("[data-i18n]").forEach((el) => { el.textContent = t(el.dataset.i18n); }); $$("[data-i18n-ph]").forEach((el) => { el.placeholder = t(el.dataset.i18nPh); }); $$("[data-i18n-title]").forEach((el) => { el.title = t(el.dataset.i18nTitle); }); document.documentElement.lang = lang; renderLockSettings(); };

/* ---------------- look (skin) ---------------- */
// Skins only swap the design tokens; layout and behaviour stay the same. Kept in localStorage
// like the light/dark choice so the first paint already has it (no flash).
function setSkin(skin) {
  if (skin) document.documentElement.dataset.skin = skin; else delete document.documentElement.dataset.skin;
  try { if (skin) localStorage.setItem("veranda.skin", skin); else localStorage.removeItem("veranda.skin"); } catch (e) {}
  const sel = $("#set-skin"); if (sel && sel.value !== (skin || "")) sel.value = skin || "";
}

/* ---------------- app lock ---------------- */
// The backend refuses every state command while locked ("locked"); this overlay is the front
// of that gate, not the gate itself. Idle time is measured here (input events) and the lock is
// re-armed through `app_lock`.
let lockInfo = { enabled: false, locked: false, kind: "pin", totp: false, auto_lock_minutes: 0, wait_secs: 0 };
let lastActivity = Date.now();
let unlockResolve = null;
function showLock(on) {
  const el = $("#lock-screen");
  el.hidden = !on; el.classList.toggle("hidden", !on);
  $("#app").classList.toggle("is-locked", on);
  const secret = $("#lock-secret");
  secret.inputMode = lockInfo.kind === "pin" ? "numeric" : "text";
  $("#lock-code").classList.toggle("hidden", !lockInfo.totp);
  if (on) { secret.value = ""; $("#lock-code").value = ""; $("#lock-err").textContent = ""; setTimeout(() => secret.focus(), 60); }
}
async function refreshLockInfo() {
  try { lockInfo = await invoke("lock_status"); } catch (e) { console.error(e); }
  $("#btn-lock").classList.toggle("hidden", !lockInfo.enabled);
  renderLockSettings();
}
function renderLockSettings() {
  const st = $("#lock-state"); if (!st) return;
  st.textContent = lockInfo.enabled ? t("lock.on") : t("lock.off");
  $("#btn-lock-off").classList.toggle("hidden", !lockInfo.enabled);
  $("#btn-lock-set").textContent = t(lockInfo.enabled ? "lock.change" : "lock.enable");
  const sel = $("#lock-timer"); sel.disabled = !lockInfo.enabled; sel.value = String(lockInfo.auto_lock_minutes ?? 0);
  $("#totp-row").classList.toggle("hidden", !lockInfo.enabled);
  $("#totp-state").textContent = lockInfo.totp ? t("totp.on") : t("totp.off");
  $("#btn-totp-on").classList.toggle("hidden", lockInfo.totp);
  $("#btn-totp-off").classList.toggle("hidden", !lockInfo.totp);
}
function waitUnlock() { return new Promise((resolve) => { unlockResolve = resolve; }); }
async function lockNow() {
  if (!lockInfo.enabled || lockInfo.locked) return;
  try { await invoke("app_lock"); } catch (e) { fail(e); return; }
  lockInfo.locked = true; showLock(true);
  await waitUnlock();
  await refresh(true);
}
$("#lock-form").onsubmit = async (ev) => {
  ev.preventDefault();
  const secret = $("#lock-secret").value;
  if (!secret) return;
  if (lockInfo.totp && $("#lock-code").value.replace(/\D/g, "").length !== 6) { $("#lock-err").textContent = t("lock.codeWrong"); $("#lock-code").focus(); return; }
  const btn = $("#btn-unlock"); btn.disabled = true;
  try {
    await invoke("app_unlock", { secret, code: lockInfo.totp ? $("#lock-code").value : null });
    lockInfo.locked = false; lastActivity = Date.now(); showLock(false);
    if (unlockResolve) { const r = unlockResolve; unlockResolve = null; r(); }
  } catch (e) {
    const m = String(e);
    $("#lock-err").textContent = m.startsWith("wait:") ? t("lock.wait", { s: m.slice(5) }) : m === "code" ? t("lock.codeWrong") : t("lock.wrong");
    if (m === "code") { $("#lock-code").value = ""; $("#lock-code").focus(); } else $("#lock-secret").select();
  } finally { btn.disabled = false; }
};
$("#btn-lock").onclick = lockNow;
["pointerdown", "keydown", "wheel", "mousemove"].forEach((n) => document.addEventListener(n, () => { lastActivity = Date.now(); }, { passive: true }));
setInterval(() => {
  const m = Number(lockInfo.auto_lock_minutes) || 0;
  if (lockInfo.enabled && !lockInfo.locked && m > 0 && Date.now() - lastActivity > m * 60000) lockNow();
}, 10000);
let lockDialogMode = "set";
function openLockDialog(mode) {
  lockDialogMode = mode;
  const f = $("#form-lock"); f.reset();
  $("#dlg-lock-title").textContent = t(mode === "off" ? "lock.offTitle" : lockInfo.enabled ? "lock.changeTitle" : "lock.setTitle");
  $("#lock-current-row").classList.toggle("hidden", !lockInfo.enabled);
  $("#lock-kind-row").classList.toggle("hidden", mode === "off");
  f.elements.kind.value = lockInfo.kind === "password" ? "password" : "pin";
  $("#lock-new-row").classList.toggle("hidden", mode === "off");
  $("#lock-confirm-row").classList.toggle("hidden", mode === "off");
  f.elements.new.required = mode !== "off"; f.elements.confirm.required = mode !== "off";
  $("#dlg-lock").showModal();
  (lockInfo.enabled ? f.elements.current : f.elements.new).focus();
}
$("#form-lock").onsubmit = async (ev) => {
  ev.preventDefault();
  const f = ev.target;
  const current = lockInfo.enabled ? f.elements.current.value : null;
  const fresh = lockDialogMode === "off" ? null : f.elements.new.value;
  if (fresh !== null && fresh !== f.elements.confirm.value) { toast(t("lock.mismatch"), "warn"); f.elements.confirm.focus(); return; }
  const kind = f.elements.kind.value;
  if (fresh !== null && kind === "pin" && !/^\d+$/.test(fresh)) { toast(t("lock.pinDigits"), "warn"); f.elements.new.focus(); return; }
  try {
    const on = await invoke("app_lock_set", { current, new: fresh, autoLockMinutes: Number($("#lock-timer").value) || 10, kind });
    $("#dlg-lock").close();
    toast(t(on ? "lock.saved" : "lock.removed"), "ok");
    await refreshLockInfo();
  } catch (e) { fail(String(e) === "wrong" ? t("lock.wrong") : e); }
};
$("#dlg-lock [data-close]").onclick = () => $("#dlg-lock").close();

/* Second factor: the secret is generated by the backend, shown once as a setup key, and only
   saved after the authenticator app proves it with a matching code. */
let totpMode = "on", totpSetup = null;
function openTotpDialog(mode) {
  totpMode = mode; totpSetup = null;
  const f = $("#form-totp"); f.reset();
  $("#dlg-totp-title").textContent = t(mode === "off" ? "totp.offTitle" : "totp.setTitle");
  $("#totp-step1").classList.toggle("hidden", mode === "off");
  $("#totp-step2").classList.add("hidden");
  $("#totp-off").classList.toggle("hidden", mode !== "off");
  $("#btn-totp-next").textContent = t(mode === "off" ? "totp.disable" : "btn.next");
  $("#dlg-totp").showModal();
  (mode === "off" ? f.elements.current_off : f.elements.current).focus();
}
$("#form-totp").onsubmit = async (ev) => {
  ev.preventDefault();
  const f = ev.target;
  try {
    if (totpMode === "off") {
      await invoke("app_totp_disable", { current: f.elements.current_off.value });
      $("#dlg-totp").close(); toast(t("totp.disabled"), "ok"); await refreshLockInfo(); return;
    }
    if (!totpSetup) {
      totpSetup = await invoke("app_totp_begin", { current: f.elements.current.value });
      $("#totp-key").textContent = totpSetup.secret.replace(/(.{4})/g, "$1 ").trim();
      $("#totp-step1").classList.add("hidden"); $("#totp-step2").classList.remove("hidden");
      $("#btn-totp-next").textContent = t("btn.save");
      f.elements.code.focus();
      return;
    }
    await invoke("app_totp_confirm", { code: f.elements.code.value });
    $("#dlg-totp").close(); toast(t("totp.enabled"), "ok"); await refreshLockInfo();
  } catch (e) {
    const m = String(e);
    if (m === "wrong") fail(t("lock.wrong")); else if (m === "code") { fail(t("lock.codeWrong")); f.elements.code.select(); } else fail(e);
  }
};
$("#dlg-totp [data-close]").onclick = () => $("#dlg-totp").close();
$("#btn-totp-copy-key").onclick = async () => { if (totpSetup) { await navigator.clipboard.writeText(totpSetup.secret); toast(t("msg.copied"), "ok"); } };
$("#btn-totp-copy-uri").onclick = async () => { if (totpSetup) { await navigator.clipboard.writeText(totpSetup.uri); toast(t("msg.copied"), "ok"); } };
$("#btn-totp-on").onclick = () => openTotpDialog("on");
$("#btn-totp-off").onclick = () => openTotpDialog("off");
$("#btn-lock-set").onclick = () => openLockDialog("set");
$("#btn-lock-off").onclick = () => openLockDialog("off");
$("#lock-timer").onchange = async (ev) => {
  try { await invoke("app_lock_timer", { minutes: Number(ev.target.value) || 0 }); lockInfo.auto_lock_minutes = Number(ev.target.value) || 0; toast(t("lock.saved"), "ok"); } catch (e) { fail(e); }
};
$("#set-skin").onchange = (ev) => setSkin(ev.target.value);
$("#set-theme-mode").onchange = (ev) => setTheme(ev.target.value || null);

/* ---------------- theme ---------------- */
function setTheme(mode) {
  if (mode) document.documentElement.dataset.theme = mode; else delete document.documentElement.dataset.theme;
  const sel = $("#set-theme-mode"); if (sel && sel.value !== (mode || "")) sel.value = mode || "";
  try { if (mode) localStorage.setItem("veranda.theme", mode); else localStorage.removeItem("veranda.theme"); } catch (e) {}
}
$("#theme-toggle").onclick = () => {
  const dark = document.documentElement.dataset.theme === "dark" || (!document.documentElement.dataset.theme && matchMedia("(prefers-color-scheme: dark)").matches);
  setTheme(dark ? "light" : "dark");
};

/* ---------------- state ---------------- */
let lastSnapshot = "", uiShown = false;
function uiReady() { if (uiShown) return; uiShown = true; invoke("ui_ready").catch(() => {}); }
const MODULE_TABS = { remote: ["hosts", "terminal", "sftp", "forward", "snippets", "keychain", "knownhosts", "history"], files: ["files"], move: [] };
function applyModules() {
  const m = state.config?.modules || {};
  $$("[data-module]").forEach((el) => el.classList.toggle("hidden", m[el.dataset.module] === false));
  for (const [mod, tabs] of Object.entries(MODULE_TABS)) if (m[mod] === false && tabs.includes(currentTab)) showTab("dashboard");
}
function renderAll() {
  applyModules();
  for (const [name, fn] of [["nav", renderNav], ["services", renderServices], ["service", renderService], ["sites", renderSites], ["database", renderDatabase], ["settings", renderSettings]]) {
    try { fn(); } catch (e) { fail(`${name}: ${e?.message || e}`); }
  }
}
async function refresh(force = false) {
  try {
    const next = await invoke("get_state");
    const snap = JSON.stringify(next);
    if (!force && snap === lastSnapshot) return;   // nothing changed: leave the DOM alone (no scroll jank)
    lastSnapshot = snap;
    state = next;
    lang = state.config.general.language || "en";
    try { localStorage.setItem("veranda.lang", lang); } catch (e) {}
    applyI18n();
    $("#version").textContent = `v${state.version}`;
    renderAll();
  } catch (e) { fail(e); }
  uiReady();
}
function showTab(id) {
  currentTab = id;
  $$(".nav-item").forEach((x) => x.classList.toggle("active", x.dataset.tab === id));
  $$(".tab").forEach((x) => x.classList.toggle("active", x.id === `tab-${id}`));
  $$(".svc-item").forEach((x) => x.classList.toggle("active", id === "service" && x.dataset.svc === currentKind));
  if (id === "logs") loadLogList();
  window.scrollTo(0, 0);
  document.dispatchEvent(new CustomEvent("veranda:tab", { detail: id }));
}

/* ---------------- helpers over state ---------------- */
const isHidden = (id) => (state.config.general.hidden_services || []).includes(id);
const installedVersions = (kind) => state.installed.filter((i) => i.kind === kind).map((i) => i.version).sort(cmpVer).reverse();
const externalVersions = (kind) => (state.external || []).filter((x) => x.kind === kind);
// The Node.js that goes first on the PATH: the configured choice, else the newest Veranda version.
const nodeDefault = () => {
  if (state.config.node?.default) return state.config.node.default;
  const own = installedVersions("node")[0]; if (own) return own;
  const ext = externalVersions("node").slice().sort((a, b) => cmpVer(b.version, a.version))[0];
  return ext ? `${ext.source}:${ext.version}` : "";
};
const svcStatus = (svc) => state.services.find((s) => s.id === svc);
function activeKind(svc) {
  return { web: state.config.web.server, db: state.config.db.engine, pg: "postgres", php: "php", mongodb: "mongodb" }[svc];
}
const isActive = (kind) => !!KIND_META[kind].svc && activeKind(KIND_META[kind].svc) === kind;
function kindOn(kind) {
  const m = KIND_META[kind];
  return !!m.svc && isActive(kind) && !!state.config[SVC_SECTION[m.svc]].enabled && !isHidden(m.svc);
}
const serviceName = (id) => (state?.services.find((s) => s.id === id) || {}).name || id;
function busyOf(svc) {
  const st = svcStatus(svc);
  if (!st) return null;
  if (busy[svc]) return busy[svc];
  if (busyAll && st.enabled && st.installed && (busyAll === "start" ? !st.running : st.running)) return busyAll;
  return null;
}
const busyText = (act) => t(act === "stop" ? "status.stopping" : act === "start" ? "status.starting" : "status.restarting");
function stateBadge(st, act) {
  if (act) return `<span class="state busy">${ICONS.spinner}${busyText(act)}</span>`;
  if (!st) return `<span class="state"><i></i>${t("svc.notSelected")}</span>`;
  if (st.running) return `<span class="state on"><i></i>${t("status.running")}</span>`;
  if (st.installed) return `<span class="state"><i></i>${t("status.stopped")}</span>`;
  return `<span class="state warn"><i></i>${t("pkg.notInstalled")}</span>`;
}

/* ---------------- sidebar ---------------- */
/* ---------------- sidebar search ---------------- */
// Filters the sidebar (menus, local tools, services) by the text typed in #nav-search.
// Matches the visible label and the service id (so "node", "apache", "mysql" all work);
// groups without a visible entry are hidden; Enter opens the first match; Esc clears.
function applyNavFilter() {
  const box = $("#nav-search");
  if (!box) return;
  const q = box.value.trim().toLowerCase();
  const nav = $("#nav");
  const items = $$(".nav-item, .svc-item", nav);
  let shown = 0;
  items.forEach((el) => {
    const label = (el.querySelector(".sname, span")?.textContent || el.textContent || "").toLowerCase();
    const key = (el.dataset.svc || el.dataset.tab || "").toLowerCase();
    const hit = !q || label.includes(q) || key.includes(q);
    el.classList.toggle("is-filtered-out", !hit);
    if (hit) shown++;
  });
  $$(".sgroup", nav).forEach((g) => {
    const any = $$(".nav-item, .svc-item", g).some((el) => !el.classList.contains("is-filtered-out"));
    g.classList.toggle("is-filtered-out", !!q && !any);
  });
  let empty = $(".nav-nomatch", nav);
  if (q && !shown) {
    if (!empty) { empty = document.createElement("div"); empty.className = "nav-nomatch"; nav.append(empty); }
    empty.textContent = t("nav.noMatch");
  } else if (empty) empty.remove();
}
function initNavSearch() {
  const box = $("#nav-search");
  if (!box || box.dataset.ready) return;
  box.dataset.ready = "1";
  box.addEventListener("input", applyNavFilter);
  box.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape") { box.value = ""; applyNavFilter(); box.blur(); }
    if (ev.key === "Enter") {
      const first = $$(".nav-item, .svc-item", $("#nav")).find((el) => !el.classList.contains("is-filtered-out") && el.offsetParent !== null);
      if (first) { first.click(); box.value = ""; applyNavFilter(); }
    }
  });
  document.addEventListener("keydown", (ev) => {
    const typing = /^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement?.tagName || "") || document.activeElement?.isContentEditable;
    if ((ev.ctrlKey || ev.metaKey) && ev.key.toLowerCase() === "k") { ev.preventDefault(); box.focus(); box.select(); }
    else if (ev.key === "/" && !typing) { ev.preventDefault(); box.focus(); }
  });
}
function renderNav() {
  initNavSearch();
  $("#svc-nav").innerHTML = GROUPS.map(([g, key]) => {
    const kinds = Object.keys(KIND_META).filter((k) => KIND_META[k].group === g);
    return `<div class="sgroup"><h4>${t(key)}</h4>${kinds.map((kind) => {
      const m = KIND_META[kind];
      const st = m.svc && isActive(kind) ? svcStatus(m.svc) : null;
      const act = st ? busyOf(m.svc) : null;
      const dot = act ? "busy" : st?.running ? "run" : installedVersions(kind).length || externalVersions(kind).length ? "inst" : "";
      const on = kindOn(kind);
      return `<div class="svc-item ${currentTab === "service" && currentKind === kind ? "active" : ""}" data-svc="${kind}" role="button" tabindex="0">
        <span class="sico">${kindIcon(kind)}</span><span class="sname">${m.label}</span><i class="sdot ${dot}" title="${st?.running ? t("status.running") : ""}"></i>
        ${m.svc ? `<input type="checkbox" class="switch mini" data-sw="${kind}" ${on ? "checked" : ""} title="${t("svc.enable")}">` : ""}
      </div>`;
    }).join("")}</div>`;
  }).join("");
  applyNavFilter();
}
async function toggleKind(kind, on) {
  const m = KIND_META[kind];
  if (!m.svc) return;
  const svc = m.svc, sec = SVC_SECTION[svc];
  const cfg = clone(state.config);
  const hidden = new Set(cfg.general.hidden_services || []);
  if (on) {
    // switching engines while the old one runs: stop it first so the status stays truthful
    if ((svc === "web" || svc === "db") && activeKind(svc) !== kind && svcStatus(svc)?.running) {
      busy[svc] = "stop"; renderAll();
      try { await invoke("service_action", { id: svc, action: "stop" }); } catch (e) { fail(e); }
      delete busy[svc];
    }
    if (svc === "web") cfg.web.server = kind;
    if (svc === "db") cfg.db.engine = kind;
    cfg[sec].enabled = true;
    hidden.delete(svc);
  } else {
    cfg[sec].enabled = false;
    hidden.add(svc);
  }
  cfg.general.hidden_services = [...hidden];
  if (!on && svcStatus(svc)?.running) {
    busy[svc] = "stop"; renderAll();
    try { await invoke("service_action", { id: svc, action: "stop" }); } catch (e) { fail(e); }
    delete busy[svc];
  }
  try { await invoke("save_config", { config: cfg }); } catch (e) { fail(e); }
  await refresh(true);
  if (on && svcStatus(svc)?.installed && !svcStatus(svc)?.running) await serviceAction(svc, "start");
}

/* ---------------- startup groups ---------------- */
let busyGroup = null;
function renderGroups() {
  const groups = state.config.general.startup_groups || [];
  const def = state.config.general.default_group || "";
  const chips = groups.map((g) => {
    const ids = g.services.filter((id) => svcStatus(id));
    const running = ids.filter((id) => svcStatus(id)?.running).length;
    const on = ids.length > 0 && running === ids.length;
    const busy = busyGroup === g.name;
    return `<div class="gchip ${on ? "on" : running ? "partial" : ""}" data-group="${esc(g.name)}">
      <span class="gname">${esc(g.name)}${g.name === def ? `<i title="${t("groups.default")}">${ICONS.play}</i>` : ""}</span>
      <span class="gsvc">${ids.map((id) => `<b class="${svcStatus(id)?.running ? "run" : ""}">${esc(serviceName(id))}</b>`).join("")}</span>
      <span class="gact">
        <button class="icon-btn small go" data-group-action="start" data-group="${esc(g.name)}" title="${t("btn.start")}" ${busy ? "disabled" : ""}>${busy ? ICONS.spinner : ICONS.play}</button>
        <button class="icon-btn small danger" data-group-action="stop" data-group="${esc(g.name)}" title="${t("btn.stop")}" ${busy ? "disabled" : ""}>${ICONS.stop}</button>
        <button class="icon-btn small" data-group-edit="${esc(g.name)}" title="${t("btn.edit")}">${ICONS.edit}</button>
      </span></div>`;
  }).join("");
  $("#groups").innerHTML = chips + `<button class="gchip add" id="btn-group-new">${ICONS.plus}<span>${t("groups.new")}</span></button>`;
}
function openGroupSvcDialog(g) {
  const f = $("#form-group-svc"); f.reset();
  f.elements.orig.value = g?.name || ""; f.elements.name.value = g?.name || "";
  f.elements.default.checked = !!g && state.config.general.default_group === g.name;
  const order = g?.services || [];
  const all = state.services.filter((s) => s.installed || order.includes(s.id));
  const sorted = [...all].sort((a, b) => { const ia = order.indexOf(a.id), ib = order.indexOf(b.id); return (ia < 0 ? 99 : ia) - (ib < 0 ? 99 : ib); });
  $("#group-svc-list").innerHTML = sorted.map((s) => `<label class="check"><input type="checkbox" name="svc" value="${esc(s.id)}" ${order.includes(s.id) ? "checked" : ""}><span>${esc(s.name)} <small class="mono">${s.ports.map((p) => ":" + p).join(" ")}</small></span></label>`).join("") || `<div class="empty small">${t("msg.installFirst")}</div>`;
  $("#btn-group-svc-delete").classList.toggle("hidden", !g);
  $("#dlg-group-svc").showModal(); f.elements.name.focus();
}
$("#form-group-svc").onsubmit = async (ev) => {
  ev.preventDefault(); const f = ev.target;
  const name = f.elements.name.value.trim(); if (!name) return;
  const services = $$("input[name=svc]:checked", f).map((i) => i.value);
  const cfg = clone(state.config);
  const groups = cfg.general.startup_groups || [];
  const orig = f.elements.orig.value;
  const idx = groups.findIndex((g) => g.name === orig);
  if (idx >= 0) groups[idx] = { name, services }; else groups.push({ name, services });
  cfg.general.startup_groups = groups;
  if (f.elements.default.checked) cfg.general.default_group = name; else if (cfg.general.default_group === orig || cfg.general.default_group === name) cfg.general.default_group = "";
  try { await invoke("save_config", { config: cfg }); $("#dlg-group-svc").close(); toast(t("msg.saved2"), "ok"); } catch (e) { fail(e); }
  refresh(true);
};
$("#btn-group-svc-delete").onclick = async () => {
  const f = $("#form-group-svc"); const orig = f.elements.orig.value;
  if (!orig || !(await askConfirm(t("groups.confirmDelete", { name: orig })))) return;
  const cfg = clone(state.config);
  cfg.general.startup_groups = (cfg.general.startup_groups || []).filter((g) => g.name !== orig);
  if (cfg.general.default_group === orig) cfg.general.default_group = "";
  try { await invoke("save_config", { config: cfg }); $("#dlg-group-svc").close(); } catch (e) { fail(e); }
  refresh(true);
};
async function runGroup(name, action) {
  if (busyGroup) return;
  busyGroup = name; renderGroups();
  let results = null;
  try { results = await invoke("group_action", { name, action }); } catch (e) { fail(e); }
  busyGroup = null;
  await refresh(true);
  if (results) await reportResults(results, action);
}

/* ---------------- overview ---------------- */
function renderServices() {
  renderGroups();
  const visible = state.services.filter((s) => !isHidden(s.id) && (s.enabled || s.running));
  const enabled = visible.filter((s) => s.enabled && s.installed);
  const running = enabled.filter((s) => s.running).length;
  const pill = $("#global-status");
  pill.classList.toggle("on", !busyAll && running > 0 && running === enabled.length);
  pill.classList.toggle("partial", !busyAll && running > 0 && running < enabled.length);
  pill.classList.toggle("busy", !!busyAll);
  $(".txt", pill).textContent = busyAll ? busyText(busyAll)
    : running === 0 ? t("status.stopped") : running === enabled.length ? t("status.running") : t("status.partial");
  for (const [id, kind] of [["#btn-start-all", "start"], ["#btn-stop-all", "stop"]]) {
    const b = $(id);
    b.disabled = !!busyAll;
    b.classList.toggle("busy", busyAll === kind);
  }
  $("#hosts-notice").classList.toggle("hidden", state.hosts_in_sync);
  $$("[data-open='adminer']").forEach((b) => b.classList.toggle("hidden", isHidden("db")));
  if (!visible.length) { $("#services").innerHTML = `<div class="empty">${ICONS.info}<span>${t("dash.empty")}</span></div>`; return; }
  $("#services").innerHTML = visible.map((s) => {
    const kind = activeKind(s.id);
    const versions = installedVersions(kind);
    const act = busyOf(s.id);
    const cls = act ? "busy" : s.running ? "on" : s.installed ? "off" : "missing";
    const version = versions.length > 1
      ? `<span class="pill"><select data-version-kind="${kind}" data-service="${s.id}">${versions.map((v) => `<option ${v === s.version ? "selected" : ""}>${esc(v)}</option>`).join("")}</select></span>`
      : s.installed ? `<span class="pill">${esc(s.version)}</span>`
      : installing && installing.kind === kind ? `<span data-dash="${kind}">${progressMarkup(installing)}</span>`
      : `<button class="btn small" data-install="${kind}" data-latest="1">${t("btn.install")}</button>`;
    const ports = s.ports.map((p) => `<span class="pill">:${p}</span>`).join("");
    const dis = act ? "disabled" : "";
    const action = s.running
      ? `<button class="icon-btn" data-action="${s.id === "web" ? "reload" : "restart"}" data-id="${s.id}" title="${s.id === "web" ? t("btn.reload") : t("btn.restart")}" ${dis}>${ICONS.refresh}</button>
         <button class="icon-btn danger" data-action="stop" data-id="${s.id}" title="${t("btn.stop")}" ${dis}>${ICONS.stop}</button>`
      : `<button class="icon-btn go" data-action="start" data-id="${s.id}" title="${t("btn.start")}" ${s.installed && !act ? "" : "disabled"}>${ICONS.play}</button>`;
    return `<div class="row ${cls}">
      <div class="ico">${kindIcon(kind)}</div>
      <div><div class="name"><button class="linkish" data-svc="${kind}">${esc(KIND_META[kind].label)}</button> ${version}</div><div class="meta">${stateBadge(s, act)}${ports}${s.message && s.installed ? `<span>${esc(s.message)}</span>` : ""}</div></div>
      <div class="act">${action}</div>
    </div>`;
  }).join("");
}

async function serviceAction(id, action) {
  if (busy[id] || busyAll) return;
  busy[id] = action; renderAll();
  let results = null;
  try { results = await invoke("service_action", { id, action }); } catch (e) { fail(e); }
  delete busy[id];
  await refresh(true);
  if (results) await reportResults(results, action);
}
async function runAll(action) {
  if (busyAll) return;
  busyAll = action; renderAll();
  let results = null;
  try { results = await invoke(action === "stop" ? "stop_all" : "start_all"); } catch (e) { fail(e); }
  busyAll = null;
  await refresh(true);
  if (results) await reportResults(results, action);
}
async function reportResults(results, action) {
  const bad = results.filter((r) => !r.ok);
  const verb = action === "stop" ? "stopped" : action === "reload" ? "reloaded" : action === "restart" ? "restarted" : "started";
  if (!bad.length) {
    if (!results.length) { toast(t("msg.nothingToDo"), "info"); return; }
    toast(results.length > 1 ? t(`msg.all.${verb}`) : t(`msg.one.${verb}`, { name: serviceName(results[0].id) }), "ok");
    return;
  }
  bad.forEach((r) => logActivity(`${serviceName(r.id)}: ${r.error}`, "bad"));
  const perm = bad.find((r) => r.port_permission);
  const conflict = bad.find((r) => r.port_in_use && r !== perm);
  const rest = bad.filter((r) => r !== perm && r !== conflict);
  if (rest.length) {
    const allMissing = rest.every((r) => /not installed/i.test(r.error));
    const head = t(action === "stop" ? "msg.stopFailed" : "msg.startFailed");
    toast(allMissing ? t("msg.installFirst") : `${head}:\n${rest.map((r) => `${serviceName(r.id)}: ${r.error.split("\n")[0]}`).join("\n")}`, "bad");
  }
  if (perm) {
    $("#confirm-title").textContent = t("ports.title", { port: perm.port_permission });
    const ok = await askConfirm(t("ports.body"), t("btn.allow"));
    $("#confirm-title").textContent = t("confirm.title");
    if (!ok) return;
    try { await invoke("grant_web_ports"); toast(t("msg.portsGranted"), "ok"); } catch (e) { fail(e); return; }
    await serviceAction(perm.id, "start");
  }
  if (conflict) await offerPortMove(conflict);
}
// Every port a service listens on, as [section, key] in the config.
const PORT_FIELDS = { web: [["web", "http_port"], ["web", "https_port"]], db: [["db", "port"]], pg: [["pg", "port"]], php: [["php", "fcgi_port"]], mongodb: [["mongodb", "port"]] };
async function offerPortMove(r) {
  const c = r.port_in_use, name = serviceName(r.id);
  const DEFAULT_PORT = { mongodb: 27017 };
  const field = (PORT_FIELDS[r.id] || []).find(([sec, key]) => (state.config[sec][key] || DEFAULT_PORT[sec] || 0) === c.port);
  if (!field) { toast(`${name}: ${r.error.split("\n")[0]}`, "bad"); return; }
  $("#confirm-title").textContent = t("ports.busyTitle", { port: c.port });
  const ok = await askConfirm(t("ports.busyBody", { owner: c.owner || "?", name, new: c.suggestion }), t("btn.usePort", { new: c.suggestion }));
  $("#confirm-title").textContent = t("confirm.title");
  if (!ok) return;
  const cfg = clone(state.config);
  cfg[field[0]][field[1]] = c.suggestion;
  try { await invoke("save_config", { config: cfg }); toast(t("msg.portMoved", { name, new: c.suggestion }), "ok"); } catch (e) { fail(e); return; }
  await refresh(true);
  await serviceAction(r.id, "start");
}

/* ---------------- service / tool page ---------------- */
function openService(kind) {
  if (!KIND_META[kind]) return;
  currentKind = kind;
  if (!KIND_META[kind].svc) svcTab = "versions";
  renderService();
  showTab("service");
  if (!upstream && !upstreamLoading) loadUpstream();
}
function versionsFor(kind) {
  const ups = upstream ? upstream.filter((p) => p.kind === kind) : [];
  const built = builtin ? builtin.filter((p) => p.kind === kind) : [];
  const inst = installedVersions(kind);
  const ext = externalVersions(kind);
  const all = [...new Set([...ups.map((p) => p.version), ...built.map((p) => p.version), ...inst, ...ext.map((x) => x.version)])].sort(cmpVer).reverse();
  const source = ups[0]?.source || built[0]?.source || "";
  const newest = [...new Set([...ups.map((p) => p.version), ...built.map((p) => p.version)])].sort(cmpVer).reverse()[0] || all[0] || "";
  return { all, inst, ext, source, newest };
}
function renderService() {
  const box = $("#service-page");
  if (!currentKind || !state) { box.innerHTML = ""; return; }
  const kind = currentKind, m = KIND_META[kind];
  const st = m.svc && isActive(kind) ? svcStatus(m.svc) : null;
  const act = st ? busyOf(m.svc) : null;
  const on = kindOn(kind);
  const { all, inst, ext, source, newest } = versionsFor(kind);
  const headCls = act ? "busy" : st?.running ? "on" : "";

  // header actions: enable switch + start/stop for services, Open for web tools
  let actions = "";
  if (m.svc) {
    actions += `<label class="sw-label"><span>${t("svc.enable")}</span><input type="checkbox" class="switch" data-sw="${kind}" ${on ? "checked" : ""}></label>`;
    if (st && st.installed) {
      const dis = act ? "disabled" : "";
      actions += st.running
        ? `<button class="btn" data-action="${m.svc === "web" ? "reload" : "restart"}" data-id="${m.svc}" ${dis}>${ICONS.refresh}<span>${m.svc === "web" ? t("btn.reload") : t("btn.restart")}</span></button>
           <button class="btn danger" data-action="stop" data-id="${m.svc}" ${dis}>${ICONS.stop}<span>${t("btn.stop")}</span></button>`
        : `<button class="btn primary" data-action="start" data-id="${m.svc}" ${dis || !on ? "disabled" : ""}>${ICONS.play}<span>${t("btn.start")}</span></button>`;
    }
  }
  if (m.open) actions += `<button class="btn" data-open="${m.open}" ${inst.length ? "" : "disabled"}>${ICONS.open}<span>${t("btn.open")}</span></button>`;

  const tabs = m.svc
    ? `<div class="tabs"><button class="tab-btn ${svcTab === "service" ? "active" : ""}" data-svctab="service">${t("svc.tab.service")}</button><button class="tab-btn ${svcTab === "versions" ? "active" : ""}" data-svctab="versions">${t("svc.tab.versions")} <span class="count">${all.length}</span></button></div>`
    : "";

  let body = "";
  if (m.svc && svcTab === "service") {
    let notice = "";
    if (!isActive(kind)) notice = `<div class="notice">${ICONS.info}<span>${t("svc.offAlt", { name: m.label, role: t(SVC_ROLE[m.svc] || "role.service") })}</span></div>`;
    else if (!on) notice = `<div class="notice">${ICONS.info}<span>${t("svc.offHint")}</span></div>`;
    else if (st && !st.installed) notice = `<div class="notice">${ICONS.info}<span>${t("svc.installHint")}</span></div>`;
    const ver = st
      ? inst.length > 1
        ? `<select class="small" data-version-kind="${kind}" data-service="${m.svc}">${inst.map((v) => `<option ${v === st.version ? "selected" : ""}>${esc(v)}</option>`).join("")}</select>`
        : st.installed ? `<code>${esc(st.version)}</code>` : `<span class="state warn"><i></i>${t("pkg.notInstalled")}</span>`
      : `<code>${esc(inst[0] || "—")}</code>`;
    const ports = st ? st.ports.map((p) => `<span class="pill">:${p}</span>`).join("") : "—";
    const pids = st && st.pids.length ? `<code>${st.pids.join(", ")}</code>` : "—";
    const quick = [];
    if (m.svc === "web") quick.push(`<button class="btn small" data-open="web">${ICONS.open}<span>${t("btn.openWeb")}</span></button>`);
    if (m.svc === "db") { quick.push(`<button class="btn small" data-open="adminer">${ICONS.open}<span>Adminer</span></button>`); quick.push(`<button class="btn small" data-tab="database">${ICONS.db}<span>${t("nav.database")}</span></button>`); }
    if (m.svc === "pg") quick.push(`<button class="btn small" data-open="adminer-pg">${ICONS.open}<span>Adminer (PostgreSQL)</span></button>`);
    if (m.open && m.svc) quick.unshift(`<button class="btn small" data-open="${m.open}" ${st?.running ? "" : "disabled"}>${ICONS.open}<span>${t("btn.openUi")}</span></button>`);
    if (m.svc === "php") quick.push(`<button class="btn small" data-open="phpini">${ICONS.edit}<span>php.ini</span></button>`);
    quick.push(`<button class="btn small" data-open="config">${ICONS.file}<span>${t("btn.configFile")}</span></button>`);
    quick.push(`<button class="btn small" data-open="logs">${ICONS.folder}<span>${t("btn.logsFolder")}</span></button>`);
    quick.push(`<button class="btn small" data-tab="logs">${ICONS.file}<span>${t("nav.logs")}</span></button>`);
    body = `${notice}<div class="svc-cards">
      <section class="card"><h2>${t("svc.tab.service")}</h2>
        <div class="kv"><span>${t("svc.state")}</span>${stateBadge(st, act)}</div>
        <div class="kv"><span>${t("svc.versionInUse")}</span>${ver}</div>
        <div class="kv"><span>${t("svc.ports")}</span>${ports}</div>
        <div class="kv"><span>${t("svc.pids")}</span>${pids}</div>
        ${m.svc === "pg" ? `<div class="kv"><span>${t("db.user")}</span><code>postgres</code></div><div class="kv"><span>${t("db.password")}</span><code id="pg-password" class="secret">••••••••••••</code><button type="button" class="icon-btn small" id="btn-pg-show" title="${t("db.password")}">${ICONS.open}</button></div>` : ""}
        ${st?.message && st.installed ? `<div class="kv"><span></span><span class="sub" style="margin:0">${esc(st.message)}</span></div>` : ""}
      </section>
      <section class="card"><h2>${t("svc.files")}</h2><div class="btn-row">${quick.join("")}</div></section>
    </div>`;
  } else {
    const rows = all.map((v) => {
      const isInst = inst.includes(v);
      const x = ext.find((e) => e.version === v);
      if (x && !isInst) {
        const isRemovingExt = removing && removing.kind === kind && removing.version === v;
        const removeBtn = x.source === "nvm"
          ? isRemovingExt ? `<span class="state busy">${ICONS.spinner}${t("pkg.removing")}</span>`
            : `<button class="btn small subtle" data-remove-ext="${kind}" data-version="${esc(v)}" data-source="${esc(x.source)}" ${installing || removing ? "disabled" : ""}>${ICONS.trash}<span>${t("btn.removeNvm")}</span></button>`
          : "";
        const isDef = kind === "node" && nodeDefault() === `${x.source}:${v}`;
        const defBtn = kind !== "node" ? "" : isDef ? "" : `<button class="btn small" data-node-default="${esc(x.source)}:${esc(v)}" title="${t("ver.defaultHint")}">${ICONS.check}<span>${t("btn.setDefault")}</span></button>`;
        return `<div class="vrow ${isRemovingExt ? "removing" : ""} ${isDef ? "inuse" : ""}" data-row="${kind}:${esc(v)}"><span class="v">${esc(v)}</span><span class="src">${esc(x.bin_dir)}</span><span>${isDef ? `<span class="state on"><i></i>${t("ver.default")}</span>` : `<span class="pill ok">${esc(x.source)}</span>`}</span>
          <span class="act">${defBtn}<button class="icon-btn small" data-open="path:${esc(x.bin_dir)}" title="${esc(x.bin_dir)}">${ICONS.folder}</button>${removeBtn}</span></div>`;
      }
      const inUse = !!st && st.installed && st.version === v;
      const isInstalling = installing && installing.kind === kind && installing.version === v;
      const isRemoving = removing && removing.kind === kind && removing.version === v;
      const isNodeDef = kind === "node" && isInst && nodeDefault() === v;
      const status = inUse ? `<span class="state on"><i></i>${t("ver.inUse")}</span>` : isNodeDef ? `<span class="state on"><i></i>${t("ver.default")}</span>` : isInst ? `<span class="pill ok">${t("ver.installed")}</span>` : v === newest ? `<span class="pill accent">${t("ver.newest")}</span>` : "";
      const nodeDefBtn = kind === "node" && isInst && !isNodeDef && !isInstalling && !isRemoving ? `<button class="btn small" data-node-default="${esc(v)}" title="${t("ver.defaultHint")}">${ICONS.check}<span>${t("btn.setDefault")}</span></button>` : "";
      const action = isInstalling ? progressMarkup(installing)
        : isRemoving ? `<span class="state busy">${ICONS.spinner}${t("pkg.removing")}</span>`
        : isInst ? `<button class="btn small subtle" data-remove="${kind}" data-version="${esc(v)}" ${installing || removing ? "disabled" : ""}>${ICONS.trash}<span>${t("btn.remove")}</span></button>`
        : `<button class="btn small primary" data-install="${kind}" data-version="${esc(v)}" ${installing || removing ? "disabled" : ""}>${kind === "node" && state.nvm_available ? t("btn.installNvm") : t("btn.install")}</button>`;
      return `<div class="vrow ${inUse ? "inuse" : ""} ${isInstalling ? "installing" : ""} ${isRemoving ? "removing" : ""} ${upstreamFresh && !isInst ? "fresh" : ""}" data-row="${kind}:${esc(v)}">
        <span class="v">${esc(v)}</span><span class="src">${esc(source)}</span><span>${status}</span><span class="act">${nodeDefBtn}${action}</span></div>`;
    }).join("");
    const skeleton = upstreamLoading
      ? [0, 1, 2].map(() => `<div class="vrow skeleton"><span class="sk w1"></span><span class="sk w2"></span><span class="sk w3"></span><span class="act"><span class="sk w4"></span></span></div>`).join("")
      : "";
    body = `${kind === "node" && state.nvm_available ? `<div class="notice">${ICONS.info}<span>${t("ver.nvmInstall")}</span></div>` : ""}${ext.length ? `<div class="notice">${ICONS.info}<span>${t("ver.external", { n: ext.length, source: [...new Set(ext.map((e) => e.source))].join(", ") })}</span></div>` : ""}
      <div class="vhead ${upstreamLoading ? "loading" : ""}"><span>${upstreamLoading ? `${ICONS.spinner}${t("ver.fetching", { source: source || "—" })}` : t("ver.hint", { source: source || "—" })}</span>
        <button class="btn small ${upstreamLoading ? "busy" : ""}" id="btn-check-upstream" ${upstreamLoading ? "disabled" : ""}>${ICONS.refresh}<span>${t("btn.refresh")}</span></button></div>
      <div class="vtable ${upstreamLoading ? "loading" : ""}">${upstreamLoading ? `<div class="loadbar"><div class="fill"></div></div>` : ""}<div class="vrow head"><span>${t("ver.version")}</span><span class="src">${t("ver.source")}</span><span>${t("ver.status")}</span><span class="act">${t("ver.action")}</span></div>
      ${rows}${skeleton}${!rows && !upstreamLoading ? `<div class="vrow"><span class="sub" style="margin:0">${t("ver.empty")}</span></div>` : ""}</div>`;
  }

  box.innerHTML = `<header class="top svc-head ${headCls}">
      <div class="ico">${kindIcon(kind)}</div>
      <div class="titles"><h1>${esc(m.label)}</h1><div class="meta">${m.svc ? stateBadge(st, act) : inst.length ? `<span class="pill ok">${t("ver.installed")} ${esc(inst[0])}</span>` : ext.length ? `<span class="pill ok">${esc(ext[0].source)} ${esc(ext[0].version)}</span>` : `<span class="state"><i></i>${t("pkg.notInstalled")}</span>`}${source ? `<span class="pill">${esc(source)}</span>` : ""}</div></div>
      <div class="spacer"></div>
      <div class="acts">${actions}</div>
    </header>${tabs}${body}`;
}
let upstreamFresh = false;   // true right after a fetch, so new rows fade in once
async function loadUpstream() {
  upstreamLoading = true; renderService();
  try { upstream = await invoke("upstream_versions"); upstreamFresh = true; } catch (e) { fail(e); }
  upstreamLoading = false;
  renderService();
  upstreamFresh = false;
}

/* ---------------- install / remove ---------------- */
function progressText(p) {
  if (p.note) return p.note.length > 48 ? `…${p.note.slice(-46)}` : p.note;
  if (p.phase === "extract") return t("pkg.extracting");
  const mb = (p.downloaded / 1048576).toFixed(1);
  return p.total ? `${(100 * p.downloaded / p.total).toFixed(0)}% · ${mb}/${(p.total / 1048576).toFixed(1)} MiB` : `${mb} MiB`;
}
function progressMarkup(p) {
  const pct = p.phase === "extract" ? 100 : p.total ? (100 * p.downloaded / p.total) : 0;
  const cancel = p.phase === "download" && !p.note && !p.cancelling
    ? `<button class="icon-btn small" data-cancel-install="1" title="${t("btn.cancel")}">${ICONS.x}</button>` : "";
  return `<div class="row-progress"><div class="bar"><div class="fill ${p.phase === "extract" ? "indeterminate" : ""}" style="width:${pct.toFixed(1)}%"></div></div><span class="txt">${p.cancelling ? t("pkg.cancelling") : progressText(p)}</span>${cancel}</div>`;
}
function paintProgress() {
  if (!installing) return;
  const html = progressMarkup(installing);
  const cell = $(`[data-row="${installing.kind}:${installing.version}"] .act`);
  if (cell) cell.innerHTML = html;
  const dash = $(`[data-dash="${installing.kind}"]`);
  if (dash) dash.innerHTML = html;
}
async function installPackage(kind, latest, version = null) {
  if (installing) { toast(t("pkg.installing")); return; }
  const v = version || versionsFor(kind).newest || "";
  installing = { kind, version: v, downloaded: 0, total: null, phase: "download" };
  renderAll(); paintProgress();
  try {
    const i = await invoke("install_package", { kind, version: version || null, latest: !!latest || !version });
    toast(t("msg.installed", { name: KIND_META[i.kind]?.label || i.kind, version: i.version }), "ok");
  } catch (e) {
    if (/download cancelled/i.test(String(e?.message || e))) toast(t("msg.cancelled", { name: KIND_META[kind]?.label || kind }), "info"); else fail(e);
  }
  installing = null;
  await refresh(true);
}
async function removePackage(kind, version) {
  const name = KIND_META[kind]?.label || kind;
  if (!(await askConfirm(t("msg.confirmRemove", { name, version })))) return;
  removing = { kind, version }; renderAll();
  try { await invoke("remove_package", { kind, version }); toast(t("msg.removed", { name, version }), "ok"); } catch (e) { fail(e); }
  removing = null;
  await refresh(true);
}
listen("install-progress", (ev) => {
  const p = ev.payload;
  if (!installing || installing.kind !== p.kind) return;
  installing.downloaded = p.downloaded; installing.total = p.total ?? installing.total; installing.note = p.note || null;
  installing.phase = p.note ? "extract" : installing.total && installing.downloaded >= installing.total ? "extract" : "download";
  paintProgress();
});
listen("services-changed", () => refresh());
listen("autostart-done", async (ev) => { await refresh(true); await reportResults(ev.payload, "start"); });
listen("backup-done", (ev) => toast(t("msg.backup", { path: ev.payload }), "ok"));
listen("service-reloaded", (ev) => toast(t("msg.reloaded", ev.payload), "ok"));

/* ---------------- close behaviour ---------------- */
listen("close-requested", () => {
  const dlg = $("#dlg-close");
  $("#form-close").elements.remember.checked = false;
  if (!dlg.open) dlg.showModal();
});
async function closeWith(action) {
  const remember = $("#form-close").elements.remember.checked;
  $("#dlg-close").close();
  try { await invoke("close_decision", { action, remember }); } catch (e) { fail(e); }
  if (remember) refresh(true);
}
$("#btn-close-tray").onclick = () => closeWith("tray");
$("#btn-close-quit").onclick = () => closeWith("quit");
$("#dlg-close").oncancel = (ev) => { ev.preventDefault(); $("#dlg-close").close(); };

/* ---------------- sites ---------------- */
function renderSites() {
  const root = state.config.general.document_root || `${state.root}/www`;
  $("#sites-hint").textContent = t("sites.hint", { root, tld: state.config.general.tld });
  $("#sites").innerHTML = state.sites.map((s) => `<div class="row on">
    <div class="ico">${ICONS.site}</div>
    <div><div class="name">${esc(s.hostname)}${s.auto ? "" : ' <span class="pill">manual</span>'}</div><div class="meta"><span class="mono">${esc(s.root)}</span></div></div>
    <div class="act">
      <button class="icon-btn" data-open="site:${esc(s.hostname)}" title="${t("btn.open")}">${ICONS.open}</button>
      ${state.config.modules?.files === false ? "" : `<button class="icon-btn" data-files="${esc(s.path)}" title="${t("files.editFiles")}">${ICONS.edit}</button>`}
      <button class="icon-btn" data-open="path:${esc(s.path)}" title="${t("quick.root")}">${ICONS.folder}</button>
      <button class="icon-btn" data-open="edit:${esc(s.path)}" title="${t("settings.editor")}">${ICONS.edit}</button>
      ${s.auto ? `<button class="icon-btn danger" data-delete-project="${esc(s.name)}" title="${t("btn.remove")}">${ICONS.trash}</button>` : `<button class="icon-btn danger" data-remove-site="${esc(s.hostname)}" title="${t("btn.remove")}">${ICONS.x}</button>`}
    </div></div>`).join("") || `<div class="empty">${ICONS.site}<span>${esc(root)}</span></div>`;
}
async function syncSites() {
  try { const r = await invoke("sync_sites"); if (r.missing_hosts.length) await invoke("hosts_sync"); toast(t("msg.synced"), "ok"); } catch (e) { fail(e); }
  refresh();
}

/* ---------------- migrate from another stack ---------------- */
let migrateSources = [];
async function openMigrate() {
  const dlg = $("#dlg-migrate"), f = $("#form-migrate");
  try { migrateSources = await invoke("migrate_detect"); } catch (e) { fail(e); return; }
  const sel = $("#migrate-platform");
  sel.innerHTML = migrateSources.map((s) => `<option value="${s.platform}">${esc(s.label)}${s.found ? " ✓" : s.installed_at ? " ·" : ""}</option>`).join("");
  const first = migrateSources.find((s) => s.found) || migrateSources.find((s) => s.installed_at) || migrateSources[0];
  sel.value = first.platform;
  $("#migrate-report").classList.add("hidden"); $("#migrate-report").innerHTML = "";
  $("#migrate-progress").classList.add("hidden");
  $("#btn-migrate-run").disabled = false;
  f.elements.databases.checked = false; $("#migrate-db").classList.add("hidden");
  applyMigrateSource();
  // a migration started earlier (dialog closed meanwhile, or the page reloaded) is shown as is
  try { const job = await invoke("migrate_status"); if (job.running) { migrateJob = job; showMigrateProgress(job.progress); $("#btn-migrate-run").disabled = true; } else if (job.report || job.error) showMigrateResult(job); } catch (e) {}
  dlg.showModal();
}
async function migrateControl(action) {
  try { migrateJob = await invoke("migrate_control", { action }); const st = await invoke("migrate_status"); showMigrateProgress(st.progress); } catch (e) { fail(e); }
}
$("#btn-migrate-pause").onclick = () => migrateControl(migrateJob.paused ? "resume" : "pause");
$("#btn-migrate-cancel-run").onclick = () => migrateControl(migrateJob.cancel >= 1 ? "stop" : "cancel");
const fmtMB = (b) => b >= 1024 * 1024 * 1024 ? `${(b / 1024 / 1024 / 1024).toFixed(2)} GB` : b >= 1024 * 1024 ? `${(b / 1024 / 1024).toFixed(1)} MB` : `${Math.round(b / 1024)} KB`;
let migrateJob = { paused: false, cancel: 0 };
function showMigrateProgress(pr) {
  const box = $("#migrate-progress"); box.classList.remove("hidden");
  $(".fill", box).style.width = `${pr.percent || 0}%`;
  $(".fill", box).classList.toggle("indeterminate", !pr.total && !migrateJob.paused);
  $(".pct", box).textContent = pr.total ? `${pr.percent || 0}%` : "";
  const state = migrateJob.cancel >= 2 ? t("migrate.stopping") : migrateJob.cancel === 1 ? t("migrate.cancelling") : migrateJob.paused ? t("migrate.paused", { pct: pr.percent || 0 }) : "";
  $(".txt", box).textContent = state || pr.message || t("migrate.working");
  $(".detail", box).textContent = pr.total ? `· ${pr.done}/${pr.total} · ${fmtMB(pr.bytes || 0)}` : "";
  box.classList.toggle("paused", !!migrateJob.paused);
  $("#btn-migrate-pause span").textContent = t(migrateJob.paused ? "migrate.resume" : "migrate.pause");
  $("#btn-migrate-pause .ic").innerHTML = ICONS[migrateJob.paused ? "play" : "pause"] || "";
  $("#btn-migrate-cancel-run span").textContent = t(migrateJob.cancel >= 1 ? "migrate.stopNow" : "migrate.cancel");
  $("#btn-migrate-cancel-run").disabled = migrateJob.cancel >= 2;
  const b = $("#btn-migrate"); b.classList.add("busy"); $(".ic", b).innerHTML = ICONS.spinner;
  $("span", b).textContent = migrateJob.paused ? t("migrate.paused", { pct: pr.percent || 0 }) : pr.total ? t("migrate.running", { pct: pr.percent || 0 }) : t("migrate.working");
}
async function showMigrateResult(job) {
  migrateJob = { paused: false, cancel: 0 };
  $("#migrate-progress").classList.add("hidden");
  const b = $("#btn-migrate"); b.classList.remove("busy"); $(".ic", b).innerHTML = ICONS.migrate; $("span", b).textContent = t("btn.migrate");
  $("#btn-migrate-run").disabled = false;
  const r = job.report;
  const line = (cls, label, items) => items.length ? `<div class="rep ${cls}"><b>${label} (${items.length})</b><ul>${items.map((x) => `<li>${esc(x)}</li>`).join("")}</ul></div>` : "";
  $("#migrate-report").innerHTML = job.error ? `<div class="rep bad"><b>${esc(job.error)}</b></div>`
    : (line("ok", t("migrate.sites"), r.sites) + line("ok", t("migrate.databases"), r.databases) + line("", t("migrate.skipped"), r.skipped) + line("bad", t("migrate.errors"), r.errors) || `<div class="rep">${t("migrate.nothing")}</div>`);
  $("#migrate-report").classList.remove("hidden");
}
function applyMigrateSource() {
  const f = $("#form-migrate");
  const s = migrateSources.find((x) => x.platform === f.elements.platform.value);
  if (!s) return;
  const stored = s.platform.startsWith("veranda:");
  f.elements.path.value = s.path || s.installed_at || "";
  f.elements.path.placeholder = s.platform === "flyenv" ? "…/FlyEnv  (folder, host.json or vhost)" : "/path/to/www";
  f.elements.path.required = !stored;
  // Veranda's own stored engine: no folder, no sites, databases only. The temporary server is
  // started by the backend, so host/port/user are fixed; only an old root password can be given.
  f.elements.path.closest("label").classList.toggle("hidden", stored);
  $(".radio-row", f).classList.toggle("hidden", stored);
  f.elements.databases.closest("label").classList.toggle("hidden", stored);
  $$("#migrate-db .two")[0].classList.toggle("hidden", stored);
  f.elements.db_user.closest("label").classList.toggle("hidden", stored);
  if (stored) { f.elements.databases.checked = true; $("#migrate-db").classList.remove("hidden"); }
  else { $("#migrate-db").classList.toggle("hidden", !f.elements.databases.checked); }
  $("#migrate-detected").textContent = stored
    ? (s.found ? t("migrate.storedHint") : t("migrate.storedNotInstalled"))
    : s.found ? t("migrate.found", { path: s.path }) : s.installed_at ? t("migrate.installedEmpty", { path: s.installed_at }) : t("migrate.notFound");
  f.elements.db_port.value = s.db_port; f.elements.db_user.value = s.db_user; f.elements.db_password.value = s.db_password;
  f.elements.db_password.placeholder = stored ? t("migrate.storedPassword") : "";
}
$("#btn-migrate").onclick = openMigrate;
$("#migrate-platform").onchange = applyMigrateSource;
$("#btn-migrate-cancel").onclick = () => $("#dlg-migrate").close();
$("#form-migrate").elements.databases.onchange = (ev) => $("#migrate-db").classList.toggle("hidden", !ev.target.checked);
$("#btn-migrate-browse").onclick = async () => {
  const f = $("#form-migrate");
  try {
    const picked = await window.__TAURI__.dialog.open({ directory: true, multiple: false, title: t("migrate.folder") });
    if (picked) f.elements.path.value = typeof picked === "string" ? picked : picked.path || "";
  } catch (e) { toast(t("migrate.noPicker"), "info"); }
};
listen("migrate-progress", (ev) => showMigrateProgress(ev.payload));
listen("migrate-done", async (ev) => {
  const job = ev.payload, r = job.report;
  await showMigrateResult(job);
  if (r) {
    if (r.sites.length) { try { const s = await invoke("sync_sites"); if (s.missing_hosts.length) await invoke("hosts_sync"); } catch (e) { fail(e); } }
    toast(t("migrate.done", { sites: r.sites.length, dbs: r.databases.length }), r.errors.length ? "info" : "ok");
  } else fail(job.error);
  refresh(true);
});
$("#form-migrate").onsubmit = async (ev) => {
  ev.preventDefault(); const f = ev.target;
  const req = {
    platform: f.elements.platform.value, path: f.elements.path.value.trim(), mode: f.elements.mode.value,
    databases: f.elements.databases.checked, db_host: f.elements.db_host.value.trim(), db_port: Number(f.elements.db_port.value) || 3306,
    db_user: f.elements.db_user.value.trim(), db_password: f.elements.db_password.value,
  };
  const btn = $("#btn-migrate-run"); btn.disabled = true;
  $("#migrate-report").classList.add("hidden");
  migrateJob = { paused: false, cancel: 0 };
  showMigrateProgress({ message: t("migrate.working"), total: 0, percent: 0 });
  // returns at once; progress and the result arrive as events (and survive closing the dialog)
  try { await invoke("migrate_run", { req }); } catch (e) { fail(e); $("#migrate-progress").classList.add("hidden"); btn.disabled = false; $("#btn-migrate").classList.remove("busy"); $("#btn-migrate .ic").innerHTML = ICONS.migrate; $("#btn-migrate span").textContent = t("btn.migrate"); }
};

/* ---------------- storage ---------------- */
async function renderStorage() {
  const box = $("#storage-usage"); if (!box) return;
  try {
    const u = await invoke("storage_usage");
    const total = u.reduce((a, x) => a + x.bytes, 0);
    box.innerHTML = u.filter((x) => x.bytes > 0 || x.id === "packages").map((x) => `<span class="pill"><b>${t(`storage.${x.id}`)}</b>${fmtBytes(x.bytes)}</span>`).join("") + `<span class="pill accent"><b>${t("storage.total")}</b>${fmtBytes(total)}</span>`;
  } catch (e) { box.innerHTML = ""; }
}
listen("clean-progress", (ev) => { $("#free-space-status").textContent = ev.payload; });
$("#btn-free-space").onclick = async () => {
  const b = $("#btn-free-space"); b.disabled = true; $("#free-space-report").classList.add("hidden");
  try {
    const r = await invoke("free_space");
    $("#free-space-status").textContent = "";
    $("#free-space-report").innerHTML = `<div class="rep ok"><b>${esc(t("storage.freed", { size: fmtBytes(r.freed) }))}</b><ul>${r.actions.map((a) => `<li>${esc(a)}</li>`).join("")}</ul></div>`;
    $("#free-space-report").classList.remove("hidden");
    toast(t("storage.freed", { size: fmtBytes(r.freed) }), "ok");
    renderStorage();
  } catch (e) { fail(e); $("#free-space-status").textContent = ""; }
  b.disabled = false;
};
document.addEventListener("veranda:tab", (ev) => { if (ev.detail === "settings") renderStorage(); });

/* ---------------- move to another computer ---------------- */
const MOVE_SECTIONS = ["veranda", "projects", "databases", "ssh", "git", "vscode", "claude", "folders"];
const fmtBytes = (n) => n >= 1073741824 ? `${(n / 1073741824).toFixed(2)} GB` : n >= 1048576 ? `${(n / 1048576).toFixed(1)} MB` : `${Math.ceil(n / 1024)} KB`;
const move = { folders: [], plan: null, timer: null, manifest: null, canWinget: false };
const sectionList = (el, infos, checked) => {
  el.innerHTML = MOVE_SECTIONS.map((id) => {
    const info = infos.find((x) => x.id === id);
    const size = info ? t("move.files", { n: info.files, size: fmtBytes(info.bytes) }) : "—";
    return `<label class="check"><input type="checkbox" name="sec" value="${id}" ${checked(id, info) ? "checked" : ""} ${info ? "" : "disabled"}><span>${t(`move.sec.${id}`)}<small>${esc(size)}</small></span></label>`;
  }).join("");
};
const moveSummary = (p) => t("move.summary", { pkgs: p.packages.length, repos: p.repos.length, exts: p.vscode_extensions.length, tools: p.tools.length });
const repLine = (cls, label, items) => items.length ? `<div class="rep ${cls}"><b>${label} (${items.length})</b><ul>${items.map((x) => `<li>${esc(x)}</li>`).join("")}</ul></div>` : "";
function moveRequest(f) {
  return { file: f.elements.file.value.trim(), password: f.elements.password.value, sections: $$("input[name=sec]:checked", f).map((i) => i.value), folders: move.folders.slice(), include_heavy: f.elements.heavy.checked };
}
function renderMoveFolders() {
  $("#move-folders").innerHTML = move.folders.map((p, i) => `<div class="fold">${ICONS.folder}<span title="${esc(p)}">${esc(p)}</span><button type="button" class="icon-btn small" data-move-rm="${i}" title="${t("btn.remove")}">${ICONS.x}</button></div>`).join("");
}
async function refreshMovePlan() {
  const f = $("#form-move"); const req = moveRequest(f); req.sections = [];
  $("#move-summary").textContent = t("move.planning");
  try {
    move.plan = await invoke("move_plan", { req });
    const chosen = new Set($$("input[name=sec]:checked", f).map((i) => i.value));
    const first = !$("input[name=sec]", f);
    sectionList($("#move-sections"), move.plan.sections, (id, info) => first ? !!info : chosen.has(id));
    $("#move-summary").textContent = moveSummary(move.plan) + (move.plan.notes.length ? ` · ${move.plan.notes.join(" · ")}` : "") + (move.plan.skipped.length ? ` · ${t("move.leftOut").toLowerCase()}: ${move.plan.skipped.length}` : "");
  } catch (e) { fail(e); }
}
async function openMoveDialog() {
  const f = $("#form-move"); f.reset(); move.folders = (state.config?.move?.folders || []).slice(); renderMoveFolders();
  $("#move-suggest").classList.add("hidden"); $("#move-suggest").innerHTML = "";
  $("#move-sections").innerHTML = ""; $("#move-report").classList.add("hidden"); $("#move-progress").classList.add("hidden");
  const d = new Date(), pad = (n) => String(n).padStart(2, "0");
  const sep = (state.root || "").includes("\\") ? "\\" : "/";
  f.elements.file.value = `${state.root || ""}${sep}backups${sep}veranda-move-${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}.veranda-move`;
  $("#dlg-move").showModal();
  await refreshMovePlan();
}
$("#btn-move-create").onclick = openMoveDialog;
$("#btn-move-add-folder").onclick = async () => {
  try {
    const picked = await window.__TAURI__.dialog.open({ directory: true, multiple: true, title: t("move.addFolder") });
    const list = Array.isArray(picked) ? picked : picked ? [picked] : [];
    for (const p of list) { const v = typeof p === "string" ? p : p.path; if (v && !move.folders.includes(v)) move.folders.push(v); }
    renderMoveFolders(); refreshMovePlan();
  } catch (e) { toast(t("migrate.noPicker"), "info"); }
};
$("#btn-move-suggest").onclick = async () => {
  const box = $("#move-suggest"); box.classList.remove("hidden"); box.innerHTML = `<div class="empty small">${t("move.planning")}</div>`;
  try {
    const list = await invoke("move_suggest");
    box.innerHTML = list.length ? list.map((s) => `<label class="check"><input type="checkbox" data-sug="${esc(s.path)}" ${move.folders.includes(s.path) ? "checked" : ""}><span><b>${esc(s.name)}</b><span class="pill">${esc(s.kind)}</span><small title="${esc(s.path)}">${esc(s.path)}</small></span></label>`).join("")
      : `<div class="empty small">${t("move.noSuggest")}</div>`;
  } catch (e) { fail(e); box.classList.add("hidden"); }
};
$("#move-suggest").addEventListener("change", (ev) => {
  const i = ev.target.closest("[data-sug]"); if (!i) return;
  const p = i.dataset.sug;
  if (i.checked) { if (!move.folders.includes(p)) move.folders.push(p); } else move.folders = move.folders.filter((x) => x !== p);
  renderMoveFolders(); refreshMovePlan();
});
$("#move-folders").addEventListener("click", (ev) => { const b = ev.target.closest("[data-move-rm]"); if (!b) return; const [gone] = move.folders.splice(Number(b.dataset.moveRm), 1); const sug = $(`#move-suggest [data-sug="${CSS.escape(gone)}"]`); if (sug) sug.checked = false; renderMoveFolders(); refreshMovePlan(); });
$("#form-move").addEventListener("change", (ev) => { if (ev.target.name === "heavy") refreshMovePlan(); });
$("#btn-move-browse").onclick = async () => {
  try {
    const p = await window.__TAURI__.dialog.save({ title: t("move.file"), defaultPath: $("#form-move").elements.file.value, filters: [{ name: "Veranda move bundle", extensions: ["veranda-move"] }] });
    if (p) $("#form-move").elements.file.value = typeof p === "string" ? p : p.path;
  } catch (e) { toast(t("migrate.noPicker"), "info"); }
};
listen("move-progress", (ev) => { for (const id of ["#move-progress", "#restore-progress"]) { const el = $(id); if (!el.classList.contains("hidden")) $(".txt", el).textContent = ev.payload; } });
$("#form-move").onsubmit = async (ev) => {
  ev.preventDefault(); const f = ev.target;
  if (f.elements.password.value !== f.elements.confirm.value) { toast(t("rm.pwMismatch"), "bad"); f.elements.confirm.focus(); return; }
  if (f.elements.password.value.length < 8) { toast(t("move.pwShort"), "bad"); f.elements.password.focus(); return; }
  const req = moveRequest(f);
  if (!req.sections.length) { toast(t("move.nothing"), "bad"); return; }
  const btn = $("#btn-move-run"); btn.disabled = true;
  const prog = $("#move-progress"); prog.classList.remove("hidden"); $(".txt", prog).textContent = t("move.working");
  $("#move-report").classList.add("hidden");
  try {
    const m = await invoke("move_create", { req });
    const total = m.sections.reduce((a, s) => a + s.bytes, 0), files = m.sections.reduce((a, s) => a + s.files, 0);
    $("#move-report").innerHTML = `<div class="rep ok"><b>${esc(t("move.created", { file: req.file }))}</b><ul><li>${esc(t("move.files", { n: files, size: fmtBytes(total) }))}</li><li>${esc(moveSummary(m))}</li></ul></div>`;
    $("#move-report").classList.remove("hidden");
    toast(t("move.created", { file: req.file }), "ok");
  } catch (e) { fail(e); }
  prog.classList.add("hidden"); btn.disabled = false;
};
async function openRestoreDialog() {
  const f = $("#form-restore"); f.reset(); move.manifest = null;
  $("#restore-info").classList.add("hidden"); $("#restore-report").classList.add("hidden"); $("#restore-progress").classList.add("hidden");
  $("#btn-restore-run").disabled = true;
  try { move.canWinget = await invoke("move_can_winget"); } catch (e) { move.canWinget = false; }
  $("#restore-winget-row").classList.toggle("hidden", !move.canWinget);
  $("#dlg-restore").showModal();
}
$("#btn-move-restore").onclick = openRestoreDialog;
$("#btn-restore-browse").onclick = async () => {
  try {
    const p = await window.__TAURI__.dialog.open({ multiple: false, title: t("move.bundle"), filters: [{ name: "Veranda move bundle", extensions: ["veranda-move"] }] });
    if (p) $("#form-restore").elements.file.value = typeof p === "string" ? p : p.path;
  } catch (e) { toast(t("migrate.noPicker"), "info"); }
};
function showManifest(m) {
  move.manifest = m;
  $("#restore-from").textContent = t("move.from", { host: m.hostname || "?", os: m.from_os, date: m.created, ver: m.veranda_version });
  sectionList($("#restore-sections"), m.sections, (id, info) => !!info);
  $("#restore-summary").textContent = moveSummary(m);
  $("#restore-info").classList.remove("hidden"); $("#btn-restore-run").disabled = false;
}
$("#btn-restore-check").onclick = async () => {
  const f = $("#form-restore"); const file = f.elements.file.value.trim(), password = f.elements.password.value;
  if (!file || !password) { toast(t("move.nothing"), "info"); return; }
  const b = $("#btn-restore-check"); b.disabled = true;
  try { showManifest(await invoke("move_inspect", { file, password })); } catch (e) { fail(e); }
  b.disabled = false;
};
$("#form-restore").onsubmit = async (ev) => {
  ev.preventDefault(); const f = ev.target;
  if (!move.manifest) { $("#btn-restore-check").click(); return; }
  const sections = $$("input[name=sec]:checked", f).map((i) => i.value);
  if (!sections.length) { toast(t("move.nothing"), "bad"); return; }
  const btn = $("#btn-restore-run"); btn.disabled = true; $("#btn-restore-check").disabled = true;
  const prog = $("#restore-progress"); prog.classList.remove("hidden"); $(".txt", prog).textContent = t("move.working");
  $("#restore-report").classList.add("hidden");
  try {
    const r = await invoke("move_restore", { req: { file: f.elements.file.value.trim(), password: f.elements.password.value, sections } });
    let html = `<div class="rep ok"><b>${esc(t("move.restored", { n: r.restored, size: fmtBytes(r.bytes) }))}</b>${r.backup_dir ? `<ul><li>${esc(t("move.backupDir", { n: r.replaced, dir: r.backup_dir }))}</li></ul>` : ""}</div>`;
    html += repLine("", t("move.skipped"), r.skipped) + repLine("bad", t("move.failed"), r.failed);
    if (f.elements.finish.checked) {
      const fin = await invoke("move_finish", { req: { manifest: r.manifest, packages: r.manifest.packages, repos: r.repos_to_clone, extensions: r.manifest.vscode_extensions, tools: f.elements.winget.checked && move.canWinget ? r.tools_missing : [], database_dumps: r.database_dumps } });
      html += repLine("ok", t("move.installed"), fin.installed) + repLine("bad", t("move.failed"), fin.failed);
    }
    if (r.tools_missing.length && !(f.elements.winget.checked && move.canWinget)) html += repLine("", t("move.leftOut"), r.tools_missing.map((x) => `${x.label} (winget install --id ${x.winget})`));
    $("#restore-report").innerHTML = html; $("#restore-report").classList.remove("hidden");
    toast(t("move.done"), r.failed.length ? "info" : "ok");
    refresh(true);
  } catch (e) { fail(e); }
  prog.classList.add("hidden"); btn.disabled = false; $("#btn-restore-check").disabled = false;
};

/* ---------------- database ---------------- */
function renderDatabase() {
  $("#db-host").textContent = `127.0.0.1:${state.config.db.port}`;
  if (!state.db_password_known) { $("#db-password").textContent = "—"; $("#db-password").title = t("db.unknownPassword"); $("#db-password").classList.remove("secret"); }
}

/* ---------------- logs ---------------- */
let currentLog = "";
async function loadLogList() {
  try {
    const files = await invoke("list_logs");
    const sel = $("#log-file");
    const prev = currentLog || sel.value;
    sel.innerHTML = files.map((f) => `<option value="${esc(f.name)}">${esc(f.name)} · ${(f.size / 1024).toFixed(0)} KB</option>`).join("") || `<option value="">—</option>`;
    if (files.some((f) => f.name === prev)) sel.value = prev;
    currentLog = sel.value;
    await loadLog();
  } catch (e) { fail(e); }
}
async function loadLog() {
  const view = $("#log-view");
  view.dataset.empty = t("logs.empty");
  if (!currentLog) { view.textContent = ""; return; }
  try {
    const raw = await invoke("read_log", { name: currentLog, lines: 400 });
    const text = raw.replace(/\x1b\[[0-9;]*[A-Za-z]/g, "");   // strip ANSI colour codes some servers print
    view.innerHTML = text.split("\n").map((l) => {
      const cls = /error|fatal|panic|denied|failed/i.test(l) ? "ln-err" : /warn/i.test(l) ? "ln-warn" : "";
      return cls ? `<span class="${cls}">${esc(l)}</span>` : esc(l);
    }).join("\n");
    view.scrollTop = view.scrollHeight;
  } catch (e) { fail(e); }
}
$("#log-file").onchange = (ev) => { currentLog = ev.target.value; loadLog(); };
$("#btn-log-refresh").onclick = loadLogList;
$("#btn-log-clear").onclick = async () => { if (!currentLog) return; try { await invoke("clear_log", { name: currentLog }); await loadLog(); } catch (e) { fail(e); } };
$("#btn-activity-clear").onclick = () => { activity.length = 0; renderActivity(); };

/* ---------------- settings ---------------- */
function renderSettings() {
  $("#root-path").textContent = state.root; $("#root-path").title = state.root;
  $("#set-theme-mode").value = document.documentElement.dataset.theme || "";
  $("#set-skin").value = document.documentElement.dataset.skin || "";
  renderLockSettings();
  $$("[data-os]").forEach((el) => el.classList.toggle("hidden", !state.platform.startsWith(el.dataset.os)));
  for (const el of $("#form-settings").elements) {
    if (!el.name) continue;
    const [sec, key] = el.name.split(".");
    const v = state.config[sec]?.[key];
    if (el.type === "checkbox") el.checked = !!v; else el.value = v ?? "";
  }
}
function readSettings() {
  const cfg = clone(state.config);
  for (const el of $("#form-settings").elements) {
    if (!el.name) continue;
    const [sec, key] = el.name.split(".");
    cfg[sec][key] = el.type === "checkbox" ? el.checked : el.type === "number" ? Number(el.value) : el.value;
  }
  return cfg;
}

/* ---------------- events ---------------- */
document.addEventListener("click", async (ev) => {
  const item = ev.target.closest("[data-svc]");
  if (item && !ev.target.closest("input")) { openService(item.dataset.svc); return; }
  const b = ev.target.closest("button");
  if (!b) return;
  if (b.dataset.svctab) { svcTab = b.dataset.svctab; renderService(); return; }
  if (b.dataset.tab) { showTab(b.dataset.tab); return; }
  if (b.id === "btn-check-upstream") { loadUpstream(); return; }
  if (b.id === "btn-pg-show") {
    const el = $("#pg-password");
    if (el.classList.contains("secret")) { try { el.textContent = (await invoke("pg_password")) || t("db.unknownPassword"); el.classList.remove("secret"); } catch (e) { fail(e); } }
    else { el.textContent = "••••••••••••"; el.classList.add("secret"); }
    return;
  }
  if (b.dataset.open) { try { await invoke("open_target", { what: b.dataset.open }); } catch (e) { fail(e); } return; }
  if (b.dataset.files) { window.VerandaFiles?.open(b.dataset.files); return; }
  if (b.dataset.copy) {
    let text = $(b.dataset.copy).textContent;
    if (b.dataset.copySecret) text = (await invoke("db_password")) || "";
    if (text) { await navigator.clipboard.writeText(text); toast(t("msg.copied"), "ok"); }
    return;
  }
  if (b.dataset.action) { b.disabled = true; await serviceAction(b.dataset.id, b.dataset.action); return; }
  if (b.dataset.install) { await installPackage(b.dataset.install, !!b.dataset.latest, b.dataset.version || null); return; }
  if (b.dataset.cancelInstall) { if (installing) { installing.cancelling = true; paintProgress(); } try { await invoke("cancel_install"); } catch (e) { fail(e); } return; }
  if (b.dataset.remove) { await removePackage(b.dataset.remove, b.dataset.version); return; }
  if (b.id === "btn-group-new") { openGroupSvcDialog(null); return; }
  if (b.dataset.groupEdit !== undefined && b.dataset.groupAction === undefined && b.closest("#groups")) { openGroupSvcDialog((state.config.general.startup_groups || []).find((g) => g.name === b.dataset.groupEdit)); return; }
  if (b.dataset.groupAction) { await runGroup(b.dataset.group, b.dataset.groupAction); return; }
  if (b.dataset.copyText) { await navigator.clipboard.writeText(b.dataset.copyText); toast(t("msg.copied"), "ok"); return; }
  if (b.dataset.nodeDefault !== undefined) {
    b.disabled = true;
    try { const label = await invoke("node_set_default", { selector: b.dataset.nodeDefault }); toast(t("msg.nodeDefault", { version: label }), "ok"); } catch (e) { fail(e); }
    await refresh(true); return;
  }
  if (b.dataset.removeExt) {
    const kind = b.dataset.removeExt, version = b.dataset.version, source = b.dataset.source;
    if (!(await askConfirm(t("msg.confirmRemoveExt", { version, source })))) return;
    removing = { kind, version }; renderAll();
    try { await invoke("remove_external", { kind, version, source }); toast(t("msg.removedExt", { version, source }), "ok"); } catch (e) { fail(e); }
    removing = null; await refresh(true); return;
  }
  if (b.dataset.deleteProject) {
    if (!(await askConfirm(t("msg.confirmDelete", { name: b.dataset.deleteProject })))) return;
    try { await invoke("delete_project", { name: b.dataset.deleteProject }); await syncSites(); } catch (e) { fail(e); }
    return;
  }
  if (b.dataset.removeSite) { try { await invoke("remove_site", { hostname: b.dataset.removeSite }); await syncSites(); } catch (e) { fail(e); } }
});
document.addEventListener("click", (ev) => {
  const a = ev.target.closest("a[data-link]"); if (!a) return;
  ev.preventDefault(); invoke("remote_open_link", { url: a.dataset.link }).catch(fail);
});
document.addEventListener("keydown", (ev) => {
  const item = ev.target.closest?.("[data-svc]");
  if (item && (ev.key === "Enter" || ev.key === " ")) { ev.preventDefault(); openService(item.dataset.svc); }
});
document.addEventListener("change", async (ev) => {
  const el = ev.target;
  if (el.dataset.sw) { await toggleKind(el.dataset.sw, el.checked); return; }
  if (el.dataset.versionKind) {
    const cfg = clone(state.config);
    cfg[SVC_SECTION[el.dataset.service]].version = el.value;
    try { await invoke("save_config", { config: cfg }); toast(t("msg.saved"), "ok"); } catch (e) { fail(e); }
    refresh(true);
  }
});

$("#btn-start-all").onclick = () => runAll("start");
$("#btn-stop-all").onclick = () => runAll("stop");
$("#btn-hosts-sync").onclick = async () => { try { await invoke("hosts_sync"); toast(t("msg.hostsUpdated"), "ok"); } catch (e) { fail(e); } refresh(); };
$("#btn-sync-sites").onclick = syncSites;
$("#btn-db-show").onclick = async () => {
  const el = $("#db-password");
  if (!state.db_password_known) return;
  if (el.classList.contains("secret")) { el.textContent = (await invoke("db_password")) || t("db.none"); el.classList.remove("secret"); }
  else { el.textContent = "••••••••••••"; el.classList.add("secret"); }
};
$("#btn-db-rotate").onclick = async () => { try { const pw = await invoke("db_set_password", { new: null }); $("#db-password").textContent = pw; $("#db-password").classList.remove("secret"); toast(t("msg.pwRotated"), "ok"); } catch (e) { fail(e); } };
$("#btn-db-nopass").onclick = async () => {
  if (!(await askConfirm(t("msg.confirmNoPassword"), t("db.noPassword")))) return;
  try { await invoke("db_set_password", { new: "" }); $("#db-password").textContent = t("db.none"); $("#db-password").classList.remove("secret"); toast(t("msg.pwCleared"), "ok"); } catch (e) { fail(e); }
};
$("#btn-db-backup").onclick = async () => { try { const p = await invoke("db_backup"); toast(t("msg.backup", { path: p }), "ok"); } catch (e) { fail(e); } };
$("#form-db").onsubmit = async (ev) => {
  ev.preventDefault(); const name = ev.target.elements.name.value.trim();
  try { await invoke("db_create", { name }); toast(t("msg.created", { name }), "ok"); ev.target.reset(); const l = await invoke("db_list"); $("#db-list").innerHTML = l.map((d) => `<span class="chip">${esc(d)}</span>`).join(""); } catch (e) { fail(e); }
};
$("#form-site").onsubmit = async (ev) => {
  ev.preventDefault(); const f = ev.target;
  try { await invoke("add_site", { hostname: f.elements.hostname.value.trim(), path: f.elements.path.value.trim(), public: f.elements.public.value.trim() }); f.reset(); await syncSites(); } catch (e) { fail(e); }
};
$("#form-settings").onsubmit = async (ev) => { ev.preventDefault(); try { await invoke("save_config", { config: readSettings() }); toast(t("msg.saved"), "ok"); } catch (e) { fail(e); } refresh(true); };
$("#btn-ssl-trust").onclick = async () => {
  try { const r = await invoke("ssl_trust"); toast(t("msg.trust", { ok: r.filter((x) => x.ok).length, total: r.length }), "ok"); r.filter((x) => !x.ok).forEach((x) => toast(`${x.description}: ${x.error}`, "bad")); } catch (e) { fail(e); }
};

/* ---------------- app updates ---------------- */
let pendingUpdate = null;
let lastUpdateCheck = Date.now(); // the backend checks 8 s after launch, then hourly
function showUpdate(info) {
  pendingUpdate = info;
  const b = $("#update-banner");
  $("#update-version").textContent = t("update.available", { version: info.version });
  $("#update-notes").textContent = info.installable ? (info.notes || "").split("\n")[0] : t("update.manual");
  $("#btn-update-install").classList.toggle("hidden", !info.installable);
  $("#update-progress").classList.add("hidden");
  b.classList.remove("hidden");
}
async function checkForUpdate(interactive) {
  lastUpdateCheck = Date.now();
  try {
    const r = await invoke("update_check");
    if (r) { showUpdate(r); if (state.config.update.auto_install && r.installable) $("#btn-update-install").click(); }
    else if (interactive) toast(t("msg.upToDate", { version: state.version }), "ok");
  } catch (e) { if (interactive) fail(e); }
}
$("#btn-check-update").onclick = () => checkForUpdate(true);
$("#btn-update-dismiss").onclick = () => $("#update-banner").classList.add("hidden");
$("#btn-update-page").onclick = () => { if (pendingUpdate?.url) invoke("open_url_external", { url: pendingUpdate.url }).catch(() => invoke("open_target", { what: "web" })); };
$("#btn-update-install").onclick = async () => {
  const btn = $("#btn-update-install"); btn.disabled = true;
  const prog = $("#update-progress"); prog.classList.remove("hidden"); $(".fill", prog).style.width = "0%"; $(".txt", prog).textContent = "";
  try { await invoke("update_install"); toast(t("update.restarting"), "ok"); } catch (e) { fail(e); btn.disabled = false; }
};
listen("update-progress", (ev) => {
  const p = ev.payload, prog = $("#update-progress");
  if (p.total) { $(".fill", prog).style.width = `${(100 * p.downloaded / p.total).toFixed(0)}%`; $(".txt", prog).textContent = `${(p.downloaded / 1048576).toFixed(1)} / ${(p.total / 1048576).toFixed(1)} MiB`; }
  else $(".txt", prog).textContent = `${(p.downloaded / 1048576).toFixed(1)} MiB`;
  if (p.done) $(".txt", prog).textContent = t("update.restarting");
});
listen("update-available", (ev) => { lastUpdateCheck = Date.now(); showUpdate(ev.payload); });
// Coming back to the window (from the tray, another desktop, sleep) after a while: check again,
// so a release published while Veranda sat in the background shows up without a click.
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState !== "visible" || !state.config?.update?.check_on_start) return;
  if (Date.now() - lastUpdateCheck > 30 * 60 * 1000) checkForUpdate(false);
});
listen("update-auto", () => { const b = $("#btn-update-install"); b.disabled = true; $("#update-notes").textContent = t("update.auto"); $("#update-progress").classList.remove("hidden"); });
listen("update-error", (ev) => { fail(ev.payload); $("#btn-update-install").disabled = false; });

/* ---------------- new project dialog ---------------- */
$("#btn-new-project").onclick = () => {
  const sel = $("#project-template");
  sel.innerHTML = state.templates.map(([id, label]) => `<option value="${id}">${esc(label)}</option>`).join("");
  sel.onchange(); $("#project-url").textContent = ""; $("#dlg-project").showModal();
};
$("#project-template").onchange = function () {
  const v = this.value;
  $("#project-arg-row").classList.toggle("hidden", !(v === "composer" || v === "git"));
  $("#project-arg-label").textContent = v === "git" ? "URL" : "Package";
  $("#form-project").elements.arg.placeholder = v === "git" ? "https://github.com/user/repo.git" : "vendor/package";
};
$("#form-project").elements.name.oninput = (ev) => { $("#project-url").textContent = ev.target.value ? `${state.scheme}://${ev.target.value}.${state.config.general.tld}` : ""; };
$("#btn-project-cancel").onclick = () => $("#dlg-project").close();
$("#form-project").onsubmit = async (ev) => {
  ev.preventDefault(); const f = ev.target;
  const req = { name: f.elements.name.value.trim(), template: f.elements.template.value, arg: f.elements.arg.value.trim(), create_db: f.elements.create_db.checked };
  const btn = $("button.primary", f); btn.disabled = true;
  try { const p = await invoke("create_project", { req }); toast(t("msg.created", { name: p }), "ok"); $("#dlg-project").close(); f.reset(); await syncSites(); } catch (e) { fail(e); }
  btn.disabled = false;
};

/* ---------------- scroll benchmark (VERANDA_UI_PERF=1, dev only) ---------------- */
async function perfBenchmark() {
  showTab("settings");
  await new Promise((r) => setTimeout(r, 800));
  const doc = document.documentElement;
  const frames = [];
  let last = performance.now(), dir = 1, start = last;
  await new Promise((done) => {
    const step = (now) => {
      frames.push(now - last); last = now;
      if (doc.scrollTop + window.innerHeight >= doc.scrollHeight - 2) dir = -1;
      if (doc.scrollTop <= 0) dir = 1;
      window.scrollBy(0, 8 * dir);
      if (now - start < 4000) requestAnimationFrame(step); else done();
    };
    requestAnimationFrame(step);
  });
  frames.shift();
  const sorted = [...frames].sort((a, b) => a - b);
  const avg = frames.reduce((a, b) => a + b, 0) / frames.length;
  const p95 = sorted[Math.floor(sorted.length * 0.95)];
  const long = frames.filter((f) => f > 20).length;
  const text = `frames=${frames.length} fps=${(1000 / avg).toFixed(1)} avg=${avg.toFixed(2)}ms p95=${p95.toFixed(2)}ms long(>20ms)=${long} max=${sorted[sorted.length - 1].toFixed(1)}ms scrollHeight=${doc.scrollHeight}`;
  invoke("perf_report", { text }).catch(() => {});
}

/* ---------------- scripted checks (VERANDA_UI_AUTOTEST=..., dev only) ---------------- */
async function autotest(name) {
  const report = (text) => invoke("perf_report", { text: `autotest ${name}: ${text}` }).catch(() => {});
  if (name === "cancel-install") {
    // start the biggest download we have (Go), cancel it after a few seconds, report what happened
    openService("go");
    const { newest } = versionsFor("go");
    const started = performance.now();
    const done = installPackage("go", false, newest);
    setTimeout(() => { const b = $("[data-cancel-install]"); if (b) b.click(); else report("no cancel button found"); }, 4000);
    await done;
    const last = activity[0]?.msg || "";
    report(`newest=${newest} elapsed=${((performance.now() - started) / 1000).toFixed(1)}s installed=${installedVersions("go").length} last="${last}"`);
  } else if (name === "groups") {
    const cfg = clone(state.config);
    cfg.general.startup_groups = [{ name: "Web stack", services: ["web", "php", "db"] }, { name: "Database only", services: ["db"] }];
    cfg.general.default_group = "Web stack";
    await invoke("save_config", { config: cfg }); await refresh(true); showTab("dashboard"); report(`groups=${(state.config.general.startup_groups || []).length}`);
  } else if (name === "projectdlg") { showTab("sites"); $("#btn-new-project").click(); report("open");
  } else if (name === "migratedlg") { showTab("sites"); await openMigrate(); report("open");
  } else if (name === "movedlg") { showTab("settings"); await openMoveDialog(); await new Promise((r) => setTimeout(r, 1500)); $("#btn-move-suggest").click(); await new Promise((r) => setTimeout(r, 2500)); report(`plan=${JSON.stringify(move.plan?.sections.map((x) => `${x.id}:${x.files}`))} suggestions=${$$("#move-suggest [data-sug]").length}`);
  } else if (name.startsWith("files")) { await window.VerandaFiles.autotest(name.split(":")[1] || "", report);
  } else if (name === "storage") { showTab("settings"); await renderStorage(); $("#storage-card").scrollIntoView(); report(`usage=${$$("#storage-usage .pill").length}`);
  } else if (name === "restoredlg") { showTab("settings"); await openRestoreDialog(); report(`open card=${!!$("#btn-move-create")}`);
  } else if (name === "svcgroupdlg") { showTab("dashboard"); openGroupSvcDialog({ name: "Web stack", services: ["web", "php", "db"] }); report("open");
  } else if (window.VerandaRemote?.autotest) {
    await window.VerandaRemote.autotest(name, report);
  }
}

/* ---------------- boot ---------------- */
(async () => {
  applyI18n();
  await refreshLockInfo();
  if (lockInfo.locked) { showLock(true); uiReady(); await waitUnlock(); }
  await refresh();
  if (state?.ui_theme === "light" || state?.ui_theme === "dark") setTheme(state.ui_theme);
  if (state?.ui_tab) {
    const [tab, sub] = state.ui_tab.split(":");   // e.g. "php:versions" (screenshot helper)
    if (KIND_META[tab]) { if (sub) svcTab = sub; openService(tab); } else { showTab(tab); if (sub === "migrate") openMigrate(); }
  }
  try { builtin = await invoke("builtin_versions"); renderService(); } catch (e) { fail(e); }
  try { const job = await invoke("migrate_status"); if (job.running) { migrateJob = job; showMigrateProgress(job.progress); } } catch (e) {}
  if (window.VerandaRemote) window.VerandaRemote.init();
  if (state?.ui_perf) setTimeout(perfBenchmark, 1200);
  if (state?.ui_autotest) setTimeout(() => autotest(state.ui_autotest), 1500);
  setInterval(() => { if (document.visibilityState !== "visible" || installing || removing) return; refresh(); if (currentTab === "logs") loadLog(); }, 15000);
  setInterval(() => invoke("ui_heartbeat").catch(() => {}), 5000);
})();
