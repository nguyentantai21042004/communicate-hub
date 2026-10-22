import { SidecarError } from "../errors.js";
import type { ZaloSession } from "./api.js";
import type { DirectoryEntry } from "./members.js";

export interface FriendRequestEntry {
  id: string;
  name: string;
  message?: string;
}

/** One-shot directory calls. The listener and the send path stay on the bridge. */
export class ZaloDirectory {
  constructor(private readonly session: ZaloSession) {}

  async listFriends(): Promise<DirectoryEntry[]> {
    const friends = await this.session.require().getAllFriends();
    return friends.map((friend) => ({
      id: friend.userId,
      name: friend.alias?.trim() || friend.displayName || friend.zaloName || friend.userId,
    }));
  }

  async listGroups(): Promise<DirectoryEntry[]> {
    const api = this.session.require();
    const all = await api.getAllGroups();
    const ids = Object.keys(all.gridVerMap ?? {});
    if (ids.length === 0) return [];
    const info = await api.getGroupInfo(ids);
    return ids.map((id) => ({
      id,
      name: info.gridInfoMap?.[id]?.name?.trim() || id,
    }));
  }

  async listFriendRequests(): Promise<FriendRequestEntry[]> {
    const response = await this.session.require().getFriendRecommendations();
    return (response.recommItems ?? [])
      .map((item) => ({
        id: item.userId ?? item.uid ?? "",
        name: item.displayName ?? item.dName ?? item.userId ?? "",
        message: item.message,
      }))
      .filter((item) => item.id.length > 0);
  }

  async acceptFriendRequest(userId: string): Promise<void> {
    await this.session.require().acceptFriendRequest(userId);
  }

  async sendFriendRequest(userId: string | undefined, phone: string | undefined, message: string): Promise<void> {
    const api = this.session.require();
    let target = userId;
    if (!target && phone) {
      const found = await api.findUser(phone);
      target = found.uid;
      if (!target) throw new SidecarError("not_found");
    }
    if (!target) throw new SidecarError("unsupported");
    await api.sendFriendRequest(message, target);
  }

  async joinGroup(link: string): Promise<void> {
    await this.session.require().joinGroupLink(link);
  }

  async leaveGroup(groupId: string): Promise<void> {
    await this.session.require().leaveGroup(groupId);
  }
}
