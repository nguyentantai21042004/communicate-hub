/** Value of `data.msgType` on an incoming message. */
export const ZALO_MSG_TYPE = {
  TEXT: "webchat",
  PHOTO: "chat.photo",
  VOICE: "chat.voice",
  STICKER: "chat.sticker",
  DOODLE: "chat.doodle",
  LINK: "chat.recommended",
  VIDEO: "chat.video.msg",
  FILE: "share.file",
  GIF: "chat.gif",
  LOCATION: "chat.location.new",
  WEBCONTENT: "chat.webcontent",
  POLL: "group.poll",
  CONTACT: "chat.forward",
  ECARD: "chat.ecard",
  DELETE: "chat.delete",
} as const;

export interface ZaloMediaContent {
  href?: string;
  thumb?: string;
  title?: string;
  description?: string;
  params?: string;
  action?: string;
  childnumber?: number;
  type?: string | number;
  id?: number;
  contactUid?: string;
  qrCodeUrl?: string;
  latitude?: number | string;
  longitude?: number | string;
}

export interface ZaloMention {
  uid: string;
  pos: number;
  len: number;
  type: 0 | 1;
}

export interface ZaloQuote {
  ownerId?: string;
  cliMsgId?: number;
  globalMsgId?: number;
  ts?: number;
  msg?: string;
  attach?: string;
  fromD?: string;
  ttl?: number;
}

export interface ZaloMessageData {
  content: string | ZaloMediaContent | Record<string, unknown>;
  msgId: string;
  cliMsgId?: string;
  realMsgId?: string;
  uidFrom: string;
  dName?: string;
  idTo: string;
  ts: string;
  msgType?: string;
  ttl?: number;
  quote?: ZaloQuote;
  mentions?: ZaloMention[];
  textProperties?: string;
}

export interface ZaloWireMessage {
  type: number;
  data: ZaloMessageData;
  isSelf: boolean;
  threadId: string;
}

export interface ZaloStyleSpan {
  start: number;
  len: number;
  st: string;
}

export function mediaContentOf(content: ZaloMessageData["content"]): ZaloMediaContent {
  if (content && typeof content === "object") return content as ZaloMediaContent;
  return {};
}

export function textContentOf(content: ZaloMessageData["content"]): string | undefined {
  return typeof content === "string" ? content : undefined;
}

export function parseParams<T>(params: string | undefined): T | undefined {
  if (!params) return undefined;
  try {
    return JSON.parse(params) as T;
  } catch {
    return undefined;
  }
}

/** First style document that parses wins. `textProperties`, then `content.params`. */
export function parseStyles(data: ZaloMessageData): ZaloStyleSpan[] | undefined {
  const media = mediaContentOf(data.content);
  for (const raw of [data.textProperties, media.params]) {
    const parsed = parseParams<{ styles?: ZaloStyleSpan[] }>(raw);
    if (parsed?.styles?.length) return parsed.styles;
  }
  return undefined;
}

/** Photos carry a higher-resolution URL in `params.hd` when one exists. */
export function bestPhotoUrl(media: ZaloMediaContent): string | undefined {
  return parseParams<{ hd?: string }>(media.params)?.hd ?? media.href;
}

/**
 * Ids the quoted message may be stored under.
 * `globalMsgId` is 0 in DMs. `"0"` is Zalo's empty sentinel.
 */
export function quoteCandidateIds(quote: ZaloQuote | undefined): string[] {
  if (!quote) return [];
  const out: string[] = [];
  for (const value of [quote.globalMsgId, quote.cliMsgId]) {
    if (value === undefined || value === null) continue;
    const id = String(value);
    if (id && id !== "0" && !out.includes(id)) out.push(id);
  }
  return out;
}
