import { describe, expect, it } from "vitest";

import { bankCardText, parseBankCardHtml } from "../src/zalo/bank.js";

describe("bank card html", () => {
  it("reads the BIN, bank, account, holder, and vietqr url", () => {
    const html = "<p>970436</p><p>Vietcombank</p><p>0123456789</p><p>NGUYEN A</p> content=https://img.vietqr.io/image/970436-1";
    const info = parseBankCardHtml(html);
    expect(info).toEqual({
      bankName: "Vietcombank",
      accountNumber: "0123456789",
      holderName: "NGUYEN A",
      vietqr: "https://img.vietqr.io/image/970436-1",
    });
    expect(bankCardText(info!)).toBe("Vietcombank\n0123456789\nNGUYEN A");
  });

  it("returns null when the vietqr url is missing", () => {
    expect(parseBankCardHtml("<p>Vietcombank</p>")).toBeNull();
  });
});
