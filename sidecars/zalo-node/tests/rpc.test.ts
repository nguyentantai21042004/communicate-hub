import { PassThrough } from "node:stream";

import { describe, expect, it } from "vitest";

import { LineBuffer, parseRequest, runStdio, type Handler } from "../src/rpc.js";

function handlers(): Map<string, Handler> {
  const map = new Map<string, Handler>();
  map.set("health", async () => ({ listener: "down", session: "disconnected" }));
  map.set("login.qr.cancel", async () => ({}));
  return map;
}

describe("rpc framing", () => {
  it("ignores a malformed line and answers a bad request that still has an id", async () => {
    const input = new PassThrough();
    const lines: string[] = [];
    const done = runStdio(input, (line) => lines.push(line), handlers());
    input.write("not json\n");
    input.write('{"method":"health"}\n');
    input.write('{"id":3,"method":1}\n');
    input.end();
    await done;
    expect(lines).toHaveLength(1);
    expect(JSON.parse(lines[0] ?? "{}")).toMatchObject({ id: 3, ok: false, error: { code: "internal" } });
  });

  it("waits for a partial line and correlates overlapping calls by id", async () => {
    let release: () => void = () => {};
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const map = new Map<string, Handler>();
    map.set("health", async () => {
      await gate;
      return { listener: "up", session: "valid" };
    });
    map.set("login.qr.cancel", async () => ({ cancelled: true }));
    const input = new PassThrough();
    const lines: string[] = [];
    const done = runStdio(input, (line) => lines.push(line), map);
    input.write('{"id":1,"meth');
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(lines).toHaveLength(0);
    input.write('od":"health","params":{}}\n');
    input.write('{"id":2,"method":"login.qr.cancel","params":{}}\n');
    await waitFor(() => lines.length >= 1);
    expect(JSON.parse(lines[0] ?? "{}").id).toBe(2);
    release();
    input.end();
    await done;
    expect(JSON.parse(lines[1] ?? "{}").id).toBe(1);
  });

  it("rejects an unknown method and an oversized line", async () => {
    const input = new PassThrough();
    const lines: string[] = [];
    const done = runStdio(input, (line) => lines.push(line), handlers(), 64);
    input.write('{"id":4,"method":"nope","params":{}}\n');
    const huge = `{"id":5,"method":"health","params":{"pad":"${"x".repeat(80)}"}}`;
    input.write(`${huge}\n`);
    input.end();
    await done;
    const byId = new Map(lines.map((line) => { const body = JSON.parse(line) as { id: number }; return [body.id, JSON.parse(line) as { error: { code: string } }]; }));
    expect(byId.get(4)?.error.code).toBe("unsupported");
    expect(byId.get(5)?.error.code).toBe("internal");
  });

  it("splits a chunk that ends mid-line", () => {
    const buffer = new LineBuffer(100);
    expect(buffer.push('{"id":1')).toEqual([]);
    expect(buffer.push('}\n')[0]).toEqual({ kind: "line", line: '{"id":1}' });
    expect(parseRequest('{"id":1}').ok).toBe(false);
  });
});

async function waitFor(ready: () => boolean): Promise<void> {
  for (let i = 0; i < 50; i += 1) {
    if (ready()) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  throw new Error("timed out waiting for a frame");
}