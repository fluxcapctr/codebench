// Codebench on the phone: an inbox of tasks, a task view with the live
// terminal and one-tap keys, a reply box, and new tasks. Plain JS, no build.
"use strict";

const $ = (sel, el = document) => el.querySelector(sel);
const view = $("#view");
const title = $("#title");
const back = $("#back");
const conn = $("#conn");

let token = localStorage.getItem("cb.token") || "";
let state = null;
let socket = null;
let watching = null;
let fit = localStorage.getItem("cb.fit") !== "off";

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
    if (msg.type === "screen" && msg.task === watching) showScreen(msg.html);
    if (msg.type === "theme") reloadTheme();
  };
  socket.onclose = () => {
    conn.classList.remove("live");
    setTimeout(connect, 2000);
  };
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
  if (!token) return pairView();
  const [, route, arg] = (location.hash || "#/").split("/");
  if (route === "task" && arg) return taskView(decodeURIComponent(arg));
  if (route === "new") return newView(arg ? decodeURIComponent(arg) : "");
  if (route === "pair") return token ? go("#/") : pairView();
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
  watching = null;
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

async function keys(task, list) {
  try { await api(`/api/task/${encodeURIComponent(task)}/keys`, { keys: list }); }
  catch (e) { toast(e.message); }
}

function statusParts(id, p, t) {
  const [glyph, color, word] = LOOK[t?.kind] || LOOK.idle;
  const toggle = () => {
    fit = !fit;
    localStorage.setItem("cb.fit", fit ? "on" : "off");
    $(".screen")?.classList.toggle("fit", fit);
    $(".status").replaceChildren(...statusParts(id, p, t));
    api(`/api/task/${encodeURIComponent(id)}/screen`).then(r => showScreen(r.html));
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
  if (watching !== id) {
    watching = id;
    if (socket?.readyState === 1) socket.send(JSON.stringify({ watch: id }));
    api(`/api/task/${encodeURIComponent(id)}/open`, {}).catch(e => toast(e.message));
  }
  // Keep the typed reply and scroll across live re-renders.
  const draft = $(".reply textarea")?.value || "";
  if ($(".task") && $(".task").dataset.id === id) {
    $(".status").replaceChildren(...statusParts(id, p, t));
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
  view.replaceChildren(h("div", { class: "task", "data-id": id },
    h("div", { class: "status" }, ...statusParts(id, p, t)),
    screen,
    h("div", { class: "keys" },
      k("yes ✓", ["enter"], "ok"), k("no ✗", ["esc"], "no"), k("↑", ["up"]), k("↓", ["down"]), k("1", ["1"]), k("2", ["2"]),
      k("tab", ["tab"]), k("⇧tab", ["shift-tab"]), k("⏎", ["enter"]), k("esc", ["esc"]), k("3", ["3"]), k("^C", ["ctrl-c"])),
    h("div", { class: "reply" }, reply, h("button", { class: "primary", onclick: sendReply }, "send"))));
  api(`/api/task/${encodeURIComponent(id)}/screen`).then(r => showScreen(r.html)).catch(e => toast(e.message));
}

// ---------------------------------------------------------------- new task

function newView(pid) {
  watching = null;
  back.hidden = false;
  title.textContent = "new task";
  const projects = state?.projects || [];
  const project = h("select", {}, projects.map(p => h("option", { value: p.id, selected: p.id === pid || undefined }, p.name)));
  const agent = h("select", {}, (state?.agents || ["claude"]).map(a => h("option", { value: a }, a)));
  const name = h("input", { placeholder: "task name (optional)" });
  const prompt = h("textarea", { placeholder: "what should the agent do?" });
  view.replaceChildren(h("div", { class: "form" },
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
    } }, "start task")));
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

render();
connect();
document.addEventListener("visibilitychange", () => { if (!document.hidden) connect(); });
