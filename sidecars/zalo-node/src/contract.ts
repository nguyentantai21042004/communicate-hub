import { z } from "zod";

/** Method names, in the order the design doc lists them. */
export const METHOD_NAMES = [
  "health",
  "login.qr.start",
  "login.qr.cancel",
  "send.text",
  "send.media",
  "send.reaction",
  "send.unsend",
  "directory.friends",
  "directory.groups",
  "directory.group_members",
  "friend.requests",
  "friend.accept",
  "friend.send_request",
  "group.join",
  "group.leave",
] as const;
export type MethodName = (typeof METHOD_NAMES)[number];

/** Event names, in the order the design doc lists them. */
export const EVENT_NAMES = [
  "message",
  "reaction",
  "undo",
  "group_event",
  "friend_request",
  "session",
  "listener",
  "login.qr.state",
] as const;
export type EventName = (typeof EVENT_NAMES)[number];

/** Closed error codes. */
export const ERROR_CODES = [
  "auth_expired",
  "rate_limited",
  "not_found",
  "unsupported",
  "timeout",
  "internal",
] as const;
export type ErrorCode = (typeof ERROR_CODES)[number];

export const ERROR_MESSAGE: { readonly [K in ErrorCode]: string } = {
  auth_expired: "authentication expired",
  rate_limited: "rate limited",
  not_found: "not found",
  unsupported: "unsupported",
  timeout: "timed out",
  internal: "internal error",
};

export const SESSION_STATES = ["valid", "expired", "disconnected", "reconnecting"] as const;
export type SessionStateName = (typeof SESSION_STATES)[number];

export const QR_STATES = ["waiting", "scanned", "expired", "done"] as const;
export const GROUP_KINDS = [
  "join",
  "leave",
  "remove_member",
  "block_member",
  "join_request",
  "rename",
  "other",
] as const;
export const ATTACHMENT_KINDS = [
  "photo",
  "video",
  "voice",
  "audio",
  "file",
  "sticker",
  "gif",
  "location",
  "poll",
  "bank_card",
  "contact",
] as const;
export type AttachmentKindName = (typeof ATTACHMENT_KINDS)[number];

const errorCodeSchema = z.enum(ERROR_CODES);

export const errorSchema = z.object({
  code: errorCodeSchema,
  message: z.string(),
  retry_after_secs: z.number().int().nonnegative().optional(),
});

export const requestSchema = z.object({
  id: z.number().int().nonnegative(),
  method: z.string().min(1),
  params: z.unknown().optional(),
});

export const responseSchema = z.object({
  id: z.number().int().nonnegative(),
  ok: z.boolean(),
  result: z.unknown().optional(),
  error: errorSchema.optional(),
});

export const eventFrameSchema = z.object({
  event: z.enum(EVENT_NAMES),
  data: z.unknown(),
});

const conversationSchema = z.string().min(1);

const replySchema = z.object({
  message_id: z.string().min(1),
  cli_msg_id: z.string().optional(),
});

const emptyParams = z.object({}).passthrough();

export const healthResultSchema = z.object({
  listener: z.enum(["up", "down"]),
  session: z.enum(SESSION_STATES),
  detail: z.string().optional(),
});

export const qrStartResultSchema = z.object({
  png_base64: z.string().min(1),
});

export const sendResultSchema = z.object({
  message_id: z.string().min(1),
  message_ids: z.array(z.string().min(1)).min(1),
});

export const directoryEntrySchema = z.object({
  id: z.string().min(1),
  name: z.string(),
});

export const friendRequestEntrySchema = z.object({
  id: z.string().min(1),
  name: z.string(),
  message: z.string().optional(),
});

export const attachmentSchema = z.object({
  kind: z.enum(ATTACHMENT_KINDS),
  mime: z.string().optional(),
  size_bytes: z.number().int().nonnegative().optional(),
  local_path: z.string().optional(),
  url: z.string().optional(),
  file_name: z.string().optional(),
});

export const messageEventSchema = z.object({
  conversation: conversationSchema,
  message_id: z.string().min(1),
  cli_msg_id: z.string().optional(),
  sender_id: z.string(),
  sender_display_name: z.string(),
  text: z.string(),
  attachments: z.array(attachmentSchema),
  reply_to: z.object({ message_id: z.string().min(1) }).optional(),
  sent_at_unix_secs: z.number().int(),
  mentions: z.array(z.object({ user_id: z.string(), display_name: z.string() })),
  from_self: z.boolean(),
});

export const reactionEventSchema = z.object({
  conversation: conversationSchema,
  message_id: z.string().min(1),
  emoji: z.string(),
  added: z.boolean(),
});

export const undoEventSchema = z.object({
  conversation: conversationSchema,
  message_id: z.string().min(1),
});

export const groupEventSchema = z.object({
  conversation: conversationSchema,
  text: z.string(),
  sent_at_unix_secs: z.number().int(),
  kind: z.enum(GROUP_KINDS),
});

export const friendRequestEventSchema = z.object({
  sender_id: z.string().min(1),
  sender_display_name: z.string(),
  message: z.string(),
});

export const sessionEventSchema = z.object({
  state: z.enum(SESSION_STATES),
  detail: z.string().optional(),
});

export const listenerEventSchema = z.object({
  state: z.enum(["up", "down"]),
  reason: z.string().min(1),
});

export const qrStateEventSchema = z.object({
  state: z.enum(QR_STATES),
  display_name: z.string().optional(),
  png_base64: z.string().optional(),
});

export const PARAM_SCHEMAS = {
  health: emptyParams,
  "login.qr.start": emptyParams,
  "login.qr.cancel": emptyParams,
  "send.text": z.object({
    conversation: conversationSchema,
    text: z.string(),
    reply_to: replySchema.optional(),
    mentions: z
      .array(
        z.object({
          uid: z.string().min(1),
          pos: z.number().int().nonnegative(),
          len: z.number().int().positive(),
        }),
      )
      .optional(),
  }),
  "send.media": z.object({
    conversation: conversationSchema,
    path: z.string().min(1),
    caption: z.string().optional(),
    kind: z.string().optional(),
  }),
  "send.reaction": z.object({
    conversation: conversationSchema,
    message_id: z.string().min(1),
    cli_msg_id: z.string().optional(),
    emoji: z.string().nullable(),
  }),
  "send.unsend": z.object({
    conversation: conversationSchema,
    message_id: z.string().min(1),
    cli_msg_id: z.string().optional(),
  }),
  "directory.friends": emptyParams,
  "directory.groups": emptyParams,
  "directory.group_members": z.object({ group_id: z.string().min(1) }),
  "friend.requests": emptyParams,
  "friend.accept": z.object({ user_id: z.string().min(1) }),
  "friend.send_request": z.object({
    user_id: z.string().min(1).optional(),
    phone: z.string().min(1).optional(),
    message: z.string(),
  }),
  "group.join": z.object({ link: z.string().min(1) }),
  "group.leave": z.object({ group_id: z.string().min(1) }),
} satisfies { [K in MethodName]: z.ZodTypeAny };

export function isMethodName(value: string): value is MethodName {
  return (METHOD_NAMES as readonly string[]).includes(value);
}

export function isEventName(value: string): value is EventName {
  return (EVENT_NAMES as readonly string[]).includes(value);
}
