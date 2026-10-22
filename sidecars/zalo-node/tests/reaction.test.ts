import { describe, expect, it } from "vitest";

import { emojiForZaloIcon, TELEGRAM_ALLOWED_REACTIONS, zaloIconForEmoji } from "../src/zalo/reaction.js";

describe("reaction map", () => {
  it("round-trips the core icons onto the Telegram set", () => {
    for (const icon of ["/-heart", "/-strong", "/-weak", ":-*", "/-break"]) {
      const emoji = emojiForZaloIcon(icon);
      expect(emoji).toBeTruthy();
      expect(TELEGRAM_ALLOWED_REACTIONS.has(emoji ?? "")).toBe(true);
      expect(zaloIconForEmoji(emoji ?? "")).toBe(icon);
    }
  });

  it("maps both heart glyphs to the same icon and the cry glyph with two parens", () => {
    expect(zaloIconForEmoji("❤️")).toBe("/-heart");
    expect(zaloIconForEmoji("❤")).toBe("/-heart");
    expect(zaloIconForEmoji("😢")).toBe(":-((");
    expect(emojiForZaloIcon(":-(( ")).toBeUndefined();
    expect(emojiForZaloIcon(":-((")).toBe("😢");
  });

  it("drops an icon outside the allowed set", () => {
    expect(emojiForZaloIcon("not-an-icon")).toBeUndefined();
    expect(zaloIconForEmoji("not-emoji")).toBeUndefined();
  });
});
