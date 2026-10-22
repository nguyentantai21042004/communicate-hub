import { describe, expect, it } from "vitest";

import type { AppMemberApi } from "../src/zalo/app-api.js";
import type { GroupInfoRecord, UserProfiles, ZaloApi } from "../src/zalo/api.js";
import { GroupMemberCache, loadGroupMembers } from "../src/zalo/members.js";

function app(info: GroupInfoRecord | null, names?: Map<string, string>): AppMemberApi & { groupCalls: number } {
  return {
    groupCalls: 0,
    async groupInfo() {
      this.groupCalls += 1;
      return info;
    },
    async memberNames() {
      return names ?? null;
    },
  };
}

describe("group members", () => {
  it("prefers the app payload and caches it until the ttl", async () => {
    const source = app(
      { currentMems: [{ id: "1", dName: "An" }], memVerList: ["1_0", "2_3"] },
      new Map([["2", "Binh"]]),
    );
    let webCalls = 0;
    const web = {
      async getGroupInfo(): Promise<{ gridInfoMap?: Record<string, GroupInfoRecord> }> {
        webCalls += 1;
        return {};
      },
      async getUserInfo(): Promise<UserProfiles> {
        return {};
      },
    };
    const members = await loadGroupMembers("g", web, source);
    expect(webCalls).toBe(0);
    expect(members).toEqual([
      { id: "1", name: "An" },
      { id: "2", name: "Binh" },
    ]);

    let now = 0;
    let loads = 0;
    const cache = new GroupMemberCache(
      {
        async load() {
          loads += 1;
          return members;
        },
      },
      1000,
      () => now,
    );
    await cache.list("g");
    await cache.list("g");
    expect(loads).toBe(1);
    now = 1000;
    await cache.list("g");
    expect(loads).toBe(2);
  });

  it("drops the least recently used group past the cap", async () => {
    const loads: string[] = [];
    const cache = new GroupMemberCache(
      {
        async load(groupId) {
          loads.push(groupId);
          return [{ id: groupId, name: groupId }];
        },
      },
      10_000,
      () => 0,
      2,
    );
    await cache.list("a");
    await cache.list("b");
    await cache.list("a");
    await cache.list("c");
    await cache.list("a");
    await cache.list("b");
    expect(loads).toEqual(["a", "b", "c", "b"]);
  });

  it("falls back to the web api when the app session is absent", async () => {
    const web: Pick<ZaloApi, "getGroupInfo" | "getUserInfo"> = {
      async getGroupInfo() {
        return { gridInfoMap: { g: { memVerList: ["9_0"], currentMems: [] } } };
      },
      async getUserInfo(): Promise<UserProfiles> {
        return { changed_profiles: { "9_0": { displayName: "Chi" } } };
      },
    };
    const members = await loadGroupMembers("g", web, app(null));
    expect(members).toEqual([{ id: "9", name: "Chi" }]);
  });
});
