"use strict";
(function () {
  const KEY = "captain.conversations";
  let offset = 0;
  let generation = 0;

  function fmt() { return globalThis.CaptainFormat; }

  function readConv() {
    const saved = Captain.readJson(KEY, {});
    const source = ["all", "telegram", "zalo"].includes(saved.source) ? saved.source : "all";
    const kind = ["", "dm", "group", "channel", "forum"].includes(saved.kind) ? saved.kind : "";
    const rawDays = saved.days === undefined || saved.days === null ? "7" : String(saved.days);
    const days = ["", "1", "7", "14", "30"].includes(rawDays) ? rawDays : "7";
    const q = typeof saved.q === "string" ? saved.q : "";
    return { source, kind, days, q };
  }

  function currentConv() {
    const form = document.getElementById("conversations-filters");
    return {
      source: form.elements.source.value,
      kind: form.elements.kind.value,
      days: form.elements.days.value,
      q: form.elements.q.value.trim(),
    };
  }

  function field(name, label, options, selected) {
    return Captain.h("label", {}, label, Captain.h("select", { name }, options.map(([value, text]) => Captain.h("option", {
      value,
      selected: value === selected,
    }, text))));
  }

  function ensureFilters() {
    const form = document.getElementById("conversations-filters");
    if (form.dataset.bound) return;
    form.dataset.bound = "1";
    const saved = readConv();
    form.addEventListener("submit", (event) => event.preventDefault());
    const search = Captain.h("label", {}, "Tìm hội thoại", Captain.h("input", {
      type: "search",
      name: "q",
      value: saved.q,
      placeholder: "Tên hoặc id",
      "aria-label": "Tên hoặc id",
      autocomplete: "off",
    }));
    Captain.replace(form,
      field("source", "Nguồn", [["all", "Tất cả"], ["telegram", "Telegram"], ["zalo", "Zalo"]], saved.source),
      field("kind", "Loại", [["", "Tất cả"], ["dm", "DM"], ["group", "Nhóm"], ["channel", "Kênh"], ["forum", "Diễn đàn"]], saved.kind),
      field("days", "Hoạt động trong", [["1", "1 ngày qua"], ["7", "7 ngày qua"], ["14", "14 ngày qua"], ["30", "30 ngày qua"], ["", "Mọi lúc"]], saved.days),
      search);
    form.addEventListener("change", (event) => {
      if (event.target.name === "q") return;
      offset = 0;
      Captain.writeJson(KEY, currentConv());
      loadConversations();
    });
    const run = fmt().debounce(() => {
      if (Captain.ui.view !== "conversations") return;
      offset = 0;
      Captain.writeJson(KEY, currentConv());
      loadConversations();
    }, fmt().SEARCH_DEBOUNCE_MS);
    form.elements.q.addEventListener("input", run);
  }

  function loadConversations() {
    if (Captain.ui.view !== "conversations") return;
    ensureFilters();
    const token = Captain.ui.loadToken;
    const gen = ++generation;
    const filters = currentConv();
    const params = new URLSearchParams();
    params.set("source", filters.source);
    if (filters.kind) params.set("kind", filters.kind);
    if (filters.days) params.set("days", filters.days);
    params.set("limit", "50");
    params.set("offset", String(offset));
    if (filters.q) params.set("q", filters.q);
    const body = document.getElementById("conversations-body");
    const status = document.getElementById("conversations-count");
    const banner = document.getElementById("conversations-banner");
    status.textContent = "Đang tải hội thoại";
    Captain.replace(banner);
    Captain.replace(body, Captain.skeleton(5));
    Promise.all([
      Captain.api(`/conversations?${params}`),
      Captain.api("/modules").catch(() => null),
    ]).then(([page, modules]) => {
      if (gen !== generation || Captain.ui.view !== "conversations" || !Captain.stillCurrent(token)) return;
      renderConversations(page, modules, filters);
    }).catch((error) => {
      if (gen !== generation || Captain.ui.view !== "conversations" || !Captain.stillCurrent(token)) return;
      status.textContent = "Không tải được hội thoại";
      Captain.replace(body, Captain.stateBox("Không tải được hội thoại", error.message, "error"));
    });
  }

  function bridgeDown(modules) {
    const list = (modules && modules.modules) || [];
    const bridge = list.find((item) => item.module === "zalo-bridge");
    return Boolean(bridge && bridge.state === "down");
  }

  function renderConversations(page, modules, filters) {
    const F = fmt();
    const rows = page.rows || [];
    const total = Number(page.total) || 0;
    const status = document.getElementById("conversations-count");
    const banner = document.getElementById("conversations-banner");
    const body = document.getElementById("conversations-body");
    status.textContent = total === 0 ? "Không có hội thoại nào khớp" : `${F.formatCount(total)} hội thoại, hoạt động gần nhất trước`;
    Captain.replace(banner, bridgeDown(modules)
      ? Captain.h("div", { class: "banner", role: "note" }, "Zalo bridge tắt. Dòng Zalo trong bảng là số đã lưu, có thể cũ.")
      : null);
    if (rows.length === 0) {
      const filtered = filters.kind || filters.q || filters.source !== "all" || filters.days !== "";
      Captain.replace(body, Captain.stateBox("Không có hội thoại nào khớp", filtered ? "Thử bỏ bớt bộ lọc hoặc nới khoảng ngày." : "Hội thoại xuất hiện khi scope.toml hoặc store có chat."));
      Captain.ui.redraw = null;
      return;
    }
    const now = Date.now() / 1000;
    const table = Captain.table(
      [["Nguồn"], ["Hội thoại"], ["Loại"], ["Tin 7 ngày", "spark-col"], ["Lần cuối"], ["Ticket mở", "num"], ["Nhãn"]],
      rows.map((row) => Captain.h("tr", {},
        Captain.h("td", {}, Captain.sourceLabel(row.source)),
        Captain.h("td", {}, Captain.h("div", {}, row.title || "Untitled"), Captain.h("div", { class: "mono muted" }, row.external_id || "-")),
        Captain.h("td", {}, Captain.kindLabel(row.kind)),
        Captain.h("td", { class: "spark-col" }, Captain.sparkline(row.messages_7d, row.source)),
        Captain.h("td", {}, F.relativeTime(row.last_activity_unix_secs, now)),
        Captain.h("td", { class: "num" }, row.open_tickets ? F.formatCount(row.open_tickets) : "-"),
        Captain.h("td", {}, row.tag || "-"))),
      true);
    const cards = rows.map((row) => Captain.h("article", { class: "row-card conv-card" },
      Captain.h("div", { class: "conv-top" },
        Captain.h("strong", { class: "conv-title" }, row.title || "Untitled"),
        Captain.pill(Captain.sourceLabel(row.source), row.source === "zalo" ? "ok" : "info", true),
        Captain.pill(Captain.kindLabel(row.kind), "muted", true),
        Captain.h("span", { class: "mono muted" }, row.external_id || "-")),
      Captain.h("div", { class: "conv-bot" },
        Captain.h("span", {}, F.relativeTime(row.last_activity_unix_secs, now)),
        Captain.h("span", {}, `Ticket mở ${row.open_tickets ? F.formatCount(row.open_tickets) : "-"}`),
        Captain.sparkline(row.messages_7d, row.source),
        row.tag ? Captain.h("span", {}, row.tag) : null)));
    const limit = 50;
    const pager = total > limit ? Captain.h("div", { class: "pager" },
      Captain.h("button", { type: "button", class: "btn btn-quiet", disabled: offset === 0, onclick: () => { offset = Math.max(0, offset - limit); loadConversations(); } }, "Trước"),
      Captain.h("span", { class: "num" }, `${F.formatCount(offset + 1)}–${F.formatCount(Math.min(offset + rows.length, total))} / ${F.formatCount(total)}`),
      Captain.h("button", { type: "button", class: "btn btn-quiet", disabled: offset + limit >= total, onclick: () => { offset += limit; loadConversations(); } }, "Sau")) : null;
    Captain.replace(body, Captain.splitView(table, cards), pager);
    Captain.ui.redraw = null;
  }

  document.getElementById("conversations-refresh").addEventListener("click", loadConversations);
  Captain.registerView("conversations", loadConversations);
})();
