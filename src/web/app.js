"use strict";

const $ = (id) => document.getElementById(id);

const STATUS_LABEL = {
  running: "running",
  waiting_input: "needs input",
  waiting_permission: "permission!",
  finished: "finished",
};

let statePoll = null;
let outputPoll = null;
let current = null; // { tmuxName, title }
let killArmed = false;

// ---------- helpers ----------

async function api(path, opts) {
  const res = await fetch(path, opts);
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new Error(text || `${res.status} ${res.statusText}`);
  }
  const ct = res.headers.get("content-type") || "";
  return ct.includes("json") ? res.json() : null;
}

function post(path, body) {
  return api(path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body || {}),
  });
}

let toastTimer = null;
function toast(msg, isError) {
  const el = $("toast");
  el.textContent = msg;
  el.className = isError ? "error" : "";
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (el.hidden = true), 3500);
}

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text != null) node.textContent = text;
  return node;
}

function diffSpan(diff, className) {
  const span = el("span", className || "diff");
  if (!diff || (diff.added === 0 && diff.removed === 0)) return span;
  const plus = el("span", "diff-added", `+${diff.added}`);
  const minus = el("span", "diff-removed", ` -${diff.removed}`);
  span.append(plus, minus);
  return span;
}

// ---------- list view ----------

async function refreshState() {
  let state;
  try {
    state = await api("/api/state");
  } catch (e) {
    $("loading") && ($("loading").textContent = "connection lost — retrying…");
    return;
  }
  $("host").textContent = `@${state.host || "?"}`;
  renderProjects(state.projects || []);
}

function renderProjects(projects) {
  const root = $("projects");
  root.textContent = "";

  if (projects.length === 0) {
    root.append(el("div", "empty", "no projects configured"));
    return;
  }

  for (const p of projects) {
    const proj = el("section", "project");

    const header = el("div", "project-header");
    const name = el("div", "project-name", p.name);
    if (p.branch) name.append(el("span", "project-branch", `⎇ ${p.branch}`));
    const addBtn = el("button", "new-task-btn", "+ task");
    addBtn.onclick = () => openSheet({ mode: "task", project: p.name });
    header.append(name, addBtn);
    proj.append(header);

    const tasks = (p.tasks || []).filter((t) => !t.archived);
    for (const t of tasks) proj.append(renderTask(p, t));

    for (const s of p.adhoc_sessions || []) {
      const card = el("div", "task");
      card.append(sessionRow(s, `adhoc · ${s.name}`));
      proj.append(card);
    }

    if (tasks.length === 0 && (p.adhoc_sessions || []).length === 0) {
      proj.append(el("div", "empty", "no tasks"));
    }

    root.append(proj);
  }
}

function renderTask(project, task) {
  const card = el("div", "task");

  const header = el("div", "task-header");
  header.append(el("div", "task-name", task.name));

  const meta = el("div", "task-meta");
  if (task.stacked) meta.append(el("span", "stacked-badge", "≡"));
  meta.append(diffSpan(task.diff));
  if (task.pr_url) {
    const a = el("a", "pr-link", "PR ↗");
    a.href = task.pr_url;
    a.target = "_blank";
    meta.append(a);
  }
  header.append(meta);
  card.append(header);

  for (const s of task.sessions || []) {
    card.append(sessionRow(s, `${task.name} · ${s.name}`));
  }

  const add = el("button", "add-session", "+ session");
  add.onclick = async () => {
    add.disabled = true;
    add.textContent = "creating…";
    try {
      await post("/api/sessions", { project: project.name, task: task.name });
      toast("session created");
      refreshState();
    } catch (e) {
      toast(e.message, true);
      add.disabled = false;
      add.textContent = "+ session";
    }
  };
  card.append(add);

  return card;
}

function sessionRow(session, title) {
  const row = el("button", "session-row");
  const dot = el("span", `dot ${session.status || ""}`);
  const label = el("span", "label", `#${session.name}`);
  const status = el(
    "span",
    `status-label ${session.status || ""}`,
    STATUS_LABEL[session.status] || "…"
  );
  row.append(dot, label, diffSpan(session.diff, "diff"), status, el("span", "chev", "›"));
  row.onclick = () => openSession(session.tmux_name, title, session.status);
  return row;
}

// ---------- session view ----------

function openSession(tmuxName, title, status) {
  current = { tmuxName, title };
  killArmed = false;
  $("kill-btn").textContent = "kill";
  $("kill-btn").classList.remove("confirm");
  $("sv-title").textContent = title;
  setStatus(status);
  $("term").textContent = "";
  $("session-view").hidden = false;
  refreshOutput(true);
  clearInterval(outputPoll);
  outputPoll = setInterval(refreshOutput, 1500);
}

function closeSession() {
  current = null;
  clearInterval(outputPoll);
  outputPoll = null;
  $("session-view").hidden = true;
  refreshState();
}

function setStatus(status) {
  $("sv-dot").className = `dot ${status || ""}`;
  $("sv-status").textContent = STATUS_LABEL[status] || "…";
}

function termPinnedToBottom() {
  const t = $("term");
  return t.scrollHeight - t.scrollTop - t.clientHeight < 60;
}

async function refreshOutput(force) {
  if (!current) return;
  try {
    const data = await api(
      `/api/sessions/${encodeURIComponent(current.tmuxName)}/output?lines=1000`
    );
    const t = $("term");
    const pinned = force || termPinnedToBottom();
    const text = (data.text || "").replace(/\s+$/, "");
    if (t.textContent !== text) {
      t.textContent = text;
      if (pinned) t.scrollTop = t.scrollHeight;
    }
  } catch (e) {
    // session likely gone
  }
  // refresh status from list state occasionally is handled by state poll; keep simple
}

async function refreshSessionStatus() {
  if (!current) return;
  try {
    const state = await api("/api/state");
    $("host").textContent = `@${state.host || "?"}`;
    for (const p of state.projects || []) {
      const all = [...(p.tasks || []).flatMap((t) => t.sessions || []), ...(p.adhoc_sessions || [])];
      const found = all.find((s) => s.tmux_name === current.tmuxName);
      if (found) {
        setStatus(found.status);
        return;
      }
    }
  } catch (e) {}
}

async function sendMessage() {
  if (!current) return;
  const msg = $("msg");
  const text = msg.value.trim();
  if (!text) return;
  $("send-btn").disabled = true;
  try {
    await post(`/api/sessions/${encodeURIComponent(current.tmuxName)}/send`, {
      text,
      submit: true,
    });
    msg.value = "";
    msg.style.height = "auto";
    setTimeout(() => refreshOutput(true), 400);
  } catch (e) {
    toast(e.message, true);
  } finally {
    $("send-btn").disabled = false;
  }
}

async function sendKey(key) {
  if (!current) return;
  try {
    await post(`/api/sessions/${encodeURIComponent(current.tmuxName)}/keys`, { key });
    setTimeout(() => refreshOutput(true), 300);
  } catch (e) {
    toast(e.message, true);
  }
}

async function killSession() {
  if (!current) return;
  const btn = $("kill-btn");
  if (!killArmed) {
    killArmed = true;
    btn.textContent = "sure?";
    btn.classList.add("confirm");
    setTimeout(() => {
      killArmed = false;
      btn.textContent = "kill";
      btn.classList.remove("confirm");
    }, 3000);
    return;
  }
  btn.disabled = true;
  try {
    await post(`/api/sessions/${encodeURIComponent(current.tmuxName)}/kill`);
    toast("session killed");
    closeSession();
  } catch (e) {
    toast(e.message, true);
  } finally {
    btn.disabled = false;
  }
}

// ---------- new-task sheet ----------

let sheetCtx = null;

function openSheet(ctx) {
  sheetCtx = ctx;
  $("sheet-title").textContent = `new task · ${ctx.project}`;
  $("sheet-name").value = "";
  $("sheet-prompt").value = "";
  $("sheet-backdrop").hidden = false;
  $("sheet-name").focus();
}

function closeSheet() {
  sheetCtx = null;
  $("sheet-backdrop").hidden = true;
}

async function createFromSheet() {
  if (!sheetCtx) return;
  const name = $("sheet-name").value.trim();
  if (!name) {
    toast("task name required", true);
    return;
  }
  const btn = $("sheet-create");
  btn.disabled = true;
  btn.textContent = "creating…";
  try {
    await post("/api/tasks", {
      project: sheetCtx.project,
      name,
      prompt: $("sheet-prompt").value.trim() || null,
    });
    toast(`task '${name}' created`);
    closeSheet();
    refreshState();
  } catch (e) {
    toast(e.message, true);
  } finally {
    btn.disabled = false;
    btn.textContent = "create";
  }
}

// ---------- wiring ----------

$("back-btn").onclick = closeSession;
$("kill-btn").onclick = killSession;
$("send-btn").onclick = sendMessage;
$("sheet-cancel").onclick = closeSheet;
$("sheet-create").onclick = createFromSheet;
$("sheet-backdrop").onclick = (e) => {
  if (e.target === $("sheet-backdrop")) closeSheet();
};

for (const btn of $("quick-keys").querySelectorAll("button")) {
  btn.onclick = () => sendKey(btn.dataset.key);
}

const msg = $("msg");
msg.addEventListener("input", () => {
  msg.style.height = "auto";
  msg.style.height = `${Math.min(msg.scrollHeight, 120)}px`;
});

refreshState();
statePoll = setInterval(() => {
  if (current) refreshSessionStatus();
  else refreshState();
}, 3000);

document.addEventListener("visibilitychange", () => {
  if (!document.hidden) {
    if (current) refreshOutput(true);
    else refreshState();
  }
});
