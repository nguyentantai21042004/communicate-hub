import { mkdtempSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import { afterEach, describe, expect, it, vi } from "vitest";

import { ZaloBridge, type BridgeDeps } from "../src/zalo/adapter.js";
import type { ZaloApi, ZaloListener, ZaloSendResult, ZaloSession } from "../src/zalo/api.js";
import { enforceCredentialsMode } from "../src/zalo/client.js";
import type { ZaloWireMessage } from "../src/zalo/wire.js";

interface Emitted {
  event: string;
  data: unknown;
}

class FakeListener implements ZaloListener {
  readonly handlers = new Map<string, Array<(payload: never) => void>>();
  started = 0;
  stopped = 0;

  start(): void {
    this.started += 1;
  }

  stop(): void {
    this.stopped += 1;
  }

  on(event: string, handler: (payload: never) => void): void {
    const list = this.handlers.get(event) ?? [];
    list.push(handler);
    this.handlers.set(event, list);
  }

  once(event: string, handler: (...args: never[]) => void): void {
    this.on(event, handler as (payload: never) => void);
  }

  emit(event: string, payload: unknown): void {
    for (const handler of this.handlers.get(event) ?? []) handler(payload as never);
  }
}

class FakeSession implements ZaloSession {
  readonly listener = new FakeListener();
  loginCalls = 0;
  failOnCall = 0;
  credentials = true;
  cancelled = 0;
  private loggedIn = false;
  readonly api: ZaloApi;
  qr: (hooks: Parameters<ZaloSession["loginWithQr"]>[0]) => Promise<ZaloApi>;

  constructor() {
    const listener = this.listener;
    this.api = {
      listener,
      async sendMessage(): Promise<ZaloSendResult> {
        return { msgId: "m-sent", cliMsgId: "c-sent" };
      },
      async uploadAttachment(): Promise<ZaloSendResult> {
        return { msgId: "m-file" };
      },
      async addReaction() {
        return {};
      },
      async undo() {
        return {};
      },
      async getStickersDetail() {
        return [];
      },
      async getAllFriends() {
        return [];
      },
      async getAllGroups() {
        return {};
      },
      async getGroupInfo() {
        return {};
      },
      async getUserInfo() {
        return {};
      },
      async findUser() {
        return {};
      },
      async sendFriendRequest() {
        return {};
      },
      async acceptFriendRequest() {
        return {};
      },
      async getFriendRecommendations() {
        return {};
      },
      async joinGroupLink() {
        return {};
      },
      async leaveGroup() {
        return {};
      },
    };
    this.qr = async (hooks) => {
      await hooks.onQrCode(Buffer.from("png-bytes"));
      return this.api;
    };
  }

  hasCredentials(): boolean {
    return this.credentials;
  }

  current(): ZaloApi | undefined {
    return this.loggedIn ? this.api : undefined;
  }

  require(): ZaloApi {
    if (!this.current()) throw new Error("not logged in");
    return this.api;
  }

  reset(): void {
    this.listener.stop();
  }

  async loginWithSavedCredentials(): Promise<ZaloApi> {
    this.loginCalls += 1;
    if (this.loginCalls === this.failOnCall) throw new Error("auth");
    this.loggedIn = true;
    return this.api;
  }

  loginWithQr(hooks: Parameters<ZaloSession["loginWithQr"]>[0]): Promise<ZaloApi> {
    return this.qr(hooks).then((api) => {
      this.loggedIn = true;
      return api;
    });
  }

  cancelQr(): void {
    this.cancelled += 1;
  }
}

function bridgeFor(session: FakeSession, events: Emitted[]): ZaloBridge {
  const deps: BridgeDeps = {
    session,
    members: { async list() { return []; } },
    emit: (event, data) => events.push({ event, data }),
    now: () => 1_700_000_000_000,
  };
  return new ZaloBridge(deps);
}

const dirs: string[] = [];

afterEach(() => {
  vi.useRealTimers();
  for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

describe("adapter session", () => {
  it("stays up without credentials and does not reconnect a normal close", async () => {
    vi.useFakeTimers();
    const events: Emitted[] = [];
    const session = new FakeSession();
    session.credentials = false;
    const bridge = bridgeFor(session, events);
    await bridge.start();
    expect(bridge.health()).toMatchObject({ listener: "down", session: "disconnected", detail: "not_logged_in" });

    session.credentials = true;
    const logged: Emitted[] = [];
    const live = new FakeSession();
    const loggedIn = bridgeFor(live, logged);
    await loggedIn.start();
    expect(live.loginCalls).toBe(1);
    live.listener.emit("disconnected", 1000);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(live.loginCalls).toBe(1);
    expect(logged.some((event) => event.event === "listener" && (event.data as { reason?: string }).reason === "close_1000")).toBe(true);
  });

  it("backs off after an unintended close and resets after success", async () => {
    vi.useFakeTimers();
    const events: Emitted[] = [];
    const session = new FakeSession();
    session.failOnCall = 2;
    const bridge = bridgeFor(session, events);
    await bridge.start();
    session.listener.emit("disconnected", 1006);
    expect(events.map((event) => event.event)).toEqual(expect.arrayContaining(["listener", "session"]));
    expect(events.some((event) => (event.data as { state?: string }).state === "disconnected")).toBe(true);
    expect(events.some((event) => (event.data as { state?: string }).state === "reconnecting")).toBe(true);
    await vi.advanceTimersByTimeAsync(4_999);
    expect(session.loginCalls).toBe(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(session.loginCalls).toBe(2);
    expect(events.some((event) => (event.data as { state?: string }).state === "expired")).toBe(true);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(session.loginCalls).toBe(3);
    expect(bridge.health().session).toBe("valid");
  });

  it("does not schedule a retry when the first login fails", async () => {
    vi.useFakeTimers();
    const session = new FakeSession();
    session.failOnCall = 1;
    const bridge = bridgeFor(session, []);
    await bridge.start();
    expect(bridge.health()).toMatchObject({ session: "expired", detail: "auth_failed" });
    await vi.advanceTimersByTimeAsync(5 * 60_000);
    expect(session.loginCalls).toBe(1);
  });

  it("suppresses the echo of a send and still emits a real self message", async () => {
    const events: Emitted[] = [];
    const session = new FakeSession();
    const bridge = bridgeFor(session, events);
    await bridge.start();
    const sent = await bridge.sendText({ conversation: "zalo:dm:u-1", text: "hi" });
    expect(sent.message_id).toBe("m-sent");
    const echo: ZaloWireMessage = {
      type: 0,
      threadId: "u-1",
      isSelf: true,
      data: { content: "hi", msgId: "m-sent", cliMsgId: "c-sent", uidFrom: "me", idTo: "u-1", ts: "1700000000000", msgType: "webchat" },
    };
    session.listener.emit("message", echo);
    await flush();
    expect(events.filter((event) => event.event === "message")).toHaveLength(0);
    session.listener.emit("message", {
      ...echo,
      data: { ...echo.data, msgId: "m-real", cliMsgId: "c-real" },
    });
    await flush();
    const inbound = events.filter((event) => event.event === "message");
    expect(inbound).toHaveLength(1);
    expect((inbound[0]?.data as { from_self?: boolean }).from_self).toBe(true);
  });

  it("returns the first QR and cancels the previous login on a second start", async () => {
    const events: Emitted[] = [];
    const session = new FakeSession();
    let finishFirst: (() => void) | undefined;
    session.qr = (hooks) =>
      new Promise((resolve) => {
        void hooks.onQrCode(Buffer.from("one"));
        finishFirst = () => resolve(session.api);
      });
    const bridge = bridgeFor(session, events);
    const first = await bridge.startQr();
    expect(first.png_base64).toBe(Buffer.from("one").toString("base64"));
    expect(events.some((event) => event.event === "login.qr.state" && (event.data as { state?: string }).state === "waiting")).toBe(true);
    session.qr = async (hooks) => {
      await hooks.onQrCode(Buffer.from("two"));
      return session.api;
    };
    const second = await bridge.startQr();
    expect(second.png_base64).toBe(Buffer.from("two").toString("base64"));
    expect(session.cancelled).toBeGreaterThan(0);
    finishFirst?.();
    await new Promise((resolve) => setTimeout(resolve, 10));
    const dones = events.filter((event) => event.event === "login.qr.state" && (event.data as { state?: string }).state === "done");
    expect(dones).toHaveLength(1);
  });
});

describe("credentials mode", () => {
  it("tightens a file this process owns", () => {
    const dir = mkdtempSync(path.join(tmpdir(), "zalo-cred-"));
    dirs.push(dir);
    const file = path.join(dir, "credentials.json");
    writeFileSync(file, JSON.stringify({ imei: "test-imei", cookie: [], userAgent: "test-agent" }), { mode: 0o644 });
    enforceCredentialsMode(file);
    expect(statSync(file).mode & 0o077).toBe(0);
  });
});

function flush(): Promise<void> {
  return new Promise((resolve) => setImmediate(resolve));
}
