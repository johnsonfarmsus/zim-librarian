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
}
$("tab-chats").onclick = () => showTab("chats");
$("tab-setup").onclick = () => showTab("setup");

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
  if (anyIndexing) setTimeout(refreshLibrary, 1200);
}

$("rescan").onclick = async () => {
  $("rescan").disabled = true;
  await fetch("/api/library/scan", { method: "POST" });
  $("rescan").disabled = false;
  refreshLibrary();
};

/* ---------------- file browser (add book) ---------------- */

let fsParent = null;

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
  const url = path ? `/api/fs?path=${encodeURIComponent(path)}` : "/api/fs";
  const res = await fetch(url);
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
    label.textContent = `📕 ${f.name}`;
    const size = document.createElement("span");
    size.className = "size";
    size.textContent = fmtSize(f.size);
    row.append(label, size);
    row.onclick = async () => {
      row.style.opacity = "0.5";
      if (await addBookPath(`${d.path}/${f.name}`)) {
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
    empty.textContent = "No folders or .zim files here.";
    list.appendChild(empty);
  }
}

$("add-book").onclick = () => {
  $("fs-modal").classList.remove("hidden");
  fsBrowse(null);
};
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
refreshChats();
greeting();
