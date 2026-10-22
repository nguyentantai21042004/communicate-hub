"use strict";
(function () {
  let generation = 0;

  function fmt() { return globalThis.CaptainFormat; }

  function loadHealth() {
    if (Captain.ui.view !== "health") return;
    const token = Captain.ui.loadToken;
    const gen = ++generation;
    const body = document.getElementById("health-body");
    const status = document.getElementById("health-status");
    if (status) status.textContent = "Đang tải sức khoẻ";
    Captain.replace(body, Captain.skeleton(4));
    Promise.all([
      Captain.api("/status"),
      Captain.api("/report?days=14"),
      Captain.api("/modules"),
      Captain.api("/events?limit=50"),
      Captain.api("/telegram?days=14"),
      Captain.api("/zalo?days=14"),
    ]).then(([live, report, modules, events, telegram, zalo]) => {
      if (gen !== generation || Captain.ui.view !== "health" || !Captain.stillCurrent(token)) return;
      Captain.ui.status = live;
      Captain.ui.modules = modules;
      renderPage(live, report, modules, events, telegram, zalo);
    }).catch((error) => {
      if (gen !== generation || Captain.ui.view !== "health" || !Captain.stillCurrent(token)) return;
      if (status) status.textContent = "Không tải được sức khoẻ";
      Captain.replace(body, Captain.stateBox("Could not load health", error.message, "error"));
    });
  }

  function moduleBy(modules, name) {
    return ((modules && modules.modules) || []).find((item) => item.module === name) || null;
  }

  function renderPage(live, report, modules, events, telegram, zalo) {
    const status = document.getElementById("health-status");
    const bridge = zalo && zalo.bridge;
    const bridgeDown = bridge && bridge.state === "down";
    if (status) status.textContent = bridgeDown ? "Zalo bridge tắt." : "Đã tải sức khoẻ.";
    const now = Captain.rightNowBlock(live);
    now.classList.add("section");
    const q7 = (report.sections || []).find((item) => item.question === "Q7");
    Captain.replace(document.getElementById("health-body"),
      Captain.healthBanners(live),
      bridgeDown ? Captain.h("div", { class: "banner banner-bad", role: "alert", lang: "vi" }, `Zalo bridge tắt. ${bridge.detail || "Chưa kết nối bridge."}`) : null,
      Captain.h("div", { class: "card-grid-2", lang: "vi" },
        daemonCard(live, telegram, moduleBy(modules, "captain-daemon")),
        bridgeCard(zalo, moduleBy(modules, "zalo-bridge"))),
      eventsCard(events),
      now,
      q7 ? Captain.reliabilityBlock(q7) : null);
    Captain.ui.redraw = null;
  }

  function daemonCard(live, telegram, module) {
    const F = fmt();
    const link = (telegram && telegram.connection) || {};
    const session = (telegram && telegram.session) || {};
    const state = session.connection || (module && module.state) || "offline";
    const tone = { connected: "ok", reconnecting: "warn", unauthorized: "bad", offline: "muted", ok: "ok", down: "bad", degraded: "warn" }[state] || "muted";
    const started = live.connection === "offline" ? "daemon không chạy" : `chạy từ ${F.formatStamp(live.started_at)} (${F.relativeTime(live.started_at, live.now)})`;
    return Captain.h("article", { class: "card" },
      Captain.h("div", { class: "card-head" },
        Captain.h("h2", {}, "captain-daemon"),
        Captain.pill(state, tone)),
      Captain.h("div", { class: "stat-row" },
        stat(F.formatPercent(link.uptime), "uptime 14 ngày"),
        stat(F.formatCount(link.gaps_unfilled), "gap"),
        stat(F.formatDuration(link.lag_p50_secs), "lag p50"),
        stat(live.connection === "offline" ? "-" : F.relativeTime(live.last_update_at, live.now), "cập nhật cuối")),
      Captain.h("p", { class: "kpi-sub" }, `${started}. Q7 đầy đủ nằm bên dưới.`),
      module && module.detail ? Captain.h("p", { class: "muted" }, module.detail) : null);
  }

  function bridgeCard(zalo, module) {
    const F = fmt();
    const bridge = (zalo && zalo.bridge) || { state: "unknown", detail: "" };
    const session = (zalo && zalo.session) || {};
    const queue = (zalo && zalo.send_queue) || {};
    const sessionLabel = { valid: "Hợp lệ", expired: "Hết hạn", logged_out: "Đã đăng xuất", connecting: "Đang kết nối", unknown: "Chưa rõ" }[session.state] || (session.state || "-");
    const wait = queue.retry_after_secs === null || queue.retry_after_secs === undefined
      ? "không có retry_after"
      : `retry_after ${F.formatDuration(queue.retry_after_secs)}`;
    const button = Captain.h("button", {
      type: "button",
      class: "btn",
      onclick: (event) => Captain.reloginZalo(event.currentTarget),
    }, "Đăng nhập lại Zalo (QR)");
    return Captain.h("article", { class: "card" },
      Captain.h("div", { class: "card-head" },
        Captain.h("h2", {}, "zalo-bridge"),
        Captain.pill(Captain.moduleLabel(bridge.state), Captain.moduleTone(bridge.state))),
      Captain.h("div", { class: "stat-row" },
        stat(sessionLabel, "phiên Zalo"),
        stat(F.formatCount(queue.waiting), "hàng đợi"),
        stat(F.formatCount(queue.rate_limits_24h), "lỗi 429 trong 24 giờ")),
      Captain.h("p", { class: "kpi-sub" }, wait),
      Captain.h("p", { class: "muted" }, bridge.detail || (module && module.detail) || "Không có ghi chú."),
      module ? Captain.h("p", { class: "muted" }, `Bridge cập nhật ${F.relativeTime(module.updated_at_unix_secs, Date.now() / 1000)}.`) : null,
      button);
  }

  function stat(value, label) {
    return Captain.h("div", { class: "stat" },
      Captain.h("span", { class: "stat-value" }, value),
      Captain.h("span", { class: "stat-label" }, label));
  }

  function eventsCard(page) {
    const F = fmt();
    const events = (page && page.events) || [];
    const tone = { debug: "muted", info: "ok", warn: "warn", error: "bad" };
    const rows = events.map((event) => {
      const when = F.formatStamp(event.at_unix_secs, true);
      const moduleName = event.module || "-";
      const kind = event.kind || "-";
      const detail = event.detail || "-";
      const eventCell = Captain.h("span", {}, Captain.pill(event.level || "info", tone[event.level] || "muted"), " ", kind);
      const eventCard = Captain.h("span", {}, Captain.pill(event.level || "info", tone[event.level] || "muted"), " ", kind);
      return {
        tr: Captain.h("tr", {},
          Captain.h("td", { class: "col-when num" }, when),
          Captain.h("td", {}, moduleName),
          Captain.h("td", {}, eventCell),
          Captain.h("td", {}, detail)),
        card: Captain.labeledCard([
          ["Thời điểm", when],
          ["Module", moduleName],
          ["Sự kiện", eventCard],
          ["Chi tiết", detail],
        ]),
      };
    });
    return Captain.h("section", { class: "section", lang: "vi", "aria-labelledby": "h-events" },
      Captain.h("h2", { id: "h-events" }, "Sự kiện gần đây"),
      Captain.h("p", { class: "muted" }, "Mới nhất trước. Chi tiết không phải nội dung tin."),
      rows.length
        ? Captain.splitView(Captain.table([["Thời điểm", "col-when"], ["Module"], ["Sự kiện"], ["Chi tiết"]], rows.map((row) => row.tr), true), rows.map((row) => row.card))
        : Captain.stateBox("Chưa có sự kiện", "Module chưa ghi sự kiện nào ra endpoint này."));
  }

  Captain.registerView("health", loadHealth);
})();
