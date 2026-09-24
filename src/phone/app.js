// Codebench on the phone: an inbox of tasks, a task view with the live
// terminal and one-tap keys, a reply box, and new tasks. Plain JS, no build.
"use strict";

const $ = (sel, el = document) => el.querySelector(sel);
const view = $("#view");
const title = $("#title");
const back = $("#back");
const conn = $("#conn");
const gear = $("#gear");
let installPrompt = null;
window.addEventListener("beforeinstallprompt", (e) => { e.preventDefault(); installPrompt = e; });
gear.addEventListener("click", () => go("#/settings"));

let token = localStorage.getItem("cb.token") || "";
let state = null;
let socket = null;
let watching = null;
let fit = localStorage.getItem("cb.fit") !== "off";
let tab = "chat";
let chatTimer = null;
let renders = 0;

// ---------------------------------------------------------------- helpers

function h(tag, attrs = {}, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else if (k === "class") el.className = v;
    else if (v !== undefined && v !== null && v !== false) el.setAttribute(k, v);
  }
  for (const kid of kids.flat()) {
    if (kid === null || kid === undefined || kid === false) continue;
    el.append(kid instanceof Node ? kid : document.createTextNode(String(kid)));
  }
  return el;
}

function toast(text) {
  const t = h("div", { class: "toast" }, text);
  document.body.append(t);
  setTimeout(() => t.remove(), 2600);
}

async function api(path, body) {
  const res = await fetch(path, {
    method: body ? "POST" : "GET",
    headers: { "Authorization": "Bearer " + token, ...(body ? { "Content-Type": "application/json" } : {}) },
    body: body ? JSON.stringify(body) : undefined,
  });
  if (res.status === 401) { forget(); throw new Error("not paired"); }
  if (!res.ok) throw new Error(res.status + " " + (await res.text()));
  return res.status === 204 ? null : res.json();
}

function forget() {
  token = "";
  localStorage.removeItem("cb.token");
  if (socket) socket.close();
  go("#/pair");
}

const LOOK = {
  needs: ["●", "red", "needs you"],
  working: ["●", "yellow", "working"],
  idle: ["●", "dim", ""],
  stopped: ["○", "dim", ""],
};

function taskById(id) {
  for (const p of state?.projects || []) for (const t of p.tasks) if (t.id === id) return [p, t];
  return [null, null];
}

// Only colors and plain formatting survive from the terminal's HTML.
function sanitize(html) {
  const doc = new DOMParser().parseFromString(html, "text/html");
  const out = document.createElement("pre");
  const walk = (node, into) => {
    for (const child of node.childNodes) {
      if (child.nodeType === Node.TEXT_NODE) { into.append(child.textContent); continue; }
      if (child.nodeType !== Node.ELEMENT_NODE) continue;
      const tag = child.tagName.toLowerCase();
      if (!["font", "span", "b", "i", "u", "pre", "br"].includes(tag)) { walk(child, into); continue; }
      if (tag === "br") { into.append("\n"); continue; }
      if (tag === "pre") { walk(child, into); continue; }
      const el = document.createElement(tag === "font" ? "span" : tag);
      const color = child.getAttribute("color");
      if (color && /^#[0-9a-f]{3,8}$/i.test(color)) el.style.color = color;
      const style = child.getAttribute("style") || "";
      const bg = style.match(/background-color:\s*(#[0-9a-f]{3,8})/i);
      if (bg) el.style.backgroundColor = bg[1];
      walk(child, el);
      into.append(el);
    }
  };
  walk(doc.body, out);
  return out;
}

// ---------------------------------------------------------------- live

function connect() {
  if (!token || (socket && socket.readyState <= 1)) return;
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  socket = new WebSocket(`${proto}//${location.host}/api/live`);
  socket.onopen = () => {
    socket.send(JSON.stringify({ auth: token }));
    if (watching) socket.send(JSON.stringify({ watch: watching }));
    conn.classList.add("live");
  };
  socket.onmessage = (e) => {
    const msg = JSON.parse(e.data);
    if (msg.type === "unauthorized" || msg.type === "revoked") { forget(); return; }
    if (msg.type === "state") { state = msg.data; render(); }
    if (msg.type === "screen" && msg.task === watching) { showScreen(msg.html); refreshChatSoon(); }
    if (msg.type === "theme") reloadTheme();
  };
  socket.onclose = () => {
    conn.classList.remove("live");
    setTimeout(connect, 2000);
  };
}

// The task whose screen this phone follows; null stops following.
function watch(id) {
  if (watching === id) return false;
  watching = id;
  if (socket?.readyState === 1) socket.send(JSON.stringify({ watch: id }));
  return true;
}

function reloadTheme() {
  const link = $('link[href^="/theme.css"]');
  link.href = "/theme.css?" + Date.now();
}

// ---------------------------------------------------------------- routes

function go(hash) {
  if (location.hash !== hash) location.hash = hash;
  else render();
}

window.addEventListener("hashchange", render);
back.addEventListener("click", () => history.length > 1 ? history.back() : go("#/"));

function render() {
  renders++;
  if (!token) return pairView();
  const [, route, arg] = (location.hash || "#/").split("/");
  if (route === "task" && arg) return taskView(decodeURIComponent(arg));
  if (route === "new") return newView(arg ? decodeURIComponent(arg) : "");
  if (route === "pair") return token ? go("#/") : pairView();
  if (route === "settings") return settingsView();
  if (route === "artifacts") return artifactsView();
  return inboxView();
}

// ---------------------------------------------------------------- inbox

function taskRow(p, t) {
  const [glyph, color, word] = LOOK[t.kind] || LOOK.idle;
  return h("div", { class: "row", onclick: () => go("#/task/" + encodeURIComponent(t.id)) },
    h("span", { class: "glyph " + color }, glyph),
    h("div", { class: "main" },
      h("div", { class: "t" }, t.title),
      h("div", { class: "s" }, `${p.name} · ${t.agent}${t.context ? " · " + t.context : ""}`)),
    word ? h("span", { class: "word " + color }, word) : null);
}

function inboxView() {
  watch(null);
  title.textContent = "codebench";
  back.hidden = true;
  view.replaceChildren();
  if (!state) { view.append(h("div", { class: "empty" }, "connecting…")); return; }
  if (!state.running) { view.append(h("div", { class: "empty" }, "Codebench is closed on the desktop.")); return; }

  if (state.usage?.length) {
    view.append(h("div", { class: "limits" }, state.usage.map(u => {
      const worst = Math.max(...u.limits.map(l => l.percent));
      return h("span", { class: "chip" + (worst >= 80 ? " hot" : worst >= 60 ? " warn" : "") },
        `${u.agent}  ` + u.limits.map(l => `${l.name} ${l.percent}%`).join(" · "));
    })));
  }

  view.append(h("div", { class: "row", onclick: () => go("#/artifacts") },
    h("span", { class: "glyph accent" }, "◆"),
    h("div", { class: "main" }, h("div", { class: "t" }, "artifacts"), h("div", { class: "s" }, "visuals your agents made"))));

  const all = state.projects.flatMap(p => p.tasks.map(t => [p, t]));
  const needs = all.filter(([, t]) => t.kind === "needs");
  const working = all.filter(([, t]) => t.kind === "working");
  view.append(h("div", { class: "section" }, needs.length ? `needs you · ${needs.length}` : "needs you"));
  view.append(needs.length ? h("div", {}, needs.map(([p, t]) => taskRow(p, t))) : h("div", { class: "empty" }, "nothing needs you"));
  if (working.length) {
    view.append(h("div", { class: "section" }, `working · ${working.length}`));
    view.append(h("div", {}, working.map(([p, t]) => taskRow(p, t))));
  }
  for (const p of state.projects) {
    const rest = p.tasks.filter(t => t.kind !== "needs" && t.kind !== "working");
    if (!rest.length) continue;
    view.append(h("div", { class: "section" }, p.name));
    view.append(h("div", {}, rest.map(t => taskRow(p, t))));
  }
  view.append(h("button", { class: "fab", onclick: () => go("#/new") }, "+ task"));
}

// ---------------------------------------------------------------- task

function showScreen(html) {
  const box = $(".screen");
  if (!box) return;
  const atBottom = box.scrollTop + box.clientHeight >= box.scrollHeight - 30;
  const pre = sanitize(html || "");
  box.replaceChildren(pre);
  if (fit) {
    pre.style.fontSize = "14px";
    const scale = Math.min(1, (box.clientWidth - 12) / Math.max(pre.scrollWidth, 1));
    pre.style.fontSize = Math.max(6, 14 * scale) + "px";
  }
  if (atBottom) box.scrollTop = box.scrollHeight;
}

// A screen fetched for a task shows only while that task is still open.
function loadScreen(id) {
  return api(`/api/task/${encodeURIComponent(id)}/screen`).then(r => { if (watching === id) showScreen(r.html); });
}

async function keys(task, list) {
  try { await api(`/api/task/${encodeURIComponent(task)}/keys`, { keys: list }); }
  catch (e) { toast(e.message); }
}

// Just enough markdown for agent replies: ``` blocks, `code` and **bold**.
// Built as DOM nodes, never as HTML.
function renderText(text) {
  const out = [];
  const parts = text.split(/```[a-zA-Z0-9_-]*\n?/);
  parts.forEach((part, i) => {
    if (i % 2 === 1) { out.push(h("pre", { class: "code" }, part.replace(/\n$/, ""))); return; }
    for (const piece of part.split(/(`[^`\n]+`|\*\*[^*\n]+\*\*)/)) {
      if (!piece) continue;
      if (piece.startsWith("`") && piece.endsWith("`") && piece.length > 2) out.push(h("code", {}, piece.slice(1, -1)));
      else if (piece.startsWith("**") && piece.endsWith("**") && piece.length > 4) out.push(h("b", {}, piece.slice(2, -2)));
      else out.push(piece);
    }
  });
  return out;
}

// ---------------------------------------------------------------- chat

function hasChat(t) { return t && (t.agent === "claude" || t.agent === "codex"); }

function refreshChatSoon() {
  if (tab !== "chat" || chatTimer) return;
  chatTimer = setTimeout(() => { chatTimer = null; loadChat(); }, 1500);
}

async function loadChat() {
  const box = $(".chat");
  if (!box || !watching) return;
  let r;
  try { r = await api(`/api/task/${encodeURIComponent(watching)}/chat`); } catch (e) { return; }
  const atBottom = box.scrollTop + box.clientHeight >= box.scrollHeight - 40;
  if (!r.entries.length) {
    box.replaceChildren(h("div", { class: "empty" }, r.unsupported ? "no chat view for this agent. see screen" : "no messages yet"));
    return;
  }
  box.replaceChildren(...r.entries.map(e => e.role === "tool"
    ? h("div", { class: "tool" }, "▸ " + e.text)
    : h("div", { class: "msg " + e.role }, ...renderText(e.text))));
  if (atBottom || !box.dataset.loaded) box.scrollTop = box.scrollHeight;
  box.dataset.loaded = "1";
}

function setTab(which) {
  tab = which;
  document.querySelectorAll(".tabs button").forEach(b => b.classList.toggle("on", b.dataset.tab === which));
  $(".chat").hidden = which !== "chat";
  $(".screen").hidden = which !== "screen";
  if (which === "chat") loadChat();
  else loadScreen(watching).catch(() => {});
}

function statusParts(id, p, t) {
  const [glyph, color, word] = LOOK[t?.kind] || LOOK.idle;
  const toggle = () => {
    fit = !fit;
    localStorage.setItem("cb.fit", fit ? "on" : "off");
    $(".screen")?.classList.toggle("fit", fit);
    $(".status").replaceChildren(...statusParts(id, p, t));
    loadScreen(id).catch(() => {});
  };
  return [
    h("span", { class: color }, glyph + " " + (word || t?.kind || "")),
    h("span", {}, p ? `${p.name} · ${t.agent}` : ""),
    h("span", {}, t?.context || ""),
    h("span", { class: "accent", style: "margin-left:auto", onclick: toggle }, fit ? "fit" : "zoom"),
  ];
}

function taskView(id) {
  const [p, t] = taskById(id);
  back.hidden = false;
  title.textContent = t ? t.title : "task";
  if (watch(id)) {
    api(`/api/task/${encodeURIComponent(id)}/open`, {}).catch(e => toast(e.message));
  }
  // Keep the typed reply and scroll across live re-renders.
  const draft = $(".reply textarea")?.value || "";
  if ($(".task") && $(".task").dataset.id === id) {
    $(".status").replaceChildren(...statusParts(id, p, t));
    // Opened before the task list arrived: chat shows up once it does.
    const tabs = $(".tabs");
    if (tabs?.hidden && hasChat(t)) { tabs.hidden = false; setTab("chat"); }
    return;
  }
  const [glyph, color, word] = LOOK[t?.kind] || LOOK.idle;
  const reply = h("textarea", { placeholder: "message the agent…", rows: 1 });
  reply.value = draft;
  reply.addEventListener("input", () => { reply.style.height = "44px"; reply.style.height = Math.min(140, reply.scrollHeight) + "px"; });
  const sendReply = async () => {
    const text = reply.value.trim();
    if (!text) return;
    try {
      await api(`/api/task/${encodeURIComponent(id)}/send`, { text });
      reply.value = "";
      reply.style.height = "44px";
      toast(t?.kind === "working" ? "queued: it goes in when the agent is free" : "sent");
    } catch (e) { toast(e.message); }
  };
  const k = (label, list, cls) => h("button", { class: cls || "", onclick: () => keys(id, list) }, label);
  const screen = h("div", { class: "screen" + (fit ? " fit" : "") }, h("pre", { class: "dim" }, "loading screen…"));
  const chat = h("div", { class: "chat" }, h("div", { class: "empty" }, "loading…"));
  tab = hasChat(t) ? "chat" : "screen";
  view.replaceChildren(h("div", { class: "task", "data-id": id },
    h("div", { class: "status" }, ...statusParts(id, p, t)),
    h("div", { class: "tabs", hidden: !hasChat(t) },
      h("button", { "data-tab": "chat", onclick: () => setTab("chat") }, "chat"),
      h("button", { "data-tab": "screen", onclick: () => setTab("screen") }, "screen")),
    chat,
    screen,
    h("div", { class: "keys" },
      k("yes ✓", ["enter"], "ok"), k("no ✗", ["esc"], "no"), k("↑", ["up"]), k("↓", ["down"]), k("1", ["1"]), k("2", ["2"]),
      k("tab", ["tab"]), k("⇧tab", ["shift-tab"]), k("⏎", ["enter"]), k("esc", ["esc"]), k("3", ["3"]), k("^C", ["ctrl-c"])),
    h("div", { class: "reply" }, reply, h("button", { class: "primary", onclick: sendReply }, "send"))));
  loadScreen(id).catch(e => toast(e.message));
  setTab(tab);
}

// ---------------------------------------------------------------- new task

// Refills a select only when its choices changed, keeping the choice made.
function setOptions(select, items, fallback) {
  const key = JSON.stringify(items);
  if (select.dataset.items === key) return;
  const keep = select.value || fallback;
  select.dataset.items = key;
  select.replaceChildren(...items.map(([value, label]) => h("option", { value, selected: value === keep || undefined }, label)));
}

function newView(pid) {
  watch(null);
  back.hidden = false;
  title.textContent = "new task";
  const projectItems = (state?.projects || []).map(p => [p.id, p.name]);
  const agentItems = (state?.agents || ["claude"]).map(a => [a, a]);
  // Live updates refresh the choices, never the form being filled in.
  const open = $(".newtask");
  if (open && open.dataset.pid === pid) {
    const [project, agent] = open.querySelectorAll("select");
    const was = project.value;
    setOptions(project, projectItems, pid);
    setOptions(agent, agentItems);
    if (project.value !== was) project.onchange?.();
    return;
  }
  const project = h("select", {});
  setOptions(project, projectItems, pid);
  const agent = h("select", {});
  setOptions(agent, agentItems);
  const name = h("input", { placeholder: "task name (optional)" });
  const prompt = h("textarea", { placeholder: "what should the agent do?" });
  const flows = h("div", {});
  const loadFlows = async () => {
    flows.replaceChildren();
    let r;
    try { r = await api(`/api/workflows/${encodeURIComponent(project.value)}`); } catch (e) { return; }
    if (!r.workflows.length) return;
    flows.append(h("div", { class: "section" }, "or run a workflow"));
    for (const w of r.workflows) {
      flows.append(h("div", { class: "row", onclick: async () => {
        if (!confirm(`Run "${w.name}" in ${project.selectedOptions[0].textContent}?`)) return;
        try {
          await api(`/api/workflows/${encodeURIComponent(project.value)}/run`, { path: w.path });
          toast("workflow started");
          go("#/");
        } catch (e) { toast(e.message); }
      } },
        h("span", { class: "glyph accent" }, "▶"),
        h("div", { class: "main" },
          h("div", { class: "t" }, w.name),
          h("div", { class: "s" }, [w.agent, w.schedule, w.from].filter(Boolean).join(" · ")))));
    }
  };
  project.onchange = loadFlows;
  loadFlows();
  view.replaceChildren(h("div", { class: "form newtask", "data-pid": pid },
    h("label", {}, "project", project),
    h("label", {}, "agent", agent),
    h("label", {}, "name", name),
    h("label", {}, "prompt", prompt),
    h("button", { class: "primary", onclick: async () => {
      if (!prompt.value.trim()) { toast("write a prompt first"); return; }
      try {
        await api("/api/tasks", { project: project.value, agent: agent.value, title: name.value.trim(), prompt: prompt.value.trim() });
        toast("started");
        go("#/");
      } catch (e) { toast(e.message); }
    } }, "start task")), flows);
}

// ---------------------------------------------------------------- artifacts

async function artifactsView() {
  const mine = renders;
  watch(null);
  back.hidden = false;
  title.textContent = "artifacts";
  view.replaceChildren(h("div", { class: "empty" }, "loading…"));
  let r;
  try { r = await api("/api/artifacts"); } catch (e) { if (mine === renders) toast(e.message); return; }
  // Another page may be showing by now.
  if (mine !== renders) return;
  if (!r.artifacts.length) {
    view.replaceChildren(h("div", { class: "empty" }, "no artifacts yet. ask an agent to show you something."));
    return;
  }
  const rows = [];
  let last = null;
  for (const a of r.artifacts.sort((x, y) => x.project.localeCompare(y.project) || y.modified - x.modified)) {
    if (a.project !== last) { rows.push(h("div", { class: "section" }, a.project)); last = a.project; }
    rows.push(h("div", { class: "row", onclick: () => a.url ? window.open(a.url, "_blank", "noopener") : toast("run  tailscale serve --bg --https=8443 47824  on the computer to view artifacts here") },
      h("span", { class: "glyph accent" }, "◆"),
      h("div", { class: "main" }, h("div", { class: "t" }, a.title), h("div", { class: "s" }, a.kind + " · " + new Date(a.modified * 1000).toLocaleString()))));
  }
  view.replaceChildren(...rows);
}

// ---------------------------------------------------------------- settings

function b64ToBytes(b64) {
  const s = atob(b64.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat((4 - b64.length % 4) % 4));
  return Uint8Array.from(s, c => c.charCodeAt(0));
}

async function pushStatus() {
  if (!("serviceWorker" in navigator) || !("PushManager" in window)) return "not supported in this browser";
  if (!window.isSecureContext) return "needs the https address (tailscale serve)";
  if (Notification.permission === "denied") return "blocked in the browser's site settings";
  const reg = await navigator.serviceWorker.ready;
  const sub = await reg.pushManager.getSubscription();
  return sub ? "on" : "off";
}

async function turnOnPush() {
  const perm = await Notification.requestPermission();
  if (perm !== "granted") { toast("notifications not allowed"); return; }
  const reg = await navigator.serviceWorker.ready;
  const { key } = await api("/api/push/key");
  const sub = await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: b64ToBytes(key) });
  await api("/api/push/subscribe", sub.toJSON());
  toast("notifications on");
}

async function settingsView() {
  watch(null);
  back.hidden = false;
  title.textContent = "settings";
  const status = h("span", { class: "accent" }, "…");
  const standalone = matchMedia("(display-mode: standalone)").matches;
  view.replaceChildren(h("div", { class: "form" },
    h("div", { class: "section", style: "padding:0" }, "notifications"),
    h("div", {}, "Tasks that need you or finish: ", status),
    h("button", { class: "primary", onclick: async () => {
      try { await turnOnPush(); status.textContent = await pushStatus(); } catch (e) { toast(e.message); }
    } }, "turn on notifications"),
    h("button", { onclick: () => api("/api/push/test", {}).then(() => toast("sent. it should arrive in a moment")).catch(e => toast(e.message)) }, "send a test notification"),
    h("div", { class: "section", style: "padding:0" }, "app"),
    standalone ? h("div", { class: "dim" }, "Installed on the home screen.")
      : installPrompt ? h("button", { onclick: async () => { installPrompt.prompt(); installPrompt = null; } }, "install on home screen")
      : h("div", { class: "dim" }, "To install: Chrome menu ⋮ → Add to Home screen."),
    h("div", { class: "section", style: "padding:0" }, "this phone"),
    h("button", { onclick: () => { if (confirm("Forget this phone? You will need to pair again.")) forget(); } }, "forget this phone")));
  status.textContent = await pushStatus().catch(e => e.message);
}

// ---------------------------------------------------------------- pairing

function pairView() {
  back.hidden = true;
  title.textContent = "codebench";
  const name = h("input", { value: /Android/.test(navigator.userAgent) ? "Android phone" : "phone", placeholder: "name for this phone" });
  view.replaceChildren(h("div", { class: "pair" },
    h("div", {}, "Pair this phone with Codebench."),
    h("label", { class: "dim" }, "name", name),
    h("button", { class: "primary", onclick: () => startPairing(name.value.trim() || "phone") }, "pair")));
}

async function startPairing(name) {
  let res;
  try {
    const r = await fetch("/api/pair", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ name }) });
    if (!r.ok) throw new Error(await r.text());
    res = await r.json();
  } catch (e) { toast(e.message); return; }
  view.replaceChildren(h("div", { class: "pair" },
    h("div", { class: "dim" }, "Approve this code in Codebench on your computer:"),
    h("div", { class: "code" }, res.code.slice(0, 3) + " " + res.code.slice(3)),
    h("div", { class: "dim" }, "waiting…")));
  for (let i = 0; i < 90; i++) {
    await new Promise(r => setTimeout(r, 2000));
    const s = await (await fetch("/api/pair/" + res.id)).json().catch(() => ({ state: "pending" }));
    if (s.state === "approved") {
      token = s.token;
      localStorage.setItem("cb.token", token);
      toast("paired");
      connect();
      go("#/");
      return;
    }
    if (s.state === "denied" || s.state === "expired") break;
  }
  toast("not approved. try again");
  pairView();
}

// ---------------------------------------------------------------- start

if ("serviceWorker" in navigator) navigator.serviceWorker.register("/sw.js").catch(() => {});
render();
connect();
document.addEventListener("visibilitychange", () => { if (!document.hidden) connect(); });
