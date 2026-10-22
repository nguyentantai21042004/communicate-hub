import { createCipheriv, createDecipheriv } from "node:crypto";
import { chmodSync, readFileSync, statSync } from "node:fs";

import { logWarn } from "../log.js";
import type { GroupInfoRecord } from "./api.js";

const GROUP_DOMAIN = "https://group-wpa.zaloapp.com";
const PROFILE_DOMAIN = "https://profile-wpa.zaloapp.com";
const API_TYPE = 30;
const API_VERSION = 671;
const PC_UA =
  "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) ZaloPC/23.12.1 Chrome/102.0.5005.167 Electron/19.1.9 Safari/537.36";
const MEMBER_BATCH = 50;
const FILE_MODE = 0o600;
const FETCH_TIMEOUT_MS = 15_000;

export interface AppSession {
  zpw_enk: string;
  imei: string;
  cookies: Array<{ name: string; value: string; domain: string }>;
}

export interface AppMemberApi {
  groupInfo(groupId: string): Promise<GroupInfoRecord | null>;
  memberNames(uids: string[]): Promise<Map<string, string> | null>;
}

/** PC app cipher. The IV is 16 zero bytes, matching the Zalo PC client. */
export function encodeAes(plaintext: string, zpwEnk: string): string {
  const key = Buffer.from(zpwEnk, "base64");
  const iv = Buffer.alloc(16, 0);
  const cipher = createCipheriv(aesCipher(key), key, iv);
  return Buffer.concat([cipher.update(plaintext, "utf8"), cipher.final()]).toString("base64");
}

/** Inverse of {@link encodeAes}. The ciphertext is URI-encoded base64, as on the wire. */
export function decodeAes(ciphertext: string, zpwEnk: string): string {
  const key = Buffer.from(zpwEnk, "base64");
  const iv = Buffer.alloc(16, 0);
  const decipher = createDecipheriv(aesCipher(key), key, iv);
  const ct = Buffer.from(decodeURIComponent(ciphertext), "base64");
  return Buffer.concat([decipher.update(ct), decipher.final()]).toString("utf8");
}

/**
 * Read `app-session.json` when it is mode 0600, or after this process tightens
 * a file it owns. A file that stays world-readable is treated as missing.
 * The session fields are never logged.
 */
export function loadAppSession(filePath: string): AppSession | null {
  let info;
  try {
    info = statSync(filePath);
  } catch {
    return null;
  }
  if (!tightenOwnerOnly(filePath, info.mode, info.uid)) return null;
  try {
    const parsed: unknown = JSON.parse(readFileSync(filePath, "utf8"));
    return parseSession(parsed);
  } catch {
    return null;
  }
}

export function createAppApi(sessionPath: string, fetchImpl: typeof fetch = fetch): AppMemberApi {
  return {
    groupInfo: (groupId) => appGetGroupInfo(sessionPath, groupId, fetchImpl),
    memberNames: (uids) => appGetGroupMembersInfo(sessionPath, uids, fetchImpl),
  };
}

function aesCipher(key: Buffer): string {
  const bits = key.length * 8;
  if (bits === 128) return "aes-128-cbc";
  if (bits === 192) return "aes-192-cbc";
  return "aes-256-cbc";
}

function tightenOwnerOnly(filePath: string, mode: number, uid: number): boolean {
  if ((mode & 0o077) === 0) return true;
  const self = typeof process.getuid === "function" ? process.getuid() : undefined;
  if (self === undefined || self !== uid) return false;
  try {
    chmodSync(filePath, FILE_MODE);
  } catch {
    return false;
  }
  try {
    return (statSync(filePath).mode & 0o077) === 0;
  } catch {
    return false;
  }
}

function parseSession(value: unknown): AppSession | null {
  if (!value || typeof value !== "object") return null;
  const rec = value as Record<string, unknown>;
  if (typeof rec["zpw_enk"] !== "string" || typeof rec["imei"] !== "string") return null;
  if (!Array.isArray(rec["cookies"])) return null;
  const cookies: AppSession["cookies"] = [];
  for (const item of rec["cookies"]) {
    if (!item || typeof item !== "object") return null;
    const cookie = item as Record<string, unknown>;
    if (typeof cookie["name"] !== "string" || typeof cookie["value"] !== "string" || typeof cookie["domain"] !== "string") {
      return null;
    }
    cookies.push({ name: cookie["name"], value: cookie["value"], domain: cookie["domain"] });
  }
  return { zpw_enk: rec["zpw_enk"], imei: rec["imei"], cookies };
}

async function appGetGroupInfo(
  sessionPath: string,
  groupId: string,
  fetchImpl: typeof fetch,
): Promise<GroupInfoRecord | null> {
  const sess = loadAppSession(sessionPath);
  if (!sess) return null;
  const url = `${GROUP_DOMAIN}/api/group/getmg-v2`;
  const body = { gridVerMap: JSON.stringify({ [groupId]: 0 }) };
  const payload = await postEncrypted(url, body, sess, fetchImpl);
  if (!payload || typeof payload !== "object") return null;
  const data = (payload as { data?: { gridInfoMap?: Record<string, GroupInfoRecord> } }).data;
  return data?.gridInfoMap?.[groupId] ?? null;
}

async function appGetGroupMembersInfo(
  sessionPath: string,
  uids: string[],
  fetchImpl: typeof fetch,
): Promise<Map<string, string> | null> {
  const sess = loadAppSession(sessionPath);
  if (!sess) return null;
  const result = new Map<string, string>();
  for (let i = 0; i < uids.length; i += MEMBER_BATCH) {
    const batch = uids.slice(i, i + MEMBER_BATCH);
    const friendPversionMap = batch.map((uid) => (uid.endsWith("_0") ? uid : `${uid}_0`));
    const url = `${PROFILE_DOMAIN}/api/social/group/members`;
    const payload = await postEncrypted(url, { friend_pversion_map: friendPversionMap }, sess, fetchImpl);
    const profiles = profilesOf(payload);
    for (const uid of batch) {
      const key = uid.endsWith("_0") ? uid : `${uid}_0`;
      const profile = profiles?.[key] ?? profiles?.[uid];
      const name = profile?.displayName?.trim() || profile?.zaloName?.trim();
      if (name) result.set(uid, name);
    }
  }
  return result;
}

function profilesOf(payload: unknown): Record<string, { displayName?: string; zaloName?: string }> | undefined {
  if (!payload || typeof payload !== "object") return undefined;
  const data = (payload as { data?: { profiles?: Record<string, { displayName?: string; zaloName?: string }> } }).data;
  return data?.profiles;
}

async function postEncrypted(
  url: string,
  body: unknown,
  sess: AppSession,
  fetchImpl: typeof fetch,
): Promise<unknown> {
  let enc = "";
  try {
    enc = encodeAes(JSON.stringify(body), sess.zpw_enk);
  } catch {
    return null;
  }
  const query = new URL(url);
  query.searchParams.set("zpw_type", String(API_TYPE));
  query.searchParams.set("zpw_ver", String(API_VERSION));
  query.searchParams.set("imei", sess.imei);
  try {
    const response = await fetchImpl(query.toString(), {
      method: "POST",
      headers: {
        "content-type": "application/x-www-form-urlencoded",
        "user-agent": PC_UA,
        cookie: cookieHeader(sess.cookies, url),
      },
      body: `params=${encodeURIComponent(enc)}`,
      signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
    });
    if (!response.ok) return null;
    const json: unknown = await response.json();
    if (!json || typeof json !== "object") return null;
    const rec = json as { error_code?: number; data?: string };
    if (rec.error_code !== 0 || typeof rec.data !== "string") return null;
    return JSON.parse(decodeAes(rec.data, sess.zpw_enk)) as unknown;
  } catch {
    logWarn("app api request failed");
    return null;
  }
}

function cookieHeader(cookies: AppSession["cookies"], url: string): string {
  const hostname = new URL(url).hostname;
  return cookies
    .filter((cookie) => hostname === cookie.domain || hostname.endsWith(cookie.domain))
    .map((cookie) => `${cookie.name}=${cookie.value}`)
    .join("; ");
}
