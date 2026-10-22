// Captain monitoring UI. Plain DOM, no dependencies. Message text from Telegram is only ever
// inserted with textContent, never as HTML.
"use strict";

const STATUS_POLL_MS = 10000;
const LIST_POLL_MS = 30000;
const TOAST_MS = 3500;
const STALE_UPDATE_SECS = 1800; // no update for this long is worth a warning
const SVG_NS = "http://www.w3.org/2000/svg";
const FILTERS_KEY = "captain.filters";

const TRIGGERS = {
  dm_all: "Direct message",
  mention: "Mention",
  reply_to_me: "Reply to me",
  keyword: "Keyword",
  manual: "Manual",
};
const STATE_PILL = { queued: "info", digest: "warn", resolved: "ok", skipped: "muted" };
const IMPLICIT = {
  answered: ["Answered", "ok"],
  answered_elsewhere: ["Answered elsewhere", "ok"],
  read_only: ["Read only", "warn"],
  unseen: ["Unseen", "bad"],
};
const EXPLICIT = { want: "Want", noise: "Noise", skip: "Skip" };
const RULE_PILL = { fires: "bad", holds: "ok", not_enough_data: "muted", suggestion: "info" };
const RULE_TEXT = { fires: "Act", holds: "Fine", not_enough_data: "Too little data", suggestion: "Suggestion" };

const ui = {
  view: "tickets",
  status: null,
  modules: null,
  tickets: [],
  selected: null,
  detail: null,
  showText: false,
  report: null,
  timers: [],
  loadToken: null,
  redraw: null,
};

// DOM helpers

function h(tag, attrs, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === null || v === undefined || v === false) continue;
    if (k === "class") el.className = v;
    else if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else el.setAttribute(k, v === true ? "" : String(v));
  }
  append(el, children);
  return el;
}

function svg(tag, attrs, ...children) {
  const el = document.createElementNS(SVG_NS, tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === null || v === undefined) continue;
    if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else el.setAttribute(k, String(v));
  }
  append(el, children);
  return el;
}

function append(el, children) {
  for (const c of children.flat(Infinity)) {
    if (c === null || c === undefined || c === false) continue;
    el.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
}

function $(id) { return document.getElementById(id); }

function replace(el, ...children) {
  el.replaceChildren();
  append(el, children);
}

function pill(text, tone, plain) {
  return h("span", { class: `pill pill-${tone || "muted"}${plain ? " pill-plain" : ""}` }, text);
}

function stateBox(title, body, tone) {
  return h("div", { class: `state-box${tone === "error" ? " state-error" : ""}`, role: tone === "error" ? "alert" : null },
    h("h2", {}, title), body ? h("p", {}, body) : null);
}

function skeleton(rows) {
  return Array.from({ length: rows }, () => h("div", { class: "skeleton", "aria-hidden": "true" }));
}

function toast(message, bad) {
  const el = $("toast");
  el.textContent = message;
  el.className = `toast${bad ? " toast-bad" : ""}`;
  el.hidden = false;
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => { el.hidden = true; }, TOAST_MS);
}

// Formatting

function when(unix) {
  if (!unix) return "-";
  return new Date(unix * 1000).toLocaleString(undefined, {
    month: "short", day: "numeric", hour: "2-digit", minute: "2-digit",
  });
}

function ago(unix, now) {
  if (!unix) return "never";
  const s = Math.max(0, (now || Date.now() / 1000) - unix);
  if (s < 60) return `${Math.round(s)}s ago`;
  if (s < 3600) return `${Math.round(s / 60)}m ago`;
  if (s < 86400) return `${Math.round(s / 3600)}h ago`;
  return `${Math.round(s / 86400)}d ago`;
}

function duration(secs) {
  if (secs === null || secs === undefined) return "-";
  if (secs >= 3600) return `${(secs / 3600).toFixed(1)}h`;
  if (secs >= 60) return `${(secs / 60).toFixed(1)}m`;
  return `${Math.round(secs)}s`;
}

function statValue(s) {
  const v = s.value;
  if (v === null || v === undefined) return "-";
  switch (s.unit) {
    case "ratio": return `${Math.round(v * 100)}%`;
    case "seconds": return duration(v);
    case "per_day": return v.toFixed(1);
    case "megabytes": return `${v.toFixed(1)} MB`;
    case "megabytes_per_day": return `${v.toFixed(2)} MB`;
    default: return Number.isInteger(v) ? String(v) : v.toFixed(1);
  }
}

function statLabel(s) {
  const daily = s.unit === "per_day" || s.unit === "megabytes_per_day";
  return daily && !s.label.includes("per day") ? `${s.label} per day` : s.label;
}

// API

class ApiError extends Error {
  constructor(status, message) { super(message); this.status = status; }
}

async function api(path, options) {
  const opts = { credentials: "same-origin", headers: { Accept: "application/json" }, ...options };
  if (opts.method && opts.method !== "GET") {
    opts.headers = { ...opts.headers, "X-Captain": "1", "Content-Type": "application/json" };
  }
  let res;
  try {
    res = await fetch(`/api${path}`, opts);
  } catch {
    throw new ApiError(0, "Cannot reach captain. Is it still running?");
  }
  const body = await res.json().catch(() => ({}));
  if (res.status === 401) {
    window.location.reload(); // the server shows the sign-in page
  }
  if (!res.ok) throw new ApiError(res.status, body.error || `Request failed (${res.status})`);
  return body;
}

// Status bar

async function loadStatus() {
  try {
    const statusPromise = api("/status");
    const modulesPromise = api("/modules").catch(() => null);
    ui.status = await statusPromise;
    ui.modules = await modulesPromise;
    renderStatus();
  } catch (e) {
    replace($("status"), pill(e.message, "bad"));
  }
}

function bridgePill(modules) {
  const list = (modules && modules.modules) || [];
  const bridge = list.find((item) => item.module === "zalo-bridge");
  if (!bridge) return null;
  const tone = { ok: "ok", degraded: "warn", down: "bad", unknown: "muted" }[bridge.state] || "muted";
  const label = {
    ok: "Zalo bridge: đang chạy",
    degraded: "Zalo bridge: chậm",
    down: "Zalo bridge: tắt",
    unknown: "Zalo bridge: chưa rõ",
  }[bridge.state] || `Zalo bridge: ${bridge.state}`;
  return pill(label, tone);
}

function renderStatus() {
  const s = ui.status;
  $("profile").textContent = s.me && s.me.name ? `${s.profile} · ${s.me.name}` : s.profile;
  const conn = {
    connected: ["Connected", "ok"],
    unauthorized: ["Session revoked", "bad"],
    offline: ["Offline view", "muted"],
  }[s.connection] || [s.connection, "muted"];
  const parts = [pill(conn[0], conn[1])];
  const bridge = bridgePill(ui.modules);
  if (bridge) parts.push(bridge);
  if (s.paused) parts.push(pill("Paused", "warn"));
  if (s.connection === "connected") {
    const stale = s.last_update_at && s.now - s.last_update_at > STALE_UPDATE_SECS;
    parts.push(h("span", { class: stale ? "pill pill-warn pill-plain" : null },
      `Last update ${ago(s.last_update_at, s.now)}`));
    if (s.open_bursts) parts.push(h("span", {}, `${s.open_bursts} open burst${s.open_bursts === 1 ? "" : "s"}`));
  }
  const t = s.tickets_last_day || {};
  parts.push(h("span", {}, `Last day: ${t.queued || 0} queued, ${t.digest || 0} digest, ${t.skipped || 0} skipped`));
  replace($("status"), parts);

  const btn = $("pause");
  btn.disabled = false;
  btn.textContent = s.paused ? "Resume" : "Pause";
  btn.setAttribute("aria-pressed", s.paused ? "true" : "false");
  btn.title = s.paused ? "Create tickets again" : "Stop creating tickets; messages are still stored";
}

async function togglePause() {
  const btn = $("pause");
  const resume = ui.status && ui.status.paused;
  // Pause stops new tickets for both sources, so it asks first. Resume does not.
  if (!resume) {
    const ok = await confirmDialog({
      title: "Tạm dừng tạo ticket?",
      body: "Captain vẫn lưu tin, nhưng mọi burst mới từ Telegram và Zalo bị bỏ qua cho tới khi bấm Resume. Zalo bridge vẫn chạy và chuyển tiếp tin như cũ.",
      ok: "Tạm dừng",
    });
    if (!ok) return;
  }
  btn.disabled = true;
  try {
    await api(resume ? "/resume" : "/pause", { method: "POST" });
    toast(resume ? "Resumed: new bursts become tickets again" : "Paused: bursts are stored but skipped");
  } catch (e) {
    toast(e.message, true);
  }
  await loadStatus();
}

// Tickets

function filters() {
  const f = new FormData($("filters"));
  return {
    state: f.get("state") || "",
    trigger: f.get("trigger") || "",
    days: f.get("days") || "7",
    unlabelled: f.get("unlabelled") === "on",
  };
}

function restoreFilters() {
  try {
    const saved = JSON.parse(localStorage.getItem(FILTERS_KEY) || "{}");
    const form = $("filters");
    for (const name of ["state", "trigger", "days"]) {
      if (saved[name] !== undefined) form.elements[name].value = saved[name];
    }
    form.elements.unlabelled.checked = Boolean(saved.unlabelled);
  } catch { /* a bad saved value just means default filters */ }
}

async function loadTickets(quiet) {
  const f = filters();
  localStorage.setItem(FILTERS_KEY, JSON.stringify(f));
  const q = new URLSearchParams({ days: f.days });
  if (f.state) q.set("state", f.state);
  if (f.trigger) q.set("trigger", f.trigger);
  if (f.unlabelled) q.set("unlabelled", "true");
  const list = $("ticket-list");
  if (!quiet) {
    replace($("ticket-count"), "Loading tickets");
    replace(list, skeleton(5).map((s) => h("li", {}, s)));
  }
  try {
    ui.tickets = await api(`/tickets?${q}`);
    renderTickets();
  } catch (e) {
    replace($("ticket-count"));
    replace(list, h("li", {}, stateBox("Could not load tickets", e.message, "error")));
  }
}

function renderTickets() {
  const list = $("ticket-list");
  const n = ui.tickets.length;
  $("ticket-count").textContent = n === 0 ? "" : `${n} ticket${n === 1 ? "" : "s"}, newest first`;
  if (n === 0) {
    const f = filters();
    const filtered = f.state || f.trigger || f.unlabelled;
    replace(list, h("li", {}, stateBox(
      "No tickets here",
      filtered ? "Nothing matches these filters. Try a wider window or clear a filter."
        : "Tickets appear when a message in scope fires a trigger. Check the Scope tab if you expected some.",
    )));
    return;
  }
  replace(list, ui.tickets.map((t) => h("li", {}, ticketButton(t))));
}

function ticketButton(t) {
  const meta = [
    pill(t.state, STATE_PILL[t.state]),
    h("span", {}, TRIGGERS[t.trigger] || t.trigger),
    h("span", {}, `${t.message_count} msg`),
  ];
  if (t.stale) meta.push(pill("stale", "warn", true));
  if (t.state_reason && t.state !== "resolved" && t.state_reason !== "stale") meta.push(h("span", {}, t.state_reason.replace(/_/g, " ")));
  if (t.label_implicit) {
    const [text, tone] = IMPLICIT[t.label_implicit] || [t.label_implicit, "muted"];
    meta.push(pill(text, tone, true));
  }
  if (t.label_explicit) meta.push(pill(`You: ${EXPLICIT[t.label_explicit]}`, "info", true));
  return h("button", {
    type: "button",
    class: "ticket",
    "aria-current": ui.selected === t.id ? "true" : "false",
    onclick: () => openTicket(t.id),
  },
  h("span", { class: "ticket-title" }, t.chat_label, t.topic ? h("span", { class: "muted" }, ` · topic ${t.topic}`) : null),
  h("span", { class: "ticket-time" }, when(t.opened_at)),
  h("span", { class: "ticket-meta" }, meta));
}

async function openTicket(id, keepText) {
  ui.selected = id;
  if (!keepText) ui.showText = false;
  const pane = $("detail");
  pane.hidden = false;
  document.querySelector(".split").classList.add("has-detail");
  renderTickets();
  replace(pane, skeleton(3));
  try {
    ui.detail = await api(`/tickets/${id}${ui.showText ? "?text=1" : ""}`);
    renderDetail();
  } catch (e) {
    replace(pane, detailHead(`Ticket ${id}`), stateBox("Could not load this ticket", e.message, "error"));
  }
}

function closeTicket() {
  ui.selected = null;
  ui.detail = null;
  ui.showText = false;
  $("detail").hidden = true;
  document.querySelector(".split").classList.remove("has-detail");
  renderTickets();
}

function detailHead(title) {
  return h("div", { class: "detail-head" },
    h("div", {},
      h("button", { type: "button", class: "btn btn-quiet btn-sm detail-back", onclick: closeTicket }, "Back to list"),
      h("h2", { id: "detail-title" }, title)),
    h("button", { type: "button", class: "btn btn-quiet btn-sm detail-close", onclick: closeTicket, "aria-label": "Close ticket" }, "Close"));
}

function renderDetail() {
  const { ticket: t, events, messages, bodies } = ui.detail;
  const pane = $("detail");
  const implicit = t.label_implicit ? IMPLICIT[t.label_implicit] : null;
  const facts = [
    ["Trigger", `${TRIGGERS[t.trigger] || t.trigger}: ${t.trigger_reason}`],
    ["State", [pill(t.state, STATE_PILL[t.state]), t.state_reason ? ` ${t.state_reason.replace(/_/g, " ")}` : ""]],
    ["Route", t.route || "-"],
    ["Opened", `${when(t.opened_at)} (burst closed ${duration(t.closed_at - t.opened_at)} later)`],
    ["Anchor", `message ${t.anchor}, sent ${when(t.anchor_date)}`],
    ["Chat id", h("span", { class: "mono" }, String(t.chat))],
    ["You read it", t.read_at ? `${when(t.read_at)}, ${duration(t.read_at - t.anchor_date)} after` : "-"],
    ["You replied", t.reply_at ? `${when(t.reply_at)}, ${duration(t.reply_at - t.anchor_date)} after` : "-"],
    ["Outcome", implicit ? pill(implicit[0], implicit[1], true) : "Not resolved yet"],
  ];
  if (t.note) facts.push(["Note", t.note]);

  const labels = h("div", { class: "label-group", role: "group", "aria-labelledby": "label-title" },
    Object.entries(EXPLICIT).map(([value, text]) => h("button", {
      type: "button",
      class: "btn btn-quiet",
      "aria-pressed": t.label_explicit === value ? "true" : "false",
      onclick: () => setLabel(t.id, t.label_explicit === value ? null : value),
    }, text)));

  const byId = new Map(bodies.map((b) => [b.id, b]));
  const msgList = h("ol", { class: "messages" }, messages.map((m) => {
    const b = byId.get(m.id);
    return h("li", { class: `message${m.role === "anchor" ? " message-anchor" : ""}` },
      h("div", { class: "message-head" },
        h("span", {}, `#${m.id}`),
        m.role === "anchor" ? pill("anchor", "info", true) : null,
        b ? h("span", {}, b.sender_name || (b.outgoing ? "You" : "Unknown sender")) : null,
        b ? h("time", {}, when(b.date)) : null,
        b && b.deleted_at ? pill("deleted", "bad", true) : null,
        b && b.media_kind ? h("span", {}, b.media_kind) : null),
      !b ? h("div", { class: "message-text message-hidden" }, "Not in the store (outside retention, or never stored)")
        : ui.showText
          ? h("div", { class: "message-text" }, b.text || "(no text)")
          : h("div", { class: "message-text message-hidden" }, "Text hidden"));
  }));

  replace(pane,
    detailHead(`${t.chat_label}${t.topic ? ` · topic ${t.topic}` : ""}`),
    h("dl", { class: "facts" }, facts.map(([k, v]) => [h("dt", {}, k), h("dd", {}, v)])),
    h("h3", { id: "label-title" }, "Would you want a draft for this?"),
    h("p", { class: "muted" }, "Your label feeds explicit precision (M5). Press again to clear it."),
    labels,
    h("div", { class: "block" },
      h("div", { class: "view-head" },
        h("h3", {}, `Messages (${messages.length})`),
        h("button", {
          type: "button",
          class: "btn btn-quiet btn-sm",
          "aria-pressed": ui.showText ? "true" : "false",
          onclick: () => { ui.showText = !ui.showText; openTicket(t.id, true); },
        }, ui.showText ? "Hide text" : "Show text")),
      msgList),
    h("div", { class: "block" },
      h("h3", {}, "History"),
      h("ol", { class: "timeline" }, events.map((e) => h("li", {},
        h("time", {}, when(e.at)),
        h("span", {}, e.from ? `${e.from} → ${e.to}` : `created as ${e.to}`, e.reason ? ` (${e.reason.replace(/_/g, " ")})` : ""))))));
  const title = $("detail-title");
  title.setAttribute("tabindex", "-1");
  title.focus({ preventScroll: true });
  if (window.matchMedia("(max-width: 959px)").matches) pane.scrollIntoView({ block: "start" });
}

async function setLabel(id, label) {
  try {
    await api(`/tickets/${id}/label`, { method: "POST", body: JSON.stringify({ label }) });
    toast(label ? `Labelled ${EXPLICIT[label].toLowerCase()}` : "Label cleared");
    const t = ui.tickets.find((x) => x.id === id);
    if (t) t.label_explicit = label;
    if (ui.detail) ui.detail.ticket.label_explicit = label;
    renderTickets();
    renderDetail();
  } catch (e) {
    toast(e.message, true);
  }
}

// Metrics

async function loadReport() {
  const body = $("metrics-body");
  replace(body, skeleton(4));
  try {
    ui.report = await api(`/report?days=${$("report-days").value}`);
    renderMetrics();
  } catch (e) {
    replace(body, stateBox("Could not build the report", e.message, "error"));
  }
}

function renderMetrics() {
  const r = ui.report;
  const body = $("metrics-body");
  const total = r.daily.reduce((n, d) => n + d.draft + d.digest + d.skipped, 0);
  if (total === 0 && r.sections.every((s) => s.metrics.every((m) => m.n === 0))) {
    replace(body, stateBox("No data in this window yet",
      "Metrics fill in as the daemon runs. The phase 1 plan needs about two weeks before the rules mean much."));
    return;
  }
  const sections = r.sections.filter((s) => s.question !== "Q7");
  replace(body,
    h("section", { "aria-labelledby": "h-rules" },
      h("h2", { id: "h-rules" }, "Decision rules"),
      h("p", { class: "muted" }, "Each rule watches one condition from the phase 1 plan. Only rules marked Act or Suggestion ask for a change."),
      h("ul", { class: "rules" }, groupRules(r.rules).map(({ rule, count }) => {
        const acts = rule.state === "fires" || rule.state === "suggestion";
        const checks = count > 1 ? ` (${count} checks)` : "";
        return h("li", { class: "rule" },
          h("div", {}, pill(RULE_TEXT[rule.state] || rule.state, RULE_PILL[rule.state])),
          h("div", {}, h("strong", {}, rule.id.replace(/_/g, " ")), checks, ": ", acts ? rule.then : `watching for ${rule.when}`),
          acts ? h("div", { class: "rule-when" }, `When ${rule.when}`) : null);
      }))),
    h("section", { class: "section", "aria-labelledby": "h-daily" },
      h("h2", { id: "h-daily" }, "Tickets per day"),
      dailyChart(r.daily)),
    sections.map(sectionBlock));
}

// Rules that need nothing, such as one precision check per trigger, collapse into one row.
function groupRules(rules) {
  const out = [];
  for (const rule of rules) {
    const quiet = rule.state === "holds" || rule.state === "not_enough_data";
    const same = quiet && out.find((g) => g.rule.id === rule.id && g.rule.state === rule.state);
    if (same) same.count += 1;
    else out.push({ rule, count: 1 });
  }
  return out;
}

function sectionBlock(s) {
  const id = `h-${s.question}`;
  return h("section", { class: "section", "aria-labelledby": id },
    h("h2", { id }, h("span", { class: "section-q" }, s.question), s.title),
    h("div", { class: "cards" }, s.metrics.map(metricCard)));
}

function reliabilityBlock(s) {
  const note = [s.question, s.title].filter(Boolean).join(" · ");
  return h("section", { class: "section", lang: "vi", "aria-labelledby": "h-reliability" },
    h("h2", { id: "h-reliability" }, "Độ tin cậy của daemon"),
    note ? h("p", { class: "muted code-note" }, note) : null,
    h("div", { class: "cards" }, (s.metrics || []).map(reliabilityCard)));
}

function reliabilityCard(m) {
  return h("article", { class: "card" },
    h("h3", {}, m.name),
    m.id ? h("p", { class: "muted code-note" }, m.id) : null,
    h("div", { class: "stat-row" }, m.summary.map((s) => h("div", { class: "stat" },
      h("span", { class: "stat-value" }, statValue(s)),
      h("span", { class: "stat-label" }, statLabel(s))))),
    h("div", { class: "card-n" }, m.n === 0 ? "No samples yet" : `n = ${m.n}`),
    m.breakdown && m.breakdown.length ? breakdown(m) : null);
}

function metricCard(m) {
  return h("article", { class: "card" },
    h("div", { class: "card-head" }, h("h3", {}, m.name), h("span", { class: "card-id" }, m.id)),
    h("div", { class: "stat-row" }, m.summary.map((s) => h("div", { class: "stat" },
      h("span", { class: "stat-value" }, statValue(s)),
      h("span", { class: "stat-label" }, statLabel(s))))),
    h("div", { class: "card-n" }, m.n === 0 ? "No samples yet" : `n = ${m.n}`),
    m.breakdown.length ? breakdown(m) : null);
}

function breakdown(m) {
  const cols = m.breakdown[0].stats.map(statLabel);
  return h("details", { class: "breakdown" },
    h("summary", {}, `By ${m.breakdown.length} group${m.breakdown.length === 1 ? "" : "s"}`),
    h("table", {},
      h("thead", {}, h("tr", {}, h("th", {}, "Group"), cols.map((c) => h("th", { class: "num" }, c)), h("th", { class: "num" }, "n"))),
      h("tbody", {}, m.breakdown.map((b) => h("tr", {},
        h("td", {}, b.key.replace(/_/g, " ")),
        b.stats.map((s) => h("td", { class: "num" }, statValue(s))),
        h("td", { class: "num" }, b.n))))));
}

// Stacked bars; each day is a focusable group, and arrow keys move between days.
function dailyChart(days) {
  const series = [["draft", "Draft", "s1"], ["digest", "Digest", "s2"], ["skipped", "Skipped", "s3"]];
  // Drawn at the container's width so text stays at its real size.
  const W = Math.max(300, $("metrics-body").clientWidth - 30), H = 220, PAD_L = 32, PAD_B = 24, PAD_T = 8;
  const max = Math.max(1, ...days.map((d) => d.draft + d.digest + d.skipped));
  const step = (W - PAD_L) / Math.max(1, days.length);
  const barW = Math.max(4, Math.min(36, step * 0.7));
  const y = (v) => H - PAD_B - (v / max) * (H - PAD_B - PAD_T);
  const tip = h("div", { class: "chart-tip", hidden: true });
  const wrap = h("div", { class: "chart-wrap" });

  const label = (d) => {
    const date = new Date(d.day * 86400 * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric" });
    return `${date}: ${d.draft} draft, ${d.digest} digest, ${d.skipped} skipped`;
  };
  const show = (d, x) => {
    tip.textContent = label(d);
    tip.hidden = false;
    const scale = wrap.clientWidth / W;
    tip.style.left = `${x * scale}px`;
    tip.style.top = `${y(d.draft + d.digest + d.skipped) * scale - 6}px`;
  };
  const hide = () => { tip.hidden = true; };

  const ticks = [0, Math.round(max / 2), max].filter((v, i, a) => a.indexOf(v) === i);
  const cols = days.map((d, i) => {
    const x = PAD_L + i * step + (step - barW) / 2;
    let base = 0;
    const rects = series.map(([key, , cls]) => {
      const v = d[key];
      const r = svg("rect", { class: cls, x, width: barW, y: y(base + v), height: Math.max(0, y(base) - y(base + v)) });
      base += v;
      return r;
    });
    const g = svg("g", {
      class: "bar-col",
      tabindex: i === days.length - 1 ? 0 : -1,
      role: "img",
      "aria-label": label(d),
      onmouseenter: () => show(d, x + barW / 2),
      onmouseleave: hide,
      onfocus: () => show(d, x + barW / 2),
      onblur: hide,
      onkeydown: (e) => {
        const next = e.key === "ArrowRight" ? i + 1 : e.key === "ArrowLeft" ? i - 1 : null;
        if (next === null || next < 0 || next >= days.length) return;
        e.preventDefault();
        const all = wrap.querySelectorAll(".bar-col");
        all[i].setAttribute("tabindex", "-1");
        all[next].setAttribute("tabindex", "0");
        all[next].focus();
      },
    },
    svg("rect", { class: "hit", x: PAD_L + i * step, width: step, y: PAD_T, height: H - PAD_B - PAD_T }),
    rects);
    const showDate = days.length <= 14 || i % Math.ceil(days.length / 10) === 0;
    return [g, showDate ? svg("text", { class: "tick", x: x + barW / 2, y: H - 6, "text-anchor": "middle" },
      new Date(d.day * 86400 * 1000).toLocaleDateString(undefined, { day: "numeric" })) : null];
  });

  const chart = svg("svg", { class: "chart", viewBox: `0 0 ${W} ${H}`, role: "group", "aria-label": "Tickets per day, stacked by route. Use arrow keys to move between days." },
    ticks.map((v) => [
      svg("line", { class: "axis", x1: PAD_L, x2: W, y1: y(v), y2: y(v) }),
      svg("text", { class: "tick", x: PAD_L - 6, y: y(v) + 4, "text-anchor": "end" }, v),
    ]),
    cols);
  append(wrap, [chart, tip]);
  return h("div", { class: "chart-card" }, wrap,
    h("div", { class: "legend" }, series.map(([, text, cls]) => h("span", {}, h("span", { class: `swatch ${cls}` }), text))));
}

// Scope

async function loadScope() {
  const body = $("scope-body");
  replace(body, skeleton(3));
  try {
    const [scope, report] = await Promise.all([api("/scope"), ui.report ? ui.report : api("/report?days=14")]);
    ui.report = report;
    renderScope(scope, report.candidates || []);
  } catch (e) {
    replace(body, stateBox("Could not load the scope", e.message, "error"));
  }
}

function table(headers, rows, wide) {
  return h("div", { class: "table-wrap" }, h("table", { class: wide ? "data wide" : "data" },
    h("thead", {}, h("tr", {}, headers.map(([text, cls]) => h("th", { scope: "col", class: cls || null }, text)))),
    h("tbody", {}, rows)));
}

function renderScope(scope, candidates) {
  const entries = scope.entries || [];
  const parts = [];
  if (scope.missing) {
    parts.push(h("div", { class: "banner", role: "note" },
      "No scope.toml yet, so nothing is watched. Create it at ", h("span", { class: "mono" }, scope.path),
      ", using the candidates below or `captain chats` for ids."));
  }

  const entryRows = entries.map((e) => h("tr", {},
    h("td", {}, h("div", {}, e.label), h("div", { class: "mono muted" }, String(e.id))),
    h("td", {}, e.kind, e.topics ? ` (topics ${e.topics.join(", ")})` : ""),
    h("td", {}, e.triggers.map((t) => TRIGGERS[t] || t).join(", ")),
    h("td", {}, e.keywords.join(", ") || "-"),
    h("td", {}, e.profile),
    h("td", {}, e.mute_until_unix ? `until ${when(e.mute_until_unix)}` : "-")));
  parts.push(h("section", { "aria-labelledby": "h-entries" },
    h("h2", { id: "h-entries" }, `Watched chats (${entries.length})`),
    entries.length === 0
      ? stateBox("No chats in scope", "Add [[chat]] entries to scope.toml, then press Reload files.")
      : table([["Label"], ["Kind"], ["Triggers"], ["Keywords"], ["Profile"], ["Muted"]], entryRows)));

  const candidateRows = candidates.map((c) => h("tr", {},
    h("td", {}, h("div", {}, c.title || "Untitled"), h("div", { class: "mono muted" }, String(c.chat))),
    h("td", { class: "num" }, c.would_trigger),
    h("td", { class: "num" }, c.messages),
    h("td", {}, h("button", {
      type: "button",
      class: "btn btn-quiet btn-sm",
      "aria-label": `Copy scope.toml entry for ${c.title || c.chat}`,
      onclick: () => copySnippet(c),
    }, "Copy TOML"))));
  parts.push(h("section", { class: "section", "aria-labelledby": "h-candidates" },
    h("h2", { id: "h-candidates" }, "Chats outside scope that would have triggered"),
    h("p", { class: "muted" }, "Counted over the last 14 days without storing any text. A chat high on this list is a candidate for scope.toml."),
    candidates.length === 0
      ? stateBox("No candidates", "Nothing outside scope would have fired a trigger.")
      : table([["Chat"], ["Would trigger", "num"], ["Messages", "num"], [h("span", { class: "sr-only" }, "Action")]], candidateRows)));

  const p = scope.policy || {};
  const profiles = p.profile || p.profiles || {};
  const keys = ["resolve_window_secs", "stale_after_secs", "message_retention_days", "metric_retention_days",
    "manual_before_secs", "manual_after_secs", "count_out_of_scope"].filter((k) => k in p);
  const profileRows = Object.entries(profiles).map(([name, v]) => h("tr", {},
    h("td", {}, name),
    h("td", {}, duration(v.debounce_secs)),
    h("td", {}, duration(v.burst_max_secs)),
    h("td", {}, v.burst_max_messages)));
  parts.push(h("section", { class: "section", "aria-labelledby": "h-policy" },
    h("h2", { id: "h-policy" }, "Policy"),
    h("dl", { class: "facts" }, keys.map((k) => [h("dt", {}, k.replace(/_/g, " ")), h("dd", { class: "mono" }, String(p[k]))])),
    profileRows.length ? table([["Profile"], ["Debounce"], ["Longest burst"], ["Most messages"]], profileRows) : null));
  replace($("scope-body"), parts);
}

function snippet(c) {
  const dm = c.chat > 0;
  const title = (c.title || "Chat").replace(/["\\]/g, "");
  return `[[chat]]\nid = ${c.chat}\nlabel = "${title}"\nkind = "${dm ? "dm" : "group"}"\ntriggers = [${dm ? '"dm_all"' : '"mention", "reply_to_me"'}]\n`;
}

async function copySnippet(c) {
  const text = snippet(c);
  try {
    await navigator.clipboard.writeText(text);
    toast("Copied. Paste it into scope.toml, then press Reload files.");
  } catch {
    toast("Clipboard is blocked; the snippet is in the console", true);
    console.log(text); // eslint-disable-line no-console
  }
}

async function reloadScope() {
  const btn = $("scope-reload");
  btn.disabled = true;
  try {
    const r = await api("/scope/reload", { method: "POST" });
    toast(`Reloaded: ${r.scope_chats} chat${r.scope_chats === 1 ? "" : "s"} in scope`);
    await loadScope();
    await loadStatus();
  } catch (e) {
    toast(e.message, true);
  } finally {
    btn.disabled = false;
  }
}

// Health

async function loadHealth() {
  const body = $("health-body");
  replace(body, skeleton(3));
  try {
    const [status, report] = await Promise.all([api("/status"), api("/report?days=14")]);
    ui.status = status;
    renderStatus();
    renderHealth(status, report);
  } catch (e) {
    replace(body, stateBox("Could not load health", e.message, "error"));
  }
}

function healthBanners(s) {
  const parts = [];
  if (s.connection === "unauthorized") {
    parts.push(h("div", { class: "banner banner-bad", role: "alert" },
      "Telegram revoked this session. The daemon stopped reading updates. Log in again with `captain auth login` on this machine, then restart the daemon."));
  } else if (s.connection === "offline") {
    parts.push(h("div", { class: "banner", role: "note" },
      "This is `captain ui serve`: the store without Telegram. Live numbers such as update lag are not available."));
  }
  if (s.scope_missing) {
    parts.push(h("div", { class: "banner", role: "note" }, "scope.toml is missing, so no chat is watched."));
  }
  return parts;
}

function rightNowBlock(s) {
  return h("section", { "aria-labelledby": "h-now" },
    h("h2", { id: "h-now" }, "Right now"),
    h("dl", { class: "facts" }, [
      ["Connection", s.connection],
      ["Account", s.me ? `${s.me.name || ""}${s.me.username ? ` (@${s.me.username})` : ""}` : "-"],
      ["Running since", s.connection === "offline" ? "-" : `${when(s.started_at)} (${ago(s.started_at, s.now)})`],
      ["Last update", s.connection === "offline" ? "-" : ago(s.last_update_at, s.now)],
      ["Open bursts", s.open_bursts],
      ["Ticket creation", s.paused ? "Paused" : "On"],
      ["Chats in scope", s.scope_chats],
      ["UI address", s.ui_url || "-"],
    ].map(([k, v]) => [h("dt", {}, k), h("dd", {}, String(v))])));
}

function renderHealth(s, r) {
  const parts = healthBanners(s);
  parts.push(rightNowBlock(s));
  const q7 = r.sections.find((x) => x.question === "Q7");
  if (q7) parts.push(sectionBlock(q7));
  replace($("health-body"), parts);
}

// Navigation

const VIEWS = {
  tickets: () => loadTickets(),
  metrics: () => loadReport(),
  scope: () => loadScope(),
  health: () => loadHealth(),
};

function registerView(name, loader) {
  VIEWS[name] = loader;
}

function route() {
  ui.loadToken = {};
  ui.redraw = null;
  const raw = (location.hash || "").replace(/^#/, "");
  const name = VIEWS[raw] ? raw : (VIEWS.overview ? "overview" : "tickets");
  if (raw !== name) history.replaceState(null, "", `#${name}`);
  ui.view = name;
  for (const v of Object.keys(VIEWS)) {
    const section = $(`view-${v}`);
    if (section) section.hidden = v !== ui.view;
  }
  for (const a of document.querySelectorAll(".tabs a")) {
    if (a.dataset.view === ui.view) a.setAttribute("aria-current", "page");
    else a.removeAttribute("aria-current");
  }
  revealTab();
  const loader = VIEWS[ui.view];
  if (loader) loader();
}

function paintTabFade() {
  const nav = document.querySelector(".tabs");
  if (!nav) return;
  const fade = globalThis.CaptainFormat && globalThis.CaptainFormat.edgeFade
    ? globalThis.CaptainFormat.edgeFade(nav.scrollLeft, nav.clientWidth, nav.scrollWidth)
    : { moreLeft: nav.scrollLeft > 4, moreRight: nav.scrollLeft + nav.clientWidth < nav.scrollWidth - 4 };
  nav.classList.toggle("more-left", fade.moreLeft);
  nav.classList.toggle("more-right", fade.moreRight);
}

function revealTab() {
  const nav = document.querySelector(".tabs");
  if (!nav) return;
  const current = nav.querySelector("[aria-current='page']");
  if (current) {
    const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    current.scrollIntoView({ inline: "nearest", block: "nearest", behavior: reduce ? "auto" : "smooth" });
  }
  paintTabFade();
  requestAnimationFrame(paintTabFade);
}

let started = false;
function start() {
  if (started) return;
  started = true;
  restoreFilters();
  $("pause").addEventListener("click", togglePause);
  $("filters").addEventListener("change", () => loadTickets());
  $("tickets-refresh").addEventListener("click", () => loadTickets());
  $("report-days").addEventListener("change", loadReport);
  $("scope-reload").addEventListener("click", reloadScope);
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && ui.selected !== null && !document.querySelector("dialog[open]")) closeTicket();
  });
  let resizeTimer;
  window.addEventListener("resize", () => {
    clearTimeout(resizeTimer);
    resizeTimer = setTimeout(() => {
      if (typeof ui.redraw === "function") ui.redraw();
      else if (ui.view === "metrics" && ui.report) renderMetrics();
    }, 200);
  });
  const tabNav = document.querySelector(".tabs");
  if (tabNav) {
    tabNav.addEventListener("scroll", paintTabFade, { passive: true });
    window.addEventListener("resize", paintTabFade);
  }
  window.addEventListener("hashchange", () => { route(); $("main").focus({ preventScroll: true }); });
  route();
  loadStatus();
  ui.timers.push(setInterval(loadStatus, STATUS_POLL_MS));
  ui.timers.push(setInterval(() => {
    if (ui.view === "tickets" && ui.selected === null && !document.hidden) loadTickets(true);
  }, LIST_POLL_MS));
}

const RANGE_KEY = "captain.range";
const RANGE_DAYS = ["7", "14", "30", "90"];

function readJson(key, fallback) {
  try {
    const value = JSON.parse(localStorage.getItem(key) || "null");
    if (value && typeof value === "object") return value;
  } catch { /* a bad saved value means the defaults */ }
  return fallback;
}

function writeJson(key, value) {
  try { localStorage.setItem(key, JSON.stringify(value)); } catch { /* storage can be blocked */ }
}

function readRange() {
  const saved = readJson(RANGE_KEY, {});
  const source = ["all", "telegram", "zalo"].includes(saved.source) ? saved.source : "all";
  const days = RANGE_DAYS.includes(String(saved.days)) ? String(saved.days) : "14";
  return { source, days };
}

function writeRange(range) {
  writeJson(RANGE_KEY, { source: range.source, days: String(range.days) });
}

function syncRangeToolbar(el) {
  const range = readRange();
  for (const button of el.querySelectorAll("button[data-source]")) {
    button.setAttribute("aria-pressed", button.dataset.source === range.source ? "true" : "false");
  }
  const select = el.querySelector("select");
  if (select) select.value = range.days;
}

function bindRangeToolbar(el, onChange) {
  if (!el) return readRange();
  if (!el.dataset.bound) {
    el.dataset.bound = "1";
    const sources = [["all", "Tất cả"], ["telegram", "Telegram"], ["zalo", "Zalo"]];
    const seg = h("div", { class: "seg", role: "group", "aria-label": "Nguồn" });
    for (const [value, label] of sources) {
      seg.append(h("button", {
        type: "button",
        "data-source": value,
        onclick: () => {
          const next = readRange();
          next.source = value;
          writeRange(next);
          syncRangeToolbar(el);
          onChange(next);
        },
      }, label));
    }
    const select = h("select", {
      "aria-label": "Khoảng ngày",
      onchange: (event) => {
        const next = readRange();
        next.days = event.target.value;
        writeRange(next);
        onChange(next);
      },
    }, RANGE_DAYS.map((value) => h("option", { value }, `${value} ngày`)));
    replace(el, h("span", { class: "range-label" }, "Nguồn"), seg, h("label", { class: "inline" }, "Khoảng", select));
  }
  syncRangeToolbar(el);
  return readRange();
}

function bindDaysToolbar(el, storageKey, onChange) {
  if (!el) return "14";
  if (!el.dataset.bound) {
    el.dataset.bound = "1";
    const select = h("select", {
      "aria-label": "Khoảng ngày",
      onchange: (event) => {
        writeJson(storageKey, { days: event.target.value });
        onChange(event.target.value);
      },
    }, RANGE_DAYS.map((value) => h("option", { value }, `${value} ngày`)));
    replace(el, h("label", { class: "inline" }, "Khoảng", select));
  }
  const saved = readJson(storageKey, {});
  const days = RANGE_DAYS.includes(String(saved.days)) ? String(saved.days) : "14";
  const select = el.querySelector("select");
  if (select) select.value = days;
  return days;
}

function sourceLabel(source) {
  if (source === "telegram") return "Telegram";
  if (source === "zalo") return "Zalo";
  if (source === "all") return "Tất cả";
  return source || "-";
}

function kindLabel(kind) {
  return { dm: "DM", group: "Nhóm", channel: "Kênh", forum: "Diễn đàn" }[kind] || kind || "-";
}

function labelName(id) {
  if (IMPLICIT[id]) return IMPLICIT[id][0];
  if (EXPLICIT[id]) return EXPLICIT[id];
  if (id === "unlabelled") return "Unlabelled";
  return id || "-";
}

function moduleLabel(state) {
  return { ok: "Ổn", degraded: "Chậm", down: "Tắt", unknown: "Chưa rõ" }[state] || state || "-";
}

function moduleTone(state) {
  return { ok: "ok", degraded: "warn", down: "bad", unknown: "muted" }[state] || "muted";
}

function stillCurrent(token) {
  return ui.loadToken === token;
}

function openQueuedTickets() {
  const form = $("filters");
  if (form) {
    form.elements.state.value = "queued";
    form.elements.trigger.value = "";
    form.elements.unlabelled.checked = false;
    form.elements.days.value = "30";
  }
  if ((location.hash || "") === "#tickets") loadTickets();
  else location.hash = "tickets";
}

function confirmDialog(opts) {
  const back = document.activeElement;
  return new Promise((resolve) => {
    let settled = false;
    const dlg = h("dialog", { class: "modal" });
    const finish = (value) => {
      if (settled) return;
      settled = true;
      if (dlg.open) dlg.close();
      dlg.remove();
      if (back && typeof back.focus === "function") back.focus();
      resolve(value);
    };
    const cancel = h("button", { type: "button", class: "btn btn-quiet", onclick: () => finish(false) }, opts.cancel || "Huỷ");
    const ok = h("button", { type: "button", class: "btn", onclick: () => finish(true) }, opts.ok || "Tiếp tục");
    append(dlg, [
      h("h2", { id: "modal-title" }, opts.title),
      h("p", {}, opts.body || ""),
      h("div", { class: "modal-actions" }, cancel, ok),
    ]);
    dlg.setAttribute("aria-labelledby", "modal-title");
    dlg.addEventListener("cancel", (event) => {
      event.preventDefault();
      finish(false);
    });
    document.body.append(dlg);
    dlg.showModal();
    cancel.focus();
  });
}

function qrSource(body) {
  if (!body || typeof body !== "object") return "";
  const fields = [body.qr, body.image, body.png, body.data_url, body.url];
  for (const value of fields) {
    if (typeof value !== "string" || !value) continue;
    if (value.startsWith("data:image/")) return value;
    if (value.startsWith("/api/") && !value.includes("..") && !value.includes("\\")) return value;
  }
  const raw = typeof body.qr_base64 === "string" ? body.qr_base64 : (typeof body.png_base64 === "string" ? body.png_base64 : "");
  const b64 = raw.replace(/\s/g, "");
  if (b64 && /^[A-Za-z0-9+/=]+$/.test(b64) && b64.length > 32) return `data:image/png;base64,${b64}`;
  return "";
}

function qrDialog() {
  const dlg = h("dialog", { class: "modal" });
  const box = h("div", { class: "qr-box", role: "status", "aria-live": "polite" }, "Đang xin mã QR.");
  const close = h("button", { type: "button", class: "btn", onclick: () => { if (dlg.open) dlg.close(); dlg.remove(); } }, "Đóng");
  append(dlg, [
    h("h2", { id: "qr-title" }, "Quét mã QR bằng Zalo"),
    h("p", {}, "Mở Zalo trên điện thoại, vào Cài đặt, Quét mã, rồi quét mã trên màn hình này."),
    box,
    h("div", { class: "modal-actions" }, close),
  ]);
  dlg.setAttribute("aria-labelledby", "qr-title");
  dlg.addEventListener("cancel", (event) => {
    event.preventDefault();
    if (dlg.open) dlg.close();
    dlg.remove();
  });
  document.body.append(dlg);
  dlg.showModal();
  return {
    showImage(src) {
      const img = h("img", { alt: "Mã QR đăng nhập Zalo", width: "220", height: "220", src });
      box.replaceChildren(img);
    },
    showMessage(text) {
      box.replaceChildren(document.createTextNode(text));
    },
  };
}

async function reloginZalo(button) {
  const ok = await confirmDialog({
    title: "Đăng nhập lại Zalo bằng QR?",
    body: "Bridge dừng listener và tạo mã QR. Phiên Zalo hiện tại bị thay khi quét xong, nên tin Zalo có thể không về Telegram trong lúc chờ. Cần điện thoại đang giữ tài khoản Zalo của bridge.",
    ok: "Tiếp tục, hiện mã QR",
  });
  if (!ok) return;
  if (button) button.disabled = true;
  const dialog = qrDialog();
  try {
    const body = await api("/zalo/relogin", { method: "POST" });
    const src = qrSource(body);
    if (src) dialog.showImage(src);
    else dialog.showMessage("Đã gọi đăng nhập, nhưng phản hồi không có ảnh QR.");
  } catch (e) {
    dialog.showMessage(e.status === 501 ? "Chưa kết nối bridge." : (e.message || "Không xin được mã QR."));
  } finally {
    if (button) button.disabled = false;
  }
}

async function retryZalo(id, button) {
  if (button) button.disabled = true;
  try {
    const body = await api(`/zalo/retry/${encodeURIComponent(id)}`, { method: "POST" });
    toast(body && body.accepted === false ? "Bridge không nhận lần thử này." : "Đã gửi yêu cầu thử lại.");
  } catch (e) {
    toast(e.status === 501 ? "Chưa kết nối bridge." : e.message, true);
    if (button) button.disabled = false;
  }
}

async function copyText(text, okMessage) {
  try {
    await navigator.clipboard.writeText(text);
    toast(okMessage || "Đã sao chép");
    return true;
  } catch {
    toast("Trình duyệt chặn clipboard", true);
    return false;
  }
}

function placeTip(wrap, tip, x, y, viewWidth) {
  tip.hidden = false;
  const scale = viewWidth > 0 ? wrap.clientWidth / viewWidth : 1;
  tip.style.left = `${x * scale}px`;
  tip.style.top = `${Math.max(0, y * scale - 8)}px`;
}

function rove(event, items, index) {
  let next = null;
  if (event.key === "ArrowRight") next = index + 1;
  else if (event.key === "ArrowLeft") next = index - 1;
  else if (event.key === "Home") next = 0;
  else if (event.key === "End") next = items.length - 1;
  else return false;
  if (next < 0 || next >= items.length) return false;
  event.preventDefault();
  items[index].setAttribute("tabindex", "-1");
  items[next].setAttribute("tabindex", "0");
  items[next].focus();
  return true;
}

function sparkline(values, source) {
  const fmt = globalThis.CaptainFormat;
  const series = Array.isArray(values) ? values : [];
  const scaled = fmt.sparklineScale(series, 24);
  const sum = series.reduce((total, value) => total + (Number(value) || 0), 0);
  const host = h("span", {
    class: "spark",
    role: "img",
    "aria-label": `7 ngày: ${series.map((value) => fmt.formatCount(value)).join(", ")}. Tổng ${fmt.formatCount(sum)}`,
  });
  series.forEach((value, index) => {
    const positive = Number(value) > 0;
    const bar = h("i", { class: positive ? (source === "zalo" ? "spark-zalo" : "spark-tg") : "spark-zero" });
    bar.style.height = `${positive ? Math.max(2, scaled.heights[index]) : 2}px`;
    host.append(bar);
  });
  return h("span", { class: "spark-line" }, host, h("span", { class: "num" }, fmt.formatCount(sum)));
}

function splitView(tableNode, cards) {
  return h("div", { class: "split-view" },
    h("div", { class: "split-table" }, tableNode),
    h("div", { class: "split-cards" }, cards));
}

function labeledCard(pairs) {
  return h("article", { class: "row-card" }, pairs.map(([label, value]) => h("p", { class: "row-field" },
    h("span", { class: "row-label" }, label),
    h("span", { class: "row-value" }, value instanceof Node ? value : String(value ?? "-")))));
}

function hbar(value, max, tone) {
  const amount = Math.max(0, Number(value) || 0);
  const peak = Math.max(0, Number(max) || 0);
  const width = peak > 0 ? Math.round((amount / peak) * 200) : 0;
  const cls = { zalo: "fill-2", rest: "fill-rest", warn: "fill-warn", bad: "fill-bad", info: "fill-info", alt: "fill-3" }[tone] || "fill-1";
  return svg("svg", {
    class: "hbar",
    viewBox: "0 0 200 12",
    width: "100%",
    height: "12",
    "aria-hidden": "true",
  },
  svg("rect", { class: "hbar-bg", x: 0, y: 1, width: 200, height: 10, rx: 2 }),
  svg("rect", { class: cls, x: 0, y: 1, width: Math.max(amount > 0 ? 2 : 0, width), height: 10, rx: 2 }));
}

window.Captain = {
  $, h, svg, append, replace, pill, stateBox, skeleton, toast, api, ApiError,
  when, ago, duration, table, ui, VIEWS, registerView, start, TRIGGERS,
  sectionBlock, reliabilityBlock, healthBanners, rightNowBlock, confirmDialog, reloginZalo, retryZalo,
  placeTip, rove, readJson, writeJson, readRange, writeRange, bindRangeToolbar, bindDaysToolbar,
  copyText, sourceLabel, kindLabel, labelName, moduleLabel, moduleTone, stillCurrent,
  openQueuedTickets, sparkline, hbar, splitView, labeledCard,
};
