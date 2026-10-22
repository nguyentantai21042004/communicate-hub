import { SidecarError, classifyError } from "./errors.js";
import { isMethodName, type ErrorCode } from "./contract.js";

/** Largest accepted frame. A longer line is discarded through the next newline. */
export const DEFAULT_MAX_FRAME_BYTES = 8 * 1024 * 1024;

export interface RpcRequest {
  id: number;
  method: string;
  params: unknown;
}

export interface RpcErrorBody {
  code: ErrorCode;
  message: string;
  retry_after_secs?: number;
}

export type RpcResponse =
  | { id: number; ok: true; result: unknown }
  | { id: number; ok: false; error: RpcErrorBody };

export type Frame =
  | { kind: "line"; line: string }
  | { kind: "too_long"; id: number | undefined };

/**
 * Splits a byte stream into lines.
 *
 * A line over `maxBytes` is reported as `too_long` and is not parsed as JSON.
 * Bytes after an oversized line stay available for the next frame.
 */
export class LineBuffer {
  private pending = "";
  private overflow = false;

  constructor(private readonly maxBytes: number) {}

  push(chunk: string): Frame[] {
    const out: Frame[] = [];
    let rest = chunk;
    if (this.overflow) {
      const nl = rest.indexOf("\n");
      if (nl < 0) return out;
      rest = rest.slice(nl + 1);
      this.overflow = false;
      out.push({ kind: "too_long", id: undefined });
    }
    this.pending += rest;
    while (true) {
      const nl = this.pending.indexOf("\n");
      if (nl < 0) {
        if (Buffer.byteLength(this.pending) > this.maxBytes) {
          this.pending = "";
          this.overflow = true;
        }
        break;
      }
      const line = this.pending.slice(0, nl);
      this.pending = this.pending.slice(nl + 1);
      if (line.length === 0) continue;
      if (Buffer.byteLength(line) > this.maxBytes) {
        out.push({ kind: "too_long", id: idHint(line) });
        continue;
      }
      out.push({ kind: "line", line });
    }
    return out;
  }
}

export function parseRequest(line: string): { ok: true; request: RpcRequest } | { ok: false; id: number | undefined } {
  let value: unknown;
  try {
    value = JSON.parse(line) as unknown;
  } catch {
    return { ok: false, id: undefined };
  }
  if (!value || typeof value !== "object") return { ok: false, id: undefined };
  const rec = value as Record<string, unknown>;
  const id = rec["id"];
  if (typeof id !== "number" || !Number.isInteger(id) || id < 0) {
    return { ok: false, id: undefined };
  }
  const method = rec["method"];
  if (typeof method !== "string" || method.length === 0) {
    return { ok: false, id };
  }
  return { ok: true, request: { id, method, params: rec["params"] === undefined ? {} : rec["params"] } };
}

export type Handler = (params: unknown) => Promise<unknown>;

export async function dispatch(request: RpcRequest, handlers: ReadonlyMap<string, Handler>): Promise<RpcResponse> {
  if (!isMethodName(request.method) || !handlers.has(request.method)) {
    return failure(request.id, new SidecarError("unsupported"));
  }
  const handler = handlers.get(request.method);
  if (!handler) return failure(request.id, new SidecarError("unsupported"));
  try {
    const result = await handler(request.params);
    return { id: request.id, ok: true, result };
  } catch (err) {
    return failure(request.id, classifyError(err));
  }
}

export function failure(id: number, err: SidecarError): RpcResponse {
  const error: RpcErrorBody = { code: err.code, message: err.message };
  if (err.retryAfterSecs !== undefined) error.retry_after_secs = err.retryAfterSecs;
  return { id, ok: false, error };
}

export function encodeLine(value: unknown): string {
  return `${JSON.stringify(value)}\n`;
}

export interface StdioEnds {
  input: AsyncIterable<Buffer | string>;
  write: (line: string) => void;
}

/**
 * Reads requests and writes responses. Handlers run concurrently so a slow
 * call does not hold up the id that finished first. Writes are ordered so two
 * frames cannot interleave.
 */
export async function runStdio(
  input: AsyncIterable<Buffer | string>,
  write: (line: string) => void,
  handlers: ReadonlyMap<string, Handler>,
  maxBytes = DEFAULT_MAX_FRAME_BYTES,
): Promise<void> {
  const buffer = new LineBuffer(maxBytes);
  let chain: Promise<void> = Promise.resolve();
  const writeJson = (value: unknown): void => {
    const line = encodeLine(value);
    chain = chain.then(() => {
      write(line);
    });
  };
  const pending: Array<Promise<void>> = [];

  for await (const chunk of input) {
    const text = typeof chunk === "string" ? chunk : chunk.toString("utf8");
    for (const frame of buffer.push(text)) {
      if (frame.kind === "too_long") {
        if (frame.id !== undefined) writeJson(failure(frame.id, new SidecarError("internal")));
        continue;
      }
      const parsed = parseRequest(frame.line);
      if (!parsed.ok) {
        if (parsed.id !== undefined) writeJson(failure(parsed.id, new SidecarError("internal")));
        continue;
      }
      const request = parsed.request;
      pending.push(
        dispatch(request, handlers).then((response) => {
          writeJson(response);
        }),
      );
    }
  }
  await Promise.all(pending);
  await chain;
}

function idHint(line: string): number | undefined {
  const match = /"id"\s*:\s*(\d+)/.exec(line);
  if (!match?.[1]) return undefined;
  const id = Number(match[1]);
  return Number.isInteger(id) ? id : undefined;
}
