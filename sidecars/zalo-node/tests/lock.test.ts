import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawn, spawnSync } from "node:child_process";

import { afterEach, describe, expect, it } from "vitest";

import { acquireListenerLock, LOCK_FILE_NAME, releaseListenerLock } from "../src/lock.js";

const dirs: string[] = [];

function tempDir(): string {
  const dir = mkdtempSync(path.join(tmpdir(), "zalo-lock-"));
  dirs.push(dir);
  return dir;
}

afterEach(() => {
  releaseListenerLock();
  for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

describe("listener lock", () => {
  it("refuses a second acquire in this process", () => {
    const dir = tempDir();
    acquireListenerLock(dir);
    expect(() => acquireListenerLock(dir)).toThrow(/listener lock is held/);
  });

  it("replaces a dead pid and refuses a live one", () => {
    const stale = tempDir();
    const finished = spawnSync(process.execPath, ["-e", "process.exit(0)"]);
    writeFileSync(path.join(stale, LOCK_FILE_NAME), String(finished.pid), { mode: 0o600 });
    acquireListenerLock(stale);
    releaseListenerLock();

    const live = tempDir();
    const child = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"], { stdio: "ignore" });
    writeFileSync(path.join(live, LOCK_FILE_NAME), String(child.pid), { mode: 0o600 });
    try {
      expect(() => acquireListenerLock(live)).toThrow(/listener lock is held/);
    } finally {
      child.kill("SIGTERM");
    }
  });
});
