/* Veranda Files - a small text editor for project files: folder tree, syntax colouring,
   Markdown preview, CSV table, JSON check, save with Ctrl+S. No dependencies: the highlighter
   and the Markdown renderer live here and escape everything they output.
   Uses the helpers from app.js (invoke, $, $$, t, esc, toast, fail, state, askConfirm, showTab). */
(function () {
  const F = {
    root: "",            // folder shown in the tree
    roots: [],           // offered folders
    expanded: new Set(), // open folders in the tree
    showHidden: false,
    query: "",           // name filter typed in the search box
    doc: null,           // { path, name, text, crlf, mtime, language }
    dirty: false,
    mode: "edit",        // edit | preview | split
    wrap: false,
    hlTimer: null,
  };
  try { F.root = localStorage.getItem("veranda.files.root") || ""; F.wrap = localStorage.getItem("veranda.files.wrap") === "1"; } catch (e) {}
  const HL_MAX = 300 * 1024;
  const fmtSize = (n) => n < 1024 ? `${n} B` : n < 1048576 ? `${(n / 1024).toFixed(1)} KB` : `${(n / 1048576).toFixed(1)} MB`;
  const sep = () => (F.root.includes("\\") ? "\\" : "/");
  const join = (dir, name) => dir.replace(/[\\/]+$/, "") + sep() + name;
  const parentOf = (p) => p.replace(/[\\/][^\\/]+$/, "") || p;
  const baseOf = (p) => p.split(/[\\/]/).pop();

  /* ---------------- syntax colouring ---------------- */
  const KW = {
    javascript: "async await break case catch class const continue debugger default delete do else export extends finally for from function if import in instanceof let new of return static super switch this throw try typeof var void while with yield true false null undefined",
    typescript: "async await break case catch class const continue debugger default delete do else enum export extends finally for from function if implements import in instanceof interface let namespace new of private protected public readonly return static super switch this throw try type typeof var void while with yield true false null undefined any number string boolean never unknown",
    php: "abstract and array as break callable case catch class clone const continue declare default do echo else elseif empty enddeclare endfor endforeach endif endswitch endwhile enum extends final finally fn for foreach function global goto if implements include include_once instanceof insteadof interface isset list match namespace new or print private protected public readonly require require_once return static switch throw trait try unset use var while xor yield true false null self parent",
    python: "False None True and as assert async await break class continue def del elif else except finally for from global if import in is lambda nonlocal not or pass raise return try while with yield print self",
    go: "break case chan const continue default defer else fallthrough for func go goto if import interface map package range return select struct switch type var true false nil int string bool error byte rune float64 int64",
    rust: "as async await break const continue crate dyn else enum extern fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait type unsafe use where while true false Some None Ok Err",
    java: "abstract assert boolean break byte case catch char class const continue default do double else enum extends final finally float for if implements import instanceof int interface long native new package private protected public return short static strictfp super switch synchronized this throw throws transient try void volatile while true false null var val fun object when",
    c: "auto break case char const continue default do double else enum extern float for goto if inline int long register return short signed sizeof static struct switch typedef union unsigned void volatile while class namespace public private protected template typename using new delete this nullptr true false bool string var",
    ruby: "alias and begin break case class def defined do else elsif end ensure false for if in module next nil not or redo rescue retry return self super then true undef unless until when while yield require puts",
    sql: "SELECT FROM WHERE INSERT INTO VALUES UPDATE SET DELETE CREATE TABLE DATABASE DROP ALTER ADD COLUMN INDEX PRIMARY KEY FOREIGN REFERENCES NOT NULL DEFAULT AUTO_INCREMENT UNIQUE JOIN LEFT RIGHT INNER OUTER ON AS AND OR IN IS LIKE BETWEEN ORDER BY GROUP HAVING LIMIT OFFSET UNION ALL DISTINCT CASE WHEN THEN ELSE END IF EXISTS USE SHOW GRANT TO IDENTIFIED WITH ENGINE CHARSET COLLATE INT VARCHAR TEXT DATETIME TIMESTAMP BOOLEAN DECIMAL BIGINT TRUE FALSE",
    shell: "if then else elif fi for while do done case esac in function return exit export local readonly set unset echo cd source alias test true false",
    powershell: "function param if else elseif foreach for while do switch return try catch finally throw begin process end",
    batch: "echo set if else goto call exit for in do rem pause setlocal endlocal not exist defined",
    css: "important inherit initial none auto flex grid block inline absolute relative fixed sticky",
    dockerfile: "FROM RUN CMD LABEL EXPOSE ENV ADD COPY ENTRYPOINT VOLUME USER WORKDIR ARG ONBUILD STOPSIGNAL HEALTHCHECK SHELL AS",
    makefile: "ifeq ifneq ifdef ifndef else endif include export define endef",
  };
  const KWSET = {}; for (const k in KW) KWSET[k] = new Set(KW[k].split(" "));
  const CFG = {
    javascript: { line: "//", block: ["/*", "*/"], str: "\"'`", ci: false },
    typescript: { line: "//", block: ["/*", "*/"], str: "\"'`", ci: false },
    php: { line: "//", hash: true, block: ["/*", "*/"], str: "\"'", ci: true, vars: true },
    python: { hash: true, str: "\"'", triple: true, ci: false },
    go: { line: "//", block: ["/*", "*/"], str: "\"'`", ci: false },
    rust: { line: "//", block: ["/*", "*/"], str: "\"", ci: false },
    java: { line: "//", block: ["/*", "*/"], str: "\"'", ci: false },
    c: { line: "//", block: ["/*", "*/"], str: "\"'", ci: false, pre: true },
    ruby: { hash: true, str: "\"'", ci: false },
    sql: { line: "--", hash: true, block: ["/*", "*/"], str: "\"'`", ci: true },
    shell: { hash: true, str: "\"'", ci: false, vars: true },
    powershell: { hash: true, str: "\"'", ci: true, vars: true },
    batch: { line: "::", str: "\"", ci: true },
    css: { block: ["/*", "*/"], str: "\"'", ci: true, css: true },
    dockerfile: { hash: true, str: "\"'", ci: true, vars: true },
    makefile: { hash: true, str: "\"'", ci: false, vars: true },
    json: { str: "\"", ci: false, json: true },
    yaml: { hash: true, str: "\"'", ci: false, keys: true },
    toml: { hash: true, str: "\"'", ci: false, keys: true },
    ini: { hash: true, line: ";", str: "\"'", ci: false, keys: true },
    env: { hash: true, str: "\"'", ci: false, keys: true },
    log: { ci: false, log: true },
    html: { tags: true },
    text: {}, csv: {}, markdown: {},
  };
  const isIdent = (c) => /[A-Za-z0-9_$]/.test(c);
  function highlight(text, lang) {
    if (text.length > HL_MAX) return esc(text);
    const cfg = CFG[lang] || {};
    if (cfg.tags) return hlMarkup(text);
    if (cfg.log) return text.split("\n").map((l) => /error|fatal|panic|exception|failed/i.test(l) ? `<span class="tk-e">${esc(l)}</span>` : /warn/i.test(l) ? `<span class="tk-w">${esc(l)}</span>` : esc(l)).join("\n");
    if (!Object.keys(cfg).length) return esc(text);
    const kw = KWSET[lang] || new Set();
    let out = "", i = 0, lineStart = true;
    const n = text.length;
    while (i < n) {
      const c = text[i];
      // keys at line start (yaml/toml/ini/env/json)
      if (lineStart && cfg.keys) {
        const m = /^(\s*)([A-Za-z0-9_.\-\[\]"' ]+?)(\s*[:=])/.exec(text.slice(i, i + 200));
        if (m && !m[2].startsWith("#")) { out += esc(m[1]) + `<span class="tk-key">${esc(m[2])}</span>` + esc(m[3]); i += m[0].length; lineStart = false; continue; }
      }
      if (lineStart && cfg.pre && c === "#") { const e = text.indexOf("\n", i); const end = e < 0 ? n : e; out += `<span class="tk-k">${esc(text.slice(i, end))}</span>`; i = end; continue; }
      lineStart = c === "\n";
      if (c === "\n") { out += "\n"; i++; continue; }
      // comments
      if (cfg.line && text.startsWith(cfg.line, i)) { const e = text.indexOf("\n", i); const end = e < 0 ? n : e; out += `<span class="tk-c">${esc(text.slice(i, end))}</span>`; i = end; continue; }
      if (cfg.hash && c === "#" && !(cfg.css)) { const e = text.indexOf("\n", i); const end = e < 0 ? n : e; out += `<span class="tk-c">${esc(text.slice(i, end))}</span>`; i = end; continue; }
      if (cfg.block && text.startsWith(cfg.block[0], i)) { const e = text.indexOf(cfg.block[1], i + 2); const end = e < 0 ? n : e + cfg.block[1].length; out += `<span class="tk-c">${esc(text.slice(i, end))}</span>`; i = end; lineStart = false; continue; }
      // strings
      if (cfg.str && cfg.str.includes(c)) {
        let j = i + 1;
        if (cfg.triple && text.startsWith(c + c + c, i)) { const e = text.indexOf(c + c + c, i + 3); j = e < 0 ? n : e + 3; }
        else { while (j < n && text[j] !== c) { if (text[j] === "\\") j++; if (text[j] === "\n" && c !== "`") break; j++; } j = Math.min(n, j + 1); }
        const s = text.slice(i, j);
        if (cfg.json) {
          // a key when followed by ':'
          let k = j; while (k < n && (text[k] === " " || text[k] === "\t")) k++;
          out += `<span class="${text[k] === ":" ? "tk-key" : "tk-s"}">${esc(s)}</span>`;
        } else out += `<span class="tk-s">${esc(s)}</span>`;
        i = j; continue;
      }
      // variables ($x)
      if (cfg.vars && c === "$") { let j = i + 1; if (text[j] === "{") { const e = text.indexOf("}", j); j = e < 0 ? n : e + 1; } else while (j < n && isIdent(text[j])) j++; out += `<span class="tk-v">${esc(text.slice(i, j))}</span>`; i = j; continue; }
      // numbers
      if (/[0-9]/.test(c) && (i === 0 || !isIdent(text[i - 1]))) { let j = i; while (j < n && /[0-9a-fA-FxX._]/.test(text[j])) j++; out += `<span class="tk-n">${esc(text.slice(i, j))}</span>`; i = j; continue; }
      // css selectors / properties
      if (cfg.css) {
        if (c === "." || c === "#" ) { let j = i + 1; while (j < n && /[A-Za-z0-9_\-]/.test(text[j])) j++; out += `<span class="tk-t">${esc(text.slice(i, j))}</span>`; i = j; continue; }
        if (/[a-z-]/.test(c)) { let j = i; while (j < n && /[a-z\-]/.test(text[j])) j++; let k = j; while (k < n && text[k] === " ") k++; out += `<span class="${text[k] === ":" ? "tk-key" : "tk-a"}">${esc(text.slice(i, j))}</span>`; i = j; continue; }
      }
      // identifiers / keywords
      if (isIdent(c)) {
        let j = i; while (j < n && isIdent(text[j])) j++;
        const w = text.slice(i, j);
        const hit = kw.has(w) || (cfg.ci && kw.has(w.toUpperCase())) || (cfg.ci && kw.has(w.toLowerCase()));
        if (hit) out += `<span class="tk-k">${esc(w)}</span>`;
        else if (text[j] === "(") out += `<span class="tk-f">${esc(w)}</span>`;
        else out += esc(w);
        i = j; continue;
      }
      out += esc(c); i++;
    }
    return out;
  }
  function hlMarkup(text) {
    return esc(text)
      .replace(/(&lt;!--[\s\S]*?--&gt;)/g, '<span class="tk-c">$1</span>')
      .replace(/(&lt;\/?)([A-Za-z][\w:.-]*)([^&]*?)(\/?&gt;)/g, (m, a, tag, attrs, z) => {
        const at = attrs.replace(/([\w:.-]+)(=)(&quot;[^&]*?&quot;|&#39;[^&]*?&#39;|[^\s]+)?/g, (mm, k, eq, v) => `<span class="tk-a">${k}</span>${eq}${v ? `<span class="tk-s">${v}</span>` : ""}`);
        return `${a}<span class="tk-t">${tag}</span>${at}${z}`;
      });
  }

  /* ---------------- markdown ---------------- */
  function inline(s) {
    let h = esc(s);
    h = h.replace(/`([^`]+)`/g, (m, c) => `<code>${c}</code>`);
    h = h.replace(/!\[([^\]]*)\]\(([^)\s]+)[^)]*\)/g, (m, alt, src) => `<span class="md-img">[${t("files.image")}: ${alt || src}]</span>`);
    h = h.replace(/\[([^\]]+)\]\(([^)\s]+)(?:\s+&quot;[^&]*&quot;)?\)/g, (m, txt, href) => {
      const safe = /^(https?:|mailto:)/i.test(href) ? href : /^[\w./#-][^:]*$/.test(href) ? href : "#";
      return `<a href="#" data-md-link="${safe}">${txt}</a>`;
    });
    h = h.replace(/(^|[^*])\*\*([^*]+)\*\*/g, "$1<strong>$2</strong>").replace(/(^|[^_])__([^_]+)__/g, "$1<strong>$2</strong>");
    h = h.replace(/(^|[^*])\*([^*\n]+)\*/g, "$1<em>$2</em>").replace(/(^|[^_\w])_([^_\n]+)_(?!\w)/g, "$1<em>$2</em>");
    h = h.replace(/~~([^~]+)~~/g, "<del>$1</del>");
    h = h.replace(/(^|\s)(https?:\/\/[^\s<]+)/g, (m, pre, url) => `${pre}<a href="#" data-md-link="${url}">${url}</a>`);
    return h;
  }
  function markdown(src) {
    const lines = src.replace(/\r\n/g, "\n").split("\n");
    let out = "", i = 0;
    const para = [];
    const flush = () => { if (para.length) { out += `<p>${inline(para.join(" "))}</p>`; para.length = 0; } };
    while (i < lines.length) {
      const l = lines[i];
      if (/^\s*$/.test(l)) { flush(); i++; continue; }
      let m;
      if ((m = /^```\s*([\w+-]*)/.exec(l))) {
        flush(); const lang = m[1].toLowerCase(); const buf = []; i++;
        while (i < lines.length && !/^```/.test(lines[i])) buf.push(lines[i++]);
        i++;
        const map = { js: "javascript", ts: "typescript", py: "python", rs: "rust", sh: "shell", bash: "shell", yml: "yaml", html: "html", xml: "html", vue: "html", md: "text", jsonc: "json" };
        const code = buf.join("\n");
        out += `<pre class="md-code"><code>${highlight(code, map[lang] || (CFG[lang] ? lang : "text"))}</code></pre>`;
        continue;
      }
      if ((m = /^(#{1,6})\s+(.*)$/.exec(l))) { flush(); const n = m[1].length; out += `<h${n}>${inline(m[2].replace(/\s+#+$/, ""))}</h${n}>`; i++; continue; }
      if (/^(-{3,}|\*{3,}|_{3,})\s*$/.test(l)) { flush(); out += "<hr>"; i++; continue; }
      if (/^\s*>/.test(l)) {
        flush(); const buf = [];
        while (i < lines.length && /^\s*>/.test(lines[i])) buf.push(lines[i++].replace(/^\s*>\s?/, ""));
        out += `<blockquote>${markdown(buf.join("\n"))}</blockquote>`; continue;
      }
      if (/^\s*\|/.test(l) && i + 1 < lines.length && /^\s*\|?\s*:?-{2,}/.test(lines[i + 1])) {
        flush();
        const cells = (row) => row.trim().replace(/^\||\|$/g, "").split("|").map((c) => c.trim());
        const head = cells(l); i += 2; const rows = [];
        while (i < lines.length && /^\s*\|/.test(lines[i])) rows.push(cells(lines[i++]));
        out += `<table><thead><tr>${head.map((c) => `<th>${inline(c)}</th>`).join("")}</tr></thead><tbody>${rows.map((r) => `<tr>${r.map((c) => `<td>${inline(c)}</td>`).join("")}</tr>`).join("")}</tbody></table>`;
        continue;
      }
      if (/^\s*([-*+]|\d+[.)])\s+/.test(l)) {
        flush();
        const items = []; const ordered = /^\s*\d/.test(l);
        while (i < lines.length && (/^\s*([-*+]|\d+[.)])\s+/.test(lines[i]) || (/^\s{2,}\S/.test(lines[i]) && items.length))) {
          const cur = lines[i];
          if (/^\s*([-*+]|\d+[.)])\s+/.test(cur)) {
            const indent = cur.match(/^\s*/)[0].length;
            items.push({ indent, text: cur.replace(/^\s*([-*+]|\d+[.)])\s+/, "") });
          } else items[items.length - 1].text += " " + cur.trim();
          i++;
        }
        const render = (list, depth) => {
          let h = ordered && depth === 0 ? "<ol>" : "<ul>";
          let k = 0;
          while (k < list.length) {
            const it = list[k]; const sub = [];
            k++;
            while (k < list.length && list[k].indent > it.indent) sub.push(list[k++]);
            let txt = it.text;
            const task = /^\[([ xX])\]\s+/.exec(txt);
            if (task) txt = `<input type="checkbox" disabled ${task[1] !== " " ? "checked" : ""}> ` + inline(txt.slice(task[0].length)); else txt = inline(txt);
            h += `<li>${txt}${sub.length ? render(sub, depth + 1) : ""}</li>`;
          }
          return h + (ordered && depth === 0 ? "</ol>" : "</ul>");
        };
        out += render(items, 0); continue;
      }
      para.push(l.trim()); i++;
    }
    flush();
    return out;
  }

  /* ---------------- csv ---------------- */
  function csvTable(text, name) {
    const delim = /\.tsv$/i.test(name) ? "\t" : (text.split("\n")[0] || "").includes(";") && !(text.split("\n")[0] || "").includes(",") ? ";" : ",";
    const rows = []; let row = [], cell = "", q = false;
    for (let i = 0; i < text.length && rows.length < 2000; i++) {
      const c = text[i];
      if (q) { if (c === '"') { if (text[i + 1] === '"') { cell += '"'; i++; } else q = false; } else cell += c; }
      else if (c === '"') q = true;
      else if (c === delim) { row.push(cell); cell = ""; }
      else if (c === "\n") { row.push(cell); rows.push(row); row = []; cell = ""; }
      else if (c !== "\r") cell += c;
    }
    if (cell || row.length) { row.push(cell); rows.push(row); }
    if (!rows.length) return `<div class="empty small">${t("files.empty")}</div>`;
    const [head, ...body] = rows;
    return `<table><thead><tr>${head.map((c) => `<th>${esc(c)}</th>`).join("")}</tr></thead><tbody>${body.map((r) => `<tr>${r.map((c) => `<td>${esc(c)}</td>`).join("")}</tr>`).join("")}</tbody></table>${rows.length >= 2000 ? `<p class="sub">${t("files.csvCut")}</p>` : ""}`;
  }

  /* ---------------- tree ---------------- */
  const nodes = new Map(); // path -> entries
  async function loadDir(path) {
    try { const l = await invoke("fs_list", { path }); nodes.set(path, l); return l; } catch (e) { fail(e); return []; }
  }
  function renderTree() {
    const box = $("#files-tree");
    if (!F.root) { box.innerHTML = `<div class="empty small">${t("files.pickFolder")}</div>`; return; }
    const q = F.query.trim().toLowerCase();
    const label = (name) => {
      const i = q ? name.toLowerCase().indexOf(q) : -1;
      return i < 0 ? esc(name) : `${esc(name.slice(0, i))}<mark>${esc(name.slice(i, i + q.length))}</mark>${esc(name.slice(i + q.length))}`;
    };
    // While searching, every loaded folder is shown open and only matching names (or folders holding one) stay.
    const html = (dir, depth) => (nodes.get(dir) || []).filter((e) => F.showHidden || !e.hidden).map((e) => {
      const open = e.dir && (F.expanded.has(e.path) || (q && nodes.has(e.path)));
      const sub = open ? html(e.path, depth + 1) : "";
      if (q && !e.name.toLowerCase().includes(q) && !sub) return "";
      const active = F.doc && F.doc.path === e.path ? "active" : "";
      return `<div class="fnode ${e.dir ? "dir" : "file"} ${active} ${e.heavy ? "heavy" : ""}" data-path="${esc(e.path)}" data-dir="${e.dir ? 1 : 0}" style="--d:${depth}" title="${esc(e.path)}">
          <i class="fc ${open && (!q || sub) ? "open" : ""}">${e.dir ? ICONS.chevron : ""}</i><i class="fi">${e.dir ? ICONS.folder : ICONS.file}</i><span class="fn">${label(e.name)}</span>${e.dir ? "" : `<small>${fmtSize(e.size)}</small>`}</div>${sub}`;
    }).join("");
    box.innerHTML = html(F.root, 0) || `<div class="empty small">${t(q ? "files.noMatch" : "files.emptyFolder")}</div>`;
  }
  // Search walks subfolders on demand (skipping node_modules/vendor/target and, unless shown, dotfolders),
  // at most SEARCH_MAX_DIRS listings per search so a huge tree cannot stall the app. Results are cached in `nodes`.
  const SEARCH_MAX_DIRS = 400;
  let searchSeq = 0;
  async function loadForSearch() {
    const seq = ++searchSeq;
    if (!F.root || !F.query.trim()) return;
    let budget = SEARCH_MAX_DIRS;
    const queue = [F.root], seen = new Set();
    while (queue.length && budget > 0 && seq === searchSeq) {
      const dir = queue.shift(); if (seen.has(dir)) continue; seen.add(dir);
      let list = nodes.get(dir);
      if (!list) { budget--; try { list = await invoke("fs_list", { path: dir }); nodes.set(dir, list); } catch (e) { list = []; } }
      for (const e of list) if (e.dir && !e.heavy && (F.showHidden || !e.hidden)) queue.push(e.path);
    }
    if (seq !== searchSeq) return;
    renderTree();
    if (budget <= 0 && queue.length) toast(t("files.searchCut", { n: SEARCH_MAX_DIRS }), "info");
  }
  async function setRoot(path) {
    F.root = path; F.expanded.clear(); nodes.clear();
    try { localStorage.setItem("veranda.files.root", path); } catch (e) {}
    $("#files-root").value = F.roots.includes(path) ? path : "";
    $("#files-root-path").textContent = path;
    await loadDir(path); renderTree();
    if (F.query.trim()) loadForSearch();
  }
  async function toggleDir(path) {
    if (F.expanded.has(path)) F.expanded.delete(path);
    else { F.expanded.add(path); if (!nodes.has(path)) await loadDir(path); }
    renderTree();
  }
  async function refreshTree() { for (const d of [F.root, ...F.expanded]) await loadDir(d); renderTree(); }

  /* ---------------- editor ---------------- */
  const ta = () => $("#ed-text"), hl = () => $("#ed-hl code"), gutter = () => $("#ed-gutter");
  function setDirty(d) { F.dirty = d; $("#ed-dirty").classList.toggle("hidden", !d); $("#btn-ed-save").disabled = !d; }
  function renderHighlight() {
    if (!F.doc) return;
    const text = ta().value;
    hl().innerHTML = highlight(text + (text.endsWith("\n") ? " " : ""), F.doc.language);
    const lines = text.split("\n").length;
    const g = gutter(); if (g.childElementCount !== lines) g.innerHTML = Array.from({ length: lines }, (_, i) => `<span>${i + 1}</span>`).join("");
    $("#ed-stat-lines").textContent = t("files.lines", { n: lines, c: text.length });
  }
  function syncScroll() { const a = ta(); $("#ed-hl").scrollTop = a.scrollTop; $("#ed-hl").scrollLeft = a.scrollLeft; gutter().scrollTop = a.scrollTop; }
  function renderPreview() {
    const v = $("#ed-preview"); if (!F.doc) return;
    const text = ta().value;
    if (F.doc.language === "markdown") v.innerHTML = `<div class="md-view">${markdown(text)}</div>`;
    else if (F.doc.language === "csv") v.innerHTML = `<div class="md-view csv">${csvTable(text, F.doc.name)}</div>`;
    else if (F.doc.language === "json") { try { JSON.parse(text); v.innerHTML = `<div class="md-view"><p class="ok">${t("files.jsonOk")}</p></div>`; } catch (e) { v.innerHTML = `<div class="md-view"><p class="bad">${esc(e.message)}</p></div>`; } }
    else v.innerHTML = `<div class="md-view"><pre class="md-code"><code>${highlight(text, F.doc.language)}</code></pre></div>`;
  }
  function applyMode() {
    const canPreview = F.doc && ["markdown", "csv", "json"].includes(F.doc.language);
    $("#ed-modes").classList.toggle("hidden", !canPreview);
    if (!canPreview) F.mode = "edit";
    $$("#ed-modes button").forEach((b) => b.classList.toggle("on", b.dataset.mode === F.mode));
    const wrap = $("#ed-body"); wrap.dataset.mode = F.mode;
    if (F.mode !== "edit") renderPreview();
    $("#btn-ed-format").classList.toggle("hidden", !(F.doc && F.doc.language === "json"));
  }
  function applyWrap() { $("#ed-body").classList.toggle("wrap", F.wrap); $("#btn-ed-wrap").classList.toggle("on", F.wrap); gutter().classList.toggle("hidden", F.wrap); }
  async function openFile(path) {
    if (F.dirty && !(await askConfirm(t("files.discard", { name: F.doc.name }), t("files.discardBtn")))) return false;
    try {
      const d = await invoke("fs_read", { path });
      F.doc = d; ta().value = d.text; setDirty(false);
      $("#ed-name").textContent = d.name; $("#ed-path").textContent = d.path; $("#ed-path").title = d.path;
      $("#ed-lang").textContent = d.language; $("#ed-eol").textContent = d.crlf ? "CRLF" : "LF";
      $("#ed-empty").classList.add("hidden"); $("#ed-pane").classList.remove("hidden");
      F.mode = d.language === "markdown" ? "split" : "edit";
      applyMode(); applyWrap(); renderHighlight(); ta().scrollTop = 0; syncScroll(); renderTree();
      return true;
    } catch (e) { fail(e); return false; }
  }
  async function save() {
    if (!F.doc) return;
    try {
      const mtime = await invoke("fs_write", { path: F.doc.path, text: ta().value, crlf: F.doc.crlf, expectMtime: F.doc.mtime });
      F.doc.mtime = mtime; F.doc.text = ta().value; setDirty(false); toast(t("files.saved", { name: F.doc.name }), "ok"); refreshTree();
    } catch (e) {
      const msg = String(e?.message || e);
      if (/changed on disk/i.test(msg) && await askConfirm(t("files.conflict"), t("files.overwrite"))) {
        try { F.doc.mtime = await invoke("fs_write", { path: F.doc.path, text: ta().value, crlf: F.doc.crlf, expectMtime: null }); setDirty(false); toast(t("files.saved", { name: F.doc.name }), "ok"); } catch (x) { fail(x); }
      } else fail(e);
    }
  }
  function closeDoc() { F.doc = null; setDirty(false); $("#ed-pane").classList.add("hidden"); $("#ed-empty").classList.remove("hidden"); renderTree(); }

  /* ---------------- small prompts ---------------- */
  function askText(title, label, value) {
    return new Promise((resolve) => {
      const dlg = $("#dlg-text"), f = $("#form-text");
      $("#text-dlg-title").textContent = title; $("#text-dlg-label").textContent = label;
      f.elements.value.type = "text"; f.elements.value.value = value || "";
      const done = (v) => { dlg.close(); resolve(v); };
      f.onsubmit = (ev) => { ev.preventDefault(); done(f.elements.value.value.trim()); };
      dlg.oncancel = (ev) => { ev.preventDefault(); done(null); };
      $("[data-close]", f).onclick = () => done(null);
      dlg.showModal(); f.elements.value.focus(); f.elements.value.select();
    });
  }
  function contextMenu(x, y, items) {
    return new Promise((resolve) => {
      $("#files-ctx")?.remove();
      const m = document.createElement("div"); m.id = "files-ctx"; m.className = "ctx-menu";
      m.innerHTML = items.map(([id, label, cls]) => `<button type="button" class="${cls || ""}" data-act="${id}">${label}</button>`).join("");
      document.body.appendChild(m);
      const w = m.offsetWidth, h = m.offsetHeight;
      m.style.left = `${Math.min(x, window.innerWidth - w - 8)}px`; m.style.top = `${Math.min(y, window.innerHeight - h - 8)}px`;
      const close = (v) => { m.remove(); document.removeEventListener("mousedown", onDoc, true); document.removeEventListener("keydown", onKey, true); resolve(v); };
      const onDoc = (ev) => { if (!m.contains(ev.target)) close(null); };
      const onKey = (ev) => { if (ev.key === "Escape") close(null); };
      m.addEventListener("click", (ev) => { const b = ev.target.closest("[data-act]"); if (b) close(b.dataset.act); });
      setTimeout(() => { document.addEventListener("mousedown", onDoc, true); document.addEventListener("keydown", onKey, true); }, 0);
    });
  }

  /* ---------------- wiring ---------------- */
  async function init() {
    try { F.roots = await invoke("fs_roots"); } catch (e) { F.roots = []; }
    const sel = $("#files-root");
    sel.innerHTML = `<option value="">${t("files.otherFolder")}</option>` + F.roots.map((r) => `<option value="${esc(r)}">${esc(r)}</option>`).join("");
    if (!F.root || !(F.roots.includes(F.root) || F.root)) F.root = F.roots[0] || "";
    if (F.root) await setRoot(F.root); else renderTree();
    $("#files-hidden").checked = F.showHidden;
    $("#files-hidden-label").title = t("files.hiddenHint");
    applyWrap();
  }
  $("#files-root").onchange = (ev) => { if (ev.target.value) setRoot(ev.target.value); };
  $("#btn-files-open").onclick = async () => {
    try { const p = await window.__TAURI__.dialog.open({ directory: true, multiple: false, title: t("files.pickFolder") }); if (p) setRoot(typeof p === "string" ? p : p.path); } catch (e) { toast(t("migrate.noPicker"), "info"); }
  };
  $("#btn-files-refresh").onclick = refreshTree;
  $("#files-hidden").onchange = (ev) => { F.showHidden = ev.target.checked; renderTree(); if (F.query.trim()) loadForSearch(); };
  let searchTimer = null;
  $("#files-search").oninput = (ev) => {
    F.query = ev.target.value; renderTree();
    clearTimeout(searchTimer); searchSeq++;
    if (F.query.trim()) searchTimer = setTimeout(loadForSearch, 200);
  };
  $("#files-search").onkeydown = (ev) => { if (ev.key === "Escape") { ev.target.value = ""; F.query = ""; searchSeq++; renderTree(); } };
  const selectedDir = () => { const a = $("#files-tree .fnode.sel"); if (!a) return F.root; return a.dataset.dir === "1" ? a.dataset.path : parentOf(a.dataset.path); };
  $("#btn-files-newfile").onclick = async () => {
    const dir = selectedDir(); const name = await askText(t("files.newFile"), t("files.name"), "notes.md");
    if (!name) return;
    try { await invoke("fs_create", { path: join(dir, name), dir: false }); F.expanded.add(dir); await refreshTree(); await openFile(join(dir, name)); } catch (e) { fail(e); }
  };
  $("#btn-files-newdir").onclick = async () => {
    const dir = selectedDir(); const name = await askText(t("files.newFolder"), t("files.name"), "");
    if (!name) return;
    try { await invoke("fs_create", { path: join(dir, name), dir: true }); F.expanded.add(dir); await refreshTree(); } catch (e) { fail(e); }
  };
  $("#files-tree").addEventListener("click", async (ev) => {
    const node = ev.target.closest(".fnode"); if (!node) return;
    $$("#files-tree .fnode.sel").forEach((x) => x.classList.remove("sel")); node.classList.add("sel");
    if (node.dataset.dir === "1") { await toggleDir(node.dataset.path); $(`#files-tree .fnode[data-path="${CSS.escape(node.dataset.path)}"]`)?.classList.add("sel"); }
    else openFile(node.dataset.path);
  });
  $("#files-tree").addEventListener("contextmenu", async (ev) => {
    const node = ev.target.closest(".fnode"); if (!node) return; ev.preventDefault();
    const path = node.dataset.path, name = baseOf(path);
    const action = await contextMenu(ev.clientX, ev.clientY, [["rename", t("files.rename")], ["delete", t("btn.remove"), "danger"]]);
    if (action === "rename") {
      const nn = await askText(t("files.rename"), t("files.name"), name); if (!nn || nn === name) return;
      try { await invoke("fs_rename", { from: path, to: join(parentOf(path), nn) }); if (F.doc?.path === path) closeDoc(); await refreshTree(); } catch (e) { fail(e); }
    } else if (action === "delete") {
      if (!(await askConfirm(t("files.confirmDelete", { name }), t("btn.remove")))) return;
      try { await invoke("fs_delete", { path }); if (F.doc && (F.doc.path === path || F.doc.path.startsWith(path + sep()))) closeDoc(); await refreshTree(); } catch (e) { fail(e); }
    }
  });
  ta().addEventListener("input", () => { setDirty(ta().value !== F.doc?.text); clearTimeout(F.hlTimer); F.hlTimer = setTimeout(() => { renderHighlight(); if (F.mode !== "edit") renderPreview(); }, 60); });
  ta().addEventListener("scroll", syncScroll);
  ta().addEventListener("keydown", (ev) => {
    if (ev.key === "Tab") { ev.preventDefault(); const a = ta(); const s = a.selectionStart, e = a.selectionEnd; a.setRangeText("  ", s, e, "end"); a.dispatchEvent(new Event("input")); }
    if ((ev.ctrlKey || ev.metaKey) && ev.key.toLowerCase() === "s") { ev.preventDefault(); save(); }
  });
  document.addEventListener("keydown", (ev) => { if ((ev.ctrlKey || ev.metaKey) && ev.key.toLowerCase() === "s" && $("#tab-files").classList.contains("active") && F.doc && document.activeElement !== ta()) { ev.preventDefault(); save(); } });
  $("#btn-ed-save").onclick = save;
  $("#btn-ed-reload").onclick = async () => { if (F.doc) { const p = F.doc.path; F.dirty = false; await openFile(p); } };
  $("#btn-ed-close").onclick = async () => { if (F.dirty && !(await askConfirm(t("files.discard", { name: F.doc.name }), t("files.discardBtn")))) return; closeDoc(); };
  $("#btn-ed-external").onclick = () => { if (F.doc) invoke("open_target", { what: `path:${F.doc.path}` }).catch(fail); };
  $("#btn-ed-wrap").onclick = () => { F.wrap = !F.wrap; try { localStorage.setItem("veranda.files.wrap", F.wrap ? "1" : "0"); } catch (e) {} applyWrap(); };
  $("#btn-ed-format").onclick = () => { try { const v = JSON.stringify(JSON.parse(ta().value), null, 2) + "\n"; ta().value = v; ta().dispatchEvent(new Event("input")); } catch (e) { toast(e.message, "bad"); } };
  $("#ed-modes").addEventListener("click", (ev) => { const b = ev.target.closest("[data-mode]"); if (!b) return; F.mode = b.dataset.mode; applyMode(); });
  $("#ed-preview").addEventListener("click", (ev) => {
    const a = ev.target.closest("[data-md-link]"); if (!a) return; ev.preventDefault();
    const href = a.dataset.mdLink;
    if (/^(https?:|mailto:)/i.test(href)) invoke("open_url_external", { url: href }).catch(() => invoke("open_target", { what: `url:${href}` }).catch(fail));
    else if (href && href !== "#" && F.doc) openFile(join(parentOf(F.doc.path), href.replace(/^\.\//, "").split("#")[0]));
  });
  document.addEventListener("veranda:tab", (ev) => { if (ev.detail === "files" && !F.roots.length) init(); });
  window.addEventListener("beforeunload", (ev) => { if (F.dirty) { ev.preventDefault(); ev.returnValue = ""; } });

  window.VerandaFiles = {
    init,
    open: async (path) => { showTab("files"); if (!F.roots.length) await init(); await setRoot(path); },
    openFile: async (path) => { showTab("files"); if (!F.roots.length) await init(); const dir = parentOf(path); if (!F.root || !path.startsWith(F.root)) await setRoot(dir); return openFile(path); },
    autotest: async (tab, report) => {
      const root = `${state.root}${state.root.includes("\\") ? "\\" : "/"}www`;
      const s = root.includes("\\") ? "\\" : "/";
      const demo = `${root}${s}demo-notes`;
      try { await invoke("fs_create", { path: demo, dir: true }); } catch (e) {}
      try { await invoke("fs_write", { path: `${demo}${s}README.md`, text: "# Demo notes\n\nA **bold** idea, some `code`, a [link](https://example.com).\n\n- first\n- second\n  - nested\n- [x] done\n\n| col | value |\n|---|---|\n| a | 1 |\n| b | 2 |\n\n```js\nconst x = 42; // answer\nfunction hi(name) { return `hi ${name}`; }\n```\n", crlf: false, expectMtime: null }); } catch (e) {}
      try { await invoke("fs_write", { path: `${demo}${s}config.json`, text: '{\n  "name": "demo",\n  "port": 8081,\n  "debug": true,\n  "tags": ["a", "b"]\n}\n', crlf: false, expectMtime: null }); } catch (e) {}
      try { await invoke("fs_write", { path: `${demo}${s}index.php`, text: "<?php\n// routes\nfunction hello(string $name): string {\n    return \"Hello, $name\";\n}\necho hello('world');\n", crlf: false, expectMtime: null }); } catch (e) {}
      showTab("files"); await init(); await setRoot(root); F.expanded.add(demo); await loadDir(demo); renderTree();
      const file = tab === "json" ? "config.json" : tab === "php" ? "index.php" : "README.md";
      const ok = await openFile(`${demo}${s}${file}`);
      if (tab === "md-edit") { F.mode = "edit"; applyMode(); }
      report(`open=${ok} lang=${F.doc?.language} mode=${F.mode} tree=${$$("#files-tree .fnode").length}`);
    },
  };
})();
