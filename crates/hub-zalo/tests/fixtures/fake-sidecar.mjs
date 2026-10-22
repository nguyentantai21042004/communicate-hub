import { writeSync } from "node:fs";

const mode = process.argv[2] ?? "run";
if (mode === "crash") process.exit(1);

let mediaSeq = 0;

function writeFd(fd, value) {
  writeSync(fd, `${JSON.stringify(value)}\n`);
}

function ok(id, result) {
  writeFd(1, { id, ok: true, result });
}

function fail(id, error) {
  writeFd(1, { id, ok: false, error });
}

let buf = "";
process.stdin.on("data", (chunk) => {
  buf += chunk.toString("utf8");
  let nl = buf.indexOf("\n");
  while (nl >= 0) {
    const line = buf.slice(0, nl);
    buf = buf.slice(nl + 1);
    if (line.trim()) {
      let req;
      try {
        req = JSON.parse(line);
      } catch {
        req = undefined;
      }
      if (req) handle(req);
    }
    nl = buf.indexOf("\n");
  }
});

function handle(req) {
  const { id, method } = req;
  const params = req.params ?? {};
  if (method === "health") {
    ok(id, { listener: "up", session: "valid" });
    return;
  }
  if (method === "send.text") {
    if (params.text === "limit") {
      fail(id, { code: "rate_limited", message: "rate limited", retry_after_secs: 7 });
      return;
    }
    ok(id, { message_id: "m-sent", message_ids: ["m-sent"] });
    return;
  }
  if (method === "send.media") {
    mediaSeq += 1;
    const messageId = `m-file-${mediaSeq}`;
    ok(id, { message_id: messageId, message_ids: [messageId] });
    return;
  }
  if (method === "send.reaction" || method === "send.unsend" || method === "login.qr.cancel") {
    ok(id, {});
    return;
  }
  if (method === "test.crash") process.exit(1);
  if (method === "test.hang") return;
  if (method === "test.malformed") {
    writeSync(1, "this is not json\n");
    ok(id, {});
    return;
  }
  if (method === "test.stderr") {
    writeFd(2, { id, ok: true, result: { from: "stderr" } });
    ok(id, { from: "stdout" });
    return;
  }
  if (method === "test.emit") {
    writeFd(1, { event: params.event, data: params.data });
    ok(id, {});
    return;
  }
  fail(id, { code: "unsupported", message: "unsupported" });
}
