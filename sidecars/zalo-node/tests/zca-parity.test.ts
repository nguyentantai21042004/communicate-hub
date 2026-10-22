import path from "node:path";
import { pathToFileURL } from "node:url";

import { describe, expect, it } from "vitest";

import { FRIEND_REQUEST_TYPE } from "../src/zalo/api.js";
import { groupKindOf } from "../src/zalo/normalize.js";

const root = path.resolve(import.meta.dirname, "../node_modules/zca-js/dist/models");

describe("zca-js enums", () => {
  it("uses the package friend-request value", async () => {
    const friend = (await import(pathToFileURL(path.join(root, "FriendEvent.js")).href)) as {
      FriendEventType: { REQUEST: number };
    };
    expect(friend.FriendEventType.REQUEST).toBe(FRIEND_REQUEST_TYPE);
  });

  it("maps every group event type onto a contract kind", async () => {
    const group = (await import(pathToFileURL(path.join(root, "GroupEvent.js")).href)) as {
      GroupEventType: Record<string, string>;
    };
    for (const value of Object.values(group.GroupEventType)) {
      if (typeof value !== "string") continue;
      expect(typeof groupKindOf(value)).toBe("string");
    }
  });
});
