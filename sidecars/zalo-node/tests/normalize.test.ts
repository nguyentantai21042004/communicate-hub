import { describe, expect, it } from "vitest";

import { FRIEND_REQUEST_TYPE } from "../src/zalo/api.js";
import { ZALO_THREAD_GROUP, ZALO_THREAD_USER } from "../src/zalo/ids.js";
import {
  albumKeyOf,
  groupKindOf,
  isAlbumFollower,
  normalizeFriendRequest,
  normalizeGroupEvent,
  normalizeReaction,
  normalizeUndo,
  normalizeZaloMessage,
} from "../src/zalo/normalize.js";
import { ZALO_MSG_TYPE, type ZaloMessageData, type ZaloWireMessage } from "../src/zalo/wire.js";

function message(data: Partial<ZaloMessageData>, overrides: Partial<ZaloWireMessage> = {}): ZaloWireMessage {
  return {
    type: ZALO_THREAD_GROUP,
    threadId: "g-1",
    isSelf: false,
    data: {
      content: "",
      msgId: "m-1",
      cliMsgId: "c-1",
      uidFrom: "u-9",
      idTo: "g-1",
      ts: "1700000000000",
      dName: "Tai",
      ...data,
    },
    ...overrides,
  };
}

describe("text, reply, mention", () => {
  it("maps a plain group message", async () => {
    const result = await normalizeZaloMessage(message({ content: "chao", msgType: ZALO_MSG_TYPE.TEXT }));
    expect(result).toMatchObject({
      conversation: "zalo:group:g-1",
      message_id: "m-1",
      cli_msg_id: "c-1",
      sender_id: "u-9",
      sender_display_name: "Tai",
      text: "chao",
      attachments: [],
      sent_at_unix_secs: 1_700_000_000,
      from_self: false,
    });
  });

  it("treats a user thread as a dm", async () => {
    const result = await normalizeZaloMessage(
      message({ content: "hi", msgType: ZALO_MSG_TYPE.TEXT }, { type: ZALO_THREAD_USER, threadId: "u-2" }),
    );
    expect(result?.conversation).toBe("zalo:dm:u-2");
  });

  it("drops blank text, deletes, and unknown media", async () => {
    expect(await normalizeZaloMessage(message({ content: "   ", msgType: ZALO_MSG_TYPE.TEXT }))).toBeUndefined();
    expect(await normalizeZaloMessage(message({ content: "x", msgType: ZALO_MSG_TYPE.DELETE }))).toBeUndefined();
    expect(await normalizeZaloMessage(message({ content: {}, msgType: "chat.brand.new" }))).toBeUndefined();
  });

  it("keeps an unknown msgType that still has text", async () => {
    const result = await normalizeZaloMessage(message({ content: "la", msgType: "chat.brand.new" }));
    expect(result?.text).toBe("la");
  });

  it("reads a reply id and skips the zero sentinel", async () => {
    const result = await normalizeZaloMessage(
      message({
        content: "reply",
        msgType: ZALO_MSG_TYPE.TEXT,
        quote: { globalMsgId: 55, cliMsgId: 0 },
      }),
    );
    expect(result?.reply_to).toEqual({ message_id: "55" });
  });

  it("labels a person and names @all", async () => {
    const result = await normalizeZaloMessage(
      message({
        content: "hi",
        msgType: ZALO_MSG_TYPE.TEXT,
        mentions: [
          { uid: "u-7", pos: 0, len: 2, type: 0 },
          { uid: "-1", pos: 3, len: 4, type: 1 },
          { uid: "u-8", pos: 0, len: 0, type: 0 },
        ],
      }),
      { contactName: (uid) => (uid === "u-7" ? "Tuan" : undefined) },
    );
    expect(result?.mentions).toEqual([
      { user_id: "u-7", display_name: "Tuan" },
      { user_id: "-1", display_name: "all" },
    ]);
  });
});

describe("attachments", () => {
  it("prefers the HD photo url and downloads it", async () => {
    const result = await normalizeZaloMessage(
      message({
        msgType: ZALO_MSG_TYPE.PHOTO,
        content: { href: "https://cdn.example/low.jpg", title: "pic.jpg", params: JSON.stringify({ hd: "https://cdn.example/hd.jpg" }) },
      }),
      { downloadUrl: async (url) => `/data/media/${url.endsWith("hd.jpg") ? "hd" : "low"}.jpg` },
    );
    expect(result?.text).toBe("pic.jpg");
    expect(result?.attachments[0]).toMatchObject({
      kind: "photo",
      url: "https://cdn.example/hd.jpg",
      local_path: "/data/media/hd.jpg",
      mime: "image/jpeg",
    });
  });

  it("treats a doodle as a photo", async () => {
    const result = await normalizeZaloMessage(
      message({ msgType: ZALO_MSG_TYPE.DOODLE, content: { href: "https://cdn.example/d.png" } }),
      { downloadUrl: async () => "/data/media/d.png" },
    );
    expect(result?.attachments[0]?.kind).toBe("photo");
  });

  it("maps voice, gif, and a file with an empty caption", async () => {
    const voice = await normalizeZaloMessage(
      message({ msgType: ZALO_MSG_TYPE.VOICE, content: { href: "https://cdn.example/a.m4a", title: "note" } }),
      { downloadUrl: async () => "/data/media/a.m4a" },
    );
    expect(voice?.attachments[0]?.kind).toBe("voice");
    const gif = await normalizeZaloMessage(
      message({ msgType: ZALO_MSG_TYPE.GIF, content: { href: "https://cdn.example/a.gif" } }),
      { downloadUrl: async () => "/data/media/a.gif" },
    );
    expect(gif?.attachments[0]?.kind).toBe("gif");
    const file = await normalizeZaloMessage(
      message({ msgType: ZALO_MSG_TYPE.FILE, content: { href: "https://cdn.example/a.pdf", title: "notes.pdf" } }),
      { downloadUrl: async () => "/data/media/notes.pdf" },
    );
    expect(file?.text).toBe("");
    expect(file?.attachments[0]).toMatchObject({ kind: "file", file_name: "notes.pdf" });
  });

  it("maps a sticker, downgrades a sprite, and falls back when lookup fails", async () => {
    const still = await normalizeZaloMessage(
      message({ msgType: ZALO_MSG_TYPE.STICKER, content: { id: 7 } }),
      { stickerDetail: async () => ({ webp: "https://cdn.example/s.webp" }), downloadUrl: async () => "/data/media/s.webp" },
    );
    expect(still?.attachments[0]?.kind).toBe("sticker");
    const sprite = await normalizeZaloMessage(
      message({ msgType: ZALO_MSG_TYPE.STICKER, content: { id: 8 } }),
      { stickerDetail: async () => ({ sprite: "https://cdn.example/sheet.png" }), downloadUrl: async () => "/data/media/sheet.png" },
    );
    expect(sprite?.attachments[0]?.kind).toBe("photo");
    expect(sprite?.text).toBe("(sticker động)");
    const failed = await normalizeZaloMessage(
      message({ msgType: ZALO_MSG_TYPE.STICKER, content: { id: 9 } }),
      { stickerDetail: async () => { throw new Error("missing"); } },
    );
    expect(failed).toEqual(expect.objectContaining({ text: "[sticker]", attachments: [] }));
  });

  it("maps a location, a poll, a contact, and an e-card", async () => {
    const location = await normalizeZaloMessage(
      message({ msgType: ZALO_MSG_TYPE.LOCATION, content: { title: "home", latitude: 10.5, longitude: 106.7 } }),
    );
    expect(location?.attachments[0]).toEqual({ kind: "location", url: "https://maps.google.com/maps?q=10.5,106.7" });
    const poll = await normalizeZaloMessage(
      message({
        msgType: ZALO_MSG_TYPE.POLL,
        content: { title: "Lunch", params: JSON.stringify({ pollId: 4, question: "Lunch", options: [{ content: "pho" }] }) },
      }),
      { pollDetail: async () => ({ question: "Lunch", options: [{ content: "pho", votes: 2 }] }) },
    );
    expect(poll?.text).toBe("Lunch\n- pho (2)");
    expect(poll?.attachments[0]?.kind).toBe("poll");
    const contact = await normalizeZaloMessage(
      message({ msgType: ZALO_MSG_TYPE.CONTACT, content: { title: "An", contactUid: "u-3" } }),
    );
    expect(contact?.attachments[0]).toMatchObject({ kind: "contact", url: "https://zalo.me/u/u-3" });
    const card = await normalizeZaloMessage(
      message({
        msgType: ZALO_MSG_TYPE.ECARD,
        content: { title: "Hi", description: "There", href: "https://cdn.example/card.png", params: JSON.stringify({ notifyTxt: "open" }) },
      }),
      { downloadUrl: async () => "/data/media/card.png" },
    );
    expect(card?.text).toBe("Hi\nThere\nopen");
    expect(card?.attachments[0]?.kind).toBe("photo");
  });

  it("renders a bank card and a missed call", async () => {
    const html = `<p>970436</p><p>Vietcombank</p><p>0123456789</p><p>NGUYEN A</p> content=https://img.vietqr.io/x`;
    const bank = await normalizeZaloMessage(
      message({
        msgType: ZALO_MSG_TYPE.WEBCONTENT,
        content: { action: "zinstant.bankcard", params: JSON.stringify({ pcItem: { data_url: "https://zalo.example/card" } }) },
      }),
      {
        bankHtml: async () => html,
        writeQrPng: async () => "/data/media/bank.png",
      },
    );
    expect(bank?.text).toBe("Vietcombank\n0123456789\nNGUYEN A");
    expect(bank?.attachments[0]).toMatchObject({ kind: "bank_card", local_path: "/data/media/bank.png" });
    const missed = await normalizeZaloMessage(
      message({
        msgType: ZALO_MSG_TYPE.LINK,
        content: { action: "recommened.misscall", params: JSON.stringify({ calltype: 1 }) },
      }),
    );
    expect(missed?.text).toBe("cuộc gọi video nhỡ");
  });

  it("keeps a link as plain text and does not merge album photos", async () => {
    const link = await normalizeZaloMessage(
      message({ msgType: ZALO_MSG_TYPE.LINK, content: { title: "Docs", href: "https://example.com/a" } }),
    );
    expect(link?.text).toBe("Docs\nhttps://example.com/a");
    expect(link?.attachments).toEqual([]);
    const first = message({ msgType: ZALO_MSG_TYPE.PHOTO, content: { href: "https://cdn.example/1.jpg", childnumber: 0 } });
    const second = message({ msgType: ZALO_MSG_TYPE.PHOTO, msgId: "m-2", content: { href: "https://cdn.example/2.jpg", childnumber: 1 } });
    expect(albumKeyOf(first)).toBe(albumKeyOf(second));
    expect(isAlbumFollower(second)).toBe(true);
    const a = await normalizeZaloMessage(first);
    const b = await normalizeZaloMessage(second);
    expect(a?.message_id).toBe("m-1");
    expect(b?.message_id).toBe("m-2");
  });
});

describe("group, undo, reaction, friend", () => {
  it("maps group sentences", () => {
    expect(groupKindOf("JOIN")).toBe("join");
    const join = normalizeGroupEvent(
      { type: "join", threadId: "g-1", data: { updateMembers: [{ dName: "An" }] } },
      1_700_000_000_000,
    );
    expect(join).toMatchObject({ conversation: "zalo:group:g-1", kind: "join", text: "An joined", sent_at_unix_secs: 1_700_000_000 });
    const rename = normalizeGroupEvent({ type: "update", threadId: "g-1", data: { groupName: "New" } }, 1);
    expect(rename?.text).toBe("renamed to New");
    const other = normalizeGroupEvent({ type: "new_link", threadId: "g-1", data: {} }, 1);
    expect(other?.kind).toBe("other");
  });

  it("maps undo and reactions", () => {
    expect(normalizeUndo({ threadId: "g-1", isGroup: true, data: { content: { globalMsgId: 9, cliMsgId: 0 } } })).toEqual({
      conversation: "zalo:group:g-1",
      message_id: "9",
    });
    expect(
      normalizeReaction({ threadId: "u-1", isGroup: false, data: { content: { rIcon: "/-heart", rMsg: [{ gMsgID: "m-4" }] } } }),
    ).toEqual({ conversation: "zalo:dm:u-1", message_id: "m-4", emoji: "❤️", added: true });
    expect(normalizeReaction({ threadId: "u-1", data: { content: { rIcon: "", rMsg: [{ gMsgID: "m-4" }] } } })?.added).toBe(false);
    expect(normalizeReaction({ threadId: "u-1", data: { content: { rIcon: "nope", rMsg: [{ gMsgID: "m-4" }] } } })).toBeUndefined();
  });

  it("emits a friend request only when someone else sent it", () => {
    expect(normalizeFriendRequest({ type: FRIEND_REQUEST_TYPE, isSelf: true, data: { fromUid: "u-1" } })).toBeUndefined();
    expect(normalizeFriendRequest({ type: "REQUEST", data: { fromUid: "u-2", message: " hi " } }, () => "An")).toEqual({
      sender_id: "u-2",
      sender_display_name: "An",
      message: "hi",
    });
  });
});
