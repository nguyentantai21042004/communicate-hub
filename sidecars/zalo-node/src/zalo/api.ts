import type { ZaloWireMessage } from "./wire.js";

export interface ZaloListener {
  start(): void;
  stop(): void;
  on(event: string, handler: (payload: never) => void): void;
  once(event: string, handler: (...args: never[]) => void): void;
}

export interface ZaloSendPayload {
  msg: string;
  quote?: unknown;
  mentions?: Array<{ uid: string; pos: number; len: number; type?: number }>;
}

export interface ZaloSendResult {
  msgId?: string | number;
  cliMsgId?: string | number;
  message?: { msgId?: string | number; cliMsgId?: string | number };
}

export interface ZaloReactionTarget {
  data: { msgId: string; cliMsgId: string };
  threadId: string;
  type: number;
}

export interface ZaloFriend {
  userId: string;
  displayName?: string;
  zaloName?: string;
  alias?: string;
}

export interface ZaloUserSearchResult {
  uid?: string;
  display_name?: string;
  zalo_name?: string;
  avatar?: string;
}

export interface ZaloFriendRequest {
  userId?: string;
  uid?: string;
  displayName?: string;
  dName?: string;
  message?: string;
}

export interface GroupMemberRecord {
  id?: string;
  dName?: string;
  zaloName?: string;
}

export interface GroupInfoRecord {
  name?: string;
  avt?: string;
  memVerList?: string[];
  currentMems?: GroupMemberRecord[];
  totalMember?: number;
}

export interface UserProfiles {
  changed_profiles?: Record<string, { displayName?: string; zaloName?: string }>;
  unchanged_profiles?: Record<string, { displayName?: string; zaloName?: string }>;
}

export interface PollDetailRecord {
  question?: string;
  options?: Array<{ content?: string; votes?: number }>;
}

export interface StickerDetail {
  stickerWebpUrl?: string;
  stickerUrl?: string;
  stickerSpriteUrl?: string;
}

export interface ZaloApi {
  listener: ZaloListener;
  getOwnId?(): string;

  sendMessage(payload: ZaloSendPayload, threadId: string, type: number): Promise<ZaloSendResult>;
  uploadAttachment(paths: string[], threadId: string, type: number): Promise<ZaloSendResult | ZaloSendResult[]>;
  addReaction(icon: string, target: ZaloReactionTarget): Promise<unknown>;
  undo(target: ZaloReactionTarget): Promise<unknown>;
  getStickersDetail(ids: number[]): Promise<Array<Record<string, unknown>>>;

  getAllFriends(): Promise<ZaloFriend[]>;
  getAllGroups(): Promise<{ gridVerMap?: Record<string, string> }>;
  getGroupInfo(groupIds: string[]): Promise<{ gridInfoMap?: Record<string, GroupInfoRecord> }>;
  getUserInfo(userId: string | string[]): Promise<UserProfiles>;
  getPollDetail?(pollId: number): Promise<PollDetailRecord>;

  findUser(phoneNumber: string): Promise<ZaloUserSearchResult>;
  sendFriendRequest(message: string, userId: string): Promise<unknown>;
  acceptFriendRequest(userId: string): Promise<unknown>;
  getFriendRecommendations(): Promise<{ recommItems?: ZaloFriendRequest[] }>;
  joinGroupLink(link: string): Promise<unknown>;
  leaveGroup(groupId: string | string[], silent?: boolean): Promise<unknown>;
}

export interface QrHooks {
  onQrCode(png: Buffer): void | Promise<void>;
  onExpired?(): void | Promise<void>;
  onScanned?(displayName: string): void | Promise<void>;
  onDeclined?(): void | Promise<void>;
}

/** The session surface the adapter and directory call. zca-js stays behind this. */
export interface ZaloSession {
  hasCredentials(): boolean;
  current(): ZaloApi | undefined;
  require(): ZaloApi;
  reset(): void;
  loginWithSavedCredentials(): Promise<ZaloApi>;
  loginWithQr(hooks: QrHooks): Promise<ZaloApi>;
  cancelQr(): void;
}

export interface ZaloUndoEvent {
  data?: { content?: { globalMsgId?: number | string; cliMsgId?: number | string }; idTo?: string };
  threadId?: string;
  isGroup?: boolean;
}

export interface ZaloReactionEvent {
  data?: {
    content?: { rIcon?: string; rMsg?: Array<{ gMsgID?: string | number }> };
    uidFrom?: string;
    dName?: string;
    idTo?: string;
  };
  threadId?: string;
  isGroup?: boolean;
  isSelf?: boolean;
}

export interface ZaloGroupEvent {
  type?: string;
  threadId?: string;
  isGroup?: boolean;
  data?: {
    groupId?: string;
    groupName?: string;
    updateMembers?: Array<{ dName?: string; id?: string }>;
    uids?: string[];
  };
}

export interface ZaloFriendEvent {
  type?: number | string;
  isSelf?: boolean;
  data?: { fromUid?: string; toUid?: string; message?: string };
}

export type WireMessage = ZaloWireMessage;

/** zca-js `FriendEventType.REQUEST`. Checked against the package in tests. */
export const FRIEND_REQUEST_TYPE = 2;
