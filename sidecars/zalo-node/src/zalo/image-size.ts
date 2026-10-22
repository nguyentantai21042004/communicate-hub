import { readFile, stat } from "node:fs/promises";

export interface ImageSize {
  width: number;
  height: number;
  size: number;
}

/** Read width and height from a PNG or JPEG header. Other files return null. */
export async function imageMetadata(filePath: string): Promise<ImageSize | null> {
  const [buf, info] = await Promise.all([readFile(filePath), stat(filePath)]);
  const png = pngSize(buf);
  if (png) return { width: png.width, height: png.height, size: info.size };
  const jpeg = jpegSize(buf);
  if (jpeg) return { width: jpeg.width, height: jpeg.height, size: info.size };
  return null;
}

function pngSize(buf: Buffer): { width: number; height: number } | undefined {
  const sig = "89504e470d0a1a0a";
  if (buf.length < 24 || buf.subarray(0, 8).toString("hex") !== sig) return undefined;
  const chunk = buf.subarray(12, 16).toString("ascii");
  if (chunk !== "IHDR") return undefined;
  return { width: buf.readUInt32BE(16), height: buf.readUInt32BE(20) };
}

function jpegSize(buf: Buffer): { width: number; height: number } | undefined {
  if (buf.length < 4 || buf[0] !== 0xff || buf[1] !== 0xd8) return undefined;
  let offset = 2;
  while (offset + 9 < buf.length) {
    if (buf[offset] !== 0xff) return undefined;
    const marker = buf[offset + 1] ?? 0;
    if (marker === 0xd8 || marker === 0xd9) {
      offset += 2;
      continue;
    }
    const length = buf.readUInt16BE(offset + 2);
    if (length < 2) return undefined;
    const isSof = marker >= 0xc0 && marker <= 0xcf && marker !== 0xc4 && marker !== 0xc8 && marker !== 0xcc;
    if (isSof && offset + 8 < buf.length) {
      return { height: buf.readUInt16BE(offset + 5), width: buf.readUInt16BE(offset + 7) };
    }
    offset += 2 + length;
  }
  return undefined;
}
