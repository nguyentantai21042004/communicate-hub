import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";

export const MEDIA_DIR_NAME = "media";
const DIR_MODE = 0o700;
const FILE_MODE = 0o600;

export function mediaDir(dataDir: string): string {
  return path.join(dataDir, MEDIA_DIR_NAME);
}

/** Basename only, so a remote name cannot escape the media directory. */
export function safeFileName(name: string): string {
  const base = path.basename(name).replace(/[^A-Za-z0-9._-]+/g, "_");
  const trimmed = base.replace(/^\.+/, "").slice(0, 80);
  return trimmed || "file.bin";
}

export async function downloadToMedia(
  dataDir: string,
  url: string,
  fileName: string,
  fetchImpl: typeof fetch = fetch,
): Promise<string> {
  if (!/^https?:\/\//i.test(url)) throw new Error("download url is not http");
  const dir = mediaDir(dataDir);
  await mkdir(dir, { recursive: true, mode: DIR_MODE });
  const target = path.join(dir, safeFileName(fileName));
  if (path.dirname(target) !== dir) throw new Error("download name escapes media dir");
  const response = await fetchImpl(url);
  if (!response.ok) throw new Error("download failed");
  const bytes = Buffer.from(await response.arrayBuffer());
  await writeFile(target, bytes, { mode: FILE_MODE });
  return target;
}

export async function writeMediaBytes(dataDir: string, fileName: string, bytes: Buffer): Promise<string> {
  const dir = mediaDir(dataDir);
  await mkdir(dir, { recursive: true, mode: DIR_MODE });
  const target = path.join(dir, safeFileName(fileName));
  if (path.dirname(target) !== dir) throw new Error("media name escapes media dir");
  await writeFile(target, bytes, { mode: FILE_MODE });
  return target;
}
