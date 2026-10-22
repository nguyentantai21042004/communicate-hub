import { SidecarError } from "../errors.js";
import { withTimeout } from "../timeout.js";
import type {
  ZaloFriendEvent,
  ZaloGroupEvent,
  ZaloReactionEvent,
  ZaloSendResult,
  ZaloSession,
  ZaloUndoEvent,
} from "./api.js";
import { ZaloDirectory } from "./directory.js";
import { parseConversation, realId } from "./ids.js";
import type { DirectoryEntry } from "./members.js";
import {
  normalizeFriendRequest,
  normalizeGroupEvent,
  normalizeReaction,
  normalizeUndo,
  normalizeZaloMessage,
  type NormalizeDeps,
} from "./normalize.js";
import { zaloIconForEmoji } from "./reaction.js";
import { splitText, ZALO_TEXT_LIMIT } from "./text.js";
import type { ZaloWireMessage } from "./wire.js";

const RECONNECT_DELAY_MS = 5_000;
const MAX_RECONNECT_DELAY_MS = 5 * 60_000;
const CLOSE_NORMAL = 1000;
const QR_WAIT_MS = 20_000;

type SessionName = "valid" | "expired" | "disconnected" | "reconnecting";

interface QuoteEnvelope {
  msgId: string;
  cliMsgId: string;
  uidFrom?: string;
  ts?: string;
  msgType?: string;
  content?: unknown;
  ttl: number;
}

export interface SendTextParams {
  conversation: string;
  text: string;
  reply_to?: { message_id: string; cli_msg_id?: string };
  mentions?: Array<{ uid: string; pos: number; len: number }>;
}

export interface SendMediaParams {
  conversation: string;
  path: string;
  caption?: string;
  kind?: string;
}

export interface SendReactionParams {
  conversation: string;
  message_id: string;
  cli_msg_id?: string;
  emoji: string | null;
}

export interface SendUnsendParams {
  conversation: string;
  message_id: string;
  cli_msg_id?: string;
}

export interface BridgeDeps {
  session: ZaloSession;
  members: { list(groupId: string): Promise<DirectoryEntry[]> };
  emit(event: string, data: unknown): void;
  now(): number;
  normalize?: NormalizeDeps;
}

interface HealthBody {
  listener: "up" | "down";
  session: SessionName;
  detail?: string;
}

/**
 * Listener, sends, and QR login. Directory lookups are delegated.
 * A message id this process just sent is not echoed.
 */
export class ZaloBridge {
  private listenerUp = false;
  private sessionState: SessionName = "disconnected";
  private detail = "not_logged_in";
  private stopping = false;
  private reconnectDelayMs = RECONNECT_DELAY_MS;
  private reconnectTimer: ReturnType<typeof setTimeout> | undefined;
  private readonly sentIds = new Set<string>();
  private readonly quotes = new Map<string, QuoteEnvelope>();
  private readonly names = new Map<string, string>();
  private qrRun = 0;
  private readonly directory: ZaloDirectory;

  constructor(private readonly deps: BridgeDeps) {
    this.directory = new ZaloDirectory(deps.session);
  }

  async start(): Promise<void> {
    this.stopping = false;
    if (!this.deps.session.hasCredentials()) {
      this.listenerUp = false;
      this.setSession("disconnected", "not_logged_in");
      return;
    }
    try {
      await this.connect();
    } catch {
      this.listenerUp = false;
      this.setSession("expired", "auth_failed");
    }
  }

  stop(): void {
    this.stopping = true;
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    this.reconnectTimer = undefined;
    try {
      this.deps.session.current()?.listener.stop();
    } catch {
      // Already stopped.
    }
    this.listenerUp = false;
  }

  health(): HealthBody {
    const body: HealthBody = {
      listener: this.listenerUp ? "up" : "down",
      session: this.sessionState,
    };
    if (this.detail) body.detail = this.detail;
    return body;
  }

  async startQr(): Promise<{ png_base64: string }> {
    this.qrRun += 1;
    const run = this.qrRun;
    this.deps.session.cancelQr();
    let resolveFirst: (png: string) => void = () => {};
    let rejectFirst: (err: unknown) => void = () => {};
    const firstImage = new Promise<string>((resolve, reject) => {
      resolveFirst = resolve;
      rejectFirst = reject;
    });
    let delivered = false;
    const login = this.deps.session.loginWithQr({
      onQrCode: (png) => {
        if (run !== this.qrRun) return;
        const encoded = png.toString("base64");
        if (!delivered) {
          delivered = true;
          resolveFirst(encoded);
        }
        this.deps.emit("login.qr.state", { state: "waiting", png_base64: encoded });
      },
      onExpired: () => {
        if (run !== this.qrRun) return;
        this.deps.emit("login.qr.state", { state: "expired" });
      },
      onScanned: (displayName) => {
        if (run !== this.qrRun) return;
        this.deps.emit("login.qr.state", { state: "scanned", display_name: displayName });
      },
      onDeclined: () => {
        if (run !== this.qrRun) return;
        this.deps.emit("login.qr.state", { state: "expired" });
        rejectFirst(new SidecarError("auth_expired"));
      },
    });
    void login.then(
      () => {
        if (run !== this.qrRun) return;
        this.deps.emit("login.qr.state", { state: "done" });
        this.attachListener();
      },
      () => {
        if (run !== this.qrRun || delivered) return;
        rejectFirst(new SidecarError("auth_expired"));
      },
    );
    try {
      const png = await withTimeout(firstImage, QR_WAIT_MS);
      return { png_base64: png };
    } catch (err) {
      if (run === this.qrRun) this.deps.session.cancelQr();
      throw err;
    }
  }

  cancelQr(): void {
    this.qrRun += 1;
    this.deps.session.cancelQr();
  }

  async sendText(params: SendTextParams): Promise<{ message_id: string; message_ids: string[] }> {
    if (!params.text.trim()) throw new SidecarError("unsupported");
    const thread = threadOf(params.conversation);
    const api = this.deps.session.require();
    const ids: string[] = [];
    let messageId = "";
    for (const chunk of splitText(params.text, ZALO_TEXT_LIMIT)) {
      const result = await api.sendMessage(
        {
          msg: chunk,
          quote: this.quoteFor(params.reply_to),
          mentions: params.mentions?.map((mention) => ({
            uid: mention.uid,
            pos: mention.pos,
            len: mention.len,
            type: 0,
          })),
        },
        thread.threadId,
        thread.threadType,
      );
      const sent = this.rememberSent(result);
      messageId = sent.messageId;
      ids.push(...sent.ids);
    }
    if (!messageId) throw new SidecarError("internal");
    return { message_id: messageId, message_ids: ids };
  }

  async sendMedia(params: SendMediaParams): Promise<{ message_id: string; message_ids: string[] }> {
    const thread = threadOf(params.conversation);
    const api = this.deps.session.require();
    const uploaded = await api.uploadAttachment([params.path], thread.threadId, thread.threadType);
    const results = Array.isArray(uploaded) ? uploaded : [uploaded];
    const ids: string[] = [];
    let messageId = "";
    for (const result of results) {
      const sent = this.rememberSent(result);
      messageId = sent.messageId;
      ids.push(...sent.ids);
    }
    const caption = params.caption?.trim() ?? "";
    if (caption) {
      const follow = await this.sendText({ conversation: params.conversation, text: caption });
      messageId = follow.message_id;
      ids.push(...follow.message_ids);
    }
    if (!messageId) throw new SidecarError("internal");
    return { message_id: messageId, message_ids: ids };
  }

  async sendReaction(params: SendReactionParams): Promise<Record<string, never>> {
    const thread = threadOf(params.conversation);
    const icon = params.emoji === null ? "" : zaloIconForEmoji(params.emoji);
    if (icon === undefined) throw new SidecarError("unsupported");
    await this.deps.session.require().addReaction(icon, {
      data: { msgId: params.message_id, cliMsgId: params.cli_msg_id ?? "0" },
      threadId: thread.threadId,
      type: thread.threadType,
    });
    return {};
  }

  async sendUnsend(params: SendUnsendParams): Promise<Record<string, never>> {
    const thread = threadOf(params.conversation);
    await this.deps.session.require().undo({
      data: { msgId: params.message_id, cliMsgId: params.cli_msg_id ?? "0" },
      threadId: thread.threadId,
      type: thread.threadType,
    });
    return {};
  }

  async friends(): Promise<{ friends: DirectoryEntry[] }> {
    const friends = await this.directory.listFriends();
    for (const friend of friends) this.names.set(friend.id, friend.name);
    return { friends };
  }

  async groups(): Promise<{ groups: DirectoryEntry[] }> {
    return { groups: await this.directory.listGroups() };
  }

  async groupMembers(groupId: string): Promise<{ members: DirectoryEntry[] }> {
    return { members: await this.deps.members.list(groupId) };
  }

  async friendRequests(): Promise<{ requests: Array<{ id: string; name: string; message?: string }> }> {
    return { requests: await this.directory.listFriendRequests() };
  }

  async acceptFriend(userId: string): Promise<Record<string, never>> {
    await this.directory.acceptFriendRequest(userId);
    return {};
  }

  async sendFriendRequest(userId: string | undefined, phone: string | undefined, message: string): Promise<Record<string, never>> {
    await this.directory.sendFriendRequest(userId, phone, message);
    return {};
  }

  async joinGroup(link: string): Promise<Record<string, never>> {
    await this.directory.joinGroup(link);
    return {};
  }

  async leaveGroup(groupId: string): Promise<Record<string, never>> {
    await this.directory.leaveGroup(groupId);
    return {};
  }

  private async connect(): Promise<void> {
    this.setSession("reconnecting");
    await this.deps.session.loginWithSavedCredentials();
    this.attachListener();
  }

  private attachListener(): void {
    const api = this.deps.session.require();
    const listener = api.listener;
    listener.on("message", ((raw: ZaloWireMessage) => {
      void this.onMessage(raw);
    }) as never);
    listener.on("undo", ((raw: ZaloUndoEvent) => {
      this.onUndo(raw);
    }) as never);
    listener.on("reaction", ((raw: ZaloReactionEvent) => {
      this.onReaction(raw);
    }) as never);
    listener.on("group_event", ((raw: ZaloGroupEvent) => {
      this.onGroupEvent(raw);
    }) as never);
    listener.on("friend_event", ((raw: ZaloFriendEvent) => {
      void this.onFriend(raw);
    }) as never);
    listener.once("disconnected", ((code: number) => {
      this.onDisconnected(code);
    }) as never);
    listener.start();
    this.reconnectDelayMs = RECONNECT_DELAY_MS;
    this.listenerUp = true;
    this.deps.emit("listener", { state: "up", reason: "started" });
    this.setSession("valid");
  }

  private async onMessage(raw: ZaloWireMessage): Promise<void> {
    this.rememberQuote(raw);
    if (raw.data.dName && raw.data.uidFrom) this.names.set(raw.data.uidFrom, raw.data.dName);
    const message = await normalizeZaloMessage(raw, this.normalizeDeps());
    if (!message) return;
    if (this.consumeEcho(message.message_id, message.cli_msg_id)) return;
    this.deps.emit("message", message);
  }

  private onUndo(raw: ZaloUndoEvent): void {
    const event = normalizeUndo(raw);
    if (event) this.deps.emit("undo", event);
  }

  private onReaction(raw: ZaloReactionEvent): void {
    const event = normalizeReaction(raw);
    if (event) this.deps.emit("reaction", event);
  }

  private onGroupEvent(raw: ZaloGroupEvent): void {
    const event = normalizeGroupEvent(raw, this.deps.now());
    if (event) this.deps.emit("group_event", event);
  }

  private async onFriend(raw: ZaloFriendEvent): Promise<void> {
    const event = normalizeFriendRequest(raw, (uid) => this.contactName(uid));
    if (!event) return;
    if (event.sender_display_name === event.sender_id) {
      const api = this.deps.session.current();
      if (api) {
        try {
          const profiles = await api.getUserInfo(event.sender_id);
          const key = `${event.sender_id}_0`;
          const profile =
            profiles.changed_profiles?.[key] ??
            profiles.changed_profiles?.[event.sender_id] ??
            profiles.unchanged_profiles?.[key] ??
            profiles.unchanged_profiles?.[event.sender_id];
          const name = profile?.displayName?.trim() || profile?.zaloName?.trim();
          if (name) {
            this.names.set(event.sender_id, name);
            event.sender_display_name = name;
          }
        } catch {
          // The id is still a usable display name.
        }
      }
    }
    this.deps.emit("friend_request", event);
  }

  private onDisconnected(code: number): void {
    this.listenerUp = false;
    this.deps.emit("listener", { state: "down", reason: `close_${code}` });
    if (this.stopping || code === CLOSE_NORMAL) return;
    this.setSession("disconnected", `close_${code}`);
    this.setSession("reconnecting");
    this.scheduleReconnect();
  }

  private scheduleReconnect(): void {
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    const delay = this.reconnectDelayMs;
    this.reconnectTimer = setTimeout(() => {
      void this.reconnect();
    }, delay);
    this.reconnectDelayMs = Math.min(this.reconnectDelayMs * 2, MAX_RECONNECT_DELAY_MS);
  }

  private async reconnect(): Promise<void> {
    if (this.stopping) return;
    try {
      this.deps.session.reset();
      await this.connect();
    } catch {
      this.listenerUp = false;
      this.setSession("expired", "auth_failed");
      this.scheduleReconnect();
    }
  }

  private setSession(state: SessionName, detail?: string): void {
    this.sessionState = state;
    if (detail !== undefined) this.detail = detail;
    const data: { state: SessionName; detail?: string } = { state };
    if (this.detail) data.detail = this.detail;
    this.deps.emit("session", data);
  }

  private normalizeDeps(): NormalizeDeps {
    return {
      contactName: (uid) => this.contactName(uid),
      ...this.deps.normalize,
    };
  }

  private contactName(uid: string): string | undefined {
    return this.deps.normalize?.contactName?.(uid) ?? this.names.get(uid);
  }

  private rememberQuote(raw: ZaloWireMessage): void {
    const msgId = realId(raw.data.msgId);
    const cli = realId(raw.data.cliMsgId);
    const envelope: QuoteEnvelope = {
      msgId: msgId ?? "0",
      cliMsgId: cli ?? "0",
      uidFrom: raw.data.uidFrom,
      ts: raw.data.ts,
      msgType: raw.data.msgType,
      content: raw.data.content,
      ttl: raw.data.ttl ?? 0,
    };
    if (msgId) this.quotes.set(msgId, envelope);
    if (cli) this.quotes.set(cli, envelope);
  }

  private quoteFor(reply: SendTextParams["reply_to"]): unknown {
    if (!reply) return undefined;
    return (
      this.quotes.get(reply.message_id) ??
      (reply.cli_msg_id ? this.quotes.get(reply.cli_msg_id) : undefined) ?? {
        msgId: reply.message_id,
        cliMsgId: reply.cli_msg_id ?? "0",
        ttl: 0,
      }
    );
  }

  private rememberSent(result: ZaloSendResult | undefined): { messageId: string; ids: string[] } {
    const payload = result?.message ?? result;
    const msgId = realId(payload?.msgId);
    const cli = realId(payload?.cliMsgId);
    const ids = [msgId, cli].filter((id): id is string => Boolean(id));
    for (const id of ids) this.sentIds.add(id);
    const messageId = msgId ?? cli;
    if (!messageId) throw new SidecarError("internal");
    return { messageId, ids };
  }

  private consumeEcho(messageId: string, cli: string | undefined): boolean {
    const hit = this.sentIds.has(messageId) || (cli ? this.sentIds.has(cli) : false);
    if (!hit) return false;
    this.sentIds.delete(messageId);
    if (cli) this.sentIds.delete(cli);
    return true;
  }
}

function threadOf(conversation: string): { threadId: string; threadType: number } {
  const parsed = parseConversation(conversation);
  if (!parsed) throw new SidecarError("unsupported");
  return parsed;
}

