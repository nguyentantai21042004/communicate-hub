import { describe, expect, it } from "vitest";

import { SidecarError } from "../src/errors.js";
import { DIRECTORY_TTL_MS, DirectoryBook, type DirectoryBookSource } from "../src/zalo/directory-book.js";
import type { DirectoryEntry } from "../src/zalo/members.js";

function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void; reject: (err: unknown) => void } {
  let resolve: (value: T) => void = () => {};
  let reject: (err: unknown) => void = () => {};
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe("directory book", () => {
  it("fetches once per start, once for an unknown id, and once when the ttl expires", async () => {
    let now = 1_000;
    let friends = 0;
    let groups = 0;
    const source: DirectoryBookSource = {
      async listFriends() {
        friends += 1;
        return [{ id: "u1", name: "An", avatar_url: "https://cdn.example/u1.jpg" }];
      },
      async listGroups() {
        groups += 1;
        return [{ id: "g1", name: "Team", avatar_url: "https://cdn.example/g1.jpg" }];
      },
    };
    const book = new DirectoryBook(source, DIRECTORY_TTL_MS, () => now);
    book.warm();
    await book.lookup("dm", "u1");
    await book.lookup("group", "g1");
    expect(friends).toBe(1);
    expect(groups).toBe(1);
    expect((await book.lookup("dm", "u1"))?.avatar_url).toBe("https://cdn.example/u1.jpg");

    expect(await book.lookup("dm", "stranger")).toBeUndefined();
    expect(friends).toBe(2);
    expect(await book.lookup("group", "other")).toBeUndefined();
    expect(friends).toBe(2);

    now += DIRECTORY_TTL_MS;
    await book.lookup("dm", "u1");
    expect(friends).toBe(3);
    expect(groups).toBe(3);
  });

  it("shares one in-flight fetch and backs off on rate_limited", async () => {
    let now = 0;
    let friends = 0;
    const gate = deferred<DirectoryEntry[]>();
    const source: DirectoryBookSource = {
      listFriends() {
        friends += 1;
        return gate.promise;
      },
      async listGroups() {
        return [];
      },
    };
    const book = new DirectoryBook(source, DIRECTORY_TTL_MS, () => now);
    const first = book.lookup("dm", "u1");
    const second = book.lookup("dm", "u2");
    gate.resolve([{ id: "u1", name: "An" }]);
    await Promise.all([first, second]);
    expect(friends).toBe(1);

    const limited: DirectoryBookSource = {
      async listFriends() {
        friends += 1;
        throw new SidecarError("rate_limited", 30);
      },
      async listGroups() {
        throw new Error("groups should not run");
      },
    };
    const blocked = new DirectoryBook(limited, DIRECTORY_TTL_MS, () => now);
    expect(await blocked.lookup("dm", "u1")).toBeUndefined();
    const afterLimit = friends;
    now += 1_000;
    expect(await blocked.lookup("dm", "u1")).toBeUndefined();
    expect(friends).toBe(afterLimit);
    now += 30_000;
    expect(await blocked.lookup("dm", "u1")).toBeUndefined();
    expect(friends).toBe(afterLimit + 1);
  });
});
