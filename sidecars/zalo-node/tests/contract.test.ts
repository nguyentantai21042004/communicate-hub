import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { ERROR_CODES, EVENT_NAMES, METHOD_NAMES } from "../src/contract.js";

const docPath = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../../docs/design/zalo-sidecar.md");

function section(marker: string): string[] {
  const doc = readFileSync(docPath, "utf8");
  const after = doc.split(marker)[1];
  if (!after) throw new Error(`missing ${marker}`);
  const fenced = after.split("```json")[1];
  if (!fenced) throw new Error(`missing json fence after ${marker}`);
  const body = fenced.split("```")[0];
  if (!body) throw new Error(`empty fence after ${marker}`);
  return JSON.parse(body) as string[];
}

describe("contract names", () => {
  it("matches the design doc lists", () => {
    expect([...METHOD_NAMES]).toEqual(section("<!-- contract:methods -->"));
    expect([...EVENT_NAMES]).toEqual(section("<!-- contract:events -->"));
    expect([...ERROR_CODES]).toEqual(section("<!-- contract:errors -->"));
  });
});
