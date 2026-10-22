"use strict";
(function () {
  const DRAFT_P1 = "not available in P1";
  const LABEL_ORDER = ["answered", "answered_elsewhere", "read_only", "unseen", "want", "noise", "skip", "unlabelled"];
  const LABEL_TONE = {
    answered: "fill-1",
    answered_elsewhere: "fill-2",
    read_only: "fill-3",
    unseen: "fill-rest",
    want: "fill-info",
    noise: "fill-warn",
    skip: "fill-bad",
    unlabelled: "fill-rest",
  };

  function fmt() { return globalThis.CaptainFormat; }

  function loadAnalytics() {
    if (Captain.ui.view !== "analytics") return;
    const token = Captain.ui.loadToken;
    const range = Captain.bindRangeToolbar(document.getElementById("analytics-toolbar"), loadAnalytics);
    const body = document.getElementById("analytics-body");
    const status = document.getElementById("analytics-status");
    status.textContent = "Đang tải phân tích";
    Captain.replace(body, Captain.skeleton(4));
    Captain.api(`/analytics?days=${range.days}&source=${range.source}`).then((data) => {
      if (Captain.ui.view !== "analytics" || !Captain.stillCurrent(token)) return;
      renderAnalytics(data, range);
    }).catch((error) => {
      if (Captain.ui.view !== "analytics" || !Captain.stillCurrent(token)) return;
      status.textContent = "Không tải được phân tích";
      Captain.replace(body, Captain.stateBox("Không tải được phân tích", error.message, "error"));
    });
  }

  function renderAnalytics(data, range) {
    const F = fmt();
    const heat = F.heatmapGrid(data.heatmap);
    const total = heat.grid.flat().reduce((sum, value) => sum + value, 0);
    document.getElementById("analytics-status").textContent = total === 0
      ? `${range.days} ngày. Chưa có tin theo giờ.`
      : `${range.days} ngày. ${F.formatCount(total)} tin trên heatmap.`;
    const heatCard = Captain.h("article", { class: "card" });
    const funnelCard = Captain.h("article", { class: "card" });
    const topCard = Captain.h("article", { class: "card" });
    const lineCard = Captain.h("article", { class: "card" });
    Captain.replace(document.getElementById("analytics-body"),
      Captain.h("div", { class: "layout-2" }, heatCard, funnelCard),
      Captain.h("div", { class: "layout-2" }, topCard, lineCard));
    drawHeat(heatCard, heat, range);
    drawFunnel(funnelCard, data.funnel, range);
    drawTop(topCard, data.top_conversations || [], range);
    drawResponse(lineCard, data.response_p50 || [], range);
    Captain.ui.redraw = () => renderAnalytics(data, range);
  }

  function drawHeat(card, heat, range) {
    const F = fmt();
    const cell = 20;
    const gap = 3;
    const step = cell + gap;
    const padT = 22;
    const W = 24 * step + 8;
    const H = padT + 7 * step + 4;
    const total = heat.grid.flat().reduce((sum, value) => sum + value, 0);
    const summary = total === 0
      ? `${range.days} ngày, giờ UTC. Chưa có tin theo giờ. 168 ô đều là 0. Mũi tên chuyển ô.`
      : `${range.days} ngày, giờ UTC. Tổng ${F.formatCount(total)} tin. Ô đậm nhất là ${F.formatCount(heat.max)}. Mũi tên chuyển ô.`;
    const marks = [];
    for (let hour = 0; hour < 24; hour += 1) {
      marks.push(Captain.svg("text", {
        class: "tick",
        x: hour * step + cell / 2,
        y: 14,
        "text-anchor": "middle",
      }, String(hour)));
    }
    const flat = [];
    for (let weekday = 0; weekday < 7; weekday += 1) {
      for (let hour = 0; hour < 24; hour += 1) {
        const value = heat.grid[weekday][hour];
        const level = heat.levels[weekday][hour];
        const index = flat.length;
        const label = `${F.WEEKDAYS[weekday]} ${String(hour).padStart(2, "0")}:00 UTC, ${F.formatCount(value)} tin`;
        const mark = Captain.svg("rect", {
          class: `heat-cell heat-${level}`,
          x: hour * step,
          y: padT + weekday * step,
          width: cell,
          height: cell,
          rx: 2,
          tabindex: index === 0 ? "0" : "-1",
          role: "img",
          "aria-label": label,
          onmouseenter: (event) => showHeatTip(event.currentTarget, label),
          onmouseleave: hideHeatTip,
          onfocus: (event) => showHeatTip(event.currentTarget, label),
          onblur: hideHeatTip,
          onkeydown: (event) => moveHeat(event, index),
        });
        flat.push(mark);
        marks.push(mark);
      }
    }
    const chart = Captain.svg("svg", {
      class: "chart heat",
      viewBox: `0 0 ${W} ${H}`,
      width: String(W),
      height: String(H),
      role: "group",
      "aria-label": summary,
    }, marks);
    chart.style.width = `${W}px`;
    chart.style.height = `${H}px`;
    const tip = Captain.h("div", { class: "chart-tip", hidden: true });
    const days = Captain.h("div", { class: "heat-days", "aria-hidden": "true" },
      F.WEEKDAYS.map((label) => Captain.h("span", {}, label)));
    days.style.paddingTop = `${padT}px`;
    for (const span of days.querySelectorAll("span")) span.style.height = `${step}px`;
    const wrap = Captain.h("div", { class: "chart-wrap chart-scroll" });
    function showHeatTip(node, label) {
      tip.textContent = label;
      const box = node.getBoundingClientRect();
      tip.hidden = false;
      tip.style.position = "fixed";
      tip.style.left = `${box.left + box.width / 2}px`;
      tip.style.top = `${box.top}px`;
    }
    function hideHeatTip() { tip.hidden = true; }
    function moveHeat(event, index) {
      let next = null;
      if (event.key === "ArrowRight") next = index + 1;
      else if (event.key === "ArrowLeft") next = index - 1;
      else if (event.key === "ArrowDown") next = index + 24;
      else if (event.key === "ArrowUp") next = index - 24;
      else if (event.key === "Home") next = 0;
      else if (event.key === "End") next = flat.length - 1;
      else return;
      if (event.key === "ArrowRight" && index % 24 === 23) return;
      if (event.key === "ArrowLeft" && index % 24 === 0) return;
      if (next === null || next < 0 || next >= flat.length) return;
      event.preventDefault();
      flat[index].setAttribute("tabindex", "-1");
      flat[next].setAttribute("tabindex", "0");
      flat[next].focus();
      flat[next].scrollIntoView({ inline: "nearest", block: "nearest" });
    }
    Captain.append(wrap, [chart, tip]);
    Captain.replace(card,
      Captain.h("h2", {}, "Tin theo giờ và thứ"),
      Captain.h("p", { class: "muted" }, summary),
      Captain.h("div", { class: "heat-frame" }, days, wrap),
      Captain.h("div", { class: "legend" },
        Captain.h("span", {}, "ít"),
        [0, 1, 2, 3, 4].map((level) => Captain.h("span", { class: `swatch heat-${level}`, "aria-hidden": "true" })),
        Captain.h("span", {}, "nhiều")));
  }

  function drawFunnel(card, funnel, range) {
    const F = fmt();
    const resolved = [...(funnel.resolved || [])].sort((a, b) => labelRank(a.label) - labelRank(b.label));
    const resolvedTotal = resolved.reduce((sum, row) => sum + (Number(row.count) || 0), 0);
    const queued = Number(funnel.queued) || 0;
    const whole = queued + resolvedTotal;
    const draftText = funnel.draft === DRAFT_P1
      ? "Chưa có dữ liệu. Bản nháp bắt đầu ở P2."
      : (funnel.draft || "Chưa có dữ liệu.");
    const resolvedPct = whole > 0 ? F.formatPercent(resolvedTotal / whole) : "-";
    Captain.replace(card,
      Captain.h("h2", {}, "Phễu ticket"),
      Captain.h("p", { class: "muted" }, `${range.days} ngày. Digest không nằm trong phễu.`),
      funnelRow("Queued", Captain.hbar(queued, whole || 1, "warn"), F.formatCount(queued)),
      funnelRow("Draft", Captain.h("p", { class: "muted draft-note" }, draftText), "-"),
      funnelRow("Resolved", resolvedBar(resolved, whole), `${F.formatCount(resolvedTotal)} · ${resolvedPct}`),
      Captain.h("div", { class: "legend" }, resolved.map((row) => Captain.h("span", {},
        Captain.h("span", { class: `swatch ${LABEL_TONE[row.label] || "fill-rest"}`, "aria-hidden": "true" }),
        `${Captain.labelName(row.label)} ${F.formatCount(row.count)}`))),
      Captain.h("p", { class: "muted" }, queued ? `Còn ${F.formatCount(queued)} ticket queued.` : "Không còn ticket queued."));
  }

  function labelRank(label) {
    const index = LABEL_ORDER.indexOf(label);
    return index === -1 ? LABEL_ORDER.length : index;
  }

  function funnelRow(name, mid, end) {
    return Captain.h("div", { class: "funnel-row" },
      Captain.h("span", {}, name),
      mid,
      Captain.h("span", { class: "num" }, end));
  }

  function resolvedBar(rows, whole) {
    if (!rows.length) return Captain.h("p", { class: "muted" }, "Chưa có ticket resolved.");
    const F = fmt();
    let cursor = 0;
    const width = whole > 0 ? rows.reduce((sum, row) => sum + (Number(row.count) || 0), 0) / whole * 200 : 0;
    const rects = rows.map((row) => {
      const count = Number(row.count) || 0;
      const slice = whole > 0 ? (count / whole) * 200 : 0;
      const rect = Captain.svg("rect", {
        class: LABEL_TONE[row.label] || "fill-rest",
        x: cursor,
        y: 1,
        width: Math.max(count > 0 ? 1 : 0, slice),
        height: 10,
      });
      cursor += slice;
      return rect;
    });
    const label = rows.map((row) => `${Captain.labelName(row.label)} ${F.formatCount(row.count)}`).join(", ");
    return Captain.svg("svg", {
      class: "hbar",
      viewBox: "0 0 200 12",
      width: "100%",
      height: "12",
      role: "img",
      "aria-label": label || "Chưa có ticket resolved",
    }, Captain.svg("rect", { class: "hbar-bg", x: 0, y: 1, width: 200, height: 10, rx: 2 }),
      Captain.svg("g", {}, rects, Captain.svg("rect", { x: 0, y: 1, width: Math.max(0, width), height: 0 })));
  }

  function drawTop(card, rows, range) {
    const F = fmt();
    const ranked = rows.slice(0, 10);
    const max = ranked.reduce((peak, row) => Math.max(peak, Number(row.messages) || 0), 0);
    const summary = ranked.length
      ? `Top ${F.formatCount(ranked.length)} hội thoại trong ${range.days} ngày.`
      : `${range.days} ngày. Không có hội thoại trong khoảng này.`;
    const list = ranked.length === 0
      ? Captain.stateBox("Không có hội thoại trong khoảng này", "Đổi nguồn hoặc nới khoảng ngày.")
      : Captain.h("ol", { class: "rank-list" }, ranked.map((row) => Captain.h("li", { class: "rank rank-4" },
        Captain.h("span", { class: "rank-source" }, row.source === "zalo" ? "Zalo" : "TG"),
        Captain.h("span", { class: "rank-title" }, row.title || "Untitled"),
        Captain.hbar(row.messages, max, row.source === "zalo" ? "zalo" : "tg"),
        Captain.h("span", { class: "num" }, F.formatCount(row.messages)))));
    Captain.replace(card,
      Captain.h("h2", {}, "Top hội thoại theo khối lượng"),
      Captain.h("p", { class: "muted" }, summary),
      list,
      ranked.length ? Captain.h("div", { class: "legend" },
        Captain.h("span", {}, Captain.h("span", { class: "swatch s1", "aria-hidden": "true" }), "Telegram"),
        Captain.h("span", {}, Captain.h("span", { class: "swatch s2", "aria-hidden": "true" }), "Zalo")) : null);
  }

  function drawResponse(card, rows, range) {
    const F = fmt();
    const samples = rows.reduce((sum, row) => sum + (Number(row.sample) || 0), 0);
    const summary = rows.length
      ? `p50 theo ngày UTC, ${range.days} ngày, n = ${F.formatCount(samples)}. Điểm rỗng là ngày dưới ${F.LOW_SAMPLE_MIN} phản hồi. Ô xám là cuối tuần. Mũi tên chuyển ngày.`
      : "Chưa có chuỗi phản hồi.";
    Captain.replace(card,
      Captain.h("h2", {}, "Thời gian phản hồi theo ngày"),
      Captain.h("p", { class: "muted", id: "analytics-response-summary" }, summary));
    if (!rows.length) {
      card.append(Captain.stateBox("Chưa có phản hồi trong khoảng này", ""));
      return;
    }
    const host = Captain.h("div", { class: "chart-card flat" });
    card.append(host);
    const W = Math.max(280, host.clientWidth || card.clientWidth - 8);
    const H = 220;
    const padL = 36;
    const padR = 12;
    const padT = 12;
    const padB = 28;
    const values = rows.map((row) => Number(row.p50_minutes)).filter((value) => Number.isFinite(value));
    const max = Math.max(1, ...values, 0);
    const step = (W - padL - padR) / Math.max(1, rows.length);
    const y = (value) => H - padB - (value / max) * (H - padB - padT);
    const x = (index) => padL + index * step + step / 2;
    const tip = Captain.h("div", { class: "chart-tip", hidden: true });
    const wrap = Captain.h("div", { class: "chart-wrap" });
    const bands = rows.map((row, index) => F.isWeekendDay(row.day)
      ? Captain.svg("rect", { class: "weekend", x: padL + index * step, y: padT, width: step, height: H - padB - padT })
      : null);
    const paths = [];
    let segment = [];
    const flush = () => {
      if (segment.length > 1) paths.push(Captain.svg("polyline", { class: "line", points: segment.join(" ") }));
      segment = [];
    };
    rows.forEach((row, index) => {
      if (row.p50_minutes === null || row.p50_minutes === undefined) flush();
      else segment.push(`${x(index)},${y(Number(row.p50_minutes))}`);
    });
    flush();
    const dots = rows.map((row, index) => {
      const missing = row.p50_minutes === null || row.p50_minutes === undefined;
      const text = missing
        ? `${F.formatUtcDay(row.day, true)}: chưa có phản hồi`
        : `${F.formatUtcDay(row.day, true)}: ${F.formatMinutes(row.p50_minutes)}, n = ${F.formatCount(row.sample)}${row.low_sample ? ", mẫu nhỏ" : ""}`;
      const cy = missing ? y(0) : y(Number(row.p50_minutes));
      return Captain.svg("g", {
        class: "bar-col",
        tabindex: index === rows.length - 1 ? "0" : "-1",
        role: "img",
        "aria-label": text,
        onmouseenter: () => { tip.textContent = text; Captain.placeTip(wrap, tip, x(index), missing ? padT + 12 : cy, W); },
        onmouseleave: () => { tip.hidden = true; },
        onfocus: () => { tip.textContent = text; Captain.placeTip(wrap, tip, x(index), missing ? padT + 12 : cy, W); },
        onblur: () => { tip.hidden = true; },
        onkeydown: (event) => Captain.rove(event, [...wrap.querySelectorAll(".bar-col")], index),
      },
      Captain.svg("rect", { class: "hit", x: padL + index * step, y: padT, width: step, height: H - padB - padT }),
      missing ? null : Captain.svg("circle", { class: row.low_sample ? "point point-low" : "point", cx: x(index), cy, r: 4 }));
    });
    const ticks = [0, Math.round(max / 2), max].filter((value, index, all) => all.indexOf(value) === index);
    const axis = ticks.map((value) => [
      Captain.svg("line", { class: "axis", x1: padL, x2: W - padR, y1: y(value), y2: y(value) }),
      Captain.svg("text", { class: "tick", x: padL - 6, y: y(value) + 4, "text-anchor": "end" }, F.formatCount(value)),
    ]);
    const labels = rows.map((row, index) => (rows.length <= 16 || index % Math.ceil(rows.length / 8) === 0)
      ? Captain.svg("text", { class: "tick", x: x(index), y: H - 8, "text-anchor": "middle" }, F.formatUtcDay(row.day))
      : null);
    const chart = Captain.svg("svg", {
      class: "chart",
      viewBox: `0 0 ${W} ${H}`,
      role: "group",
      "aria-label": summary,
    }, bands, axis, paths, labels, dots);
    Captain.append(wrap, [chart, tip]);
    host.replaceChildren(wrap, Captain.h("div", { class: "legend" },
      Captain.h("span", {}, Captain.h("span", { class: "swatch s1", "aria-hidden": "true" }), `từ ${F.LOW_SAMPLE_MIN} phản hồi`),
      Captain.h("span", {}, Captain.h("span", { class: "swatch hollow", "aria-hidden": "true" }), "dưới 5 phản hồi"),
      Captain.h("span", {}, Captain.h("span", { class: "swatch weekend-swatch", "aria-hidden": "true" }), "cuối tuần")));
    if (values.length === 0) host.append(Captain.h("p", { class: "muted" }, "Chưa có phản hồi trong khoảng này."));
  }

  Captain.registerView("analytics", loadAnalytics);
})();
