"use strict";
(function () {
  function fmt() { return globalThis.CaptainFormat; }

  function loadOverview() {
    if (Captain.ui.view !== "overview") return;
    const token = Captain.ui.loadToken;
    const range = Captain.bindRangeToolbar(document.getElementById("overview-toolbar"), loadOverview);
    const body = document.getElementById("overview-body");
    const status = document.getElementById("overview-status");
    status.textContent = "Đang tải tổng quan";
    Captain.replace(body, Captain.skeleton(4));
    Captain.api(`/overview?days=${range.days}&source=${range.source}`).then((data) => {
      if (Captain.ui.view !== "overview" || !Captain.stillCurrent(token)) return;
      renderOverview(data, range);
    }).catch((error) => {
      if (Captain.ui.view !== "overview" || !Captain.stillCurrent(token)) return;
      status.textContent = "Không tải được tổng quan";
      Captain.replace(body, Captain.stateBox("Không tải được tổng quan", error.message, "error"));
    });
  }

  function renderOverview(data, range) {
    const F = fmt();
    const body = document.getElementById("overview-body");
    const status = document.getElementById("overview-status");
    const messages = data.kpi.messages_24h || [];
    const chats = data.kpi.conversations || [];
    const ratio = F.moduleHealthRatio(data.kpi.module_summary);
    const days = F.bucketDaily(data.series);
    const attention = data.needs_attention || [];
    const shown = attention.length;
    const queued = Number(data.kpi.needs_attention) || 0;
    status.textContent = shown < queued
      ? `${F.formatCount(shown)} / ${F.formatCount(queued)} đang chờ. Endpoint chỉ gửi nhóm cũ nhất.`
      : `${F.formatCount(queued)} đang chờ, mới nhất trước`;

    const chartCard = Captain.h("article", { class: "card chart-card" },
      Captain.h("h2", {}, "Tin mỗi ngày"),
      Captain.h("p", { class: "muted", id: "overview-chart-summary" }, chartSummary(days, range)));
    const attentionCard = attentionCardOf(attention, queued, shown);
    const strip = moduleStrip(data.modules || []);
    Captain.replace(body,
      Captain.h("div", { class: "kpi-grid" },
        kpi("Tin 24 giờ", F.formatCount(F.sumBy(messages, "messages")), messages.map((row) => `${Captain.sourceLabel(row.source)} ${F.formatCount(row.messages)}`).join(" · ") || "chưa có tin"),
        kpi("Cần xử lý", F.formatCount(queued), "đang chờ"),
        kpi("Phản hồi p50", F.formatMinutes(data.kpi.response_p50_minutes), p50Sub(data, range)),
        kpi("Chat đang hoạt động", `${F.formatCount(F.sumBy(chats, "active"))} / ${F.formatCount(F.sumBy(chats, "watched"))}`, chats.map((row) => `${Captain.sourceLabel(row.source)} ${F.formatCount(row.active)}/${F.formatCount(row.watched)}`).join(" · ") || "chưa có chat"),
        healthKpi(ratio, data.modules || [])),
      Captain.h("div", { class: "layout-2" }, chartCard, attentionCard),
      strip);
    drawChart(chartCard, days);
    Captain.ui.redraw = () => renderOverview(data, range);
  }

  function kpi(title, value, sub) {
    return Captain.h("article", { class: "card" },
      Captain.h("h3", {}, title),
      Captain.h("div", { class: "stat-value" }, value),
      Captain.h("p", { class: "kpi-sub" }, sub));
  }

  function healthKpi(ratio, modules) {
    const F = fmt();
    const sub = ratio.total > 0 && ratio.ok === ratio.total
      ? "module ổn"
      : [ratio.degraded && `${F.formatCount(ratio.degraded)} chậm`, ratio.down && `${F.formatCount(ratio.down)} tắt`, ratio.unknown && `${F.formatCount(ratio.unknown)} chưa rõ`].filter(Boolean).join(", ") || "chưa có module";
    return Captain.h("article", { class: "card kpi-health" },
      Captain.h("h3", {}, "Sức khoẻ"),
      Captain.h("div", { class: "stat-value" }, `${F.formatCount(ratio.ok)} / ${F.formatCount(ratio.total)}`),
      Captain.h("p", { class: "kpi-sub" }, sub),
      Captain.h("div", { class: "kpi-dots" }, modules.map((item) => Captain.h("span", { class: `dot dot-${Captain.moduleTone(item.state)}` }, `${shortModule(item.module)} ${Captain.moduleLabel(item.state)}`))));
  }

  function p50Sub(data, range) {
    const F = fmt();
    if (data.kpi.response_p50_minutes === null || data.kpi.response_p50_minutes === undefined) return "chưa có phản hồi";
    const sample = Number(data.kpi.response_sample) || 0;
    const small = sample < F.LOW_SAMPLE_MIN ? " · mẫu nhỏ" : "";
    return `${range.days} ngày · n = ${F.formatCount(sample)}${small}`;
  }

  function chartSummary(days, range) {
    const F = fmt();
    if (!days.length) return `${range.days} ngày. Chưa có tin trong khoảng này.`;
    const tele = F.sumBy(days, "telegram");
    const zalo = F.sumBy(days, "zalo");
    return `${range.days} ngày UTC, từ ${F.formatUtcDay(days[0].day, true)} đến ${F.formatUtcDay(days[days.length - 1].day, true)}. Telegram ${F.formatCount(tele)}, Zalo ${F.formatCount(zalo)}. Mũi tên chuyển ngày.`;
  }

  function drawChart(card, days) {
    const summary = card.querySelector("#overview-chart-summary");
    if (!days.length || days.every((day) => day.total === 0)) {
      card.append(Captain.stateBox("Chưa có tin trong khoảng này", "Đổi nguồn hoặc nới khoảng ngày."));
      return;
    }
    const F = fmt();
    const series = [["telegram", "Telegram", "s1"], ["zalo", "Zalo", "s2"]];
    const W = Math.max(280, card.clientWidth - 8);
    const H = 220;
    const padL = 40;
    const padB = 24;
    const padT = 8;
    const max = Math.max(1, ...days.map((day) => day.total));
    const step = (W - padL) / Math.max(1, days.length);
    const barW = Math.max(4, Math.min(28, step * 0.62));
    const y = (value) => H - padB - (value / max) * (H - padB - padT);
    const tip = Captain.h("div", { class: "chart-tip", hidden: true });
    const wrap = Captain.h("div", { class: "chart-wrap" });
    const ticks = [0, Math.round(max / 2), max].filter((value, index, all) => all.indexOf(value) === index);
    const cols = days.map((day, index) => {
      const x = padL + index * step + (step - barW) / 2;
      const text = `${F.formatUtcDay(day.day, true)}: Telegram ${F.formatCount(day.telegram)}, Zalo ${F.formatCount(day.zalo)}`;
      let base = 0;
      const rects = series.map(([key, , cls]) => {
        const value = day[key];
        const rect = Captain.svg("rect", { class: cls, x, width: barW, y: y(base + value), height: Math.max(0, y(base) - y(base + value)) });
        base += value;
        return rect;
      });
      const group = Captain.svg("g", {
        class: "bar-col",
        tabindex: index === days.length - 1 ? "0" : "-1",
        role: "img",
        "aria-label": text,
        onmouseenter: () => { tip.textContent = text; Captain.placeTip(wrap, tip, x + barW / 2, y(day.total), W); },
        onmouseleave: () => { tip.hidden = true; },
        onfocus: () => { tip.textContent = text; Captain.placeTip(wrap, tip, x + barW / 2, y(day.total), W); },
        onblur: () => { tip.hidden = true; },
        onkeydown: (event) => Captain.rove(event, [...wrap.querySelectorAll(".bar-col")], index),
      }, Captain.svg("rect", { class: "hit", x: padL + index * step, width: step, y: padT, height: H - padB - padT }), rects);
      const showDate = days.length <= 16 || index % Math.ceil(days.length / 8) === 0;
      return [group, showDate ? Captain.svg("text", { class: "tick", x: x + barW / 2, y: H - 6, "text-anchor": "middle" }, F.formatUtcDay(day.day)) : null];
    });
    const chart = Captain.svg("svg", {
      class: "chart",
      viewBox: `0 0 ${W} ${H}`,
      role: "group",
      "aria-label": summary ? summary.textContent : "Tin mỗi ngày",
    }, ticks.map((value) => [
      Captain.svg("line", { class: "axis", x1: padL, x2: W, y1: y(value), y2: y(value) }),
      Captain.svg("text", { class: "tick", x: padL - 6, y: y(value) + 4, "text-anchor": "end" }, F.formatCount(value)),
    ]), cols);
    Captain.append(wrap, [chart, tip]);
    card.append(wrap, Captain.h("div", { class: "legend" }, series.map(([, text, cls]) => Captain.h("span", {}, Captain.h("span", { class: `swatch ${cls}`, "aria-hidden": "true" }), text))));
  }

  function attentionCardOf(rows, queued, shown) {
    const F = fmt();
    const ordered = F.newestAttention(rows);
    const caption = shown < queued
      ? `${F.formatCount(shown)} / ${F.formatCount(queued)} đang chờ. Đây là nhóm cũ nhất endpoint trả, xếp mới trong nhóm.`
      : `${F.formatCount(shown)} đang chờ, mới nhất trước`;
    const body = ordered.length === 0
      ? Captain.stateBox("Không còn gì cần xử lý", "Ticket queued sẽ hiện ở đây.")
      : Captain.splitView(
        Captain.table(
          [["Nguồn"], ["Hội thoại"], ["Trigger"], ["Tuổi"]],
          ordered.map((row) => Captain.h("tr", {},
            Captain.h("td", {}, Captain.sourceLabel(row.source)),
            Captain.h("td", {}, row.title || "Untitled"),
            Captain.h("td", {}, Captain.TRIGGERS[row.trigger] || row.trigger || "-"),
            Captain.h("td", { class: "num" }, F.formatAge(row.age_secs)))),
          true),
        ordered.map((row) => Captain.labeledCard([
          ["Nguồn", Captain.sourceLabel(row.source)],
          ["Hội thoại", row.title || "Untitled"],
          ["Trigger", Captain.TRIGGERS[row.trigger] || row.trigger || "-"],
          ["Tuổi", F.formatAge(row.age_secs)],
        ])));
    return Captain.h("article", { class: "card" },
      Captain.h("h2", {}, "Cần xử lý"),
      Captain.h("p", { class: "muted" }, caption),
      body,
      Captain.h("p", {}, Captain.h("a", {
        class: "text-link",
        href: "#tickets",
        onclick: (event) => { event.preventDefault(); Captain.openQueuedTickets(); },
      }, "Xem tất cả trong Tickets")));
  }

  function moduleStrip(modules) {
    const F = fmt();
    const now = Date.now() / 1000;
    const cards = modules.map((item) => Captain.h("article", { class: "card module-card" },
      Captain.h("div", { class: "card-head" },
        Captain.h("h3", {}, item.module),
        Captain.pill(Captain.moduleLabel(item.state), Captain.moduleTone(item.state))),
      Captain.h("p", { class: "kpi-sub" }, item.detail || "Không có ghi chú"),
      Captain.h("p", { class: "muted" }, `cập nhật ${F.relativeTime(item.updated_at_unix_secs, now)}`)));
    return Captain.h("section", { class: "section", "aria-labelledby": "h-modules" },
      Captain.h("div", { class: "view-head" },
        Captain.h("h2", { id: "h-modules" }, "Module"),
        Captain.h("a", { class: "text-link", href: "#health" }, "Mở Health")),
      Captain.h("div", { class: "module-strip" }, cards.length ? cards : Captain.stateBox("Chưa có module", "")));
  }

  function shortModule(name) {
    if (name === "captain-daemon") return "daemon";
    if (name === "zalo-bridge") return "zalo-bridge";
    return name || "module";
  }

  Captain.registerView("overview", loadOverview);
})();
