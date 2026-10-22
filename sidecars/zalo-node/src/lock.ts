import { closeSync, mkdirSync, openSync, readFileSync, unlinkSync, writeSync } from "node:fs";
import path from "node:path";

export const LOCK_FILE_NAME = "listener.lock";
const DIR_MODE = 0o700;
const FILE_MODE = 0o600;

let held: { path: string; fd: number } | undefined;

/**
 * Refuses when another live process already recorded its pid for this data
 * directory. A dead pid is replaced. The held descriptor stays open until
 * {@link releaseListenerLock}.
 */
export function acquireListenerLock(dataDir: string): void {
  mkdirSync(dataDir, { recursive: true, mode: DIR_MODE });
  const lockPath = path.join(dataDir, LOCK_FILE_NAME);
  if (held?.path === lockPath) {
    throw new Error("refusing to start: listener lock is held");
  }
  for (let attempt = 0; attempt < 2; attempt += 1) {
    try {
      const fd = openSync(lockPath, "wx", FILE_MODE);
      writeSync(fd, String(process.pid));
      held = { path: lockPath, fd };
      return;
    } catch (err) {
      const code = (err as NodeJS.ErrnoException).code;
      if (code !== "EEXIST") throw err;
      if (lockHeldByLiveProcess(lockPath)) {
        throw new Error("refusing to start: listener lock is held");
      }
      try {
        unlinkSync(lockPath);
      } catch {
        throw new Error("refusing to start: listener lock is held");
      }
    }
  }
  throw new Error("refusing to start: listener lock is held");
}

export function releaseListenerLock(): void {
  if (!held) return;
  const current = held;
  held = undefined;
  try {
    closeSync(current.fd);
  } catch {
    // The descriptor may already be closed on the way out.
  }
  try {
    unlinkSync(current.path);
  } catch {
    // A crash after unlink is fine. The next start treats a dead pid as stale.
  }
}

function lockHeldByLiveProcess(lockPath: string): boolean {
  let text = "";
  try {
    text = readFileSync(lockPath, "utf8");
  } catch {
    return false;
  }
  const pid = Number(text.trim());
  if (!Number.isInteger(pid) || pid <= 0) return false;
  if (pid === process.pid) return true;
  return isAlive(pid);
}

function isAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (err) {
    return (err as NodeJS.ErrnoException).code === "EPERM";
  }
}
