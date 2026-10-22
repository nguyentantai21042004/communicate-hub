// Pure dashboard helpers. No DOM, no network. Node tests import this file directly.

export const SEARCH_DEBOUNCE_MS = 250;
export const LOW_SAMPLE_MIN = 5;
export const SECS_PER_DAY = 86400;
export const WEEKDAYS = ["T2", "T3", "T4", "T5", "T6", "T7", "CN"];

const countFormat = new Intl.NumberFormat("vi-VN");
const decimalFormat = new Intl.NumberFormat("vi-VN", { maximumFractionDigits: 1 });

export function relativeTime(unix, now) {
  if (unix === null || unix === undefined || unix === "") return "chưa thấy";
  const then = Number(unix);
  const current = Number(now);
  if (!Number.isFinite(then) || !Number.isFinite(current)) return "chưa thấy";
  let delta = Math.round(current - then);
  const future = delta < 0;
  delta = Math.abs(delta);
  const phrase = (n, unit) => (future ? `sau ${n} ${unit}` : `${n} ${unit} trước`);
  if (delta < 5) return future ? "sắp tới" : "vừa xong";
  if (delta < 60) return phrase(delta, "giây");
  if (delta < 3600) return phrase(Math.floor(delta / 60), "phút");
  if (delta < SECS_PER_DAY) return phrase(Math.floor(delta / 3600), "giờ");
  return phrase(Math.floor(delta / SECS_PER_DAY), "ngày");
}

export function formatAge(secs) {
  const s = Math.max(0, Math.round(Number(secs) || 0));
  if (s < 60) return `${s} giây`;
  const minutes = Math.floor(s / 60);
  if (minutes < 60) return `${minutes} phút`;
  const hours = Math.floor(minutes / 60);
  const rem = minutes % 60;
  if (hours < 48) return rem ? `${hours} giờ ${String(rem).padStart(2, "0")} phút` : `${hours} giờ`;
  return `${Math.floor(hours / 24)} ngày`;
}

export function formatCount(value) {
  if (value === null || value === undefined || value === "") return "-";
  const n = Number(value);
  if (!Number.isFinite(n)) return "-";
  return countFormat.format(n);
}

export function formatMinutes(minutes) {
  if (minutes === null || minutes === undefined || minutes === "") return "-";
  const n = Number(minutes);
  if (!Number.isFinite(n)) return "-";
  if (n < 0) return "-";
  if (n < 1) return `${countFormat.format(Math.round(n * 60))} giây`;
  const rounded = Math.round(n * 10) / 10;
  if (Math.abs(rounded - Math.round(rounded)) < 0.05) return `${countFormat.format(Math.round(rounded))} phút`;
  return `${decimalFormat.format(rounded)} phút`;
}

export function formatPercent(ratio) {
  if (ratio === null || ratio === undefined || ratio === "") return "-";
  const n = Number(ratio);
  if (!Number.isFinite(n)) return "-";
  return `${countFormat.format(Math.round(n * 100))}%`;
}

export function formatShare(count, total) {
  const whole = Number(total);
  const part = Number(count);
  if (!Number.isFinite(whole) || whole <= 0 || !Number.isFinite(part) || part < 0) return "-";
  const pct = (part / whole) * 100;
  if (pct > 0 && pct < 1) return "<1%";
  return `${countFormat.format(Math.round(pct))}%`;
}

export function formatDuration(secs) {
  if (secs === null || secs === undefined || secs === "") return "-";
  const n = Number(secs);
  if (!Number.isFinite(n) || n < 0) return "-";
  if (n >= 3600) {
    const hours = Math.floor(n / 3600);
    const minutes = Math.round((n % 3600) / 60);
    return minutes ? `${countFormat.format(hours)} giờ ${countFormat.format(minutes)} phút` : `${countFormat.format(hours)} giờ`;
  }
  if (n >= 60) {
    const minutes = Math.floor(n / 60);
    const rem = Math.round(n % 60);
    return rem ? `${countFormat.format(minutes)} phút ${countFormat.format(rem)} giây` : `${countFormat.format(minutes)} phút`;
  }
  if (n >= 10) return `${countFormat.format(Math.round(n))} giây`;
  const tenth = Math.round(n * 10) / 10;
  if (Number.isInteger(tenth)) return `${countFormat.format(tenth)} giây`;
  return `${decimalFormat.format(tenth)} giây`;
}

export function formatStamp(unix, withSeconds) {
  const n = Number(unix);
  if (!Number.isFinite(n) || n <= 0) return "-";
  const date = new Date(n * 1000);
  const day = new Intl.DateTimeFormat("vi-VN", { day: "2-digit", month: "2-digit" }).format(date);
  const time = new Intl.DateTimeFormat("vi-VN", {
    hour: "2-digit",
    minute: "2-digit",
    second: withSeconds ? "2-digit" : undefined,
    hourCycle: "h23",
  }).format(date);
  return `${day} ${time}`;
}

export function formatUtcDay(day, withMonth) {
  const n = Number(day);
  if (!Number.isFinite(n)) return "-";
  const date = new Date(n * SECS_PER_DAY * 1000);
  if (withMonth) {
    return new Intl.DateTimeFormat("vi-VN", { day: "numeric", month: "short", timeZone: "UTC" }).format(date);
  }
  return String(date.getUTCDate());
}

export function isWeekendDay(day) {
  const n = Number(day);
  if (!Number.isFinite(n)) return false;
  const weekday = new Date(n * SECS_PER_DAY * 1000).getUTCDay();
  return weekday === 0 || weekday === 6;
}

export function bucketDaily(series) {
  const byDay = new Map();
  for (const row of series || []) {
    const day = Number(row && row.day);
    if (!Number.isFinite(day)) continue;
    let bucket = byDay.get(day);
    if (!bucket) {
      bucket = { day, telegram: 0, zalo: 0, other: 0, total: 0 };
      byDay.set(day, bucket);
    }
    const messages = Math.max(0, Number(row.messages) || 0);
    if (row.source === "telegram") bucket.telegram += messages;
    else if (row.source === "zalo") bucket.zalo += messages;
    else bucket.other += messages;
    bucket.total += messages;
  }
  return [...byDay.values()].sort((a, b) => a.day - b.day);
}

export function heatmapLevel(value, max) {
  const v = Number(value) || 0;
  const m = Number(max) || 0;
  if (v <= 0 || m <= 0) return 0;
  const ratio = Math.min(1, v / m);
  if (ratio <= 0.25) return 1;
  if (ratio <= 0.5) return 2;
  if (ratio <= 0.75) return 3;
  return 4;
}

export function heatmapGrid(cells) {
  const grid = Array.from({ length: 7 }, () => Array.from({ length: 24 }, () => 0));
  let max = 0;
  for (const cell of cells || []) {
    const weekday = Number(cell && cell.weekday);
    const hour = Number(cell && cell.hour);
    if (!Number.isInteger(weekday) || !Number.isInteger(hour)) continue;
    if (weekday < 0 || weekday > 6 || hour < 0 || hour > 23) continue;
    const messages = Math.max(0, Number(cell.messages) || 0);
    grid[weekday][hour] += messages;
    if (grid[weekday][hour] > max) max = grid[weekday][hour];
  }
  const levels = grid.map((row) => row.map((value) => heatmapLevel(value, max)));
  return { grid, max, levels };
}

export function sparklineScale(values, height) {
  const nums = (values || []).map((value) => Math.max(0, Number(value) || 0));
  const peak = nums.reduce((max, value) => Math.max(max, value), 0);
  const h = Math.max(0, Number(height) || 0);
  const heights = nums.map((value) => (peak === 0 ? 0 : (value / peak) * h));
  return { max: peak, heights };
}

export function sumBy(rows, key) {
  let total = 0;
  for (const row of rows || []) total += Math.max(0, Number(row && row[key]) || 0);
  return total;
}

export function moduleHealthRatio(summary) {
  const ok = Math.max(0, Number(summary && summary.ok) || 0);
  const degraded = Math.max(0, Number(summary && summary.degraded) || 0);
  const down = Math.max(0, Number(summary && summary.down) || 0);
  const unknown = Math.max(0, Number(summary && summary.unknown) || 0);
  return { ok, degraded, down, unknown, total: ok + degraded + down + unknown };
}

export function newestAttention(rows) {
  return (rows || [])
    .map((row, index) => ({ row, index }))
    .sort((a, b) => (Number(a.row && a.row.age_secs) || 0) - (Number(b.row && b.row.age_secs) || 0) || a.index - b.index)
    .map((item) => item.row);
}

export function safeLink(url) {
  if (typeof url !== "string" || !url) return "";
  try {
    const parsed = new URL(url);
    if (parsed.protocol !== "https:" && parsed.protocol !== "http:") return "";
    return parsed.href;
  } catch {
    return "";
  }
}

export function edgeFade(scrollLeft, clientWidth, scrollWidth) {
  const left = Number(scrollLeft) || 0;
  const width = Number(clientWidth) || 0;
  const full = Number(scrollWidth) || 0;
  return {
    moreLeft: left > 4,
    moreRight: left + width < full - 4,
  };
}

export function topicMatches(topic, query) {
  const needle = String(query || "").trim().toLowerCase();
  if (!needle) return true;
  const row = topic || {};
  const hay = [row.title, row.external_id, row.topic_id]
    .filter((value) => value !== null && value !== undefined)
    .join(" ")
    .toLowerCase();
  return hay.includes(needle);
}

export function topicMute(row) {
  if (!row || typeof row !== "object") return null;
  if (Object.prototype.hasOwnProperty.call(row, "muted")) return row.muted ? "muted" : "open";
  if (Object.prototype.hasOwnProperty.call(row, "mute")) return row.mute ? "muted" : "open";
  return null;
}

export function debounce(fn, ms) {
  let timer = null;
  function wrapped(...args) {
    if (timer !== null) clearTimeout(timer);
    timer = setTimeout(() => {
      timer = null;
      fn(...args);
    }, ms);
  }
  wrapped.cancel = () => {
    if (timer !== null) clearTimeout(timer);
    timer = null;
  };
  return wrapped;
}

globalThis.CaptainFormat = {
  SEARCH_DEBOUNCE_MS,
  LOW_SAMPLE_MIN,
  SECS_PER_DAY,
  WEEKDAYS,
  relativeTime,
  formatAge,
  formatCount,
  formatMinutes,
  formatPercent,
  formatShare,
  formatDuration,
  formatStamp,
  formatUtcDay,
  isWeekendDay,
  bucketDaily,
  heatmapLevel,
  heatmapGrid,
  sparklineScale,
  sumBy,
  moduleHealthRatio,
  newestAttention,
  safeLink,
  edgeFade,
  topicMatches,
  topicMute,
  debounce,
};
