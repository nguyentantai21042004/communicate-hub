"use strict";
(function () {
  const KEY = "captain.telegram";
  let generation = 0;
  let lastSearch = null;
  let showText = false;

  function fmt() { return globalThis.CaptainFormat; }

  function loadTelegram() {
    if (Captain.ui.view !== "telegram") return;
    const token = Captain.ui.loadToken;
    const days = Captain.bindDaysToolbar(document.getElementById("telegram-toolbar"), KEY, loadTelegram);
    const gen = ++generation;
    const body = document.getElementById("telegram-body");
    const status = document.getElementById("telegram-status");
    status.textContent = "Đang tải Telegram";
    Captain.replace(body, Captain.skeleton(4));
    Promise.all([
      Captain.api(`/telegram?days=${days}`),
      Captain.api("/conversations?source=telegram&limit=200").catch(() => ({ rows: [] })),
    ]).then(([page, conversations]) => {
      if (gen !== generation || Captain.ui.view !== "telegram" || !Captain.stillCurrent(token)) return;
      renderTelegram(page, conversations.rows || [], days);
    }).catch((error) => {
      if (gen !== generation || Captain.ui.view !== "telegram" || !Captain.stillCurrent(token)) return;
      status.textContent = "Không tải được Telegram";
      Captain.replace(body, Captain.stateBox("Không tải được Telegram", error.message, "error"));
    });
  }

  function renderTelegram(page, chats, days) {
    const F = fmt();
    const offline = page.session.connection === "offline";
    document.getElementById("telegram-status").textContent = offline
      ? "Daemon đang tắt. Số lấy từ store."
      : `Telegram, ${days} ngày.`;
    const scope = page.scope || {};
    const watched = Number(scope.watched) || 0;
    Captain.replace(document.getElementById("telegram-body"),
      offline ? Captain.h("div", { class: "banner", role: "note" }, "Daemon đang tắt. Uptime và lag là số đã lưu, không phải phiên đang chạy.") : null,
      Captain.h("div", { class: "card-grid-2" }, connectionCard(page, days), sessionCard(page.session)),
      Captain.h("div", { class: "pair-1100" }, triggerCard(page.triggers || [], days), scopeCard(scope, watched, days)),
      Captain.h("div", { class: "pair-1100" }, candidateCard(page.candidates || [], days), senderCard(page.top_senders || [], days)),
      historyCard(chats));
    Captain.ui.redraw = null;
  }

  function connectionCard(page, days) {
    const F = fmt();
    const link = page.connection || {};
    const state = (page.session && page.session.connection) || "offline";
    const tone = { connected: "ok", reconnecting: "warn", unauthorized: "bad", offline: "muted" }[state] || "muted";
    return Captain.h("article", { class: "card" },
      Captain.h("div", { class: "card-head" },
        Captain.h("h2", {}, "Kết nối Telegram"),
        Captain.pill(state, tone)),
      Captain.h("div", { class: "stat-row" },
        stat(F.formatPercent(link.uptime), `uptime ${days} ngày`),
        stat(F.formatCount(link.gaps_unfilled), "gap chưa lấp"),
        stat(F.formatDuration(link.lag_p50_secs), "lag p50"),
        stat(F.formatDuration(link.lag_p99_secs), "lag p99")),
      Captain.h("p", { class: "kpi-sub" }, `Catch-up gần nhất ${F.formatStamp(link.last_catch_up_unix_secs)}. Nối lại ${F.formatCount(link.reconnects_24h)} lần trong 24 giờ.`));
  }

  function sessionCard(session) {
    const row = session || {};
    const storage = { keychain: "macOS Keychain", none: "không lưu" }[row.storage] || row.storage || "-";
    return Captain.h("article", { class: "card" },
      Captain.h("div", { class: "card-head" },
        Captain.h("h2", {}, "Session"),
        Captain.pill(row.connection || "offline", row.connection === "connected" ? "ok" : "muted")),
      Captain.h("dl", { class: "facts" }, [
        ["Tài khoản", row.account || "Chưa đăng nhập"],
        ["Lưu ở", storage],
        ["Kết nối", row.connection || "-"],
      ].map(([name, value]) => [Captain.h("dt", {}, name), Captain.h("dd", {}, value)])),
      Captain.h("p", { class: "muted" }, "Trang này không hiện API hash, auth key, hay chuỗi session."));
  }

  function stat(value, label) {
    return Captain.h("div", { class: "stat" },
      Captain.h("span", { class: "stat-value" }, value),
      Captain.h("span", { class: "stat-label" }, label));
  }

  function triggerCard(triggers, days) {
    const F = fmt();
    const rows = triggers.map((row) => {
      const name = Captain.TRIGGERS[row.trigger] || row.trigger;
      const ratio = row.resolved_ratio === null || row.resolved_ratio === undefined ? "-" : F.formatPercent(row.resolved_ratio);
      const open = row.open ? F.formatCount(row.open) : "-";
      const count = F.formatCount(row.count);
      return {
        tr: Captain.h("tr", {},
          Captain.h("td", {}, name),
          Captain.h("td", {}, row.route || "-"),
          Captain.h("td", { class: "num" }, count),
          Captain.h("td", { class: "num" }, open),
          Captain.h("td", { class: "num" }, ratio)),
        card: Captain.labeledCard([
          ["Trigger", name],
          ["Route", row.route || "-"],
          ["Ticket", count],
          ["Còn mở", open],
          ["Resolved", ratio],
        ]),
      };
    });
    return Captain.h("section", { class: "section", "aria-labelledby": "h-triggers" },
      Captain.h("h2", { id: "h-triggers" }, "Trigger"),
      Captain.h("p", { class: "muted" }, `${days} ngày, chỉ chat trong scope. Resolved là tỉ lệ đã đóng.`),
      rows.length
        ? Captain.splitView(Captain.table([["Trigger"], ["Route"], ["Ticket", "num"], ["Còn mở", "num"], ["Resolved", "num"]], rows.map((row) => row.tr), true), rows.map((row) => row.card))
        : Captain.stateBox("Chưa có trigger", ""));
  }

  function scopeCard(scope, watched, days) {
    const F = fmt();
    const steps = [
      ["Chat theo dõi", scope.watched],
      ["Có tin trong khoảng", scope.with_messages],
      ["Có ticket trong khoảng", scope.with_tickets],
      ["Còn ticket mở", scope.with_open_tickets],
    ];
    return Captain.h("article", { class: "card" },
      Captain.h("h2", {}, "Phạm vi theo dõi"),
      Captain.h("p", { class: "muted" }, `${days} ngày. Mẫu là ${F.formatCount(watched)} chat trong scope.toml.`),
      Captain.h("div", { class: "rank-list" }, steps.map(([label, value]) => Captain.h("div", { class: "rank" },
        Captain.h("span", { class: "rank-title" }, label),
        Captain.hbar(value, watched || 1, "tg"),
        Captain.h("span", { class: "num" }, F.formatCount(value))))),
      Captain.h("p", {}, Captain.h("a", { class: "text-link", href: "#scope" }, "Mở Scope để sửa danh sách")));
  }

  function senderCard(senders, days) {
    const F = fmt();
    const rows = senders.slice(0, 10);
    const max = rows.reduce((peak, row) => Math.max(peak, Number(row.count) || 0), 0);
    const list = rows.length === 0
      ? Captain.stateBox("Chưa có tên người gửi", "Store chưa gom tên người gửi.")
      : Captain.h("ol", { class: "rank-list" }, rows.map((row, index) => Captain.h("li", { class: "rank rank-4" },
        Captain.h("span", { class: "num" }, String(index + 1)),
        Captain.h("span", { class: "rank-title" }, row.name || "Không tên"),
        Captain.hbar(row.count, max, "tg"),
        Captain.h("span", { class: "num" }, F.formatCount(row.count)))));
    return Captain.h("article", { class: "card" },
      Captain.h("h2", {}, "Người gửi nhiều nhất"),
      Captain.h("p", { class: "muted" }, `${days} ngày, trong scope. Chỉ tên và số tin.`),
      list);
  }

  function candidateCard(candidates, days) {
    const F = fmt();
    const rows = candidates.map((row) => {
      const button = Captain.h("button", {
        type: "button",
        class: "btn btn-quiet btn-sm",
        "aria-label": `Sao chép mục scope cho ${row.title || "chat"}`,
        onclick: () => copySnippet(button, row.snippet || ""),
      }, "Sao chép [[chat]]");
      const would = F.formatCount(row.would_trigger);
      const messages = F.formatCount(row.messages);
      const cardButton = Captain.h("button", {
        type: "button",
        class: "btn btn-quiet btn-sm",
        "aria-label": `Sao chép mục scope cho ${row.title || "chat"}`,
        onclick: () => copySnippet(cardButton, row.snippet || ""),
      }, "Sao chép [[chat]]");
      return {
        tr: Captain.h("tr", {},
          Captain.h("td", {}, row.title || "Untitled"),
          Captain.h("td", { class: "num" }, would),
          Captain.h("td", { class: "num" }, messages),
          Captain.h("td", { class: "col-action" }, button)),
        card: Captain.labeledCard([
          ["Chat", row.title || "Untitled"],
          ["Would trigger", would],
          ["Messages", messages],
          ["Sao chép", cardButton],
        ]),
      };
    });
    return Captain.h("section", { class: "section", "aria-labelledby": "h-tg-candidates" },
      Captain.h("h2", { id: "h-tg-candidates" }, "Ứng viên ngoài scope"),
      Captain.h("p", { class: "muted" }, `${days} ngày, không lưu nội dung tin.`),
      rows.length
        ? Captain.splitView(Captain.table([["Chat"], ["Would trigger", "num"], ["Messages", "num"], ["Sao chép", "col-action"]], rows.map((row) => row.tr), true), rows.map((row) => row.card))
        : Captain.stateBox("Không có ứng viên", "Không có chat ngoài scope nào vừa kích hoạt trigger."),
      rows.length ? Captain.h("p", { class: "muted" }, `Hiện ${F.formatCount(rows.length)} ứng viên.`) : null);
  }

  async function copySnippet(button, text) {
    const ok = await Captain.copyText(text, "Đã sao chép. Dán vào scope.toml, rồi bấm Reload files ở tab Scope.");
    const next = button.nextElementSibling;
    if (next && next.classList.contains("snippet")) next.remove();
    if (!ok) button.after(Captain.h("pre", { class: "snippet" }, text));
  }

  function historyCard(chats) {
    const form = Captain.h("form", { class: "filters", id: "history-form" });
    const chatOptions = chats.map((row) => Captain.h("option", { value: row.external_id }, `${row.title || "Untitled"} (${row.external_id})`));
    const chatField = chats.length
      ? Captain.h("label", {}, "Chat", Captain.h("select", { name: "chat" }, chatOptions))
      : Captain.h("label", {}, "Chat id", Captain.h("input", { name: "chat", inputmode: "numeric", "aria-label": "Chat id Telegram" }));
    const regex = Captain.h("label", {}, "Regex", Captain.h("input", {
      type: "text",
      name: "regex",
      placeholder: "biểu thức",
      "aria-label": "Biểu thức cần tìm",
      autocomplete: "off",
    }));
    const limit = Captain.h("label", {}, "Quét", Captain.h("select", { name: "scan_limit" },
      [["1000", "1.000 tin gần nhất"], ["3000", "3.000 tin gần nhất"], ["20000", "20.000 tin gần nhất"]].map(([value, text]) => Captain.h("option", { value, selected: value === "3000" }, text))));
    const submit = Captain.h("button", { type: "submit", class: "btn" }, "Tìm");
    const toggle = Captain.h("button", {
      type: "button",
      class: "btn btn-quiet",
      id: "history-text",
      lang: "en",
      disabled: true,
      "aria-pressed": "false",
    }, "Show text");
    Captain.append(form, [chatField, regex, limit, submit, toggle]);
    const results = Captain.h("div", { id: "history-results" });
    const note = Captain.h("p", { class: "muted", id: "history-note" }, "Nội dung giữ ẩn cho tới khi bấm Show text. Mỗi lần quét tối đa 20.000 tin.");
    form.addEventListener("submit", (event) => {
      event.preventDefault();
      showText = false;
      runHistory(form, results, note, toggle);
    });
    toggle.addEventListener("click", () => {
      if (!lastSearch) return;
      showText = !showText;
      runHistory(form, results, note, toggle);
    });
    return Captain.h("section", { class: "section", "aria-labelledby": "h-history" },
      Captain.h("h2", { id: "h-history" }, "Tìm lịch sử chat"),
      Captain.h("p", { class: "muted" }, "Đọc thẳng từ Telegram qua daemon. Không đánh dấu đã đọc."),
      form,
      note,
      results);
  }

  function chatValue(form) {
    const raw = String(form.elements.chat.value || "").trim();
    if (!/^-?\d+$/.test(raw)) return null;
    const value = Number(raw);
    return Number.isSafeInteger(value) ? value : null;
  }

  async function runHistory(form, results, note, toggle) {
    const F = fmt();
    const chat = chatValue(form);
    const regex = form.elements.regex.value.trim();
    const scanLimit = Number(form.elements.scan_limit.value) || 3000;
    if (chat === null) {
      note.textContent = "Chọn một chat Telegram, hoặc nhập id dạng số.";
      return;
    }
    if (!regex) {
      note.textContent = "Nhập biểu thức cần tìm.";
      return;
    }
    note.textContent = "Đang quét lịch sử.";
    Captain.replace(results, Captain.skeleton(3));
    lastSearch = { chat, regex, scanLimit };
    try {
      const page = await Captain.api("/history", {
        method: "POST",
        body: JSON.stringify({ chat, regex, scan_limit: scanLimit, text: showText }),
      });
      toggle.disabled = false;
      toggle.textContent = showText ? "Hide text" : "Show text";
      toggle.setAttribute("aria-pressed", showText ? "true" : "false");
      renderHistory(page, results, note);
    } catch (error) {
      note.textContent = error.message || "Không quét được lịch sử";
      Captain.replace(results, Captain.stateBox("Không quét được lịch sử", error.message, "error"));
    }
  }

  function renderHistory(page, results, note) {
    const F = fmt();
    const matches = page.matches || [];
    const more = page.more_before ? " Còn tin cũ hơn." : "";
    note.textContent = `Đã quét ${F.formatCount(page.scanned)} tin, ${F.formatCount(matches.length)} khớp.${more}`;
    if (!matches.length) {
      Captain.replace(results, Captain.stateBox("Không có khớp", "Thử biểu thức khác, hoặc tăng số tin quét."));
      return;
    }
    const rows = matches.map((match) => {
      const link = F.safeLink(match.link);
      const text = showText && typeof match.text === "string" ? (match.text || "(no text)") : "Text hidden";
      const when = F.formatStamp(match.date_unix, true);
      const sender = match.sender || "-";
      const id = `#${match.id}`;
      const linkNode = link
        ? Captain.h("a", { class: "text-link", href: link, target: "_blank", rel: "noreferrer noopener" }, "Mở t.me")
        : "Không có link";
      return {
        tr: Captain.h("tr", {},
          Captain.h("td", {}, when),
          Captain.h("td", {}, sender),
          Captain.h("td", { class: "mono" }, id),
          Captain.h("td", {}, text),
          Captain.h("td", {}, linkNode instanceof Node ? linkNode : linkNode)),
        card: Captain.labeledCard([
          ["Thời điểm", when],
          ["Người gửi", sender],
          ["Tin", id],
          ["Nội dung", text],
          ["Link", link
            ? Captain.h("a", { class: "text-link", href: link, target: "_blank", rel: "noreferrer noopener" }, "Mở t.me")
            : "Không có link"],
        ]),
      };
    });
    Captain.replace(results, Captain.splitView(
      Captain.table([["Thời điểm"], ["Người gửi"], ["Tin"], ["Nội dung"], ["Link"]], rows.map((row) => row.tr), true),
      rows.map((row) => row.card)));
  }

  Captain.registerView("telegram", loadTelegram);
})();
