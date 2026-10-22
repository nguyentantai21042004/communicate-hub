import path from "node:path";

import { z } from "zod";

import { PARAM_SCHEMAS, type MethodName } from "./contract.js";
import { SidecarError } from "./errors.js";
import { acquireListenerLock, releaseListenerLock } from "./lock.js";
import { logError } from "./log.js";
import { runStdio, type Handler } from "./rpc.js";
import { withTimeout } from "./timeout.js";
import { ZaloBridge } from "./zalo/adapter.js";
import { createAppApi } from "./zalo/app-api.js";
import { ZaloClient } from "./zalo/client.js";
import { downloadToMedia, writeMediaBytes } from "./zalo/media.js";
import { GroupMemberCache, GROUP_MEMBER_CACHE_TTL_MS, loadGroupMembers } from "./zalo/members.js";
import { toBuffer } from "qrcode";

const CALL_TIMEOUT_MS = 30_000;
const APP_SESSION_NAME = "app-session.json";

async function main(): Promise<void> {
  const credentialsPath = process.env["ZALO_CREDENTIALS_PATH"];
  if (!credentialsPath) {
    logError("ZALO_CREDENTIALS_PATH is required");
    process.exit(1);
  }
  const dataDir = process.env["ZALO_DATA_DIR"] || path.dirname(credentialsPath);
  try {
    acquireListenerLock(dataDir);
  } catch (err) {
    const message = err instanceof Error ? err.message : "refusing to start: listener lock is held";
    process.stderr.write(`${message}\n`);
    process.exit(1);
  }

  const session = new ZaloClient({ credentialsPath });
  const app = createAppApi(path.join(path.dirname(credentialsPath), APP_SESSION_NAME));
  const members = new GroupMemberCache(
    { load: (groupId) => loadGroupMembers(groupId, session.require(), app) },
    GROUP_MEMBER_CACHE_TTL_MS,
    () => Date.now(),
  );
  const bridge = new ZaloBridge({
    session,
    members,
    emit: (event, data) => {
      process.stdout.write(`${JSON.stringify({ event, data })}\n`);
    },
    now: () => Date.now(),
    normalize: {
      downloadUrl: (url, fileName) => downloadToMedia(dataDir, url, fileName),
      writeQrPng: (payload, fileName) => writeQrPng(dataDir, payload, fileName),
      stickerDetail: async (id) => {
        const api = session.current();
        if (!api) return undefined;
        const details = await api.getStickersDetail([id]);
        const detail = details[0];
        if (!detail) return undefined;
        return {
          webp: stringField(detail, "stickerWebpUrl"),
          still: stringField(detail, "stickerUrl"),
          sprite: stringField(detail, "stickerSpriteUrl"),
        };
      },
      bankHtml: fetchText,
      pollDetail: async (pollId) => {
        const api = session.current();
        if (!api?.getPollDetail) return undefined;
        const detail = await api.getPollDetail(pollId);
        return { question: detail.question, options: detail.options };
      },
    },
  });

  process.on("SIGTERM", () => {
    bridge.stop();
    releaseListenerLock();
    process.exit(0);
  });

  await bridge.start();
  await runStdio(process.stdin, (line) => {
    process.stdout.write(line);
  }, handlers(bridge));
  bridge.stop();
  releaseListenerLock();
}

function handlers(bridge: ZaloBridge): Map<string, Handler> {
  const map = new Map<string, Handler>();
  const bind = <M extends MethodName>(method: M, fn: (params: z.infer<(typeof PARAM_SCHEMAS)[M]>) => Promise<unknown>): void => {
    map.set(method, (params) => withTimeout(fn(parseParams(method, params)), CALL_TIMEOUT_MS));
  };
  bind("health", async () => bridge.health());
  bind("login.qr.start", async () => bridge.startQr());
  bind("login.qr.cancel", async () => {
    bridge.cancelQr();
    return {};
  });
  bind("send.text", (params) => bridge.sendText(params));
  bind("send.media", (params) => bridge.sendMedia(params));
  bind("send.reaction", (params) => bridge.sendReaction(params));
  bind("send.unsend", (params) => bridge.sendUnsend(params));
  bind("directory.friends", async () => bridge.friends());
  bind("directory.groups", async () => bridge.groups());
  bind("directory.group_members", (params) => bridge.groupMembers(params.group_id));
  bind("friend.requests", async () => bridge.friendRequests());
  bind("friend.accept", (params) => bridge.acceptFriend(params.user_id));
  bind("friend.send_request", (params) => bridge.sendFriendRequest(params.user_id, params.phone, params.message));
  bind("group.join", (params) => bridge.joinGroup(params.link));
  bind("group.leave", (params) => bridge.leaveGroup(params.group_id));
  return map;
}

function parseParams<M extends MethodName>(method: M, params: unknown): z.infer<(typeof PARAM_SCHEMAS)[M]> {
  const parsed = PARAM_SCHEMAS[method].safeParse(params);
  if (!parsed.success) throw new SidecarError("internal");
  return parsed.data;
}

async function writeQrPng(dataDir: string, payload: string, fileName: string): Promise<string> {
  const png = await toBuffer(payload, { type: "png", margin: 1 });
  return writeMediaBytes(dataDir, fileName, png);
}

async function fetchText(url: string): Promise<string | undefined> {
  if (!/^https?:\/\//i.test(url)) return undefined;
  const response = await fetch(url);
  if (!response.ok) return undefined;
  return response.text();
}

function stringField(record: Record<string, unknown>, key: string): string | undefined {
  const value = record[key];
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

main().catch((err: unknown) => {
  logError(err instanceof Error ? err.name : "startup failed");
  releaseListenerLock();
  process.exit(1);
});
