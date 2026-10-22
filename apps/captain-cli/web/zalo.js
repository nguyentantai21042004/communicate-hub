"use strict";
(function () {
  const KEY = "captain.zalo";
  let generation = 0;
  let topicQuery = "";

  function fmt() { return globalThis.CaptainFormat; }

  const KIND_LABEL = {
    text: "Text",
    photo: "Ảnh",
    image: "Ảnh",
    sticker: "Sticker",
    voice: "Voice",
    audio: "Voice",
    video: "Video",
    file: "File",
    link: "Liên kết",
    contact: "Danh thiếp",
    bank: "Bank card",
    poll: "Poll",
    location: "Vị trí",
  };

  function loadZalo() {
    if (Captain.ui.view !== "zalo") return;
    const token = Captain.ui.loadToken;
    const days = Captain.bindDaysToolbar(document.getElementById("zalo-toolbar"), KEY, loadZalo);
    const gen = ++generation;
    const status = document.getElementById("zalo-status");
    const body = document.getElementById("zalo-body");
    status.textContent = "Đang tải Zalo";
    Captain.replace(body, Captain.skeleton(4));
    Promise.all([
      Captain.api(`/zalo?days=${days}`),
      Captain.api("/conversations?source=zalo&limit=200").catch(() => ({ rows: [] })),
    ]).then(([page, conversations]) => {
      if (gen !== generation || Captain.ui.view !== "zalo" || !Captain.stillCurrent(token)) return;
      renderZalo(page, conversations.rows || [], days);
    }).catch((error) => {
      if (gen !== generation || Captain.ui.view !== "zalo" || !Captain.stillCurrent(token)) return;
      status.textContent = "Không tải được Zalo";
      Captain.replace(body, Captain.stateBox("Không tải được Zalo", error.message, "error"));
    });
  }

  function down(page) {
    return page.bridge && page.bridge.state === "down";
  }

  function emptyText(page, healthy) {
    return down(page) ? "Bridge tắt, chưa đọc được dòng nào." : healthy;
  }

  function renderZalo(page, chats, days) {
    const F = fmt();
    const isDown = down(page);
    document.getElementById("zalo-status").textContent = isDown
      ? "Zalo bridge tắt."
      : `Zalo, khoảng ${days} ngày.`;
    const byId = new Map(chats.map((row) => [row.external_id, row]));
    Captain.replace(document.getElementById("zalo-body"),
      isDown ? Captain.h("div", { class: "banner banner-bad", role: "alert" }, `Zalo bridge tắt. ${page.bridge.detail || "Chưa kết nối bridge."} Số bên dưới là lần cuối đọc được.`) : null,
      Captain.h("div", { class: "card-grid-2" }, sessionCard(page), queueCard(page)),
      topicCard(page, byId),
      Captain.h("div", { class: "layout-2" }, kindsCard(page), failedCard(page)),
      Captain.h("div", { class: "layout-2" }, friendsCard(page), groupsCard(page)));
    Captain.ui.redraw = null;
  }

  function sessionCard(page) {
    const F = fmt();
    const session = page.session || {};
    const state = session.state || "unknown";
    const tone = { valid: "ok", expired: "bad", logged_out: "bad", connecting: "warn", unknown: "muted" }[state] || "muted";
    const label = { valid: "Hợp lệ", expired: "Hết hạn", logged_out: "Đã đăng xuất", connecting: "Đang kết nối", unknown: "Chưa rõ" }[state] || state;
    const storage = { keychain: "macOS Keychain", none: "không lưu" }[session.storage] || session.storage || "-";
    const button = Captain.readOnly() ? null : Captain.h("button", {
      type: "button",
      class: "btn btn-quiet",
      onclick: (event) => Captain.reloginZalo(event.currentTarget),
    }, "Đăng nhập lại Zalo (QR)");
    return Captain.h("article", { class: "card" },
      Captain.h("div", { class: "card-head" },
        Captain.h("h2", {}, "Phiên Zalo"),
        Captain.pill(label, tone)),
      Captain.h("dl", { class: "facts" }, [
        ["Tài khoản", session.account || "Chưa đăng nhập"],
        ["Lưu ở", storage],
        ["Trạng thái", label],
      ].map(([name, value]) => [Captain.h("dt", {}, name), Captain.h("dd", {}, value)])),
      Captain.h("p", { class: "muted" }, "Không hiện cookie, token, hay chuỗi session."),
      button,
      button && down(page) ? Captain.h("p", { class: "muted" }, "Nút này sẽ báo chưa kết nối bridge khi bridge tắt.") : null);
  }

  function queueCard(page) {
    const F = fmt();
    const queue = page.send_queue || {};
    const wait = queue.retry_after_secs === null || queue.retry_after_secs === undefined
      ? "không có hạn chờ"
      : `retry_after ${F.formatDuration(queue.retry_after_secs)}`;
    return Captain.h("article", { class: "card" },
      Captain.h("div", { class: "card-head" },
        Captain.h("h2", {}, "Hàng đợi gửi"),
        Captain.pill(down(page) ? "Tắt" : "Đang chạy", down(page) ? "bad" : "ok")),
      Captain.h("div", { class: "stat-row" },
        stat(F.formatCount(queue.waiting), "tin đang chờ"),
        stat(F.formatCount(queue.rate_limits_24h), "lỗi 429 trong 24 giờ")),
      Captain.h("p", { class: "kpi-sub" }, wait),
      Captain.h("p", { class: "muted" }, page.bridge && page.bridge.detail ? page.bridge.detail : "Hàng đợi là tin Zalo chờ gửi đi."));
  }

  function stat(value, label) {
    return Captain.h("div", { class: "stat" },
      Captain.h("span", { class: "stat-value" }, value),
      Captain.h("span", { class: "stat-label" }, label));
  }

  function topicCard(page, byId) {
    const F = fmt();
    const input = Captain.h("input", {
      type: "search",
      value: topicQuery,
      placeholder: "Tên hoặc topic",
      "aria-label": "Lọc theo tên hoặc topic",
      autocomplete: "off",
    });
    const list = Captain.h("div", { id: "zalo-topic-list" });
    const run = F.debounce(() => {
      topicQuery = input.value;
      paintTopics(list, page, byId);
    }, F.SEARCH_DEBOUNCE_MS);
    input.addEventListener("input", run);
    paintTopics(list, page, byId);
    return Captain.h("section", { class: "section", "aria-labelledby": "h-topics" },
      Captain.h("h2", { id: "h-topics" }, "Hội thoại Zalo và topic Telegram"),
      Captain.h("p", { class: "muted" }, `${F.formatCount((page.topics || []).length)} dòng mapping. Cột 7 ngày lấy từ danh sách hội thoại khi có.`),
      Captain.h("label", { class: "topic-search" }, "Tìm hội thoại", input),
      list);
  }

  function paintTopics(list, page, byId) {
    const F = fmt();
    const now = Date.now() / 1000;
    const all = page.topics || [];
    const topics = all.filter((topic) => F.topicMatches(topic, topicQuery));
    const showMute = all.some((topic) => F.topicMute(topic));
    if (!topics.length) {
      Captain.replace(list, Captain.stateBox(
        topicQuery.trim() ? "Không có hội thoại nào khớp" : "Chưa có mapping",
        topicQuery.trim() ? "Thử tên hoặc số topic khác." : emptyText(page, "Chưa có hội thoại Zalo nào gắn topic.")));
      return;
    }
    const headers = [["Hội thoại Zalo"], ["Loại"], ["Tin 7 ngày", "spark-col"], ["Lần cuối"], ["Topic Telegram"]];
    if (showMute) headers.push(["Mute trên Zalo"]);
    const built = topics.map((topic) => topicRow(topic, byId.get(topic.external_id), now, showMute));
    Captain.replace(list, Captain.splitView(
      Captain.table(headers, built.map((row) => row.tr), true),
      built.map((row) => row.card)));
  }

  function topicRow(topic, chat, now, showMute) {
    const F = fmt();
    const title = Captain.h("div", {},
      Captain.h("div", {}, topic.title || "Untitled"),
      Captain.h("div", { class: "mono muted" }, topic.external_id || "-"));
    const kind = chat ? Captain.kindLabel(chat.kind) : "-";
    const spark = chat ? Captain.sparkline(chat.messages_7d, "zalo") : "-";
    const seen = chat ? F.relativeTime(chat.last_activity_unix_secs, now) : "-";
    const topicText = topic.topic_id === null || topic.topic_id === undefined ? "chưa có topic" : `topic ${topic.topic_id}`;
    const mute = muteBadge(topic);
    const cells = [
      Captain.h("td", {}, title),
      Captain.h("td", {}, kind),
      Captain.h("td", { class: "spark-col" }, spark),
      Captain.h("td", {}, seen),
      Captain.h("td", { class: "mono" }, topicText),
    ];
    const pairs = [
      ["Hội thoại Zalo", topic.title || "Untitled"],
      ["Id", topic.external_id || "-"],
      ["Loại", kind],
      ["Tin 7 ngày", chat ? Captain.sparkline(chat.messages_7d, "zalo") : "-"],
      ["Lần cuối", seen],
      ["Topic Telegram", topicText],
    ];
    if (showMute) {
      cells.push(Captain.h("td", {}, mute));
      pairs.push(["Mute trên Zalo", muteBadge(topic)]);
    }
    return { tr: Captain.h("tr", {}, cells), card: Captain.labeledCard(pairs) };
  }

  function muteBadge(topic) {
    const flag = fmt().topicMute(topic);
    if (flag === "muted") return Captain.pill("Mute, bị bỏ qua", "warn", true);
    if (flag === "open") return "-";
    return null;
  }

  function kindsCard(page) {
    const F = fmt();
    const kinds = page.message_kinds_14d || [];
    const total = F.sumBy(kinds, "count");
    const max = kinds.reduce((peak, row) => Math.max(peak, Number(row.count) || 0), 0);
    const list = kinds.length === 0
      ? Captain.stateBox("Chưa có tin theo loại", emptyText(page, "Chưa có tin Zalo trong 14 ngày."))
      : Captain.h("div", { class: "rank-list" }, kinds.map((row) => Captain.h("div", { class: "rank" },
        Captain.h("span", { class: "rank-title" }, KIND_LABEL[row.kind] || row.kind || "-"),
        Captain.hbar(row.count, max, "zalo"),
        Captain.h("span", { class: "num" }, `${F.formatCount(row.count)} · ${F.formatShare(row.count, total)}`))));
    return Captain.h("article", { class: "card" },
      Captain.h("h2", {}, "Loại tin"),
      Captain.h("p", { class: "muted" }, "14 ngày, không theo khoảng đã chọn. Không đọc nội dung."),
      list);
  }

  function failedCard(page) {
    const F = fmt();
    const events = page.failed_events || [];
    const rows = events.map((event) => {
      const when = F.formatStamp(event.at_unix_secs, true);
      const reason = event.reason || "-";
      const conversation = event.conversation || "-";
      const button = () => (event.retryable && !Captain.readOnly()
        ? Captain.h("button", {
          type: "button",
          class: "btn btn-quiet btn-sm",
          onclick: (click) => Captain.retryZalo(event.id, click.currentTarget),
        }, "Thử lại")
        : Captain.h("span", { class: "muted" }, event.retryable ? "Chế độ xem" : "Không thử lại được"));
      const desk = button();
      const phone = button();
      return {
        tr: Captain.h("tr", {},
          Captain.h("td", { class: "col-reason" }, reason),
          Captain.h("td", { class: "col-chat" }, conversation),
          Captain.h("td", { class: "col-when" }, when),
          Captain.h("td", { class: "col-action" }, desk)),
        card: Captain.h("article", { class: "row-card fail-card" },
          Captain.h("div", { class: "fail-top" },
            Captain.h("strong", {}, reason),
            Captain.h("span", {}, conversation)),
          Captain.h("div", { class: "fail-bot" },
            Captain.h("span", { class: "num" }, when),
            phone)),
      };
    });
    return Captain.h("article", { class: "card" },
      Captain.h("h2", {}, "Tin lỗi hoặc chưa chuyển"),
      Captain.h("p", { class: "muted" }, `${F.formatCount(events.length)} dòng. Lý do không phải nội dung tin.`),
      rows.length
        ? Captain.splitView(
          Captain.table([["Nguyên nhân", "col-reason"], ["Hội thoại", "col-chat"], ["Lúc", "col-when"], ["Thử lại", "col-action"]], rows.map((row) => row.tr), true),
          rows.map((row) => row.card))
        : Captain.stateBox("Không có tin lỗi", emptyText(page, "Không có lần gửi nào lỗi.")));
  }

  function friendsCard(page) {
    const F = fmt();
    const count = Number(page.pending_friend_requests) || 0;
    return Captain.h("article", { class: "card" },
      Captain.h("h2", {}, "Lời mời kết bạn"),
      Captain.h("div", { class: "stat-value" }, F.formatCount(count)),
      Captain.h("p", { class: "kpi-sub" }, count === 0 ? emptyText(page, "Không có lời mời.") : "Đang chờ duyệt."),
      count > 0 ? Captain.h("p", { class: "muted" }, "Phản hồi chỉ có số lượng, không có tên từng người.") : null);
  }

  function groupsCard(page) {
    const F = fmt();
    const gap = page.groups_without_topic || { count: 0, names: [] };
    const names = gap.names || [];
    const list = names.length
      ? Captain.h("ul", { class: "name-list" }, names.map((name) => Captain.h("li", {}, name)))
      : Captain.stateBox("Không có nhóm thiếu topic", emptyText(page, "Mọi nhóm đã có topic, hoặc chưa đọc được nhóm."));
    const extra = Number(gap.count) > names.length
      ? Captain.h("p", { class: "muted" }, `Hiện ${F.formatCount(names.length)} / ${F.formatCount(gap.count)} tên.`)
      : null;
    return Captain.h("article", { class: "card" },
      Captain.h("h2", {}, "Nhóm Zalo chưa có topic"),
      Captain.h("div", { class: "stat-value" }, F.formatCount(gap.count)),
      list,
      extra);
  }

  Captain.registerView("zalo", loadZalo);
})();
