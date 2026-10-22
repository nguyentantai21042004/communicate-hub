import type { AppMemberApi } from "./app-api.js";
import type { GroupInfoRecord, UserProfiles, ZaloApi } from "./api.js";

/** How long a group member list is reused before the next lookup. */
export const GROUP_MEMBER_CACHE_TTL_MS = 5 * 60 * 1000;
const USER_INFO_BATCH = 50;

export interface DirectoryEntry {
  id: string;
  name: string;
}

export interface MemberSource {
  load(groupId: string): Promise<DirectoryEntry[]>;
}

export class GroupMemberCache {
  private readonly entries = new Map<string, { at: number; members: DirectoryEntry[] }>();

  constructor(
    private readonly source: MemberSource,
    private readonly ttlMs: number,
    private readonly now: () => number,
  ) {}

  async list(groupId: string): Promise<DirectoryEntry[]> {
    const hit = this.entries.get(groupId);
    if (hit && this.now() - hit.at < this.ttlMs) return hit.members;
    const members = await this.source.load(groupId);
    this.entries.set(groupId, { at: this.now(), members });
    return members;
  }
}

/**
 * Names come from the PC app group payload when that session exists, then
 * from the web API. `currentMems` wins over a later profile lookup.
 */
export async function loadGroupMembers(
  groupId: string,
  web: Pick<ZaloApi, "getGroupInfo" | "getUserInfo">,
  app: AppMemberApi,
): Promise<DirectoryEntry[]> {
  let info = await app.groupInfo(groupId);
  if (!info) {
    const response = await web.getGroupInfo([groupId]);
    info = response.gridInfoMap?.[groupId] ?? null;
  }
  if (!info) return [];
  return namesFor(info, web, app);
}

async function namesFor(info: GroupInfoRecord, web: Pick<ZaloApi, "getUserInfo">, app: AppMemberApi): Promise<DirectoryEntry[]> {
  const known = new Map<string, string>();
  for (const member of info.currentMems ?? []) {
    const name = member.dName?.trim() || member.zaloName?.trim();
    if (member.id && name) known.set(member.id, name);
  }
  const uids = (info.memVerList ?? []).map(uidOf).filter((uid) => uid.length > 0);
  const ids = uids.length > 0 ? uids : [...known.keys()];
  const missing = ids.filter((uid) => !known.has(uid));
  if (missing.length > 0) {
    const appNames = await app.memberNames(missing).catch(() => null);
    const still: string[] = [];
    for (const uid of missing) {
      const name = appNames?.get(uid);
      if (name) known.set(uid, name);
      else still.push(uid);
    }
    if (still.length > 0) await fillFromWeb(still, known, web);
  }
  return ids.map((id) => ({ id, name: known.get(id) || id }));
}

async function fillFromWeb(uids: string[], known: Map<string, string>, web: Pick<ZaloApi, "getUserInfo">): Promise<void> {
  for (let i = 0; i < uids.length; i += USER_INFO_BATCH) {
    const batch = uids.slice(i, i + USER_INFO_BATCH);
    let profiles: UserProfiles;
    try {
      profiles = await web.getUserInfo(batch);
    } catch {
      continue;
    }
    for (const uid of batch) {
      const key = uid.includes("_") ? uid : `${uid}_0`;
      const profile =
        profiles.changed_profiles?.[key] ??
        profiles.changed_profiles?.[uid] ??
        profiles.unchanged_profiles?.[key] ??
        profiles.unchanged_profiles?.[uid];
      const name = profile?.displayName?.trim() || profile?.zaloName?.trim();
      if (name) known.set(uid, name);
    }
  }
}

function uidOf(entry: string): string {
  const cut = entry.indexOf("_");
  return cut > 0 ? entry.slice(0, cut) : entry;
}
