export interface BankCardInfo {
  bankName: string;
  accountNumber: string;
  holderName?: string;
  vietqr: string;
}

/**
 * Pull a bank card out of the HTML Zalo serves for `zinstant.bankcard`.
 * The `<p>` order is BIN, bank name, account number, then the holder.
 */
export function parseBankCardHtml(html: string): BankCardInfo | null {
  const ptags = [...html.matchAll(/<p[^>]*>([^<]+)<\/p>/g)]
    .map((match) => match[1]?.trim() ?? "")
    .filter((text) => text.length > 0);

  const normalised = html.replace(/&amp;/g, "&");
  const contentMatch = /content=([^&"< ]+)/.exec(normalised);
  const raw = contentMatch?.[1];
  if (!raw) return null;
  const vietqr = decodeURIComponent(raw);
  if (!vietqr) return null;

  const numericTags = ptags.filter((text) => /^\d+$/.test(text));
  const textTags = ptags.filter((text) => !/^\d+$/.test(text));
  const accountNumber = numericTags.find((text) => text.length !== 6) ?? numericTags[1] ?? numericTags[0] ?? "";
  const bankName = textTags[0] ?? "";
  const holderName = textTags[1]?.trim() || undefined;
  return { bankName, accountNumber, holderName, vietqr };
}

export function bankCardText(info: BankCardInfo): string {
  return [info.bankName, info.accountNumber, info.holderName].filter((part) => part && part.trim()).join("\n");
}
