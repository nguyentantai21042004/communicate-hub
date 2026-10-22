import { randomBytes } from "node:crypto";
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import { describe, expect, it } from "vitest";

import { decodeAes, encodeAes, loadAppSession } from "../src/zalo/app-api.js";

describe("app session cipher", () => {
  it("round-trips AES-CBC the way the PC client encodes params", () => {
    const key = randomBytes(32).toString("base64");
    const plain = JSON.stringify({ gridVerMap: "{\"g\":0}" });
    expect(decodeAes(encodeURIComponent(encodeAes(plain, key)), key)).toBe(plain);
    const key128 = randomBytes(16).toString("base64");
    expect(decodeAes(encodeURIComponent(encodeAes("abc", key128)), key128)).toBe("abc");
  });

  it("tightens a credentials-adjacent session file this process owns", () => {
    const dir = mkdtempSync(path.join(tmpdir(), "zalo-app-"));
    const file = path.join(dir, "app-session.json");
    try {
      writeFileSync(file, JSON.stringify({ zpw_enk: "k", imei: "i", cookies: [] }), { mode: 0o644 });
      const session = loadAppSession(file);
      expect(session).toEqual({ zpw_enk: "k", imei: "i", cookies: [] });
      chmodSync(file, 0o600);
      expect(loadAppSession(file)?.imei).toBe("i");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
