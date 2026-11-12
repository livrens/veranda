/* Veranda Remote - the SSH client: hosts, terminals (xterm.js), SFTP, port forwarding, snippets,
   keychain, known hosts and history. Uses the helpers defined in app.js (invoke, listen, $, t, toast...). */
(function () {
  const R = {
    view: null,            // last `remote_state` result
    terms: new Map(),      // id -> { id, term, fit, search, el, tab, info, hostId, quick, local, closed }
    active: null,          // active terminal id
    sftp: null,            // { id, host_id, label, home }
    panes: { local: { path: "", entries: [], sel: new Set() }, remote: { path: "", entries: [], sel: new Set() } },
    transfers: [],         // { id, name, dir, done, total, state }
    search: "", tag: null,
    promptQueue: [], promptOpen: null,
    pendingTheme: null,
    side: { open: false, enter: true, q: "" },   // snippets panel in the terminal
    privacy: true,         // hide addresses/ports/user names in Hosts and History (default on)
    reveal: new Set(),     // row keys temporarily revealed while privacy mode is on
  };
  try { R.side.open = localStorage.getItem("veranda.snipPanel") === "1"; R.side.enter = localStorage.getItem("veranda.snipEnter") !== "0"; R.privacy = localStorage.getItem("veranda.privacy") !== "0"; } catch (e) {}
  const MASK = "•••";
  const maskAddr = (key, text) => (R.privacy && !R.reveal.has(key)) ? MASK : text;
  const revealFor = (key, rerender) => {
    if (R.reveal.has(key)) R.reveal.delete(key);
    else { R.reveal.add(key); setTimeout(() => { if (R.reveal.delete(key)) rerender(); }, 15000); }
    rerender();
  };
  const copyText = async (text) => { try { await navigator.clipboard.writeText(text); toast(t("msg.copied"), "ok"); } catch (e) { fail(e); } };
  const lockState = () => R.view?.snippet_lock || "off";
  const settings = () => R.view?.store.settings || {};
  const hostsOf = () => R.view?.store.hosts || [];
  const hostById = (id) => hostsOf().find((h) => h.id === id);
  const newId = () => Array.from(crypto.getRandomValues(new Uint8Array(8)), (b) => b.toString(16).padStart(2, "0")).join("");
  const fmtSize = (n) => n < 1024 ? `${n} B` : n < 1048576 ? `${(n / 1024).toFixed(1)} KB` : n < 1073741824 ? `${(n / 1048576).toFixed(1)} MB` : `${(n / 1073741824).toFixed(2)} GB`;
  const fmtTime = (s) => { if (!s) return "—"; const d = new Date(s * 1000); return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")} ${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`; };
  const osIcon = (os) => /ubuntu|debian|linux|fedora|centos|alpine|arch|rocky|alma/i.test(os) ? "lang" : /darwin|mac/i.test(os) ? "local" : /windows|microsoft/i.test(os) ? "local" : "host";
  const COLORS = ["", "#b9552d", "#4e7d4a", "#b27a19", "#b43a2e", "#6b7f2e", "#2e7d7a", "#8a5a44", "#7b7266"];
  const b64 = (s) => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));

  /* ---------------- state ---------------- */
  async function refresh() {
    try { R.view = await invoke("remote_state"); } catch (e) { fail(e); return; }
    renderAll();
  }
  function renderAll() {
    for (const fn of [renderHosts, renderForwards, renderSnippets, renderSnipPanel, renderKeychain, renderHistory, renderSftpHosts, renderCounts]) {
      try { fn(); } catch (e) { fail(`remote ${fn.name}: ${e?.message || e}`); }
    }
  }
  function renderCounts() {
    const open = [...R.terms.values()].filter((x) => !x.closed).length;
    const tc = $("#term-count"); tc.textContent = open; tc.classList.toggle("hidden", !open);
    const fa = (R.view?.forwards_active || []).length;
    const fc = $("#fwd-count"); fc.textContent = fa; fc.classList.toggle("hidden", !fa);
  }

  /* ---------------- hosts ---------------- */
  function hostMatches(h) {
    const q = R.search.trim().toLowerCase();
    if (R.tag && !h.tags.includes(R.tag)) return false;
    if (!q) return true;
    return [h.label, h.address, h.username, h.notes, h.os, ...h.tags].some((x) => (x || "").toLowerCase().includes(q));
  }
  function hostRow(h) {
    const ident = h.auth === "identity" ? (R.view.store.identities.find((i) => i.id === h.identity_id) || {}) : null;
    const key = h.auth === "key" ? (R.view.store.keys.find((k) => k.id === h.key_id) || {}) : null;
    const authPill = h.auth === "agent" ? "agent" : h.auth === "key" ? (key?.label || "key") : h.auth === "identity" ? (ident?.label || "identity") : h.auth === "password" ? (h.has_password ? "password" : "password?") : "";
    const jump = h.jump_host_id ? hostById(h.jump_host_id) : null;
    return `<div class="row host-row" data-host="${esc(h.id)}" tabindex="0" style="${h.color ? `--hc:${esc(h.color)}` : ""}">
      <div class="ico" style="${h.color ? `color:${esc(h.color)};background:color-mix(in srgb, ${esc(h.color)} 14%, transparent)` : ""}">${ICONS[osIcon(h.os)]}</div>
      <div><div class="name">${esc(h.label)}${h.os ? ` <span class="pill">${esc(h.os.split(" ")[0])}</span>` : ""}</div>
        <div class="meta"><span class="mono">${esc(maskAddr("h:" + h.id, h.summary || hostSummary(h)))}</span>${authPill ? `<span class="pill">${esc(authPill)}</span>` : ""}${jump ? `<span class="pill" title="${t("rm.jump")}">${ICONS.link}${esc(jump.label)}</span>` : ""}${h.tags.map((x) => `<span class="tag" data-tag="${esc(x)}">${esc(x)}</span>`).join("")}</div></div>
      <div class="act">
        <button class="icon-btn go" data-rm="connect" title="${t("rm.openTerminal")}">${ICONS.terminal}</button>
        <button class="icon-btn" data-rm="sftp" title="SFTP">${ICONS.sftp}</button>
        ${R.privacy ? `<button class="icon-btn ${R.reveal.has("h:" + h.id) ? "on" : ""}" data-rm="reveal" title="${t("rm.reveal")}">${ICONS.eye}</button>` : ""}
        <button class="icon-btn" data-rm="copyaddr" title="${t("rm.copyAddr")}">${ICONS.copy}</button>
        <button class="icon-btn" data-rm="edit" title="${t("btn.edit")}">${ICONS.edit}</button>
        <button class="icon-btn" data-rm="duplicate" title="${t("rm.duplicate")}">${ICONS.copy}</button>
        <button class="icon-btn danger" data-rm="delete" title="${t("btn.remove")}">${ICONS.trash}</button>
      </div></div>`;
  }
  const hostSummary = (h) => `${h.username ? h.username + "@" : ""}${h.address}${h.port && h.port !== 22 ? ":" + h.port : ""}`;
  function renderHosts() {
    if (!R.view) return;
    const { hosts, groups } = R.view.store;
    const tags = [...new Set(hosts.flatMap((h) => h.tags))].sort();
    $("#rm-tag-filter").innerHTML = tags.map((x) => `<button class="tag ${R.tag === x ? "on" : ""}" data-tag="${esc(x)}">${esc(x)}</button>`).join("");
    const visible = hosts.filter(hostMatches);
    const byGroup = (gid) => visible.filter((h) => (h.group_id || null) === gid);
    const renderGroup = (g, depth) => {
      const children = groups.filter((x) => (x.parent_id || null) === g.id);
      const own = byGroup(g.id);
      if (!own.length && !children.some((c) => countIn(c))) return "";
      return `<details class="rm-group" open style="--d:${depth}"><summary><span class="sico">${ICONS.group}</span><span>${esc(g.name)}</span><span class="count">${countIn(g)}</span>
        <span class="gact"><button class="icon-btn small" data-group-edit="${esc(g.id)}" title="${t("btn.edit")}">${ICONS.edit}</button><button class="icon-btn small" data-group-delete="${esc(g.id)}" title="${t("btn.remove")}">${ICONS.trash}</button></span></summary>
        <div class="list">${own.map(hostRow).join("")}${children.map((c) => renderGroup(c, depth + 1)).join("")}</div></details>`;
    };
    const countIn = (g) => byGroup(g.id).length + groups.filter((x) => (x.parent_id || null) === g.id).reduce((n, c) => n + countIn(c), 0);
    const top = groups.filter((g) => !g.parent_id);
    const loose = byGroup(null);
    const emptyGroups = groups.filter((g) => !countIn(g) && !g.parent_id);
    $("#rm-host-list").innerHTML =
      (loose.length ? `<div class="list">${loose.map(hostRow).join("")}</div>` : "") +
      top.map((g) => renderGroup(g, 0)).join("") +
      emptyGroups.map((g) => `<details class="rm-group" open><summary><span class="sico">${ICONS.group}</span><span>${esc(g.name)}</span><span class="count">0</span><span class="gact"><button class="icon-btn small" data-group-edit="${esc(g.id)}">${ICONS.edit}</button><button class="icon-btn small" data-group-delete="${esc(g.id)}">${ICONS.trash}</button></span></summary><div class="list"><div class="empty small">${t("rm.groupEmpty")}</div></div></details>`).join("") ||
      `<div class="empty">${ICONS.hosts}<span>${hosts.length ? t("rm.noMatch") : t("rm.noHosts")}</span></div>`;
  }
  function fillSelect(sel, items, current, none) {
    sel.innerHTML = (none !== undefined ? `<option value="">${esc(none)}</option>` : "") + items.map(([v, l]) => `<option value="${esc(v)}">${esc(l)}</option>`).join("");
    sel.value = current || "";
    if (sel.value !== (current || "")) sel.value = "";
  }
  function openHostDialog(h) {
    const f = $("#form-host"), st = R.view.store;
    f.reset();
    $("#host-dlg-title").textContent = h?.id ? t("rm.editHost") : t("rm.newHost");
    f.elements.id.value = h?.id || "";
    f.elements.label.value = h?.label || "";
    f.elements.address.value = h?.address || "";
    f.elements.port.value = h?.port || 22;
    f.elements.username.value = h?.username || "";
    f.elements.auth.value = h?.auth || "password";
    f.elements.tags.value = (h?.tags || []).join(", ");
    f.elements.env.value = (h?.env || []).join(", ");
    f.elements.notes.value = h?.notes || "";
    f.elements.keep_alive.checked = h ? !!h.keep_alive : true;
    f.elements.agent_forwarding.checked = !!h?.agent_forwarding;
    f.elements.color.value = h?.color || "";
    f.elements.password.value = ""; f.elements.password.placeholder = h?.has_password ? "•••••••• (saved)" : "";
    f.dataset.clearPw = "";
    fillSelect(f.elements.group_id, st.groups.map((g) => [g.id, g.name]), h?.group_id, t("rm.noGroup"));
    fillSelect(f.elements.key_id, st.keys.map((k) => [k.id, k.label]), h?.key_id, "—");
    fillSelect(f.elements.identity_id, st.identities.map((i) => [i.id, `${i.label} (${i.username})`]), h?.identity_id, "—");
    fillSelect(f.elements.jump_host_id, st.hosts.filter((x) => x.id !== h?.id).map((x) => [x.id, x.label]), h?.jump_host_id, t("rm.noJump"));
    fillSelect(f.elements.startup_snippet_id, st.snippets.map((s) => [s.id, s.label]), h?.startup_snippet_id, "—");
    $("#host-colors").innerHTML = COLORS.map((c) => `<button type="button" class="swatch ${(h?.color || "") === c ? "on" : ""}" data-color="${c}" style="${c ? `background:${c}` : ""}" title="${c || "none"}"></button>`).join("");
    applyAuthRows();
    $("#dlg-host").showModal();
    f.elements.label.focus();
  }
  function applyAuthRows() {
    const v = $("#form-host").elements.auth.value;
    $$("#form-host [data-auth]").forEach((el) => el.classList.toggle("hidden", el.dataset.auth !== v));
  }
  $("#form-host").elements.auth.onchange = applyAuthRows;
  $("#host-colors").onclick = (ev) => {
    const b = ev.target.closest(".swatch"); if (!b) return;
    $("#form-host").elements.color.value = b.dataset.color;
    $$("#host-colors .swatch").forEach((x) => x.classList.toggle("on", x === b));
  };
  $("#btn-host-clear-pw").onclick = () => { const f = $("#form-host"); f.elements.password.value = ""; f.elements.password.placeholder = ""; f.dataset.clearPw = "1"; };
  $("#form-host").onsubmit = async (ev) => {
    ev.preventDefault(); const f = ev.target, e = f.elements;
    const existing = e.id.value ? hostById(e.id.value) : null;
    const host = {
      ...(existing || {}),
      id: e.id.value, label: e.label.value.trim(), address: e.address.value.trim(), port: Number(e.port.value) || 22,
      username: e.username.value.trim(), group_id: e.group_id.value || null, auth: e.auth.value,
      key_id: e.key_id.value || null, identity_id: e.identity_id.value || null, jump_host_id: e.jump_host_id.value || null,
      startup_snippet_id: e.startup_snippet_id.value || null, tags: e.tags.value.split(",").map((x) => x.trim()).filter(Boolean),
      env: e.env.value.split(",").map((x) => x.trim()).filter((x) => x.includes("=")), notes: e.notes.value,
      keep_alive: e.keep_alive.checked, agent_forwarding: e.agent_forwarding.checked, color: e.color.value,
      has_password: existing?.has_password || false, os: existing?.os || "", created_at: existing?.created_at || 0, last_connected: existing?.last_connected || 0,
    };
    const password = e.password.value ? e.password.value : f.dataset.clearPw ? "" : null;
    try { await invoke("remote_save_host", { host, password }); $("#dlg-host").close(); toast(t("msg.saved2"), "ok"); } catch (x) { fail(x); }
    refresh();
  };
  function openGroupDialog(g) {
    const f = $("#form-group"); f.reset();
    f.elements.id.value = g?.id || ""; f.elements.name.value = g?.name || "";
    fillSelect(f.elements.parent_id, R.view.store.groups.filter((x) => x.id !== g?.id).map((x) => [x.id, x.name]), g?.parent_id, t("rm.noGroup"));
    fillSelect(f.elements.identity_id, R.view.store.identities.map((i) => [i.id, `${i.label} (${i.username})`]), g?.identity_id, "—");
    $("#dlg-group").showModal(); f.elements.name.focus();
  }
  $("#form-group").onsubmit = async (ev) => {
    ev.preventDefault(); const e = ev.target.elements;
    const group = { id: e.id.value, name: e.name.value.trim(), parent_id: e.parent_id.value || null, identity_id: e.identity_id.value || null, color: "" };
    try { await invoke("remote_save_group", { group }); $("#dlg-group").close(); } catch (x) { fail(x); }
    refresh();
  };
  $("#btn-rm-new-host").onclick = () => openHostDialog(null);
  $("#btn-rm-new-group").onclick = () => openGroupDialog(null);
  $("#rm-host-search").oninput = (ev) => { R.search = ev.target.value; renderHosts(); };
  $("#quick-connect").onsubmit = (ev) => {
    ev.preventDefault(); const v = ev.target.elements.target.value.trim(); if (!v) return;
    openTerminal({ quick: v, label: t("rm.quickSession") }); ev.target.reset();
  };
  $("#rm-host-list").addEventListener("click", async (ev) => {
    const tag = ev.target.closest("[data-tag]"); if (tag) { R.tag = R.tag === tag.dataset.tag ? null : tag.dataset.tag; renderHosts(); return; }
    const ge = ev.target.closest("[data-group-edit]"); if (ge) { ev.preventDefault(); openGroupDialog(R.view.store.groups.find((g) => g.id === ge.dataset.groupEdit)); return; }
    const gd = ev.target.closest("[data-group-delete]");
    if (gd) { ev.preventDefault(); if (await askConfirm(t("rm.confirmDeleteGroup"))) { try { await invoke("remote_delete_group", { id: gd.dataset.groupDelete }); } catch (x) { fail(x); } refresh(); } return; }
    const row = ev.target.closest("[data-host]"); if (!row) return;
    const h = hostById(row.dataset.host); if (!h) return;
    const b = ev.target.closest("[data-rm]");
    if (!b) { if (ev.detail === 2) openTerminal({ hostId: h.id }); return; }
    switch (b.dataset.rm) {
      case "connect": openTerminal({ hostId: h.id }); break;
      case "sftp": showTab("sftp"); $("#sftp-host").value = h.id; sftpConnect(); break;
      case "reveal": revealFor("h:" + h.id, renderHosts); break;
      case "copyaddr": await copyText(h.summary || hostSummary(h)); break;
      case "edit": openHostDialog(h); break;
      case "duplicate": try { await invoke("remote_save_host", { host: { ...h, id: "", label: `${h.label} copy`, has_password: false }, password: null }); } catch (x) { fail(x); } refresh(); break;
      case "delete": if (await askConfirm(t("rm.confirmDeleteHost", { name: h.label }))) { try { await invoke("remote_delete_host", { id: h.id }); } catch (x) { fail(x); } refresh(); } break;
    }
  });
  $("#rm-tag-filter").onclick = (ev) => { const b = ev.target.closest("[data-tag]"); if (!b) return; R.tag = R.tag === b.dataset.tag ? null : b.dataset.tag; renderHosts(); };
  /* privacy mode: one switch shared by the Hosts and History pages */
  function applyPrivacy() {
    $$(".rm-privacy-toggle").forEach((b) => { b.classList.toggle("on", R.privacy); b.title = `${t("rm.privacy")} (${R.privacy ? "ON" : "OFF"}) — ${t("rm.privacyHint")}`; });
  }
  $$(".rm-privacy-toggle").forEach((b) => b.onclick = () => {
    R.privacy = !R.privacy; R.reveal.clear();
    try { localStorage.setItem("veranda.privacy", R.privacy ? "1" : "0"); } catch (e) {}
    applyPrivacy(); renderHosts(); renderHistory();
  });
  applyPrivacy();
  $("#rm-host-list").addEventListener("keydown", (ev) => { const row = ev.target.closest?.("[data-host]"); if (row && ev.key === "Enter") openTerminal({ hostId: row.dataset.host }); });

  /* ---------------- terminal themes ---------------- */
  const PALETTES = {
    dark: { black: "#1f2633", red: "#ff7b72", green: "#3fb950", yellow: "#e3b341", blue: "#79c0ff", magenta: "#d2a8ff", cyan: "#56d4dd", white: "#d0d7e2", brightBlack: "#6e7a90", brightRed: "#ffa198", brightGreen: "#56d364", brightYellow: "#f2cc60", brightBlue: "#a5d6ff", brightMagenta: "#e2c5ff", brightCyan: "#76e3ea", brightWhite: "#ffffff", background: "#0d1117", foreground: "#dde3ee", cursor: "#e07a4f", cursorAccent: "#0d1117", selectionBackground: "#4a3524", selectionInactiveBackground: "#3a2b1f" },
    light: { black: "#0f1420", red: "#cf222e", green: "#1a7f37", yellow: "#9a6700", blue: "#0969da", magenta: "#8250df", cyan: "#0a7f8c", white: "#d9dde7", brightBlack: "#6b7384", brightRed: "#e5484d", brightGreen: "#2da44e", brightYellow: "#bf8700", brightBlue: "#218bff", brightMagenta: "#a475f9", brightCyan: "#1b9aaa", brightWhite: "#ffffff", background: "#ffffff", foreground: "#1f2430", cursor: "#b9552d", cursorAccent: "#ffffff", selectionBackground: "#f1dccd", selectionInactiveBackground: "#f6ebe2" },
    onedark: { black: "#282c34", red: "#e06c75", green: "#98c379", yellow: "#e5c07b", blue: "#61afef", magenta: "#c678dd", cyan: "#56b6c2", white: "#abb2bf", brightBlack: "#5c6370", brightRed: "#e06c75", brightGreen: "#98c379", brightYellow: "#e5c07b", brightBlue: "#61afef", brightMagenta: "#c678dd", brightCyan: "#56b6c2", brightWhite: "#ffffff", background: "#282c34", foreground: "#abb2bf", cursor: "#528bff", cursorAccent: "#282c34", selectionBackground: "#3e4451", selectionInactiveBackground: "#353a45" },
    tokyonight: { black: "#15161e", red: "#f7768e", green: "#9ece6a", yellow: "#e0af68", blue: "#7aa2f7", magenta: "#bb9af7", cyan: "#7dcfff", white: "#a9b1d6", brightBlack: "#414868", brightRed: "#f7768e", brightGreen: "#9ece6a", brightYellow: "#e0af68", brightBlue: "#7aa2f7", brightMagenta: "#bb9af7", brightCyan: "#7dcfff", brightWhite: "#c0caf5", background: "#1a1b26", foreground: "#c0caf5", cursor: "#c0caf5", cursorAccent: "#1a1b26", selectionBackground: "#33467c", selectionInactiveBackground: "#283457" },
    catppuccin: { black: "#45475a", red: "#f38ba8", green: "#a6e3a1", yellow: "#f9e2af", blue: "#89b4fa", magenta: "#f5c2e7", cyan: "#94e2d5", white: "#bac2de", brightBlack: "#585b70", brightRed: "#f38ba8", brightGreen: "#a6e3a1", brightYellow: "#f9e2af", brightBlue: "#89b4fa", brightMagenta: "#f5c2e7", brightCyan: "#94e2d5", brightWhite: "#a6adc8", background: "#1e1e2e", foreground: "#cdd6f4", cursor: "#f5e0dc", cursorAccent: "#1e1e2e", selectionBackground: "#45475a", selectionInactiveBackground: "#313244" },
    dracula: { black: "#21222c", red: "#ff5555", green: "#50fa7b", yellow: "#f1fa8c", blue: "#bd93f9", magenta: "#ff79c6", cyan: "#8be9fd", white: "#f8f8f2", brightBlack: "#6272a4", brightRed: "#ff6e6e", brightGreen: "#69ff94", brightYellow: "#ffffa5", brightBlue: "#d6acff", brightMagenta: "#ff92df", brightCyan: "#a4ffff", brightWhite: "#ffffff", background: "#282a36", foreground: "#f8f8f2", cursor: "#f8f8f2", cursorAccent: "#282a36", selectionBackground: "#44475a", selectionInactiveBackground: "#363848" },
    gruvbox: { black: "#282828", red: "#cc241d", green: "#98971a", yellow: "#d79921", blue: "#458588", magenta: "#b16286", cyan: "#689d6a", white: "#a89984", brightBlack: "#928374", brightRed: "#fb4934", brightGreen: "#b8bb26", brightYellow: "#fabd2f", brightBlue: "#83a598", brightMagenta: "#d3869b", brightCyan: "#8ec07c", brightWhite: "#ebdbb2", background: "#282828", foreground: "#ebdbb2", cursor: "#ebdbb2", cursorAccent: "#282828", selectionBackground: "#504945", selectionInactiveBackground: "#3c3836" },
    monokai: { black: "#272822", red: "#f92672", green: "#a6e22e", yellow: "#f4bf75", blue: "#66d9ef", magenta: "#ae81ff", cyan: "#a1efe4", white: "#f8f8f2", brightBlack: "#75715e", brightRed: "#f92672", brightGreen: "#a6e22e", brightYellow: "#f4bf75", brightBlue: "#66d9ef", brightMagenta: "#ae81ff", brightCyan: "#a1efe4", brightWhite: "#f9f8f5", background: "#272822", foreground: "#f8f8f2", cursor: "#f8f8f0", cursorAccent: "#272822", selectionBackground: "#49483e", selectionInactiveBackground: "#3e3d32" },
    solarized: { black: "#073642", red: "#dc322f", green: "#859900", yellow: "#b58900", blue: "#268bd2", magenta: "#d33682", cyan: "#2aa198", white: "#eee8d5", brightBlack: "#586e75", brightRed: "#cb4b16", brightGreen: "#586e75", brightYellow: "#657b83", brightBlue: "#839496", brightMagenta: "#6c71c4", brightCyan: "#93a1a1", brightWhite: "#fdf6e3", background: "#002b36", foreground: "#839496", cursor: "#93a1a1", cursorAccent: "#002b36", selectionBackground: "#073642", selectionInactiveBackground: "#06313d" },
    nord: { black: "#3b4252", red: "#bf616a", green: "#a3be8c", yellow: "#ebcb8b", blue: "#81a1c1", magenta: "#b48ead", cyan: "#88c0d0", white: "#e5e9f0", brightBlack: "#4c566a", brightRed: "#bf616a", brightGreen: "#a3be8c", brightYellow: "#ebcb8b", brightBlue: "#81a1c1", brightMagenta: "#b48ead", brightCyan: "#8fbcbb", brightWhite: "#eceff4", background: "#2e3440", foreground: "#d8dee9", cursor: "#d8dee9", cursorAccent: "#2e3440", selectionBackground: "#434c5e", selectionInactiveBackground: "#3b4252" },
  };
  const isDarkApp = () => document.documentElement.dataset.theme === "dark" || (!document.documentElement.dataset.theme && matchMedia("(prefers-color-scheme: dark)").matches);
  function xtermTheme() {
    const name = settings().theme || "auto";
    if (name !== "auto" && PALETTES[name]) return PALETTES[name];
    return isDarkApp() ? PALETTES.dark : PALETTES.light;
  }
  function xtermOptions() {
    const s = settings();
    return {
      theme: xtermTheme(),
      fontFamily: s.font_family || `"JetBrains Mono", "Cascadia Code", "SF Mono", Menlo, Consolas, ui-monospace, monospace`,
      fontSize: s.font_size || 13, cursorStyle: s.cursor_style || "block", cursorBlink: !!s.cursor_blink,
      scrollback: s.scrollback || 5000, allowProposedApi: true, convertEol: false, macOptionIsMeta: true,
      rightClickSelectsWord: false, allowTransparency: false, drawBoldTextInBrightColors: true,
    };
  }
  function applyThemeToTerms() {
    for (const x of R.terms.values()) { const o = xtermOptions(); x.term.options.theme = o.theme; x.el.style.background = o.theme.background; x.term.options.fontFamily = o.fontFamily; x.term.options.fontSize = o.fontSize; x.term.options.cursorStyle = o.cursorStyle; x.term.options.cursorBlink = o.cursorBlink; x.term.options.scrollback = o.scrollback; try { x.fit.fit(); } catch (e) {} }
  }
  new MutationObserver(() => applyThemeToTerms()).observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
  matchMedia("(prefers-color-scheme: dark)").addEventListener("change", applyThemeToTerms);

  /* ---------------- terminals ---------------- */
  function termTitle(x) { return x.title || x.info?.label || x.label || (x.local ? t("rm.local") : "…"); }
  function renderTabs() {
    const tabs = $("#term-tabs");
    tabs.innerHTML = [...R.terms.values()].map((x) => `<div class="term-tab ${x.id === R.active ? "active" : ""} ${x.closed ? "closed" : ""}" data-term="${x.id}" title="${esc(x.info?.label || "")}">
      <i class="tdot ${x.info?.state || "connecting"}"></i><span class="ttitle">${esc(termTitle(x))}</span><button class="tclose" data-term-close="${x.id}" title="${t("btn.close")}">${ICONS.x}</button></div>`).join("") +
      `<button class="term-add" id="term-add" title="${t("rm.localTerminal")}">${ICONS.plus}</button>`;
    $("#term-empty").classList.toggle("hidden", R.terms.size > 0);
    $("#term-stage").classList.toggle("hidden", R.terms.size === 0);
    renderCounts();
  }
  function barMarkup(x) {
    return `<div class="term-bar">
      <span class="tstate ${x.info?.state || "connecting"}"><i></i><span class="txt">${esc(stateText(x))}</span></span>
      <span class="ttarget mono">${esc(x.info?.label || "")}</span>
      <span class="spacer"></span>
      <button class="btn small term-snip-btn ${R.side.open ? "on" : ""}" data-tact="snippets" title="${t("nav.snippets")} (Ctrl+Shift+S)">${ICONS.snippet}<span>${t("nav.snippets")}</span></button>
      <button class="icon-btn small" data-tact="search" title="${t("rm.search")} (Ctrl+Shift+F)">${ICONS.search}</button>
      <button class="icon-btn small" data-tact="clear" title="${t("btn.clear")}">${ICONS.trash}</button>
      <button class="icon-btn small" data-tact="reconnect" title="${t("rm.reconnect")}">${ICONS.refresh}</button>
      <button class="icon-btn small" data-tact="settings" title="${t("rm.terminalSettings")}">${ICONS.gear}</button>
      <button class="icon-btn small" data-tact="close" title="${t("btn.close")}">${ICONS.x}</button>
    </div>
    <div class="term-stats hidden" data-tact="stats" title="${t("rm.statsHint")}" role="button" tabindex="0"></div>
    <div class="term-search hidden"><input placeholder="${t("rm.search")}"><button class="icon-btn small" data-tact="prev">${ICONS.up}</button><button class="icon-btn small" data-tact="next"><i class="ic ic-flip">${ICONS.up}</i></button><button class="icon-btn small" data-tact="search">${ICONS.x}</button></div>`;
  }
  const stateText = (x) => {
    const s = x.info?.state || "connecting";
    if (s === "connected") return t("status.connected");
    if (s === "closed") return `${t("rm.closed")}${x.info?.message ? " · " + x.info.message : ""}`;
    if (s === "error") return `${t("rm.failed")}: ${x.info?.message || ""}`;
    return t("rm.connecting");
  };
  /* Server stats strip: CPU / RAM / disk meters under the bar of an SSH terminal. One tiny
     command over the existing connection every 10 s, only for the terminal that is visible;
     nothing runs while the tab is hidden, the window is in the background or the user hid it. */
  const statsOn = () => { try { return localStorage.getItem("veranda.termStats") !== "0"; } catch (e) { return true; } };
  const fmtG = (kb) => kb >= 1024 * 1024 ? `${(kb / 1024 / 1024).toFixed(kb >= 10 * 1024 * 1024 ? 0 : 1)} GB` : `${Math.round(kb / 1024)} MB`;
  const fmtUp = (s) => s >= 86400 ? `${Math.floor(s / 86400)}d ${Math.floor((s % 86400) / 3600)}h` : s >= 3600 ? `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m` : `${Math.floor(s / 60)}m`;
  function meter(label, pct, text, title) {
    const p = pct == null ? 0 : Math.max(0, Math.min(100, pct));
    const lvl = pct == null ? "" : p >= 90 ? "bad" : p >= 70 ? "warn" : "";
    return `<span class="tsm ${lvl}" title="${esc(title || "")}"><b>${label}</b><i><s style="width:${p}%"></s></i><span>${esc(text)}</span></span>`;
  }
  // Tooltip: every local filesystem plus the total the way cloud consoles count it (all disks
  // together, used / size), so the number here and the number there can be compared.
  // all local disks together (used / size), the figure cloud consoles usually show
  function allDisks(st) {
    if (!st.disks || st.disks.length < 2) return "";
    const used = st.disks.reduce((a, d) => a + d.used_kb, 0), total = st.disks.reduce((a, d) => a + d.total_kb, 0);
    return total ? ` · ${Math.round((used / total) * 100)}% ${t("rm.statsAll")}` : "";
  }
  function diskTitle(st) {
    if (!st.disks?.length) return "";
    const lines = st.disks.map((d) => `${d.mount}: ${fmtG(d.used_kb)} ${t("rm.statsOf")} ${fmtG(d.total_kb)} (${Math.round((d.used_kb / d.total_kb) * 100)}%)`);
    if (st.disks.length > 1) {
      const used = st.disks.reduce((a, d) => a + d.used_kb, 0), total = st.disks.reduce((a, d) => a + d.total_kb, 0);
      lines.push(`${t("rm.statsAllDisks")}: ${fmtG(used)} ${t("rm.statsOf")} ${fmtG(total)} (${Math.round((used / total) * 100)}%)`);
    }
    return lines.join("\n");
  }
  function renderStats(x) {
    const box = $(".term-stats", x.el); if (!box) return;
    const st = x.stats;
    const show = statsOn() && x.info?.kind !== "local" && !x.closed && !!st;
    const wasHidden = box.classList.contains("hidden");
    box.classList.toggle("hidden", !show);
    // the strip changes the height left for the terminal: recount rows or the bottom lines
    // (the prompt) end up below the edge
    if (wasHidden === show) requestAnimationFrame(() => { try { x.fit.fit(); x.term.scrollToBottom(); } catch (e) {} });
    if (!show) return;
    const cpuT = st.load1 != null ? `${t("rm.statsLoad")} ${st.load1.toFixed(2)}${st.cores ? ` · ${st.cores} ${t("rm.statsCores")}` : ""}` : "";
    const memPct = st.mem_total_kb ? (st.mem_used_kb / st.mem_total_kb) * 100 : null;
    const diskPct = st.disk_total_kb ? (st.disk_used_kb / st.disk_total_kb) * 100 : null;
    box.innerHTML = meter("CPU", st.cpu, st.cpu == null ? "…" : `${st.cpu.toFixed(0)}%`, cpuT)
      + meter("RAM", memPct, st.mem_total_kb ? `${memPct.toFixed(0)}% · ${fmtG(st.mem_used_kb)} / ${fmtG(st.mem_total_kb)}` : "—", st.mem_total_kb ? `${fmtG(st.mem_used_kb)} ${t("rm.statsOf")} ${fmtG(st.mem_total_kb)} (${t("rm.statsMemNote")})` : "")
      + meter("Disk", diskPct, st.disk_total_kb ? `${diskPct.toFixed(0)}% ${st.disk_mount || "/"}${allDisks(st)}` : "—", diskTitle(st))
      + (st.uptime_secs ? `<span class="tsu">${t("rm.statsUp")} ${fmtUp(st.uptime_secs)}</span>` : "");
  }
  let statsTimer = null;
  async function pollStats(force) {
    const x = R.terms.get(R.active);
    if (!x || x.closed || x.info?.kind === "local" || x.info?.state !== "connected") return;
    if (!statsOn() || document.visibilityState !== "visible" || !$("#tab-terminal").classList.contains("active")) return;
    if (x.statsBusy) return;
    x.statsBusy = true;
    try {
      const st = await invoke("term_stats", { id: x.id });
      if (st) { x.stats = st; renderStats(x); if (st.cpu == null && !force) setTimeout(() => pollStats(true), 3000); }
    } catch (e) { /* a server without /proc or a dropped channel: keep the last values */ }
    finally { x.statsBusy = false; }
  }
  function scheduleStats() { clearInterval(statsTimer); statsTimer = setInterval(() => pollStats(false), 5000); setTimeout(() => pollStats(false), 400); }
  function updateBar(x) {
    renderStats(x);
    const bar = $(".term-bar", x.el); if (!bar) return;
    const st = $(".tstate", bar); st.className = `tstate ${x.info?.state || "connecting"}`; $(".txt", st).textContent = stateText(x);
    $(".ttarget", bar).textContent = x.info?.label || "";
  }
  async function openTerminal(opts) {
    const id = newId();
    const el = document.createElement("div"); el.className = "term-box"; el.dataset.term = id;
    el.innerHTML = `<div class="term-head"></div><div class="term-view"></div>`;
    $(".term-head", el).innerHTML = "";
    $("#term-stage").appendChild(el);
    const term = new Terminal(xtermOptions());
    const fit = new FitAddon.FitAddon(), search = new SearchAddon.SearchAddon();
    term.loadAddon(fit); term.loadAddon(search);
    term.loadAddon(new WebLinksAddon.WebLinksAddon((ev, uri) => invoke("remote_open_link", { url: uri }).catch(fail)));
    try { term.loadAddon(new Unicode11Addon.Unicode11Addon()); term.unicode.activeVersion = "11"; } catch (e) {}
    const x = { id, term, fit, search, el, title: "", info: { state: "connecting", label: opts.label || hostById(opts.hostId)?.label || (opts.local ? t("rm.local") : ""), target: opts.quick || (hostById(opts.hostId) ? hostSummary(hostById(opts.hostId)) : "") }, hostId: opts.hostId || null, quick: opts.quick || null, local: !!opts.local, closed: false };
    R.terms.set(id, x);
    $(".term-head", el).innerHTML = barMarkup(x);
    el.style.background = xtermTheme().background;
    term.open($(".term-view", el));
    new ResizeObserver(() => { if (x.el.classList.contains("active")) { try { fit.fit(); } catch (e) {} } }).observe($(".term-view", el));
    term.onData((d) => { if (!x.closed) invoke("term_write", { id, data: d }).catch(() => {}); });
    term.onResize(({ cols, rows }) => { if (!x.closed) invoke("term_resize", { id, cols, rows }).catch(() => {}); });
    term.onTitleChange((title) => { x.title = title; renderTabs(); });
    term.onSelectionChange(() => { if (settings().copy_on_select && term.hasSelection()) navigator.clipboard.writeText(term.getSelection()).catch(() => {}); });
    term.onBell(() => { if (settings().bell) { try { const ctx = new AudioContext(); const o = ctx.createOscillator(); o.frequency.value = 880; o.connect(ctx.destination); o.start(); o.stop(ctx.currentTime + 0.08); } catch (e) {} } });
    term.attachCustomKeyEventHandler((ev) => {
      if (ev.type !== "keydown") return true;
      if (ev.ctrlKey && ev.shiftKey && ev.code === "KeyC") { if (term.hasSelection()) navigator.clipboard.writeText(term.getSelection()).catch(() => {}); return false; }
      if (ev.ctrlKey && ev.shiftKey && ev.code === "KeyV") { navigator.clipboard.readText().then((tx) => tx && term.paste(tx)).catch(() => {}); return false; }
      if (ev.ctrlKey && ev.shiftKey && ev.code === "KeyF") { toggleSearch(x, true); return false; }
      if (ev.ctrlKey && ev.shiftKey && ev.code === "KeyW") { closeTerminal(id); return false; }
      if (ev.ctrlKey && ev.shiftKey && ev.code === "KeyT") { openTerminal({ local: true }); return false; }
      if (ev.ctrlKey && ev.shiftKey && ev.code === "KeyS") { toggleSnipPanel(); return false; }
      return true;
    });
    $(".term-view", el).addEventListener("contextmenu", (ev) => {
      if (!settings().right_click_paste) return;
      ev.preventDefault();
      if (term.hasSelection()) { navigator.clipboard.writeText(term.getSelection()).catch(() => {}); term.clearSelection(); }
      else navigator.clipboard.readText().then((tx) => tx && term.paste(tx)).catch(() => {});
    });
    activateTerminal(id);
    showTab("terminal");
    await new Promise((r) => requestAnimationFrame(r));
    try { fit.fit(); } catch (e) {}
    term.focus();
    try {
      const info = await invoke("term_open", { req: { id, host_id: opts.hostId || null, quick: opts.quick || null, local: !!opts.local, cols: term.cols, rows: term.rows, cwd: opts.cwd || null } });
      x.info = { ...x.info, ...info };
    } catch (e) {
      x.info.state = "error"; x.info.message = String(e?.message || e); x.closed = true;
      term.write(`\r\n\x1b[31m${t("rm.failed")}: ${String(e?.message || e).replace(/\r?\n/g, "\r\n")}\x1b[0m\r\n`);
      toast(`${x.info.label}: ${e?.message || e}`, "bad");
    }
    updateBar(x); renderTabs();
    return id;
  }
  function activateTerminal(id) {
    R.active = id;
    scheduleStats();
    for (const x of R.terms.values()) x.el.classList.toggle("active", x.id === id);
    renderTabs();
    const x = R.terms.get(id);
    if (x) requestAnimationFrame(() => { try { x.fit.fit(); } catch (e) {} x.term.focus(); });
  }
  async function closeTerminal(id, force) {
    const x = R.terms.get(id); if (!x) return;
    if (!x.closed && !force && settings().confirm_close && !(await askConfirm(t("rm.confirmCloseSession", { name: termTitle(x) }), t("btn.close")))) return;
    if (!x.closed) invoke("term_close", { id }).catch(() => {});
    x.term.dispose(); x.el.remove(); R.terms.delete(id);
    if (R.active === id) { const next = [...R.terms.keys()].pop(); R.active = null; if (next) activateTerminal(next); }
    renderTabs();
  }
  function toggleSearch(x, show) {
    const box = $(".term-search", x.el);
    const on = show ?? box.classList.contains("hidden");
    box.classList.toggle("hidden", !on);
    if (on) $("input", box).focus(); else { x.search.clearDecorations(); x.term.focus(); }
  }
  async function reconnect(x) {
    if (!x.closed) return;
    const opts = x.local ? { local: true } : x.hostId ? { hostId: x.hostId } : { quick: x.quick, label: x.info.label };
    await closeTerminal(x.id, true);
    openTerminal(opts);
  }
  $("#term-tabs").addEventListener("click", (ev) => {
    const c = ev.target.closest("[data-term-close]"); if (c) { closeTerminal(c.dataset.termClose); return; }
    if (ev.target.closest("#term-add")) { openTerminal({ local: true }); return; }
    const tab = ev.target.closest("[data-term]"); if (tab) activateTerminal(tab.dataset.term);
  });
  $("#term-tabs").addEventListener("auxclick", (ev) => { const tab = ev.target.closest("[data-term]"); if (tab && ev.button === 1) closeTerminal(tab.dataset.term); });
  $("#term-stage").addEventListener("click", async (ev) => {
    const strip = ev.target.closest(".term-stats");
    if (strip) { try { localStorage.setItem("veranda.termStats", "0"); } catch (e) {} strip.classList.add("hidden"); toast(t("rm.statsHint"), "info"); return; }
    if ($("#term-stage").classList.contains("narrow") && R.side.open && ev.target.closest(".term-view")) { toggleSnipPanel(false); }
    const box = ev.target.closest(".term-box"); if (!box) return;
    const x = R.terms.get(box.dataset.term); if (!x) return;
    const b = ev.target.closest("[data-tact]"); if (!b) return;
    switch (b.dataset.tact) {
      case "search": toggleSearch(x); break;
      case "snippets": toggleSnipPanel(); break;
      case "next": x.search.findNext($(".term-search input", x.el).value); break;
      case "prev": x.search.findPrevious($(".term-search input", x.el).value); break;
      case "clear": x.term.clear(); x.term.focus(); break;
      case "reconnect": if (x.closed) reconnect(x); else toast(t("rm.stillConnected"), "info"); break;
      case "settings": openTermSettings(); break;
      case "close": closeTerminal(x.id); break;
    }
  });
  $("#term-stage").addEventListener("input", (ev) => {
    const inp = ev.target.closest(".term-search input"); if (!inp) return;
    const x = R.terms.get(ev.target.closest(".term-box").dataset.term);
    x.search.findNext(inp.value, { incremental: true });
  });
  $("#term-stage").addEventListener("keydown", (ev) => {
    const inp = ev.target.closest?.(".term-search input"); if (!inp) return;
    const x = R.terms.get(ev.target.closest(".term-box").dataset.term);
    if (ev.key === "Enter") { ev.preventDefault(); if (ev.shiftKey) x.search.findPrevious(inp.value); else x.search.findNext(inp.value); }
    if (ev.key === "Escape") toggleSearch(x, false);
  });
  /* Snippets panel (Termius-like): stays open beside the terminal; a click types the snippet
     into the active session. Names only; the body is sent by the backend and never shown here. */
  function toggleSnipPanel(show) {
    R.side.open = show ?? !R.side.open;
    try { localStorage.setItem("veranda.snipPanel", R.side.open ? "1" : "0"); } catch (e) {}
    $("#term-side").classList.toggle("hidden", !R.side.open);
    $$(".term-snip-btn").forEach((b) => b.classList.toggle("on", R.side.open));
    renderSnipPanel();
    const x = R.terms.get(R.active);
    requestAnimationFrame(() => { try { x?.fit.fit(); } catch (e) {} if (R.side.open) $("#snip-side-q")?.focus(); else x?.term.focus(); });
  }
  function lockScreen(compact) {
    return `<form class="snip-lock ${compact ? "compact" : ""}"><div class="ic">${ICONS.lock}</div><h3>${t("rm.snipLocked")}</h3><p>${t("rm.snipLockedHint")}</p>
      <div class="row"><input type="password" name="password" placeholder="${t("db.password")}" autocomplete="current-password" required><button class="btn primary" type="submit">${ICONS.unlock}<span>${t("rm.unlock")}</span></button></div></form>`;
  }
  document.addEventListener("submit", async (ev) => {
    const f = ev.target.closest?.(".snip-lock"); if (!f) return;
    ev.preventDefault();
    const inp = f.elements.password, pw = inp.value; if (!pw) return;
    const btn = $("button", f); btn.disabled = true;
    try {
      const ok = await invoke("snippet_unlock", { password: pw });
      if (ok) { toast(t("rm.unlocked"), "ok"); await refresh(); }
      else { inp.value = ""; f.classList.remove("shake"); void f.offsetWidth; f.classList.add("shake"); toast(t("rm.pwWrong"), "bad"); inp.focus(); }
    } catch (e) { fail(e); }
    btn.disabled = false;
  });
  function renderSnipPanel() {
    const side = $("#term-side"); if (!side || !R.view) return;
    side.classList.toggle("hidden", !R.side.open);
    if (!R.side.open) return;
    const head = `<div class="ts-head">${ICONS.snippet}<span>${t("nav.snippets")}</span><span class="spacer"></span>
      ${lockState() === "unlocked" ? `<button class="icon-btn small" data-side="lock" title="${t("rm.lockNow")}">${ICONS.lock}</button>` : ""}
      <button class="icon-btn small" data-side="manage" title="${t("rm.manageSnippets")}">${ICONS.gear}</button>
      <button class="icon-btn small" data-side="close" title="${t("btn.close")}">${ICONS.x}</button></div>`;
    if (lockState() === "locked") { side.innerHTML = head + lockScreen(true); return; }
    const snippets = R.view.store.snippets;
    const folders = [...new Set(snippets.map((s) => s.folder || ""))].sort((a, b) => (a === "") - (b === "") || a.localeCompare(b));
    const list = snippets.length ? folders.map((fo) => `<div class="ts-group" data-folder="${esc(fo)}">${fo ? `<div class="ts-folder">${esc(fo)}</div>` : ""}${snippets.filter((s) => (s.folder || "") === fo).map((s) => `<button class="snip-item" data-side-send="${esc(s.id)}" data-q="${esc((s.folder + " " + s.label).toLowerCase())}" title="${t("rm.sendToTerminal")}">${ICONS.snippet}<span class="nm">${esc(s.label)}</span><span class="go">${ICONS.send}</span></button>`).join("")}</div>`).join("")
      : `<div class="empty small">${t("rm.noSnippets")}</div>`;
    side.innerHTML = head + `<div class="ts-search">${ICONS.search}<input id="snip-side-q" placeholder="${t("rm.searchSnippets")}" value="${esc(R.side.q)}"></div><div class="ts-body">${list}</div>
      <label class="ts-foot check"><input type="checkbox" id="snip-side-enter" ${R.side.enter ? "checked" : ""}><span>${t("rm.sendEnter")}</span></label>`;
    applySideFilter();
  }
  function applySideFilter() {
    const q = R.side.q.trim().toLowerCase();
    for (const g of $$("#term-side .ts-group")) {
      let shown = 0;
      for (const b of $$(".snip-item", g)) { const hit = !q || b.dataset.q.includes(q); b.classList.toggle("hidden", !hit); shown += hit; }
      g.classList.toggle("hidden", !shown);
    }
  }
  async function sendSnippet(id, fromPage) {
    const x = R.terms.get(R.active);
    if (!x || x.closed) { toast(t("rm.noTerminal"), "info"); return false; }
    try { await invoke("term_send_snippet", { id: x.id, snippetId: id, enter: R.side.enter }); }
    catch (e) { fail(e); if (/locked/i.test(String(e?.message || e))) refresh(); return false; }
    if (fromPage) showTab("terminal");
    x.term.focus();
    return true;
  }
  $("#term-side").addEventListener("click", async (ev) => {
    const a = ev.target.closest("[data-side]");
    if (a) {
      if (a.dataset.side === "close") toggleSnipPanel(false);
      else if (a.dataset.side === "manage") showTab("snippets");
      else if (a.dataset.side === "lock") { try { await invoke("snippet_lock_now"); toast(t("rm.locked"), "ok"); } catch (e) { fail(e); } refresh(); }
      return;
    }
    const b = ev.target.closest("[data-side-send]"); if (!b) return;
    if (await sendSnippet(b.dataset.sideSend)) { b.classList.add("sent"); setTimeout(() => b.classList.remove("sent"), 700); }
  });
  $("#term-side").addEventListener("input", (ev) => {
    if (ev.target.id === "snip-side-q") { R.side.q = ev.target.value; applySideFilter(); }
  });
  $("#term-side").addEventListener("change", (ev) => {
    if (ev.target.id === "snip-side-enter") { R.side.enter = ev.target.checked; try { localStorage.setItem("veranda.snipEnter", R.side.enter ? "1" : "0"); } catch (e) {} }
  });
  $("#term-side").addEventListener("keydown", (ev) => {
    if (ev.target.id === "snip-side-q" && ev.key === "Enter") { ev.preventDefault(); const first = $("#term-side .snip-item:not(.hidden)"); first?.click(); }
    if (ev.key === "Escape") toggleSnipPanel(false);
  });
  $("#btn-term-local").onclick = () => openTerminal({ local: true });
  new ResizeObserver((entries) => {
    const w = entries[0]?.contentRect?.width || 0;
    $("#term-stage").classList.toggle("narrow", w > 0 && w < 640);
    $("#term-stage").classList.toggle("tight", w > 0 && w < 440);
    const x = R.terms.get(R.active); if (x && $("#tab-terminal").classList.contains("active")) { try { x.fit.fit(); } catch (e) {} }
  }).observe($("#term-stage"));
  document.addEventListener("visibilitychange", () => { if (document.visibilityState === "visible") pollStats(false); });
  document.addEventListener("veranda:tab", (ev) => {
    if (ev.detail === "terminal") { const x = R.terms.get(R.active); if (x) requestAnimationFrame(() => { try { x.fit.fit(); } catch (e) {} x.term.focus(); }); }
    else if (["hosts", "forward", "snippets", "keychain", "history", "sftp"].includes(ev.detail)) refresh();
    if (ev.detail === "sftp" && !R.panes.local.path) loadPane("local", settings().sftp_local_root || "");
    if (ev.detail === "knownhosts") loadKnownHosts();
  });
  listen("term-out", (ev) => {
    const x = R.terms.get(ev.payload.id); if (!x) return;
    const atBottom = x.term.buffer.active.viewportY >= x.term.buffer.active.baseY;
    x.term.write(b64(ev.payload.data), () => { if (atBottom) x.term.scrollToBottom(); });
  });
  listen("term-state", (ev) => {
    const x = R.terms.get(ev.payload.id); if (!x) return;
    x.info = { ...x.info, ...ev.payload };
    if (ev.payload.state === "connected") scheduleStats();
    if (ev.payload.state === "closed" || ev.payload.state === "error") {
      if (!x.closed) x.term.write(`\r\n\x1b[2m[${t("rm.sessionEnded")}${ev.payload.message ? ": " + ev.payload.message : ""}] ${t("rm.pressReconnect")}\x1b[0m\r\n`);
      x.closed = true;
    }
    updateBar(x); renderTabs();
  });
  listen("term-exit", (ev) => { const x = R.terms.get(ev.payload.id); if (x) { x.closed = true; renderTabs(); } });
  listen("remote-notice", (ev) => {
    const x = R.terms.get(ev.payload.session);
    if (x) x.term.write(`\x1b[2m${String(ev.payload.text).replace(/\r?\n/g, "\r\n")}\x1b[0m\r\n`); else toast(ev.payload.text, "info");
  });
  listen("remote-changed", () => refresh());

  /* ---------------- prompts (host key, password, passphrase, OTP) ---------------- */
  listen("remote-prompt", (ev) => { R.promptQueue.push(ev.payload); showNextPrompt(); });
  listen("remote-prompt-done", (ev) => {
    R.promptQueue = R.promptQueue.filter((p) => p.id !== ev.payload);
    if (R.promptOpen?.id === ev.payload) { R.promptOpen = null; $("#dlg-prompt").close(); showNextPrompt(); }
  });
  function showNextPrompt() {
    if (R.promptOpen || !R.promptQueue.length) return;
    const p = R.promptQueue.shift(); R.promptOpen = p;
    const f = $("#form-prompt"); f.reset();
    const fp = $("#prompt-fp");
    $("#prompt-title").textContent = p.kind === "hostkey" ? (p.status === "changed" ? t("rm.hostKeyChanged") : t("rm.hostKeyNew")) : p.title || "";
    if (p.kind === "hostkey") {
      $("#prompt-text").textContent = p.status === "changed" ? t("rm.hostKeyChangedText", { host: `${p.host}:${p.port}` }) : t("rm.hostKeyNewText", { host: `${p.host}:${p.port}` });
      fp.innerHTML = `<b>${esc(p.algorithm)}</b><code>${esc(p.fingerprint)}</code>`; fp.classList.remove("hidden"); fp.classList.toggle("warn", p.status === "changed");
      $("#prompt-input-row").classList.add("hidden"); $("#prompt-remember-row").classList.add("hidden");
      $("#btn-prompt-ok").textContent = p.status === "changed" ? t("rm.trustAnyway") : t("rm.trust");
      $("#btn-prompt-ok").classList.toggle("danger", p.status === "changed");
    } else {
      // title: the host; subtitle: the server's/our full sentence; label: just what to type
      $("#prompt-text").textContent = p.prompt || ""; fp.classList.add("hidden");
      $("#prompt-input-row").classList.remove("hidden");
      $("#prompt-label").textContent = p.kind === "passphrase" ? t("rm.passphrase") : p.kind === "interactive" ? t("rm.answer") : t("db.password");
      f.elements.value.type = p.echo ? "text" : "password";
      $("#prompt-remember-row").classList.toggle("hidden", !p.can_remember);
      $("#btn-prompt-ok").textContent = "OK"; $("#btn-prompt-ok").classList.remove("danger");
    }
    const dlg = $("#dlg-prompt"); if (!dlg.open) dlg.showModal();
    if (p.kind !== "hostkey") setTimeout(() => f.elements.value.focus(), 50);
  }
  async function answerPrompt(accept) {
    const p = R.promptOpen; if (!p) return;
    const f = $("#form-prompt");
    R.promptOpen = null; $("#dlg-prompt").close();
    try { await invoke("remote_prompt_answer", { id: p.id, accept, value: accept ? f.elements.value.value : null, remember: accept && f.elements.remember.checked }); } catch (e) {}
    showNextPrompt();
  }
  $("#form-prompt").onsubmit = (ev) => { ev.preventDefault(); answerPrompt(true); };
  $("#btn-prompt-cancel").onclick = () => answerPrompt(false);
  $("#dlg-prompt").oncancel = (ev) => { ev.preventDefault(); answerPrompt(false); };

  /* ---------------- terminal settings ---------------- */
  function openTermSettings() {
    const f = $("#form-term-settings"), s = settings();
    for (const el of f.elements) { if (!el.name) continue; if (el.type === "checkbox") el.checked = !!s[el.name]; else el.value = s[el.name] ?? ""; }
    $("#term-stats-on").checked = statsOn();
    $("#dlg-term-settings").showModal();
  }
  $("#form-term-settings").onsubmit = async (ev) => {
    ev.preventDefault(); const s = { ...settings() };
    for (const el of ev.target.elements) { if (!el.name) continue; s[el.name] = el.type === "checkbox" ? el.checked : el.type === "number" ? Number(el.value) : el.value; }
    try { localStorage.setItem("veranda.termStats", $("#term-stats-on").checked ? "1" : "0"); } catch (e) {}
    for (const x of R.terms.values()) renderStats(x);
    scheduleStats();
    try { await invoke("remote_save_settings", { settings: s }); $("#dlg-term-settings").close(); } catch (e) { fail(e); }
    await refresh(); applyThemeToTerms(); renderPane("local"); renderPane("remote");
  };

  /* ---------------- SFTP ---------------- */
  function renderSftpHosts() {
    const sel = $("#sftp-host"); const cur = sel.value;
    sel.innerHTML = hostsOf().map((h) => `<option value="${esc(h.id)}">${esc(h.label)} · ${esc(hostSummary(h))}</option>`).join("") || `<option value="">${t("rm.noHosts")}</option>`;
    if (cur) sel.value = cur;
  }
  async function sftpConnect() {
    const hostId = $("#sftp-host").value; if (!hostId) { toast(t("rm.noHosts"), "info"); return; }
    if (R.sftp) await sftpDisconnect();
    const b = $("#btn-sftp-connect"); b.disabled = true; b.classList.add("busy");
    try {
      R.sftp = await invoke("sftp_open", { hostId });
      $("#btn-sftp-disconnect").disabled = false; $("#btn-sftp-upload").disabled = false; $("#btn-sftp-download").disabled = false;
      await Promise.all([loadPane("remote", R.sftp.home), R.panes.local.path ? Promise.resolve() : loadPane("local", settings().sftp_local_root || "")]);
      toast(t("rm.sftpConnected", { name: R.sftp.label }), "ok");
    } catch (e) { fail(e); R.sftp = null; renderPane("remote"); }
    b.disabled = false; b.classList.remove("busy");
  }
  async function sftpDisconnect() {
    if (R.sftp) { try { await invoke("sftp_close", { id: R.sftp.id }); } catch (e) {} }
    R.sftp = null; R.panes.remote = { path: "", entries: [], sel: new Set() };
    $("#btn-sftp-disconnect").disabled = true; $("#btn-sftp-upload").disabled = true; $("#btn-sftp-download").disabled = true;
    renderPane("remote");
  }
  async function loadPane(side, path) {
    const pane = R.panes[side];
    try {
      const r = side === "local" ? await invoke("local_list", { path: path ?? pane.path }) : await invoke("sftp_list", { id: R.sftp.id, path: path ?? pane.path });
      pane.path = r.path; pane.entries = r.entries; pane.roots = r.roots; pane.sel = new Set();
    } catch (e) { fail(e); }
    renderPane(side);
  }
  function paneUp(side) {
    const p = R.panes[side].path; if (!p) return;
    if (side === "local") { const sep = p.includes("\\") && !p.includes("/") ? "\\" : "/"; const parts = p.split(sep).filter(Boolean); if (parts.length <= (sep === "\\" ? 1 : 0)) { loadPane(side, sep === "\\" ? parts[0] + "\\" : "/"); return; } parts.pop(); loadPane(side, sep === "\\" ? (parts.length === 1 ? parts[0] + "\\" : parts.join("\\")) : "/" + parts.join("/")); }
    else loadPane(side, p.replace(/\/+$/, "").split("/").slice(0, -1).join("/") || "/");
  }
  function renderPane(side) {
    const pane = R.panes[side], el = $(`#pane-${side}`), connected = side === "local" || !!R.sftp;
    const showHidden = !!settings().show_hidden;
    const entries = pane.entries.filter((e) => showHidden || !e.hidden);
    el.innerHTML = `<div class="pane-head">
        <span class="pane-title">${side === "local" ? ICONS.local + `<b>${t("rm.thisComputer")}</b>` : ICONS.host + `<b>${esc(R.sftp?.label || t("rm.remote"))}</b>`}</span>
        <span class="spacer"></span>
        <button class="icon-btn small" data-pact="up" title="${t("rm.up")}" ${connected ? "" : "disabled"}>${ICONS.up}</button>
        <button class="icon-btn small" data-pact="home" title="Home" ${connected ? "" : "disabled"}>${ICONS.home}</button>
        <button class="icon-btn small" data-pact="refresh" title="${t("btn.refresh")}" ${connected ? "" : "disabled"}>${ICONS.refresh}</button>
        <button class="icon-btn small" data-pact="mkdir" title="${t("rm.newFolder")}" ${connected ? "" : "disabled"}>${ICONS.folder}</button>
        ${side === "local" ? `<button class="icon-btn small" data-pact="open" title="${t("btn.open")}">${ICONS.open}</button>` : ""}
      </div>
      <form class="pane-path"><input name="path" value="${esc(pane.path)}" spellcheck="false" ${connected ? "" : "disabled"}></form>
      <div class="pane-list" ${connected ? "" : "data-off"}>${connected ? (entries.map((e) => `<div class="prow ${pane.sel.has(e.path) ? "sel" : ""} k-${e.kind}" data-path="${esc(e.path)}" data-kind="${e.kind}" draggable="true">
          <span class="pico">${e.kind === "dir" ? ICONS.folder : e.kind === "link" ? ICONS.link : ICONS.file}</span><span class="pname">${esc(e.name)}</span><span class="psize">${e.kind === "dir" ? "" : fmtSize(e.size)}</span><span class="ptime">${fmtTime(e.mtime)}</span>
          <span class="pact"><button class="icon-btn small" data-pact="rename" title="${t("rm.rename")}">${ICONS.edit}</button>${side === "remote" && e.kind === "file" ? `<button class="icon-btn small" data-pact="edit" title="${t("btn.edit")}">${ICONS.file}</button>` : ""}${side === "remote" ? `<button class="icon-btn small" data-pact="chmod" title="chmod">${ICONS.lock}</button>` : ""}<button class="icon-btn small danger" data-pact="delete" title="${t("btn.remove")}">${ICONS.trash}</button></span>
        </div>`).join("") || `<div class="empty small">${t("rm.emptyFolder")}</div>`) : `<div class="empty small">${t("rm.sftpOff")}</div>`}</div>
      <div class="pane-foot">${connected ? t("rm.items", { n: entries.length, sel: pane.sel.size }) : ""}</div>`;
  }
  async function paneAction(side, act, row) {
    const pane = R.panes[side], path = row?.dataset.path;
    const isLocal = side === "local";
    const name = path ? path.split(/[\\/]/).pop() : "";
    switch (act) {
      case "up": paneUp(side); break;
      case "home": loadPane(side, isLocal ? R.view.home : R.sftp.home); break;
      case "refresh": loadPane(side); break;
      case "open": invoke("open_target", { what: `path:${pane.path}` }).catch(fail); break;
      case "mkdir": {
        const n = await askText(t("rm.newFolder"), t("rm.name"), ""); if (!n) return;
        const full = joinPath(side, pane.path, n);
        try { if (isLocal) await invoke("local_mkdir", { path: full }); else await invoke("sftp_mkdir", { id: R.sftp.id, path: full }); } catch (e) { fail(e); }
        loadPane(side); break;
      }
      case "rename": {
        const n = await askText(t("rm.rename"), t("rm.name"), name); if (!n || n === name) return;
        const to = joinPath(side, pane.path, n);
        try { if (isLocal) await invoke("local_rename", { from: path, to }); else await invoke("sftp_rename", { id: R.sftp.id, from: path, to }); } catch (e) { fail(e); }
        loadPane(side); break;
      }
      case "delete": {
        if (!(await askConfirm(t("rm.confirmDeleteFile", { name })))) return;
        try { if (isLocal) await invoke("local_delete", { path }); else await invoke("sftp_delete", { id: R.sftp.id, path }); } catch (e) { fail(e); }
        loadPane(side); break;
      }
      case "chmod": {
        const e = pane.entries.find((x) => x.path === path);
        const n = await askText(`chmod ${name}`, t("rm.mode"), (e?.mode || 0).toString(8).padStart(3, "0")); if (!n) return;
        try { await invoke("sftp_chmod", { id: R.sftp.id, path, mode: parseInt(n, 8) }); } catch (x) { fail(x); }
        loadPane(side); break;
      }
      case "edit": {
        try {
          const text = await invoke("sftp_read", { id: R.sftp.id, path });
          const edited = await askEditor(name, text); if (edited === null) return;
          await invoke("sftp_write", { id: R.sftp.id, path, text: edited }); toast(t("msg.saved2"), "ok"); loadPane(side);
        } catch (e) { fail(e); }
        break;
      }
    }
  }
  const joinPath = (side, dir, name) => side === "local" ? (dir.endsWith("/") || dir.endsWith("\\") ? dir + name : dir + (dir.includes("\\") ? "\\" : "/") + name) : (dir.endsWith("/") ? dir + name : dir + "/" + name);
  function wirePane(side) {
    const el = $(`#pane-${side}`);
    el.addEventListener("click", (ev) => {
      const b = ev.target.closest("[data-pact]"); const row = ev.target.closest(".prow");
      if (b) { ev.stopPropagation(); paneAction(side, b.dataset.pact, row); return; }
      if (!row) return;
      const pane = R.panes[side];
      if (ev.detail === 2) { if (row.dataset.kind === "dir") loadPane(side, row.dataset.path); else if (side === "remote") paneAction(side, "edit", row); return; }
      if (ev.ctrlKey || ev.metaKey) { pane.sel.has(row.dataset.path) ? pane.sel.delete(row.dataset.path) : pane.sel.add(row.dataset.path); }
      else if (ev.shiftKey && pane.sel.size) { const rows = $$(".prow", el).map((r) => r.dataset.path); const last = [...pane.sel].pop(); const [a, c] = [rows.indexOf(last), rows.indexOf(row.dataset.path)].sort((p, q) => p - q); rows.slice(a, c + 1).forEach((p) => pane.sel.add(p)); }
      else { pane.sel = new Set([row.dataset.path]); }
      $$(".prow", el).forEach((r) => r.classList.toggle("sel", pane.sel.has(r.dataset.path)));
      $(".pane-foot", el).textContent = t("rm.items", { n: $$(".prow", el).length, sel: pane.sel.size });
    });
    el.addEventListener("submit", (ev) => { ev.preventDefault(); loadPane(side, ev.target.elements.path.value.trim()); });
    el.addEventListener("dragstart", (ev) => { const row = ev.target.closest(".prow"); if (!row) return; const pane = R.panes[side]; if (!pane.sel.has(row.dataset.path)) { pane.sel = new Set([row.dataset.path]); renderPane(side); } ev.dataTransfer.setData("text/plain", JSON.stringify({ side, paths: [...pane.sel] })); ev.dataTransfer.effectAllowed = "copy"; });
    el.addEventListener("dragover", (ev) => { ev.preventDefault(); el.classList.add("drop"); });
    el.addEventListener("dragleave", () => el.classList.remove("drop"));
    el.addEventListener("drop", (ev) => {
      ev.preventDefault(); el.classList.remove("drop");
      try { const d = JSON.parse(ev.dataTransfer.getData("text/plain")); if (d.side !== side) transfer(d.side === "local" ? "upload" : "download", d.paths); } catch (e) {}
    });
    el.addEventListener("keydown", (ev) => { if (ev.key === "Backspace") paneUp(side); if (ev.key === "F5") loadPane(side); });
  }
  wirePane("local"); wirePane("remote");
  $("#btn-sftp-connect").onclick = sftpConnect;
  $("#btn-sftp-disconnect").onclick = sftpDisconnect;
  $("#btn-sftp-upload").onclick = () => transfer("upload", [...R.panes.local.sel]);
  $("#btn-sftp-download").onclick = () => transfer("download", [...R.panes.remote.sel]);

  async function transfer(dir, paths) {
    if (!R.sftp) { toast(t("rm.sftpOff"), "info"); return; }
    if (!paths.length) { toast(t("rm.selectFirst"), "info"); return; }
    const srcSide = dir === "upload" ? "local" : "remote", dstSide = dir === "upload" ? "remote" : "local";
    const src = R.panes[srcSide], dst = R.panes[dstSide];
    const jobs = [];
    const walk = async (p, relDir) => {
      const e = src.entries.find((x) => x.path === p) || (await statViaList(srcSide, p));
      if (!e) return;
      if (e.kind === "dir") {
        const r = srcSide === "local" ? await invoke("local_list", { path: p }) : await invoke("sftp_list", { id: R.sftp.id, path: p });
        const sub = joinPath(dstSide, relDir, e.name);
        jobs.push({ mkdir: sub });
        for (const c of r.entries) await walkEntry(c, sub);
      } else jobs.push({ src: p, dst: joinPath(dstSide, relDir, e.name), name: e.name, size: e.size });
    };
    const walkEntry = async (e, relDir) => {
      if (e.kind === "dir") {
        const r = srcSide === "local" ? await invoke("local_list", { path: e.path }) : await invoke("sftp_list", { id: R.sftp.id, path: e.path });
        const sub = joinPath(dstSide, relDir, e.name); jobs.push({ mkdir: sub });
        for (const c of r.entries) await walkEntry(c, sub);
      } else jobs.push({ src: e.path, dst: joinPath(dstSide, relDir, e.name), name: e.name, size: e.size });
    };
    try { for (const p of paths) await walk(p, dst.path); } catch (e) { fail(e); return; }
    for (const j of jobs) {
      if (j.mkdir) { try { if (dstSide === "local") await invoke("local_mkdir", { path: j.mkdir }); else await invoke("sftp_mkdir", { id: R.sftp.id, path: j.mkdir }); } catch (e) {} continue; }
      const id = newId();
      const tr = { id, name: j.name, dir, done: 0, total: j.size, state: "running" };
      R.transfers.unshift(tr); renderTransfers();
      try {
        if (dir === "upload") await invoke("sftp_upload", { id: R.sftp.id, local: j.src, remote: j.dst, transfer: id });
        else await invoke("sftp_download", { id: R.sftp.id, remote: j.src, local: j.dst, transfer: id });
        tr.state = "done"; tr.done = tr.total;
      } catch (e) { tr.state = /cancel/i.test(String(e)) ? "cancelled" : "error"; tr.error = String(e?.message || e); if (tr.state === "error") toast(`${j.name}: ${tr.error}`, "bad"); }
      renderTransfers();
    }
    loadPane(dstSide);
  }
  async function statViaList(side, p) {
    const dir = side === "local" ? p.replace(/[\\/][^\\/]+$/, "") || "/" : p.split("/").slice(0, -1).join("/") || "/";
    try { const r = side === "local" ? await invoke("local_list", { path: dir }) : await invoke("sftp_list", { id: R.sftp.id, path: dir }); return r.entries.find((x) => x.path === p); } catch (e) { return null; }
  }
  function renderTransfers() {
    const box = $("#sftp-transfers");
    R.transfers = R.transfers.slice(0, 30);
    box.innerHTML = R.transfers.map((x) => `<div class="transfer ${x.state}"><span class="tico">${x.dir === "upload" ? ICONS.upload : ICONS.download}</span><span class="tname">${esc(x.name)}</span>
      <span class="tbar"><span class="fill" style="width:${x.total ? Math.min(100, 100 * x.done / x.total).toFixed(0) : (x.state === "running" ? 30 : 100)}%"></span></span>
      <span class="ttxt mono">${x.state === "running" ? `${fmtSize(x.done)} / ${fmtSize(x.total)}` : x.state === "done" ? t("pkg.done") : x.state === "cancelled" ? t("rm.cancelled") : esc(x.error || "error")}</span>
      ${x.state === "running" ? `<button class="icon-btn small" data-cancel-transfer="${x.id}" title="${t("btn.cancel")}">${ICONS.x}</button>` : ""}</div>`).join("");
  }
  $("#sftp-transfers").onclick = (ev) => { const b = ev.target.closest("[data-cancel-transfer]"); if (b) invoke("sftp_cancel", { transfer: b.dataset.cancelTransfer }).catch(() => {}); };
  listen("sftp-progress", (ev) => { const x = R.transfers.find((q) => q.id === ev.payload.transfer); if (!x) return; x.done = ev.payload.done; x.total = ev.payload.total || x.total; const row = $(`[data-cancel-transfer="${x.id}"]`)?.closest(".transfer"); if (row) { $(".fill", row).style.width = `${x.total ? Math.min(100, 100 * x.done / x.total).toFixed(0) : 30}%`; $(".ttxt", row).textContent = `${fmtSize(x.done)} / ${fmtSize(x.total)}`; } });

  /* ---------------- port forwarding ---------------- */
  function renderForwards() {
    if (!R.view) return;
    const active = Object.fromEntries((R.view.forwards_active || []).map((f) => [f.rule_id, f]));
    const rules = R.view.store.forwards;
    $("#fwd-list").innerHTML = rules.map((r) => {
      const h = hostById(r.host_id), a = active[r.id];
      const desc = r.kind === "dynamic" ? `SOCKS5 ${r.bind_address}:${r.bind_port}` : r.kind === "local" ? `${r.bind_address}:${r.bind_port} → ${r.dest_host}:${r.dest_port}` : `${t("rm.server")} ${r.bind_address}:${a?.bound_port || r.bind_port} → ${r.dest_host}:${r.dest_port}`;
      return `<div class="row ${a ? "on" : "off"}" data-fwd="${esc(r.id)}">
        <div class="ico">${ICONS.forward}</div>
        <div><div class="name">${esc(r.label)} <span class="pill">${t(`rm.fwd${r.kind[0].toUpperCase()}${r.kind.slice(1)}`)}</span></div>
          <div class="meta"><span class="mono">${esc(desc)}</span><span class="pill">${esc(h?.label || "?")}</span>${a ? `<span class="state on"><i></i>${t("rm.connections", { n: a.connections, total: a.total })}</span>` : `<span class="state"><i></i>${t("status.stopped")}</span>`}</div></div>
        <div class="act"><input type="checkbox" class="switch" data-fwd-switch="${esc(r.id)}" ${a ? "checked" : ""} ${h ? "" : "disabled"}>
          <button class="icon-btn" data-fwd-edit="${esc(r.id)}" title="${t("btn.edit")}">${ICONS.edit}</button>
          <button class="icon-btn danger" data-fwd-delete="${esc(r.id)}" title="${t("btn.remove")}">${ICONS.trash}</button></div></div>`;
    }).join("") || `<div class="empty">${ICONS.forward}<span>${t("rm.noRules")}</span></div>`;
  }
  function openForwardDialog(r) {
    const f = $("#form-forward"); f.reset();
    f.elements.id.value = r?.id || ""; f.elements.label.value = r?.label || ""; f.elements.kind.value = r?.kind || "local";
    fillSelect(f.elements.host_id, hostsOf().map((h) => [h.id, h.label]), r?.host_id);
    f.elements.bind_address.value = r?.bind_address || "127.0.0.1"; f.elements.bind_port.value = r?.bind_port || "";
    f.elements.dest_host.value = r?.dest_host || "127.0.0.1"; f.elements.dest_port.value = r?.dest_port || "";
    applyFwdKind(); $("#dlg-forward").showModal();
  }
  function applyFwdKind() {
    const k = $("#form-forward").elements.kind.value;
    $("#fwd-dest").classList.toggle("hidden", k === "dynamic");
    $("#fwd-explain").textContent = t(`rm.fwdExplain${k[0].toUpperCase()}${k.slice(1)}`);
  }
  $("#form-forward").elements.kind.onchange = applyFwdKind;
  $("#form-forward").onsubmit = async (ev) => {
    ev.preventDefault(); const e = ev.target.elements;
    const rule = { id: e.id.value, label: e.label.value.trim(), kind: e.kind.value, host_id: e.host_id.value, bind_address: e.bind_address.value.trim() || "127.0.0.1", bind_port: Number(e.bind_port.value) || 0, dest_host: e.dest_host.value.trim(), dest_port: Number(e.dest_port.value) || 0 };
    if (rule.kind !== "remote" && !["127.0.0.1", "localhost", "::1"].includes(rule.bind_address) && !(await askConfirm(t("rm.bindWarn", { addr: rule.bind_address }), t("btn.save")))) return;
    try { await invoke("remote_save_forward", { rule }); $("#dlg-forward").close(); } catch (x) { fail(x); }
    refresh();
  };
  $("#btn-fwd-new").onclick = () => openForwardDialog(null);
  $("#fwd-list").addEventListener("click", async (ev) => {
    const e = ev.target.closest("[data-fwd-edit]"); if (e) { openForwardDialog(R.view.store.forwards.find((r) => r.id === e.dataset.fwdEdit)); return; }
    const d = ev.target.closest("[data-fwd-delete]");
    if (d && (await askConfirm(t("rm.confirmDeleteRule")))) { try { await invoke("remote_delete_forward", { id: d.dataset.fwdDelete }); } catch (x) { fail(x); } refresh(); }
  });
  $("#fwd-list").addEventListener("change", async (ev) => {
    const sw = ev.target.closest("[data-fwd-switch]"); if (!sw) return;
    const id = sw.dataset.fwdSwitch; sw.disabled = true;
    try {
      if (sw.checked) { const s = await invoke("forward_start", { id }); toast(t("rm.forwardOn", { port: s.bound_port }), "ok"); }
      else { await invoke("forward_stop", { id }); toast(t("rm.forwardOff"), "ok"); }
    } catch (x) { fail(x); }
    refresh();
  });
  setInterval(() => { if ((R.view?.forwards_active || []).length && $("#tab-forward").classList.contains("active")) refresh(); }, 5000);

  /* ---------------- snippets ---------------- */
  function renderSnippets() {
    if (!R.view) return;
    const lock = lockState();
    $("#btn-snip-new").classList.toggle("hidden", lock === "locked");
    $("#btn-snip-lock-now").classList.toggle("hidden", lock !== "unlocked");
    $("#btn-snip-pass").classList.toggle("hidden", lock === "locked");
    $("#btn-snip-pass-text").textContent = lock === "off" ? t("rm.setPassword") : t("rm.changePassword");
    if (lock === "locked") { $("#snip-list").innerHTML = lockScreen(false); $("#snip-output").classList.add("hidden"); return; }
    const snippets = R.view.store.snippets;
    const folders = [...new Set(snippets.map((s) => s.folder || ""))].sort((a, b) => (a === "") - (b === "") || a.localeCompare(b));
    const canSend = !!(R.active && R.terms.get(R.active) && !R.terms.get(R.active).closed);
    const tip = lock === "off" ? `<div class="card snip-lock-tip">${ICONS.lock}<div><b>${t("rm.lockTipTitle")}</b><p>${t("rm.lockTip")}</p></div><button class="btn small" id="btn-snip-pass-tip">${ICONS.key}<span>${t("rm.setPassword")}</span></button></div>` : "";
    $("#snip-list").innerHTML = tip + (folders.map((fo) => `${fo ? `<h3 class="rm-folder">${ICONS.group}${esc(fo)}</h3>` : ""}<div class="list">${snippets.filter((s) => (s.folder || "") === fo).map((s) => `<div class="row" data-snip="${esc(s.id)}">
        <div class="ico">${ICONS.snippet}</div>
        <div><div class="name">${esc(s.label)}</div><div class="meta"><span class="pill">${((n) => n === 1 ? t("rm.line") : t("rm.lines", { n }))((s.script.match(/\n/g) || []).length + 1)}</span>${lock !== "off" ? `<span class="pill ok">${ICONS.lock}${t("rm.encrypted")}</span>` : ""}</div></div>
        <div class="act"><button class="btn small ${canSend ? "primary" : ""}" data-snip-send="${esc(s.id)}" ${canSend ? "" : "disabled"} title="${t("rm.sendToTerminal")}">${ICONS.send}<span>${t("rm.send")}</span></button>
          <button class="btn small" data-snip-run="${esc(s.id)}">${ICONS.play}<span>${t("rm.runOnHosts")}</span></button>
          <button class="icon-btn" data-snip-copy="${esc(s.id)}" title="${t("msg.copied")}">${ICONS.copy}</button>
          <button class="icon-btn" data-snip-edit="${esc(s.id)}" title="${t("btn.edit")}">${ICONS.edit}</button>
          <button class="icon-btn danger" data-snip-delete="${esc(s.id)}" title="${t("btn.remove")}">${ICONS.trash}</button></div></div>`).join("")}</div>`).join("")
      || `<div class="empty">${ICONS.snippet}<span>${t("rm.noSnippets")}</span></div>`);
    const tb = $("#btn-snip-pass-tip"); if (tb) tb.onclick = openSnipLockDialog;
  }
  function openSnipLockDialog() {
    const f = $("#form-snip-lock"), on = lockState() !== "off"; f.reset();
    $("#snip-lock-title").textContent = on ? t("rm.changePassword") : t("rm.setPassword");
    $$("[data-lock=on]", f).forEach((el) => el.classList.toggle("hidden", !on));
    $("#btn-snip-pass-remove").classList.toggle("hidden", !on);
    f.elements.current.required = on;
    for (const o of f.elements.minutes.options) o.textContent = o.value === "0" ? t("rm.lockNever") : t("rm.lockMin", { n: o.value });
    f.elements.minutes.value = String(R.view?.snippet_lock_minutes ?? 15);
    $("#dlg-snip-lock").showModal(); (on ? f.elements.current : f.elements.new).focus();
  }
  $("#form-snip-lock").onsubmit = async (ev) => {
    ev.preventDefault(); const e = ev.target.elements;
    if (e.new.value !== e.confirm.value) { toast(t("rm.pwMismatch"), "bad"); e.confirm.focus(); return; }
    if (e.new.value.length < 6) { toast(t("rm.pwShort"), "bad"); e.new.focus(); return; }
    const btn = $("button[type=submit]", ev.target); btn.disabled = true;
    try { await invoke("snippet_lock_set", { current: e.current.value || null, new: e.new.value, autoLockMinutes: Number(e.minutes.value) }); $("#dlg-snip-lock").close(); toast(t("rm.pwSet"), "ok"); } catch (x) { fail(x); }
    btn.disabled = false; ev.target.reset(); refresh();
  };
  $("#btn-snip-pass-remove").onclick = async () => {
    const e = $("#form-snip-lock").elements;
    if (!e.current.value) { toast(t("rm.currentNeeded"), "bad"); e.current.focus(); return; }
    if (!(await askConfirm(t("rm.confirmRemovePw"), t("rm.removePassword")))) return;
    try { await invoke("snippet_lock_set", { current: e.current.value, new: "", autoLockMinutes: 0 }); $("#dlg-snip-lock").close(); toast(t("rm.pwRemoved"), "ok"); } catch (x) { fail(x); }
    $("#form-snip-lock").reset(); refresh();
  };
  $("#btn-snip-pass").onclick = openSnipLockDialog;
  $("#btn-snip-lock-now").onclick = async () => { try { await invoke("snippet_lock_now"); toast(t("rm.locked"), "ok"); } catch (x) { fail(x); } refresh(); };
  function openSnippetDialog(s) {
    const f = $("#form-snippet"); f.reset();
    f.elements.id.value = s?.id || ""; f.elements.label.value = s?.label || ""; f.elements.folder.value = s?.folder || ""; f.elements.script.value = s?.script || "";
    $("#dlg-snippet").showModal(); f.elements.label.focus();
  }
  $("#form-snippet").onsubmit = async (ev) => {
    ev.preventDefault(); const e = ev.target.elements;
    try { await invoke("remote_save_snippet", { snippet: { id: e.id.value, label: e.label.value.trim(), folder: e.folder.value.trim(), script: e.script.value } }); $("#dlg-snippet").close(); } catch (x) { fail(x); }
    refresh();
  };
  $("#btn-snip-new").onclick = () => openSnippetDialog(null);
  function pickHosts(title, preselect) {
    return new Promise((resolve) => {
      const dlg = $("#dlg-run-snippet"), f = $("#form-run-snippet");
      $("#run-snippet-title").textContent = title;
      $("#run-snippet-hosts").innerHTML = hostsOf().map((h) => `<label class="check"><input type="checkbox" name="h" value="${esc(h.id)}" ${preselect?.includes(h.id) ? "checked" : ""}><span>${esc(h.label)} <small class="mono">${esc(hostSummary(h))}</small></span></label>`).join("") || `<div class="empty small">${t("rm.noHosts")}</div>`;
      const done = (v) => { dlg.close(); resolve(v); };
      f.onsubmit = (ev) => { ev.preventDefault(); done($$("input[name=h]:checked", f).map((i) => i.value)); };
      dlg.oncancel = (ev) => { ev.preventDefault(); done(null); };
      $("[data-close]", f).onclick = () => done(null);
      dlg.showModal();
    });
  }
  $("#snip-list").addEventListener("click", async (ev) => {
    const b = ev.target.closest("button"); if (!b) return;
    const sid = b.dataset.snipSend || b.dataset.snipRun || b.dataset.snipCopy || b.dataset.snipEdit || b.dataset.snipDelete;
    const s = R.view.store.snippets.find((x) => x.id === sid); if (!s) return;
    if (b.dataset.snipSend) sendSnippet(s.id, true);
    else if (b.dataset.snipCopy) { await navigator.clipboard.writeText(s.script); toast(t("msg.copied"), "ok"); }
    else if (b.dataset.snipEdit) openSnippetDialog(s);
    else if (b.dataset.snipDelete) { if (await askConfirm(t("rm.confirmDeleteSnippet", { name: s.label }))) { try { await invoke("remote_delete_snippet", { id: s.id }); } catch (x) { fail(x); } refresh(); } }
    else if (b.dataset.snipRun) {
      const ids = await pickHosts(`${t("rm.runOnHosts")}: ${s.label}`); if (!ids?.length) return;
      const out = $("#snip-output"); out.classList.remove("hidden");
      out.innerHTML = `<div class="activity-head"><span>${esc(s.label)}</span><span class="state busy">${ICONS.spinner}${t("rm.running", { n: ids.length })}</span></div>`;
      try {
        const results = await invoke("snippet_run", { snippetId: s.id, script: null, hostIds: ids });
        out.innerHTML = `<div class="activity-head"><span>${esc(s.label)}</span><button class="btn small ghost" id="btn-snip-out-close">${t("btn.close")}</button></div>` + results.map((r) => `<div class="snip-result ${r.error || (r.exit && r.exit !== 0) ? "bad" : "ok"}"><div class="srh"><b>${esc(r.label)}</b><span class="pill">${r.error ? esc(r.error) : `exit ${r.exit ?? "?"}`}</span></div><pre>${esc((r.stdout + (r.stderr ? "\n" + r.stderr : "")).trim() || "—")}</pre></div>`).join("");
        $("#btn-snip-out-close").onclick = () => out.classList.add("hidden");
      } catch (x) { fail(x); out.classList.add("hidden"); }
    }
  });

  /* ---------------- keychain ---------------- */
  function renderKeychain() {
    if (!R.view) return;
    const { keys, identities } = R.view.store;
    $("#keychain-note").textContent = R.view.secret_backend === "keyring" ? t("rm.secretsKeyring") : t("rm.secretsFile");
    $("#key-list").innerHTML = keys.map((k) => `<div class="row" data-key="${esc(k.id)}">
        <div class="ico">${ICONS.key}</div>
        <div><div class="name">${esc(k.label)} ${k.encrypted ? `<span class="pill" title="${t("rm.passphraseProtected")}">${ICONS.lock}${k.has_passphrase ? t("rm.saved") : ""}</span>` : ""}</div><div class="meta"><span class="pill">${esc(k.algorithm)}</span><span class="mono">${esc(k.fingerprint)}</span></div></div>
        <div class="act"><button class="btn small" data-key-copy="${esc(k.id)}">${ICONS.copy}<span>${t("rm.copyPublic")}</span></button>
          <button class="btn small" data-key-deploy="${esc(k.id)}" title="${t("rm.deployHint")}">${ICONS.upload}<span>${t("rm.deploy")}</span></button>
          <button class="icon-btn" data-key-export="${esc(k.id)}" title="${t("rm.exportPublic")}">${ICONS.download}</button>
          <button class="icon-btn" data-key-export-private="${esc(k.id)}" title="${t("rm.exportPrivate")}">${ICONS.file}</button>
          <button class="icon-btn" data-key-pass="${esc(k.id)}" title="${t("rm.changePassphrase")}">${k.encrypted ? ICONS.lock : ICONS.unlock}</button>
          <button class="icon-btn danger" data-key-delete="${esc(k.id)}" title="${t("btn.remove")}">${ICONS.trash}</button></div></div>`).join("")
      || `<div class="empty">${ICONS.key}<span>${t("rm.noKeys")}</span></div>`;
    $("#ident-list").innerHTML = identities.map((i) => `<div class="row" data-ident="${esc(i.id)}">
        <div class="ico">${ICONS.identity}</div>
        <div><div class="name">${esc(i.label)}</div><div class="meta"><span class="mono">${esc(i.username)}</span>${i.has_password ? `<span class="pill">${t("db.password")}</span>` : ""}${i.key_id ? `<span class="pill">${ICONS.key}${esc(keys.find((k) => k.id === i.key_id)?.label || "?")}</span>` : ""}</div></div>
        <div class="act"><button class="icon-btn" data-ident-edit="${esc(i.id)}" title="${t("btn.edit")}">${ICONS.edit}</button><button class="icon-btn danger" data-ident-delete="${esc(i.id)}" title="${t("btn.remove")}">${ICONS.trash}</button></div></div>`).join("")
      || `<div class="empty">${ICONS.identity}<span>${t("rm.noIdentities")}</span></div>`;
  }
  async function openKeyDialog(mode) {
    const f = $("#form-key"); f.reset(); f.elements.mode.value = mode;
    $("#key-dlg-title").textContent = mode === "generate" ? t("rm.newKey") : t("rm.importKey");
    $$("#form-key [data-key]").forEach((el) => el.classList.toggle("hidden", el.dataset.key !== mode));
    if (mode === "import") {
      let cands = []; try { cands = await invoke("remote_key_candidates"); } catch (e) {}
      fillSelect(f.elements.path, cands.map((p) => [p, p]), "", t("rm.pasteOrPick"));
    }
    $("#dlg-key").showModal(); f.elements.label.focus();
  }
  $("#btn-key-new").onclick = () => openKeyDialog("generate");
  $("#btn-key-import").onclick = () => openKeyDialog("import");
  $("#btn-key-browse").onclick = async () => {
    try { const p = await window.__TAURI__.dialog.open({ multiple: false, title: t("rm.keyFile") }); if (p) { const f = $("#form-key"); const v = typeof p === "string" ? p : p.path; fillSelect(f.elements.path, [[v, v]], v); } } catch (e) { toast(t("migrate.noPicker"), "info"); }
  };
  $("#form-key").onsubmit = async (ev) => {
    ev.preventDefault(); const e = ev.target.elements;
    const args = { label: e.label.value.trim(), passphrase: e.passphrase.value || null, savePassphrase: e.save_passphrase.checked };
    try {
      if (e.mode.value === "generate") await invoke("remote_key_generate", { ...args, kind: e.kind.value });
      else await invoke("remote_key_import", { ...args, text: e.text.value || null, path: e.path.value || null });
      $("#dlg-key").close(); toast(t("msg.saved2"), "ok");
    } catch (x) { fail(x); }
    refresh();
  };
  function openIdentityDialog(i) {
    const f = $("#form-identity"); f.reset();
    f.elements.id.value = i?.id || ""; f.elements.label.value = i?.label || ""; f.elements.username.value = i?.username || "";
    f.elements.password.placeholder = i?.has_password ? "•••••••• (saved)" : "";
    fillSelect(f.elements.key_id, R.view.store.keys.map((k) => [k.id, k.label]), i?.key_id, "—");
    $("#dlg-identity").showModal(); f.elements.label.focus();
  }
  $("#btn-ident-new").onclick = () => openIdentityDialog(null);
  $("#form-identity").onsubmit = async (ev) => {
    ev.preventDefault(); const e = ev.target.elements;
    const identity = { id: e.id.value, label: e.label.value.trim(), username: e.username.value.trim(), key_id: e.key_id.value || null, has_password: false };
    try { await invoke("remote_save_identity", { identity, password: e.password.value || null }); $("#dlg-identity").close(); } catch (x) { fail(x); }
    refresh();
  };
  $("#key-list").addEventListener("click", async (ev) => {
    const b = ev.target.closest("button"); if (!b) return;
    const kid = b.dataset.keyCopy || b.dataset.keyDeploy || b.dataset.keyExport || b.dataset.keyExportPrivate || b.dataset.keyPass || b.dataset.keyDelete;
    const k = R.view.store.keys.find((x) => x.id === kid); if (!k) return;
    try {
      if (b.dataset.keyCopy) { await navigator.clipboard.writeText(k.public_key); toast(t("msg.copied"), "ok"); }
      else if (b.dataset.keyExport || b.dataset.keyExportPrivate) {
        const priv = !!b.dataset.keyExportPrivate;
        if (priv && !(await askConfirm(t("rm.exportPrivateWarn"), t("rm.export")))) return;
        const path = await window.__TAURI__.dialog.save({ defaultPath: `${k.label.replace(/[^\w.-]+/g, "_")}${priv ? "" : ".pub"}`, title: priv ? t("rm.exportPrivate") : t("rm.exportPublic") });
        if (!path) return;
        await invoke("remote_key_export", { id: k.id, path, private: priv }); toast(t("rm.exported", { path }), "ok");
      }
      else if (b.dataset.keyDeploy) {
        const ids = await pickHosts(`${t("rm.deploy")}: ${k.label}`); if (!ids?.length) return;
        for (const hid of ids) { try { toast(await invoke("remote_key_deploy", { hostId: hid, keyId: k.id }), "ok"); } catch (x) { fail(x); } }
      }
      else if (b.dataset.keyPass) {
        const current = k.encrypted && !k.has_passphrase ? await askText(t("rm.changePassphrase"), t("rm.currentPassphrase"), "", true) : null;
        if (k.encrypted && !k.has_passphrase && current === null) return;
        const next = await askText(t("rm.changePassphrase"), t("rm.newPassphrase"), "", true); if (next === null) return;
        await invoke("remote_key_passphrase", { id: k.id, current, new: next, savePassphrase: !!next }); toast(t("msg.saved2"), "ok"); refresh();
      }
      else if (b.dataset.keyDelete) { if (await askConfirm(t("rm.confirmDeleteKey", { name: k.label }))) { await invoke("remote_key_delete", { id: k.id }); refresh(); } }
    } catch (x) { fail(x); }
  });
  $("#ident-list").addEventListener("click", async (ev) => {
    const b = ev.target.closest("button"); if (!b) return;
    const i = R.view.store.identities.find((x) => x.id === (b.dataset.identEdit || b.dataset.identDelete)); if (!i) return;
    if (b.dataset.identEdit) openIdentityDialog(i);
    else if (await askConfirm(t("rm.confirmDeleteIdentity", { name: i.label }))) { try { await invoke("remote_delete_identity", { id: i.id }); } catch (x) { fail(x); } refresh(); }
  });

  /* ---------------- known hosts & history ---------------- */
  async function loadKnownHosts() {
    try {
      const list = await invoke("remote_known_hosts");
      $("#kh-list").innerHTML = list.map((k) => `<div class="row"><div class="ico">${ICONS.knownhosts}</div>
        <div><div class="name mono">${esc(k.hosts.startsWith("|1|") ? t("rm.hashedHost") : k.hosts)}</div><div class="meta"><span class="pill">${esc(k.algorithm)}</span><span class="mono">${esc(k.fingerprint)}</span></div></div>
        <div class="act"><button class="icon-btn danger" data-kh-remove="${k.line}" title="${t("btn.remove")}">${ICONS.trash}</button></div></div>`).join("") || `<div class="empty">${ICONS.knownhosts}<span>${t("rm.noKnown")}</span></div>`;
    } catch (e) { fail(e); }
  }
  $("#btn-kh-refresh").onclick = loadKnownHosts;
  $("#kh-list").addEventListener("click", async (ev) => {
    const b = ev.target.closest("[data-kh-remove]"); if (!b) return;
    if (await askConfirm(t("rm.confirmForgetHost"))) { try { await invoke("remote_known_host_remove", { line: Number(b.dataset.khRemove) }); } catch (x) { fail(x); } loadKnownHosts(); }
  });
  function renderHistory() {
    if (!R.view) return;
    $("#hist-list").innerHTML = R.view.store.history.slice(0, 200).map((h, i) => `<div class="row ${h.ok ? "" : "missing"}" data-hist="${esc(h.host_id || "")}">
        <div class="ico">${h.kind === "sftp" ? ICONS.sftp : h.kind === "forward" ? ICONS.forward : h.kind === "snippet" ? ICONS.snippet : ICONS.terminal}</div>
        <div><div class="name">${esc(h.label)} <span class="pill">${esc(h.kind)}</span></div><div class="meta"><span class="mono">${esc(maskAddr("t:" + i, h.target))}</span><span>${fmtTime(h.started_at)}</span>${h.ended_at ? `<span>· ${Math.max(1, Math.round((h.ended_at - h.started_at) / 60))} min</span>` : ""}${h.ok ? "" : `<span class="state warn"><i></i>${esc(h.error.split("\n")[0].slice(0, 120))}</span>`}</div></div>
        <div class="act">${h.host_id && hostById(h.host_id) ? `<button class="icon-btn go" data-hist-connect="${esc(h.host_id)}" title="${t("rm.openTerminal")}">${ICONS.terminal}</button>` : ""}${R.privacy ? `<button class="icon-btn ${R.reveal.has("t:" + i) ? "on" : ""}" data-hist-reveal="${i}" title="${t("rm.reveal")}">${ICONS.eye}</button>` : ""}<button class="icon-btn" data-hist-copy="${i}" title="${t("rm.copyAddr")}">${ICONS.copy}</button></div></div>`).join("")
      || `<div class="empty">${ICONS.history}<span>${t("rm.noHistory")}</span></div>`;
  }
  $("#btn-hist-clear").onclick = async () => { if (await askConfirm(t("rm.confirmClearHistory"), t("btn.clear"))) { try { await invoke("remote_history_clear"); } catch (x) { fail(x); } refresh(); } };
  $("#hist-list").addEventListener("click", (ev) => {
    const b = ev.target.closest("[data-hist-connect],[data-hist-reveal],[data-hist-copy]"); if (!b) return;
    if (b.dataset.histConnect) openTerminal({ hostId: b.dataset.histConnect });
    else if (b.dataset.histReveal !== undefined) revealFor("t:" + b.dataset.histReveal, renderHistory);
    else if (b.dataset.histCopy !== undefined) { const h = R.view?.store.history[Number(b.dataset.histCopy)]; if (h) copyText(h.target); }
  });

  /* ---------------- import ---------------- */
  $("#btn-rm-import").onclick = async () => {
    let hosts = [];
    try { hosts = await invoke("remote_import_detect"); } catch (e) { fail(e); return; }
    if (!hosts.length) { toast(t("rm.nothingToImport"), "info"); return; }
    $("#import-list").innerHTML = hosts.map((h, i) => `<label class="check"><input type="checkbox" name="h" value="${i}" checked><span>${esc(h.alias)} <small class="mono">${esc(h.user ? h.user + "@" : "")}${esc(h.hostname)}:${h.port}</small> <small>${esc(h.source)}</small></span></label>`).join("");
    const f = $("#form-import");
    f.onsubmit = async (ev) => {
      ev.preventDefault();
      const chosen = $$("input[name=h]:checked", f).map((i) => hosts[Number(i.value)]);
      try { const r = await invoke("remote_import_apply", { hosts: chosen, importKeys: f.elements.import_keys.checked }); toast(t("rm.imported", { hosts: r.hosts, keys: r.keys }), "ok"); r.skipped.forEach((s) => toast(s, "info")); $("#dlg-import").close(); } catch (x) { fail(x); }
      refresh();
    };
    $("#dlg-import").showModal();
  };

  /* ---------------- small dialogs ---------------- */
  function askText(title, label, value, secret) {
    return new Promise((resolve) => {
      const dlg = $("#dlg-text"), f = $("#form-text");
      $("#text-dlg-title").textContent = title; $("#text-dlg-label").textContent = label;
      f.elements.value.type = secret ? "password" : "text"; f.elements.value.value = value || "";
      const done = (v) => { dlg.close(); resolve(v); };
      f.onsubmit = (ev) => { ev.preventDefault(); done(f.elements.value.value.trim()); };
      dlg.oncancel = (ev) => { ev.preventDefault(); done(null); };
      $("[data-close]", f).onclick = () => done(null);
      dlg.showModal(); f.elements.value.focus(); f.elements.value.select();
    });
  }
  function askEditor(title, text) {
    return new Promise((resolve) => {
      const dlg = $("#dlg-editor"), f = $("#form-editor");
      $("#editor-title").textContent = title; f.elements.text.value = text;
      const done = (v) => { dlg.close(); resolve(v); };
      f.onsubmit = (ev) => { ev.preventDefault(); done(f.elements.text.value); };
      dlg.oncancel = (ev) => { ev.preventDefault(); done(null); };
      $("[data-close]", f).onclick = () => done(null);
      dlg.showModal(); f.elements.text.focus();
    });
  }
  document.addEventListener("click", (ev) => { const b = ev.target.closest("[data-close]"); if (b && !b.onclick) { const d = document.getElementById(b.dataset.close); if (d?.open) d.close(); } });


  /* ---------------- scripted check (VERANDA_UI_AUTOTEST=remote[:tab], dev only) ---------------- */
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  async function waitFor(cond, ms) { const end = Date.now() + ms; while (Date.now() < end) { if (cond()) return true; await sleep(100); } return cond(); }
  function bufferText(id) { const b = R.terms.get(id)?.term.buffer.active; if (!b) return ""; const lines = []; for (let i = 0; i < b.length; i++) lines.push(b.getLine(i)?.translateToString(true) || ""); return lines.join("\n"); }
  async function autotest(name, report) {
    const [base, tab] = name.split(":");
    if (base !== "remote") return false;
    const out = [];
    try {
      await refresh();
      if (lockState() !== "off") { // a previous locked run; start from an open store
        if (lockState() === "locked") await invoke("snippet_unlock", { password: "autotest-pass" });
        await invoke("snippet_lock_set", { current: "autotest-pass", new: "", autoLockMinutes: 0 }); await refresh();
      }
      let host = hostsOf().find((h) => h.label === "Refused box");
      if (!host) {
        const gid = await invoke("remote_save_group", { group: { id: "", name: "Lab", parent_id: null, identity_id: null, color: "" } });
        const kmeta = await invoke("remote_key_generate", { label: "autotest key", kind: "ed25519", passphrase: null, savePassphrase: false });
        const iid = await invoke("remote_save_identity", { identity: { id: "", label: "deploy", username: "deploy", has_password: false, key_id: kmeta.id }, password: null });
        const hid = await invoke("remote_save_host", { host: { id: "", label: "Refused box", address: "127.0.0.1", port: 1, username: "root", group_id: gid, tags: ["lab", "test"], color: "#dc2626", auth: "identity", identity_id: iid, notes: "nothing listens on port 1" }, password: null });
        await invoke("remote_save_host", { host: { id: "", label: "Example", address: "example.com", port: 22, username: "deploy", tags: ["prod"], color: "#16a34a", auth: "key", key_id: kmeta.id }, password: null });
        await invoke("remote_save_snippet", { snippet: { id: "", label: "Disk usage", folder: "Maintenance", script: "df -h" } });
        await invoke("remote_save_snippet", { snippet: { id: "", label: "Update packages", folder: "Maintenance", script: "sudo apt update && sudo apt upgrade -y" } });
        await invoke("remote_save_forward", { rule: { id: "", label: "DB tunnel", kind: "local", host_id: hid, bind_address: "127.0.0.1", bind_port: 33061, dest_host: "127.0.0.1", dest_port: 3306 } });
        await refresh();
        host = hostById(hid);
        out.push(`created fp=${kmeta.fingerprint.slice(0, 16)}`);
      }
      out.push(`hosts=${hostsOf().length} groups=${R.view.store.groups.length} keys=${R.view.store.keys.length} idents=${R.view.store.identities.length} snippets=${R.view.store.snippets.length} rules=${R.view.store.forwards.length} backend=${R.view.secret_backend}`);
      const tid = await openTerminal({ local: true });
      const up = await waitFor(() => R.terms.get(tid)?.info.state === "connected", 8000);
      await sleep(600);
      invoke("term_write", { id: tid, data: "echo VERANDA_OK_$((40+2))\n" });
      const echoed = await waitFor(() => bufferText(tid).includes("VERANDA_OK_42"), 8000);
      out.push(`local=${up ? "connected" : R.terms.get(tid)?.info.state} echo=${echoed}`);
      const tid2 = await openTerminal({ hostId: host.id });
      await waitFor(() => ["error", "closed", "connected"].includes(R.terms.get(tid2)?.info.state), 15000);
      out.push(`ssh=${R.terms.get(tid2)?.info.state} msg="${(R.terms.get(tid2)?.info.message || "").slice(0, 90)}"`);
      const rule = R.view.store.forwards[0];
      try { await invoke("forward_start", { id: rule.id }); out.push("forward=started?!"); } catch (e) { out.push(`forward=refused-ok(${String(e?.message || e).slice(0, 40)})`); }
      const l = await invoke("local_list", { path: null }); out.push(`local_list=${l.entries.length}`);
      const kh = await invoke("remote_known_hosts"); await refresh(); out.push(`known=${kh.length} history=${R.view.store.history.length} ok=${R.view.store.history.filter((h) => h.ok).length}`);
      if (tab === "sftp") { await loadPane("local", ""); }
      const lockIt = async () => { await invoke("snippet_lock_set", { current: null, new: "autotest-pass", autoLockMinutes: 15 }); await invoke("snippet_lock_now"); await refresh(); };
      const dialogs = { lockdlg: () => { showTab("snippets"); openSnipLockDialog(); }, snippanel: async () => { showTab("terminal"); activateTerminal(tid); toggleSnipPanel(true); $("#term-side .snip-item").click(); out.push(`sent=${await waitFor(() => /Filesystem|Mounted on/.test(bufferText(tid)), 6000)}`); }, sniplocked: async () => { await lockIt(); showTab("snippets"); }, snippanellocked: async () => { await lockIt(); let refused = ""; try { await invoke("term_send_snippet", { id: tid, snippetId: R.view.store.snippets[0].id, enter: true }); } catch (e) { refused = String(e?.message || e); } out.push(`locked-send="${refused.slice(0, 40)}"`); showTab("terminal"); activateTerminal(tid); toggleSnipPanel(true); }, snippetdlg: () => { showTab("snippets"); openSnippetDialog(R.view.store.snippets[0]); }, runsnippetdlg: () => { showTab("snippets"); pickHosts("Disk usage", [host.id]); }, groupdlg: () => { showTab("hosts"); openGroupDialog(R.view.store.groups[0]); }, identdlg: () => { showTab("keychain"); openIdentityDialog(R.view.store.identities[0]); }, importdlg: () => { showTab("hosts"); $("#btn-rm-import").click(); }, textdlg: () => { showTab("sftp"); askText("Rename", "New name", "notes.txt", false); }, editordlg: () => { showTab("sftp"); askEditor("notes.txt", "line one\nline two\n"); }, hostdlg: () => { showTab("hosts"); openHostDialog(hostsOf()[0]); }, fwddlg: () => { showTab("forward"); openForwardDialog(R.view.store.forwards[0]); }, keydlg: () => { showTab("keychain"); openKeyDialog("import"); }, settingsdlg: () => { showTab("terminal"); activateTerminal(tid); openTermSettings(); }, pwdlg: () => { showTab("terminal"); activateTerminal(tid); R.promptQueue.push({ id: "x", session: tid, kind: "password", title: "Refused box", prompt: "Password for root@127.0.0.1", echo: false, can_remember: true }); showNextPrompt(); }, promptdlg: () => { showTab("terminal"); activateTerminal(tid); R.promptQueue.push({ id: "x", session: tid, kind: "hostkey", title: "Example", host: "example.com", port: 22, algorithm: "ssh-ed25519", fingerprint: "SHA256:AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcdefg", status: "changed" }); showNextPrompt(); } };
      if (dialogs[tab]) await dialogs[tab]();
      else if (tab) { showTab(tab); if (tab === "terminal") activateTerminal(tid); }
    } catch (e) { out.push(`ERROR ${e?.message || e}`); }
    report(out.join(" | "));
    return true;
  }

  /* ---------------- boot ---------------- */
  window.VerandaRemote = {
    init: async () => { await refresh(); renderPane("local"); renderPane("remote"); if (state?.ui_tab === "terminal:local") openTerminal({ local: true }); },
    openTerminal, openLocal: () => openTerminal({ local: true }), refresh, autotest,
  };
})();
