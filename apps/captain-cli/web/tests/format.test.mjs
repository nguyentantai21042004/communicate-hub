import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { mock, test } from "node:test";

import {
  LOW_SAMPLE_MIN,
  SEARCH_DEBOUNCE_MS,
  WEEKDAYS,
  bucketDaily,
  debounce,
  formatAge,
  formatCount,
  formatDuration,
  formatMinutes,
  formatPercent,
  formatShare,
  heatmapGrid,
  heatmapLevel,
  isReadOnly,
  isWeekendDay,
  moduleHealthRatio,
  edgeFade,
  newestAttention,
  relativeTime,
  safeLink,
  sparklineScale,
  sumBy,
  topicMatches,
  topicMute,
} from "../format.mjs";

function fixture(name) {
  return JSON.parse(readFileSync(new URL(`./fixtures/${name}.json`, import.meta.url), "utf8"));
}

test("isReadOnly is true only when status.read_only is the boolean true", () => {
  assert.equal(isReadOnly(null), false);
  assert.equal(isReadOnly(undefined), false);
  assert.equal(isReadOnly({}), false);
  assert.equal(isReadOnly({ read_only: false }), false);
  assert.equal(isReadOnly({ read_only: "true" }), false);
  assert.equal(isReadOnly({ read_only: true }), true);
});

test("relative time uses Vietnamese units and a short just-now window", () => {
  const now = 1_700_000_000;
  assert.equal(relativeTime(null, now), "chưa thấy");
  assert.equal(relativeTime(now, now), "vừa xong");
  assert.equal(relativeTime(now - 4, now), "vừa xong");
  assert.equal(relativeTime(now - 12, now), "12 giây trước");
  assert.equal(relativeTime(now - 60, now), "1 phút trước");
  assert.equal(relativeTime(now - 3600, now), "1 giờ trước");
  assert.equal(relativeTime(now - 86400 * 2, now), "2 ngày trước");
  assert.equal(relativeTime(now + 30, now), "sau 30 giây");
  assert.equal(relativeTime("nope", now), "chưa thấy");
});

test("age and counts stay tabular and Vietnamese", () => {
  assert.equal(formatAge(47), "47 giây");
  assert.equal(formatAge(12 * 60), "12 phút");
  assert.equal(formatAge(91 * 60), "1 giờ 31 phút");
  assert.equal(formatAge(2 * 3600 + 5 * 60), "2 giờ 05 phút");
  assert.equal(formatCount(1284), "1.284");
  assert.equal(formatCount(0), "0");
  assert.equal(formatCount(null), "-");
  assert.equal(formatMinutes(18), "18 phút");
  assert.equal(formatMinutes(0.5), "30 giây");
  assert.equal(formatMinutes(null), "-");
  assert.equal(formatPercent(0.99), "99%");
  assert.equal(formatPercent(null), "-");
  assert.equal(formatShare(4, 1000), "<1%");
  assert.equal(formatShare(412, 3035), "14%");
  assert.equal(formatDuration(1), "1 giây");
  assert.equal(formatDuration(90), "1 phút 30 giây");
  assert.equal(formatDuration(null), "-");
});

test("daily buckets merge sources and stay oldest first", () => {
  const overview = fixture("overview");
  const days = bucketDaily(overview.series);
  assert.deepEqual(days.map((day) => day.day), [20369, 20370]);
  assert.equal(days[0].telegram, 8);
  assert.equal(days[0].zalo, 0);
  assert.equal(days[1].total, 52);
  const other = bucketDaily([{ day: 3, source: "sms", messages: 2 }, { day: "x", messages: 9 }]);
  assert.deepEqual(other, [{ day: 3, telegram: 0, zalo: 0, other: 2, total: 2 }]);
  assert.equal(sumBy(overview.kpi.messages_24h, "messages"), 1284);
});

test("heatmap has five levels and a full 7 by 24 grid", () => {
  assert.equal(heatmapLevel(0, 16), 0);
  assert.equal(heatmapLevel(4, 0), 0);
  assert.equal(heatmapLevel(4, 16), 1);
  assert.equal(heatmapLevel(5, 16), 2);
  assert.equal(heatmapLevel(9, 16), 3);
  assert.equal(heatmapLevel(16, 16), 4);
  assert.equal(heatmapLevel(-3, 16), 0);
  const analytics = fixture("analytics");
  const heat = heatmapGrid(analytics.heatmap);
  assert.equal(heat.grid.length, 7);
  assert.equal(heat.grid[0].length, 24);
  assert.equal(heat.max, 16);
  assert.equal(heat.grid[2][15], 16);
  assert.equal(heat.levels[2][15], 4);
  assert.equal(heat.levels[0][9], 1);
  assert.equal(heat.grid.flat().length, 168);
  assert.equal(WEEKDAYS.length, 7);
});

test("sparkline heights scale to the peak and collapse zeros", () => {
  const flat = sparklineScale([0, 0, 0], 24);
  assert.equal(flat.max, 0);
  assert.deepEqual(flat.heights, [0, 0, 0]);
  const scaled = sparklineScale([1, 2, 4, -5], 20);
  assert.equal(scaled.max, 4);
  assert.deepEqual(scaled.heights, [5, 10, 20, 0]);
  const row = fixture("conversations").rows[0];
  const spark = sparklineScale(row.messages_7d, 28);
  assert.equal(spark.max, 120);
  assert.equal(spark.heights[6], 28);
  assert.equal(spark.heights[1], 0);
  assert.equal(row.messages_7d.length, 7);
});

test("weekend days follow UTC, and attention rows sort newest first", () => {
  assert.equal(isWeekendDay(2), true);
  assert.equal(isWeekendDay(3), true);
  assert.equal(isWeekendDay(4), false);
  const overview = fixture("overview");
  const newest = newestAttention(overview.needs_attention);
  assert.equal(newest[0].title, "Anh Hùng (DM)");
  assert.equal(newest[1].title, "Backend team");
  const ratio = moduleHealthRatio(overview.kpi.module_summary);
  assert.deepEqual(ratio, { ok: 2, degraded: 0, down: 0, unknown: 1, total: 3 });
  assert.equal(LOW_SAMPLE_MIN, 5);
  assert.equal(fixture("analytics").response_p50[1].low_sample, true);
});

test("links reject non-http URLs", () => {
  assert.equal(safeLink("https://t.me/c/1/2"), "https://t.me/c/1/2");
  assert.equal(safeLink("javascript:alert(1)"), "");
  assert.equal(safeLink("data:text/html,hi"), "");
  assert.equal(safeLink(""), "");
  assert.equal(safeLink(null), "");
});

test("debounce waits out the search delay and keeps the last call", () => {
  mock.timers.enable({ apis: ["setTimeout"] });
  try {
    let calls = 0;
    const fn = debounce(() => { calls += 1; }, SEARCH_DEBOUNCE_MS);
    fn();
    fn();
    assert.equal(calls, 0);
    mock.timers.tick(SEARCH_DEBOUNCE_MS - 1);
    assert.equal(calls, 0);
    mock.timers.tick(1);
    assert.equal(calls, 1);
    fn.cancel();
  } finally {
    mock.timers.reset();
  }
});

test("tab fade only marks the side that still hides tabs", () => {
  assert.deepEqual(edgeFade(0, 200, 200), { moreLeft: false, moreRight: false });
  assert.deepEqual(edgeFade(0, 120, 400), { moreLeft: false, moreRight: true });
  assert.deepEqual(edgeFade(40, 120, 400), { moreLeft: true, moreRight: true });
  assert.deepEqual(edgeFade(280, 120, 400), { moreLeft: true, moreRight: false });
});

test("topic search matches name or topic and mute stays hidden without a flag", () => {
  const row = { title: "Nhóm Kho Quận 7", external_id: "8490…8801", topic_id: 12 };
  assert.equal(topicMatches(row, ""), true);
  assert.equal(topicMatches(row, "kho"), true);
  assert.equal(topicMatches(row, "12"), true);
  assert.equal(topicMatches(row, "8801"), true);
  assert.equal(topicMatches(row, "không có"), false);
  assert.equal(topicMute(row), null);
  assert.equal(topicMute({ muted: true }), "muted");
  assert.equal(topicMute({ muted: false }), "open");
  assert.equal(topicMute({ mute: 1 }), "muted");
});

test("fixtures match the dashboard response fields the pages read", () => {
  const overview = fixture("overview");
  assert.equal(typeof overview.kpi.needs_attention, "number");
  assert.equal(overview.needs_attention[0].trigger, "mention");
  const conversations = fixture("conversations");
  assert.equal(conversations.total, 2);
  assert.equal(conversations.rows[1].external_id.includes("…") || conversations.rows[1].external_id.includes("..."), true);
  const telegram = fixture("telegram");
  assert.equal(telegram.triggers.length, 4);
  assert.equal(telegram.triggers[0].trigger, "dm_all");
  assert.equal(telegram.session.storage, "keychain");
  assert.equal(telegram.top_senders.length, 0);
  const zalo = fixture("zalo");
  assert.equal(zalo.failed_events[0].retryable, true);
  assert.equal(zalo.topics[0].topic_id, 31);
  const down = fixture("zalo-down");
  assert.equal(down.bridge.state, "down");
  assert.equal(down.send_queue.waiting, 0);
  const modules = fixture("modules");
  assert.equal(modules.modules[0].module, "captain-daemon");
  const events = fixture("events");
  assert.equal(events.events[0].kind, "relogin");
  const history = fixture("history");
  assert.equal(history.matches[0].text, "hidden until asked");
  assert.equal(history.more_before, true);
  const analytics = fixture("analytics");
  assert.equal(analytics.funnel.draft, "not available in P1");
  assert.equal(analytics.funnel.resolved.length, 4);
});
