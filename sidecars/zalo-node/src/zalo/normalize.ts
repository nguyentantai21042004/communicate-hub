import type { AttachmentKindName } from "../contract.js";
import { bankCardText, parseBankCardHtml } from "./bank.js";
import { conversationId, realId, unixSeconds, ZALO_THREAD_GROUP } from "./ids.js";
import { emojiForZaloIcon } from "./reaction.js";
import {
  bestPhotoUrl,
  mediaContentOf,
  parseParams,
  quoteCandidateIds,
  textContentOf,
  ZALO_MSG_TYPE,
  type ZaloMediaContent,
  type ZaloWireMessage,
} from "./wire.js";
import type { ZaloFriendEvent, ZaloGroupEvent, ZaloReactionEvent, ZaloUndoEvent } from "./api.js";
import { FRIEND_REQUEST_TYPE } from "./api.js";

export interface AttachmentEvent {
  kind: AttachmentKindName;
  mime?: string;
  size_bytes?: number;
  local_path?: string;
  url?: string;
  file_name?: string;
}

export interface MessageEventData {
  conversation: string;
  message_id: string;
  cli_msg_id?: string;
  sender_id: string;
  sender_display_name: string;
  text: string;
  attachments: AttachmentEvent[];
  reply_to?: { message_id: string };
  sent_at_unix_secs: number;
  mentions: Array<{ user_id: string; display_name: string }>;
  from_self: boolean;
  conversation_title?: string;
  avatar_url?: string;
}

export interface NormalizeDeps {
  contactName?(uid: string): string | undefined;
  downloadUrl?(url: string, fileName: string): Promise<string>;
  stickerDetail?(id: number): Promise<{ webp?: string; still?: string; sprite?: string } | undefined>;
  bankHtml?(url: string): Promise<string | undefined>;
  writeQrPng?(payload: string, fileName: string): Promise<string>;
  pollDetail?(pollId: number): Promise<{ question?: string; options?: Array<{ content?: string; votes?: number }> } | undefined>;
}

const MIME_BY_EXT: Readonly<Record<string, string>> = {
  ".jpg": "image/jpeg",
  ".jpeg": "image/jpeg",
  ".png": "image/png",
  ".gif": "image/gif",
  ".webp": "image/webp",
  ".mp4": "video/mp4",
  ".m4a": "audio/mp4",
  ".mp3": "audio/mpeg",
  ".pdf": "application/pdf",
};

/** Photos of one album share this key. Zalo marks followers with `childnumber` > 0. */
export function albumKeyOf(raw: ZaloWireMessage): string | undefined {
  if (raw.data.msgType !== ZALO_MSG_TYPE.PHOTO) return undefined;
  return `${raw.threadId}:${raw.data.uidFrom}`;
}

export function isAlbumFollower(raw: ZaloWireMessage): boolean {
  return (mediaContentOf(raw.data.content).childnumber ?? 0) > 0;
}

export function groupKindOf(type: string | undefined): "join" | "leave" | "remove_member" | "block_member" | "join_request" | "rename" | "other" {
  switch ((type ?? "").toLowerCase()) {
    case "join":
      return "join";
    case "leave":
      return "leave";
    case "remove_member":
      return "remove_member";
    case "block_member":
      return "block_member";
    case "join_request":
      return "join_request";
    case "update":
    case "update_setting":
    case "rename":
      return "rename";
    default:
      return "other";
  }
}

/**
 * Turn one zca-js message into the sidecar `message` event.
 * Returns undefined when the bridge would have nothing to show.
 */
export async function normalizeZaloMessage(
  raw: ZaloWireMessage,
  deps: NormalizeDeps = {},
): Promise<MessageEventData | undefined> {
  const msgType = raw.data.msgType ?? ZALO_MSG_TYPE.TEXT;
  if (msgType === ZALO_MSG_TYPE.DELETE) return undefined;

  const media = mediaContentOf(raw.data.content);
  const text = textContentOf(raw.data.content);
  const messageId = realId(raw.data.msgId) ?? realId(raw.data.cliMsgId);
  if (!messageId) return undefined;

  const built = await buildBody(msgType, text, media, messageId, deps);
  if (!built) return undefined;

  const threadType = raw.type === ZALO_THREAD_GROUP ? ZALO_THREAD_GROUP : 0;
  const mentions = (raw.data.mentions ?? [])
    .filter((mention) => mention.len > 0 && mention.pos >= 0)
    .map((mention) => {
      const named = mention.type === 0 ? deps.contactName?.(mention.uid) : undefined;
      const displayName = mention.type === 1 ? "all" : named || mention.uid;
      return { user_id: mention.uid, display_name: displayName };
    });

  const replyId = quoteCandidateIds(raw.data.quote)[0];
  const cli = realId(raw.data.cliMsgId);
  const message: MessageEventData = {
    conversation: conversationId(raw.threadId, threadType),
    message_id: messageId,
    sender_id: raw.data.uidFrom,
    sender_display_name: raw.data.dName ?? "",
    text: built.text,
    attachments: built.attachments,
    sent_at_unix_secs: unixSeconds(raw.data.ts),
    mentions,
    from_self: raw.isSelf,
  };
  if (cli && cli !== messageId) message.cli_msg_id = cli;
  if (replyId) message.reply_to = { message_id: replyId };
  return message;
}

export function normalizeUndo(
  raw: ZaloUndoEvent,
): { conversation: string; message_id: string } | undefined {
  const target = realId(raw.data?.content?.globalMsgId) ?? realId(raw.data?.content?.cliMsgId);
  const threadId = raw.threadId ?? raw.data?.idTo;
  if (!target || !threadId) return undefined;
  return {
    conversation: conversationId(String(threadId), raw.isGroup ? ZALO_THREAD_GROUP : 0),
    message_id: target,
  };
}

export function normalizeReaction(raw: ZaloReactionEvent):
  | { conversation: string; message_id: string; emoji: string; added: boolean }
  | undefined {
  const icon = raw.data?.content?.rIcon ?? "";
  const target = realId(raw.data?.content?.rMsg?.[0]?.gMsgID);
  const threadId = raw.threadId ?? raw.data?.idTo;
  if (!target || !threadId) return undefined;
  const conversation = conversationId(String(threadId), raw.isGroup ? ZALO_THREAD_GROUP : 0);
  if (!icon) return { conversation, message_id: target, emoji: "", added: false };
  const emoji = emojiForZaloIcon(icon);
  if (!emoji) return undefined;
  return { conversation, message_id: target, emoji, added: true };
}

export function normalizeGroupEvent(
  raw: ZaloGroupEvent,
  nowMs: number,
): { conversation: string; text: string; sent_at_unix_secs: number; kind: ReturnType<typeof groupKindOf> } | undefined {
  const threadId = raw.threadId ?? raw.data?.groupId;
  if (!threadId) return undefined;
  let kind = groupKindOf(raw.type);
  const names = memberNames(raw);
  let text: string;
  if (kind === "rename") {
    const title = raw.data?.groupName?.trim();
    if (!title) {
      kind = "other";
      text = "group updated";
    } else {
      text = `renamed to ${title}`;
    }
  } else if (kind === "join") {
    text = `${names} joined`;
  } else if (kind === "leave") {
    text = `${names} left`;
  } else if (kind === "remove_member") {
    text = `${names} was removed`;
  } else if (kind === "block_member") {
    text = `${names} was blocked`;
  } else if (kind === "join_request") {
    text = `${names} requested to join`;
  } else {
    text = "group updated";
  }
  return {
    conversation: conversationId(String(threadId), ZALO_THREAD_GROUP),
    text,
    sent_at_unix_secs: unixSeconds(nowMs),
    kind,
  };
}

export function normalizeFriendRequest(
  raw: ZaloFriendEvent,
  contactName?: (uid: string) => string | undefined,
): { sender_id: string; sender_display_name: string; message: string } | undefined {
  if (raw.isSelf) return undefined;
  if (raw.type !== FRIEND_REQUEST_TYPE && raw.type !== "request" && raw.type !== "REQUEST") return undefined;
  const senderId = raw.data?.fromUid;
  if (!senderId) return undefined;
  return {
    sender_id: senderId,
    sender_display_name: contactName?.(senderId) || senderId,
    message: raw.data?.message?.trim() ?? "",
  };
}

async function buildBody(
  msgType: string,
  text: string | undefined,
  media: ZaloMediaContent,
  messageId: string,
  deps: NormalizeDeps,
): Promise<{ text: string; attachments: AttachmentEvent[] } | undefined> {
  if (msgType === ZALO_MSG_TYPE.LINK && media.action === "recommened.misscall") {
    const params = parseParams<{ calltype?: number }>(media.params);
    const call = params?.calltype === 1 ? "cuộc gọi video nhỡ" : "cuộc gọi thoại nhỡ";
    return { text: call, attachments: [] };
  }

  if (msgType === ZALO_MSG_TYPE.WEBCONTENT && media.action === "zinstant.bankcard") {
    return bankCardBody(media, messageId, deps);
  }

  if (msgType === ZALO_MSG_TYPE.WEBCONTENT || msgType === ZALO_MSG_TYPE.LINK) {
    const body = linkText(msgType, media);
    if (!body) return undefined;
    return { text: body, attachments: [] };
  }

  if (msgType === ZALO_MSG_TYPE.ECARD) {
    return ecardBody(media, messageId, deps);
  }

  if (msgType === ZALO_MSG_TYPE.LOCATION) {
    return locationBody(media);
  }

  if (msgType === ZALO_MSG_TYPE.POLL) {
    return pollBody(media, messageId, deps);
  }

  if (msgType === ZALO_MSG_TYPE.CONTACT) {
    if (!media.contactUid) return undefined;
    return contactBody(media, messageId, deps);
  }

  if (msgType === ZALO_MSG_TYPE.STICKER) {
    return stickerBody(media, text, messageId, deps);
  }

  const kind = kindOf(msgType);
  if (kind) {
    const remote = kind === "photo" ? bestPhotoUrl(media) : media.href;
    if (!remote) return undefined;
    const fileName = media.title?.trim() || fileNameFromUrl(remote);
    const attachment = await fileAttachment(kind, remote, fileName, messageId, 0, deps);
    const caption = kind === "file" ? "" : media.title?.trim() || "";
    return { text: caption, attachments: [attachment] };
  }

  const body = text ?? media.title ?? "";
  if (!body.trim()) return undefined;
  return { text: body, attachments: [] };
}

function kindOf(msgType: string): AttachmentKindName | undefined {
  switch (msgType) {
    case ZALO_MSG_TYPE.PHOTO:
    case ZALO_MSG_TYPE.DOODLE:
      return "photo";
    case ZALO_MSG_TYPE.VIDEO:
      return "video";
    case ZALO_MSG_TYPE.VOICE:
      return "voice";
    case ZALO_MSG_TYPE.GIF:
      return "gif";
    case ZALO_MSG_TYPE.FILE:
      return "file";
    default:
      return undefined;
  }
}

async function bankCardBody(
  media: ZaloMediaContent,
  messageId: string,
  deps: NormalizeDeps,
): Promise<{ text: string; attachments: AttachmentEvent[] }> {
  const params = parseParams<{ pcItem?: { data_url?: string }; item?: { data_url?: string } }>(media.params);
  const dataUrl = params?.pcItem?.data_url ?? params?.item?.data_url;
  const html = dataUrl ? await deps.bankHtml?.(dataUrl) : undefined;
  const info = html ? parseBankCardHtml(html) : null;
  if (!info) {
    return { text: webLabel(media) || "bank card", attachments: [] };
  }
  const attachments: AttachmentEvent[] = [];
  if (deps.writeQrPng) {
    try {
      const localPath = await deps.writeQrPng(info.vietqr, `bank-${messageId}.png`);
      attachments.push({ kind: "bank_card", local_path: localPath, mime: "image/png", file_name: `bank-${messageId}.png` });
    } catch {
      // A bank card without a QR file is still the three text lines.
    }
  }
  return { text: bankCardText(info), attachments };
}

async function ecardBody(
  media: ZaloMediaContent,
  messageId: string,
  deps: NormalizeDeps,
): Promise<{ text: string; attachments: AttachmentEvent[] } | undefined> {
  const notify = parseParams<{ notifyTxt?: string }>(media.params)?.notifyTxt?.trim() ?? "";
  const title = media.title?.trim() ?? "";
  const description = media.description?.trim() ?? "";
  const lines = [title];
  if (description && description !== title) lines.push(description);
  if (notify) lines.push(notify);
  const text = lines.filter((line) => line.length > 0).join("\n") || "e-card";
  const attachments: AttachmentEvent[] = [];
  if (media.href) {
    attachments.push(await fileAttachment("photo", media.href, `ecard-${messageId}.png`, messageId, 0, deps));
  }
  return { text, attachments };
}

function locationBody(media: ZaloMediaContent): { text: string; attachments: AttachmentEvent[] } {
  const lat = Number(media.latitude ?? parseParams<{ latitude?: number }>(media.params)?.latitude);
  const lng = Number(media.longitude ?? parseParams<{ longitude?: number }>(media.params)?.longitude);
  const title = media.title?.trim() || "";
  if (!Number.isFinite(lat) || !Number.isFinite(lng)) {
    return { text: title || "Location", attachments: [] };
  }
  return {
    text: title,
    attachments: [{
      kind: "location",
      url: `https://maps.google.com/maps?q=${lat},${lng}`,
    }],
  };
}

async function pollBody(
  media: ZaloMediaContent,
  messageId: string,
  deps: NormalizeDeps,
): Promise<{ text: string; attachments: AttachmentEvent[] }> {
  const params = parseParams<{ pollId?: number; question?: string; options?: Array<{ content?: string }> }>(media.params);
  const pollId = params?.pollId;
  const detail = pollId !== undefined ? await deps.pollDetail?.(pollId) : undefined;
  const question = detail?.question?.trim() || params?.question?.trim() || media.title?.trim() || "Poll";
  const options = detail?.options ?? params?.options ?? [];
  const lines = [question];
  for (const option of options) {
    const content = option.content?.trim();
    if (!content) continue;
    const votes = "votes" in option && typeof option.votes === "number" ? ` (${option.votes})` : "";
    lines.push(`- ${content}${votes}`);
  }
  return {
    text: lines.join("\n"),
    attachments: [{
      kind: "poll",
      url: `https://zalo.me/poll/${pollId ?? messageId}`,
    }],
  };
}

async function contactBody(
  media: ZaloMediaContent,
  messageId: string,
  deps: NormalizeDeps,
): Promise<{ text: string; attachments: AttachmentEvent[] }> {
  const uid = media.contactUid ?? "";
  const name = media.title?.trim() || uid;
  const text = [name, uid].filter((part, index, all) => part && all.indexOf(part) === index).join("\n");
  const attachment: AttachmentEvent = {
    kind: "contact",
    file_name: name,
    url: `https://zalo.me/u/${uid}`,
  };
  if (media.qrCodeUrl && deps.downloadUrl) {
    try {
      attachment.local_path = await deps.downloadUrl(media.qrCodeUrl, `contact-${messageId}.jpg`);
      attachment.mime = "image/jpeg";
    } catch {
      // The stand-in URL stays. The card text is still delivered.
    }
  }
  return { text, attachments: [attachment] };
}

async function stickerBody(
  media: ZaloMediaContent,
  text: string | undefined,
  messageId: string,
  deps: NormalizeDeps,
): Promise<{ text: string; attachments: AttachmentEvent[] } | undefined> {
  const id = media.id;
  if (id === undefined) return undefined;
  let webp: string | undefined;
  let still: string | undefined;
  let sprite: string | undefined;
  if (deps.stickerDetail) {
    try {
      const detail = await deps.stickerDetail(id);
      webp = detail?.webp;
      still = detail?.still;
      sprite = detail?.sprite;
    } catch {
      return { text: text?.trim() || "[sticker]", attachments: [] };
    }
  }
  const remote = webp ?? still ?? sprite ?? media.href;
  if (!remote && !deps.stickerDetail) {
    return {
      text: "",
      attachments: [{ kind: "sticker", file_name: String(id) }],
    };
  }
  if (!remote) return { text: text?.trim() || "[sticker]", attachments: [] };
  const spriteOnly = !webp && !still && Boolean(sprite);
  const kind: AttachmentKindName = spriteOnly ? "photo" : "sticker";
  const attachment = await fileAttachment(kind, remote, `sticker-${id}.webp`, messageId, 0, deps);
  const caption = text?.trim() || (spriteOnly ? "(sticker động)" : "");
  return { text: caption, attachments: [attachment] };
}

function linkText(msgType: string, media: ZaloMediaContent): string {
  if (msgType === ZALO_MSG_TYPE.WEBCONTENT) return webLabel(media);
  const href = media.href?.trim() || "";
  const title = media.title?.trim() || href;
  if (!href && !title) return "";
  return href && title && title !== href ? `${title}\n${href}` : title || href;
}

function webLabel(media: ZaloMediaContent): string {
  const params = parseParams<{ customMsg?: { msg?: { vi?: string; en?: string } } }>(media.params);
  const vi = params?.customMsg?.msg?.vi?.trim();
  const en = params?.customMsg?.msg?.en?.trim();
  return vi || en || media.title?.trim() || "";
}

async function fileAttachment(
  kind: AttachmentKindName,
  url: string,
  fileName: string,
  messageId: string,
  index: number,
  deps: NormalizeDeps,
): Promise<AttachmentEvent> {
  const attachment: AttachmentEvent = { kind, url, file_name: fileName };
  const mime = mimeOf(fileName);
  if (mime) attachment.mime = mime;
  if (!deps.downloadUrl) return attachment;
  try {
    const safeName = `${messageId}-${index}-${fileName.replace(/[^A-Za-z0-9._-]+/g, "_").slice(0, 60)}`;
    attachment.local_path = await deps.downloadUrl(url, safeName);
  } catch {
    // Keep the URL so the message is not dropped when the download fails.
  }
  return attachment;
}

function mimeOf(fileName: string): string | undefined {
  const dot = fileName.lastIndexOf(".");
  if (dot < 0) return undefined;
  return MIME_BY_EXT[fileName.slice(dot).toLowerCase()];
}

function fileNameFromUrl(url: string): string {
  const pathPart = url.split("?")[0] ?? url;
  const slash = pathPart.lastIndexOf("/");
  const name = slash >= 0 ? pathPart.slice(slash + 1) : pathPart;
  return name || "file.bin";
}

function memberNames(raw: ZaloGroupEvent): string {
  const fromMembers = (raw.data?.updateMembers ?? [])
    .map((member) => member.dName?.trim())
    .filter((name): name is string => Boolean(name));
  if (fromMembers.length > 0) return fromMembers.join(", ");
  const uids = (raw.data?.uids ?? []).filter((uid) => uid.length > 0);
  if (uids.length > 0) return uids.join(", ");
  return "someone";
}
