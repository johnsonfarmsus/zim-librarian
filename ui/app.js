/* ZIM Librarian — minimal chat frontend. No frameworks, no build step. */
"use strict";

const $ = (id) => document.getElementById(id);
const chatEl = $("chat");
let currentChatId = null;
let busy = false;

/* ---------------- sidebar tabs ---------------- */

function showTab(name) {
  const chats = name === "chats";
  $("tab-chats").classList.toggle("active", chats);
  $("tab-setup").classList.toggle("active", !chats);
  $("pane-chats").classList.toggle("hidden", !chats);
  $("pane-setup").classList.toggle("hidden", chats);
  document.body.classList.remove("nav-open"); // close the mobile drawer on select
}
$("tab-chats").onclick = () => showTab("chats");
$("tab-setup").onclick = () => showTab("setup");

/* mobile off-canvas navigation */
$("nav-toggle").onclick = () => document.body.classList.toggle("nav-open");
$("scrim").onclick = () => document.body.classList.remove("nav-open");

/* ---------------- library panel ---------------- */

let firstLibraryLoad = true;
async function refreshLibrary() {
  const res = await fetch("/api/library");
  const { books, books_dir } = await res.json();
  $("books-dir-hint").textContent =
    `Tip: .zim files dropped into ${books_dir} are added automatically.`;
  if (firstLibraryLoad) {
    firstLibraryLoad = false;
    // Fresh install: land the user where they can add books.
    if (!books.length) showTab("setup");
  }
  const status = await (await fetch("/api/status")).json();
  const prog = Object.fromEntries(status.indexing.map((p) => [p.id, p]));
  const el = $("books");
  el.innerHTML = "";
  if (!books.length) {
    const d = document.createElement("div");
    d.className = "hint";
    d.textContent = "No books yet. Add a .zim file to begin.";
    el.appendChild(d);
  }
  for (const b of books) {
    const div = document.createElement("div");
    div.className = "book";
    const p = prog[b.id];
    const indexing = p && !p.finished;
    const pct = indexing && p.total ? Math.round((100 * p.done) / p.total) : 100;

    const t = document.createElement("div");
    t.className = "t";
    t.textContent = b.title;
    t.title = "Browse this book";
    if (!b.missing) t.onclick = () => readerOpen(`/home/${b.id}`, b.title);
    const d = document.createElement("div");
    d.className = "d";
    const label = document.createElement("span");
    label.textContent = b.missing
      ? "⚠ file moved or missing"
      : indexing
        ? `indexing… ${pct}%`
        : p && p.failed
          ? "indexing failed"
          : b.indexed
            ? `${b.chunks.toLocaleString()} passages`
            : "not indexed";
    if (b.missing) label.style.color = "#c0392b";
    const rm = document.createElement("button");
    rm.textContent = "remove";
    rm.title = "Remove from library";
    rm.onclick = async () => {
      if (!confirm(`Remove “${b.title}” from the library? (The .zim file is not deleted.)`)) return;
      await fetch(`/api/library/${b.id}`, { method: "DELETE" });
      refreshLibrary();
    };
    d.append(label, rm);
    div.append(t, d);
    if (indexing) {
      const bar = document.createElement("div");
      bar.className = "progress";
      const fill = document.createElement("div");
      fill.style.width = pct + "%";
      bar.appendChild(fill);
      div.appendChild(bar);
    }
    el.appendChild(div);
  }
  const anyIndexing = status.indexing.some((p) => !p.finished);
  // Gate the chat composer on having something to answer from, and show a
  // first-run indexing banner so the user knows when the library is ready.
  const anyIndexed = books.some((b) => b.indexed);
  document.body.classList.toggle("has-indexed", anyIndexed);
  updateIndexBanner(books, prog);
  if (anyIndexing) setTimeout(refreshLibrary, 1200);
}

// Show a clear status banner in the main area during the one-time index, so the
// user knows the app is working and exactly when they can start asking.
function updateIndexBanner(books, prog) {
  const banner = $("index-banner");
  const active = books
    .map((b) => ({ b, p: prog[b.id] }))
    .find((x) => x.p && !x.p.finished);
  if (active) {
    const pct = active.p.total
      ? Math.round((100 * active.p.done) / active.p.total)
      : 0;
    banner.textContent =
      `Indexing “${active.b.title}” — ${pct}%. This happens once; you can ask ` +
      `questions as soon as it finishes.`;
    banner.classList.remove("hidden");
  } else if (books.length && !books.some((b) => b.indexed)) {
    banner.textContent = "Preparing your book for search… this only takes a moment.";
    banner.classList.remove("hidden");
  } else {
    banner.classList.add("hidden");
  }
}

$("rescan").onclick = async () => {
  $("rescan").disabled = true;
  await fetch("/api/library/scan", { method: "POST" });
  $("rescan").disabled = false;
  refreshLibrary();
};

/* ---------------- file browser (add book / add model) ---------------- */

let fsParent = null;
let fsMode = "zim"; // "zim" adds a book; "gguf" selects a model file

function fmtSize(bytes) {
  if (bytes >= 1 << 30) return (bytes / (1 << 30)).toFixed(1) + " GB";
  if (bytes >= 1 << 20) return Math.round(bytes / (1 << 20)) + " MB";
  return Math.max(1, Math.round(bytes / 1024)) + " KB";
}

async function addBookPath(path) {
  const res = await fetch("/api/library", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ path }),
  });
  if (!res.ok) {
    alert(await res.text());
    return false;
  }
  return true;
}

async function fsBrowse(path) {
  const q = `exts=${fsMode}` + (path ? `&path=${encodeURIComponent(path)}` : "");
  const res = await fetch(`/api/fs?${q}`);
  if (!res.ok) return;
  const d = await res.json();
  fsParent = d.parent;
  $("fs-cur").textContent = d.path;
  $("fs-up").disabled = !fsParent;

  const quick = $("fs-quick");
  quick.innerHTML = "";
  for (const q of d.quick) {
    const b = document.createElement("button");
    b.textContent = q.label;
    b.onclick = () => fsBrowse(q.path);
    quick.appendChild(b);
  }

  const list = $("fs-list");
  list.innerHTML = "";
  for (const name of d.dirs) {
    const row = document.createElement("div");
    row.className = "fs-row";
    row.textContent = `📁 ${name}`;
    row.onclick = () => fsBrowse(`${d.path}/${name}`);
    list.appendChild(row);
  }
  for (const f of d.files) {
    const row = document.createElement("div");
    row.className = "fs-row file";
    const label = document.createElement("span");
    label.textContent = `${fsMode === "gguf" ? "🧠" : "📕"} ${f.name}`;
    const size = document.createElement("span");
    size.className = "size";
    size.textContent = fmtSize(f.size);
    row.append(label, size);
    row.onclick = async () => {
      row.style.opacity = "0.5";
      const full = `${d.path}/${f.name}`;
      if (fsMode === "gguf") {
        // Use the model where it is: settings accept an absolute path.
        await fetch("/api/models", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ model: full }),
        });
        $("fs-modal").classList.add("hidden");
        refreshModels();
      } else if (await addBookPath(full)) {
        $("fs-modal").classList.add("hidden");
        refreshLibrary();
      } else {
        row.style.opacity = "";
      }
    };
    list.appendChild(row);
  }
  if (!d.dirs.length && !d.files.length) {
    const empty = document.createElement("div");
    empty.className = "fs-empty";
    empty.textContent = `No folders or .${fsMode} files here.`;
    list.appendChild(empty);
  }
}

function fsOpen(mode) {
  fsMode = mode;
  $("fs-title").textContent = mode === "gguf" ? "Choose a model file" : "Add a ZIM book";
  $("fs-hint").innerHTML = "";
  if (mode === "gguf") {
    $("fs-hint").textContent =
      "Pick a .gguf model — it is used where it is, nothing is copied.";
  } else {
    const a = document.createElement("a");
    a.href = "https://library.kiwix.org";
    a.target = "_blank";
    a.textContent = "library.kiwix.org";
    $("fs-hint").append("Pick a .zim file — get more at ", a);
  }
  $("fs-modal").classList.remove("hidden");
  fsBrowse(null);
}

$("add-book").onclick = () => fsOpen("zim");
$("add-model-file").onclick = () => fsOpen("gguf");
$("fs-close").onclick = () => $("fs-modal").classList.add("hidden");
$("fs-up").onclick = () => fsParent && fsBrowse(fsParent);
$("fs-modal").onclick = (e) => {
  if (e.target === $("fs-modal")) $("fs-modal").classList.add("hidden");
};

/* ---------------- model panel ---------------- */

async function refreshModels() {
  const m = await (await fetch("/api/models")).json();
  const sel = $("model-select");
  sel.innerHTML = "";
  const none = document.createElement("option");
  none.value = "";
  none.textContent = m.llama_compiled ? "— no model (passages only) —" : "— extractive mode —";
  sel.appendChild(none);
  for (const name of m.available) {
    const o = document.createElement("option");
    o.value = name;
    o.textContent = name;
    if (name === m.selected) o.selected = true;
    sel.appendChild(o);
  }
  $("model-info").textContent = m.llama_compiled
    ? `Drop .gguf files into: ${m.models_dir}`
    : "Built without llama support: answers quote passages directly.";
  sel.onchange = async () => {
    sel.disabled = true;
    await fetch("/api/models", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ model: sel.value || null }),
    });
    sel.disabled = false;
  };
}

/* ---------------- model catalog ---------------- */

let catalogPolling = false;

async function refreshCatalog() {
  const { models } = await (await fetch("/api/models/catalog")).json();
  const el = $("catalog");
  el.innerHTML = "";
  let anyDownloading = false;
  for (const m of models) {
    const item = document.createElement("div");
    item.className = "cat-item";
    const row = document.createElement("div");
    row.className = "row";
    const left = document.createElement("div");
    const t = document.createElement("div");
    t.className = "t";
    t.textContent = `${m.label} · ${fmtSize(m.bytes)}`;
    const n = document.createElement("div");
    n.className = "n";
    n.textContent = m.notes;
    left.append(t, n);
    row.appendChild(left);

    item.appendChild(row);
    if (m.status.state === "installed") {
      const s = document.createElement("span");
      s.className = "installed";
      s.textContent = "✓ installed";
      row.appendChild(s);
    } else if (m.status.state === "downloading") {
      anyDownloading = true;
      const pct = m.status.total ? Math.round((100 * m.status.done) / m.status.total) : 0;
      const s = document.createElement("span");
      s.className = "installed";
      s.textContent = `${pct}%`;
      row.appendChild(s);
      const bar = document.createElement("div");
      bar.className = "progress";
      const fill = document.createElement("div");
      fill.style.width = pct + "%";
      bar.appendChild(fill);
      item.appendChild(bar);
    } else {
      const b = document.createElement("button");
      b.textContent = "Download";
      b.onclick = async () => {
        b.disabled = true;
        await fetch("/api/models/download", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ id: m.id }),
        });
        refreshCatalog();
      };
      row.appendChild(b);
      if (m.status.state === "error") {
        const err = document.createElement("div");
        err.className = "err";
        err.textContent = `Download failed: ${m.status.message}`;
        item.appendChild(err);
      }
    }
    el.appendChild(item);
  }
  if (anyDownloading && !catalogPolling) {
    catalogPolling = true;
    setTimeout(() => {
      catalogPolling = false;
      refreshCatalog();
    }, 1500);
  }
  if (!anyDownloading) refreshModels();
}

/* ---------------- starter-library (ZIM) catalog ---------------- */

let zimPolling = false;

async function refreshZimCatalog() {
  const { zims } = await (await fetch("/api/zims/catalog")).json();
  const el = $("zim-catalog");
  el.innerHTML = "";
  let anyDownloading = false;
  for (const z of zims) {
    // Installed starter books already appear as real book cards above.
    if (z.status.state === "installed") continue;
    const item = document.createElement("div");
    item.className = "cat-item";
    const row = document.createElement("div");
    row.className = "row";
    const left = document.createElement("div");
    const t = document.createElement("div");
    t.className = "t";
    t.textContent = `${z.label} · ${fmtSize(z.bytes)}`;
    const n = document.createElement("div");
    n.className = "n";
    n.textContent = z.notes;
    left.append(t, n);
    row.appendChild(left);
    item.appendChild(row);
    if (z.status.state === "downloading") {
      anyDownloading = true;
      const pct = z.status.total ? Math.round((100 * z.status.done) / z.status.total) : 0;
      const s = document.createElement("span");
      s.className = "installed";
      s.textContent = `${pct}%`;
      row.appendChild(s);
      const bar = document.createElement("div");
      bar.className = "progress";
      const fill = document.createElement("div");
      fill.style.width = pct + "%";
      bar.appendChild(fill);
      item.appendChild(bar);
    } else {
      const b = document.createElement("button");
      b.textContent = "Download";
      b.onclick = async () => {
        b.disabled = true;
        await fetch("/api/zims/download", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ id: z.id }),
        });
        refreshZimCatalog();
      };
      row.appendChild(b);
      if (z.status.state === "error") {
        const err = document.createElement("div");
        err.className = "err";
        err.textContent = `Download failed: ${z.status.message}`;
        item.appendChild(err);
      }
    }
    el.appendChild(item);
  }
  if (anyDownloading && !zimPolling) {
    zimPolling = true;
    setTimeout(() => {
      zimPolling = false;
      refreshZimCatalog();
    }, 1500);
  } else if (!anyDownloading) {
    refreshLibrary();
  }
}

/* ---------------- add model from URL ---------------- */

let urlPolling = false;

async function pollUrlDownloads() {
  const { downloads } = await (await fetch("/api/models/downloads")).json();
  const modelEl = $("url-dl-status");
  const zimEl = $("zim-dl-status");
  modelEl.textContent = "";
  zimEl.textContent = "";
  let active = false;
  for (const d of downloads) {
    const line = document.createElement("div");
    if (d.error) {
      line.textContent = `✗ ${d.name}: ${d.error}`;
    } else {
      active = true;
      const pct = d.total ? Math.round((100 * d.done) / d.total) : 0;
      line.textContent = `↓ ${d.name} — ${pct}%`;
    }
    (d.kind === "zim" ? zimEl : modelEl).appendChild(line);
  }
  if (active && !urlPolling) {
    urlPolling = true;
    setTimeout(() => {
      urlPolling = false;
      pollUrlDownloads();
    }, 1500);
  } else if (!active) {
    refreshModels();
    refreshLibrary();
  }
}

function urlAdd(inputId, endpoint) {
  return async () => {
    const url = $(inputId).value.trim();
    if (!url) return;
    const res = await fetch(endpoint, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ url }),
    });
    if (!res.ok) {
      alert(await res.text());
      return;
    }
    $(inputId).value = "";
    pollUrlDownloads();
  };
}
$("model-url-go").onclick = urlAdd("model-url", "/api/models/download-url");
$("zim-url-go").onclick = urlAdd("zim-url", "/api/zims/download-url");

/* ---------------- first-run setup card ---------------- */

let setupDismissed = false;
let setupStarted = false;
const setupChecks = {}; // id -> bool, user's checkbox choices (default on)

function removeSetupCard() {
  const c = $("setup-card");
  if (c) c.remove();
}

async function maybeShowSetup() {
  if (setupDismissed) return;
  const [{ books }, m, { zims }, { models }] = await Promise.all([
    (await fetch("/api/library")).json(),
    (await fetch("/api/models")).json(),
    (await fetch("/api/zims/catalog")).json(),
    (await fetch("/api/models/catalog")).json(),
  ]);
  const modelReady = m.available.length > 0;
  const anyActive =
    zims.some((z) => z.status.state === "downloading") ||
    models.some((x) => x.status.state === "downloading");
  // Show on a fresh install; keep showing while its downloads run.
  if (books.length && modelReady && !anyActive && !setupStarted) return;
  if (books.length && !setupStarted) return;
  renderSetupCard(m, zims, models, modelReady);
}

function renderSetupCard(m, zims, models, modelReady) {
  removeSetupCard();
  const card = document.createElement("div");
  card.id = "setup-card";

  const h = document.createElement("h3");
  h.textContent = "Welcome! Let's stock your library";
  const intro = document.createElement("p");
  intro.className = "hint";
  intro.textContent =
    "Pick what to download — everything runs and stays on this device. " +
    "You can add more (or your own files) later in Library & Model.";
  card.append(h, intro);

  const rows = [];
  // Model row: bundled installs already have one.
  if (modelReady) {
    const done = document.createElement("div");
    done.className = "setup-row done";
    done.textContent = `✓ AI model ready (${m.selected || m.available[0]})`;
    card.appendChild(done);
  } else {
    const olmo = models.find((x) => x.id === "olmo-2-1b");
    rows.push({
      id: "model:olmo-2-1b",
      label: "OLMo 2 1B — the librarian's AI model",
      bytes: (olmo && olmo.status.total) || (olmo && olmo.bytes) || 935515296,
      state: olmo ? olmo.status.state : "absent",
      done: olmo && olmo.status.done,
      message: olmo && olmo.status.message,
      kind: "model",
    });
  }
  for (const z of zims) {
    rows.push({
      id: z.id,
      label: z.label,
      bytes: z.status.total || z.bytes,
      state: z.status.state,
      done: z.status.done,
      message: z.status.message,
      kind: "zim",
    });
  }

  const totalEl = document.createElement("div");
  const btnRow = document.createElement("div");
  const updateTotal = () => {
    const sum = rows
      .filter((r) => r.state === "absent" && setupChecks[r.id] !== false)
      .reduce((a, r) => a + r.bytes, 0);
    totalEl.textContent = sum
      ? `Selected: ${fmtSize(sum)} — downloaded once, then it's yours offline forever.`
      : "Nothing selected.";
  };

  let anyAbsent = false;
  let anyActive = false;
  for (const r of rows) {
    const row = document.createElement("div");
    row.className = "setup-row";
    if (r.state === "installed") {
      row.classList.add("done");
      row.textContent = `✓ ${r.label}`;
    } else if (r.state === "downloading") {
      anyActive = true;
      const pct = r.bytes ? Math.round((100 * (r.done || 0)) / r.bytes) : 0;
      const lab = document.createElement("span");
      lab.textContent = `↓ ${r.label} — ${pct}%`;
      const bar = document.createElement("div");
      bar.className = "progress";
      const fill = document.createElement("div");
      fill.style.width = pct + "%";
      bar.appendChild(fill);
      row.append(lab, bar);
    } else {
      anyAbsent = true;
      const label = document.createElement("label");
      const cb = document.createElement("input");
      cb.type = "checkbox";
      cb.checked = setupChecks[r.id] !== false;
      cb.onchange = () => {
        setupChecks[r.id] = cb.checked;
        updateTotal();
      };
      const span = document.createElement("span");
      span.textContent = ` ${r.label} · ${fmtSize(r.bytes)}`;
      label.append(cb, span);
      row.appendChild(label);
      if (r.state === "error") {
        const err = document.createElement("div");
        err.className = "err";
        err.textContent = `Download failed: ${r.message} — check the connection and try again.`;
        row.appendChild(err);
      }
    }
    card.appendChild(row);
  }

  totalEl.className = "hint total";
  btnRow.className = "setup-actions";
  card.append(totalEl, btnRow);

  if (anyAbsent) {
    updateTotal();
    const go = document.createElement("button");
    go.id = "setup-go";
    go.textContent = "Download selected";
    go.onclick = async () => {
      go.disabled = true;
      setupStarted = true;
      for (const r of rows) {
        if (r.state !== "absent" || setupChecks[r.id] === false) continue;
        if (r.kind === "model") {
          await fetch("/api/models/download", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ id: r.id.split(":")[1] }),
          });
        } else {
          await fetch("/api/zims/download", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ id: r.id }),
          });
        }
      }
      pollSetup();
      refreshCatalog();
      refreshZimCatalog();
    };
    const skip = document.createElement("button");
    skip.className = "small";
    skip.textContent = anyActive ? "Hide" : "Skip for now";
    skip.onclick = () => {
      setupDismissed = true;
      removeSetupCard();
    };
    btnRow.append(go, skip);
  } else if (anyActive) {
    totalEl.textContent =
      "Downloading… you can start chatting as soon as the first book is indexed.";
    const hide = document.createElement("button");
    hide.className = "small";
    hide.textContent = "Hide";
    hide.onclick = () => {
      setupDismissed = true;
      removeSetupCard();
    };
    btnRow.append(hide);
  } else {
    // Everything requested is in place.
    totalEl.textContent = "✓ All set — ask your library anything.";
    setupStarted = false;
    const ok = document.createElement("button");
    ok.id = "setup-go";
    ok.textContent = "Start";
    ok.onclick = () => {
      setupDismissed = true;
      removeSetupCard();
      refreshLibrary();
      $("question").focus();
    };
    btnRow.append(ok);
  }

  chatEl.insertBefore(card, chatEl.firstChild);
  if (anyActive) setTimeout(pollSetup, 1500);
}

let setupPolling = false;
function pollSetup() {
  if (setupPolling || setupDismissed) return;
  setupPolling = true;
  setTimeout(async () => {
    setupPolling = false;
    await maybeShowSetup();
  }, 1500);
}

/* ---------------- about ---------------- */

$("about-open").onclick = () => $("about-modal").classList.remove("hidden");
$("about-close").onclick = () => $("about-modal").classList.add("hidden");
$("about-modal").onclick = (e) => {
  if (e.target === $("about-modal")) $("about-modal").classList.add("hidden");
};

/* ---------------- chat ---------------- */

function addMsg(role, text) {
  const div = document.createElement("div");
  div.className = `msg ${role}`;
  const who = document.createElement("div");
  who.className = "who";
  who.textContent = role === "user" ? "You" : "Librarian";
  const bubble = document.createElement("div");
  bubble.className = "bubble";
  bubble.textContent = text;
  div.append(who, bubble);
  chatEl.appendChild(div);
  chatEl.scrollTop = chatEl.scrollHeight;
  return bubble;
}

function setThinking(bubble, text) {
  bubble.textContent = "";
  const s = document.createElement("span");
  s.className = "thinking";
  s.textContent = text;
  bubble.appendChild(s);
}

function setError(bubble, text) {
  bubble.textContent = "";
  const s = document.createElement("span");
  s.className = "error";
  s.textContent = text;
  bubble.appendChild(s);
}

function readerOpen(src, title) {
  $("reader-title").textContent = title || "Source";
  $("reader-frame").src = src;
  $("reader").classList.remove("hidden");
}
$("reader-close").onclick = () => {
  $("reader").classList.add("hidden");
  $("reader-frame").src = "about:blank";
};

function citationUrl(s) {
  const path = s.path.split("/").map(encodeURIComponent).join("/");
  return `/content/${s.zim_id}/${path}?hl=${encodeURIComponent(s.text.slice(0, 300))}`;
}

/* Render answer text with [n] as clickable citation chips (DOM-built, no
   HTML injection: source titles/text never enter innerHTML). */
function renderAnswer(el, text, sources) {
  el.textContent = "";
  const re = /\[(\d{1,2})\]/g;
  let last = 0;
  let m;
  while ((m = re.exec(text)) !== null) {
    const s = sources[Number(m[1]) - 1];
    el.appendChild(document.createTextNode(text.slice(last, m.index)));
    if (s) {
      const a = document.createElement("a");
      a.className = "cite";
      a.textContent = m[1];
      a.title = `${s.title} — ${s.book}`;
      a.onclick = () => readerOpen(citationUrl(s), s.title);
      el.appendChild(a);
    } else {
      el.appendChild(document.createTextNode(m[0]));
    }
    last = m.index + m[0].length;
  }
  el.appendChild(document.createTextNode(text.slice(last)));
}

function renderSources(sources, beforeEl) {
  if (!sources.length) return;
  const wrap = document.createElement("div");
  wrap.className = "sources";
  sources.forEach((s, i) => {
    const c = document.createElement("div");
    c.className = "source-card";
    const n = document.createElement("span");
    n.className = "n";
    n.textContent = i + 1;
    const t = document.createElement("span");
    t.className = "t";
    t.textContent = s.title;
    const b = document.createElement("span");
    b.className = "b";
    b.textContent = s.book;
    c.append(n, t, b);
    c.onclick = () => readerOpen(citationUrl(s), s.title);
    wrap.appendChild(c);
  });
  chatEl.insertBefore(wrap, beforeEl);
  chatEl.scrollTop = chatEl.scrollHeight;
}

/* Minimal spec-shaped SSE parser over fetch(): dispatches (event, data) with
   multi-line data joined by newlines, events separated by blank lines. */
async function readSse(res, onEvent) {
  const reader = res.body.getReader();
  const dec = new TextDecoder();
  let buf = "";
  let ev = "message";
  let data = [];
  const flush = () => {
    if (data.length) onEvent(ev, data.join("\n"));
    ev = "message";
    data = [];
  };
  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    buf += dec.decode(value, { stream: true });
    let idx;
    while ((idx = buf.indexOf("\n")) !== -1) {
      const line = buf.slice(0, idx).replace(/\r$/, "");
      buf = buf.slice(idx + 1);
      if (line === "") flush();
      else if (line.startsWith("event:")) ev = line.slice(6).trim();
      else if (line.startsWith("data:")) data.push(line.slice(5).replace(/^ /, ""));
      // comments (":keep-alive") and other fields are ignored
    }
  }
  flush();
}

/* ---------------- chats (history) ---------------- */

async function refreshChats() {
  const { chats } = await (await fetch("/api/chats")).json();
  const el = $("chats");
  el.innerHTML = "";
  const visible = chats.filter((c) => c.messages > 0);
  if (!visible.length) {
    const d = document.createElement("div");
    d.className = "hint";
    d.textContent = "No chats yet.";
    el.appendChild(d);
    return;
  }
  for (const c of visible) {
    const item = document.createElement("div");
    item.className = "chat-item" + (c.id === currentChatId ? " active" : "");
    const star = document.createElement("button");
    star.className = "star" + (c.starred ? " on" : "");
    star.textContent = c.starred ? "★" : "☆";
    star.title = c.starred
      ? "Unstar (old chats auto-delete past 15)"
      : "Star: pin to top and never auto-delete";
    star.onclick = async (e) => {
      e.stopPropagation();
      await fetch(`/api/chats/${c.id}/star`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ starred: !c.starred }),
      });
      refreshChats();
    };
    const t = document.createElement("span");
    t.className = "title";
    t.textContent = c.title;
    t.title = c.title;
    const del = document.createElement("button");
    del.className = "del";
    del.textContent = "✕";
    del.title = "Delete chat";
    del.onclick = async (e) => {
      e.stopPropagation();
      if (!confirm(`Delete chat “${c.title}”?`)) return;
      await fetch(`/api/chats/${c.id}`, { method: "DELETE" });
      if (c.id === currentChatId) startNewChat();
      else refreshChats();
    };
    item.append(star, t, del);
    item.onclick = () => loadChat(c.id);
    el.appendChild(item);
  }
}

function greeting() {
  addMsg(
    "assistant",
    "Hello! Add ZIM books to your library on the left, then ask me anything. " +
      "I answer only from your books and cite every claim — click a citation to read the original passage."
  );
}

function startNewChat() {
  currentChatId = null;
  chatEl.innerHTML = "";
  greeting();
  refreshChats();
  $("question").focus();
}

async function loadChat(id) {
  const res = await fetch(`/api/chats/${id}`);
  if (!res.ok) return;
  const { chat } = await res.json();
  currentChatId = chat.id;
  chatEl.innerHTML = "";
  for (const m of chat.messages) {
    const bubble = addMsg(m.role, "");
    if (m.role === "assistant") {
      const sources = m.sources || [];
      renderSources(sources, bubble.parentElement);
      renderAnswer(bubble, m.content, sources);
    } else {
      bubble.textContent = m.content;
    }
  }
  chatEl.scrollTop = chatEl.scrollHeight;
  refreshChats();
}

$("new-chat").onclick = startNewChat;

async function ask(question) {
  busy = true;
  $("send").disabled = true;
  addMsg("user", question);
  const bubble = addMsg("assistant", "");
  setThinking(bubble, "searching the library…");

  let sources = [];
  let answer = "";
  try {
    const res = await fetch("/api/chat", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ chat_id: currentChatId, message: question }),
    });
    if (!res.ok) throw new Error(await res.text());
    await readSse(res, (event, data) => {
      if (event === "chat") {
        const c = JSON.parse(data);
        currentChatId = c.id;
        refreshChats();
      } else if (event === "plan") {
        const p = JSON.parse(data);
        const note = document.createElement("div");
        note.className = "plan-note";
        note.textContent = p.reused
          ? "↩ continuing from the sources already on the table"
          : `🔎 searched your library for: ${p.query}`;
        chatEl.insertBefore(note, bubble.parentElement);
        setThinking(bubble, "searching the library…");
      } else if (event === "sources") {
        sources = JSON.parse(data);
        renderSources(sources, bubble.parentElement);
        setThinking(bubble, "reading sources…");
      } else if (event === "token") {
        answer += data;
        renderAnswer(bubble, answer, sources);
        chatEl.scrollTop = chatEl.scrollHeight;
      } else if (event === "done") {
        answer = data || answer;
        renderAnswer(bubble, answer, sources);
      } else if (event === "error") {
        setError(bubble, data);
      }
    });
    if (!answer && !bubble.querySelector(".error")) {
      setError(bubble, "The model returned no answer. Try rephrasing, or check that a model is selected.");
    }
  } catch (e) {
    setError(bubble, e.message || String(e));
  }
  busy = false;
  $("send").disabled = false;
}

$("composer").onsubmit = (e) => {
  e.preventDefault();
  if (busy) return;
  const q = $("question").value.trim();
  if (!q) return;
  $("question").value = "";
  ask(q);
};
$("question").addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    $("composer").requestSubmit();
  }
});

refreshLibrary();
refreshModels();
refreshCatalog();
refreshZimCatalog();
refreshChats();
greeting();
maybeShowSetup();
pollUrlDownloads();
