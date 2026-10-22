/** Zalo rejects one text message past this many characters. */
export const ZALO_TEXT_LIMIT = 2000;

/**
 * Split `text` on a newline, then a space, when the cut stays in the second
 * half of the window. Matches the chat-bridges splitter.
 */
export function splitText(text: string, limit = ZALO_TEXT_LIMIT): string[] {
  if (text.length <= limit) return text ? [text] : [];

  const chunks: string[] = [];
  let rest = text;
  while (rest.length > limit) {
    const window = rest.slice(0, limit);
    let cut = window.lastIndexOf("\n");
    if (cut < limit * 0.5) cut = window.lastIndexOf(" ");
    if (cut < limit * 0.5) cut = limit;
    chunks.push(rest.slice(0, cut));
    rest = rest.slice(cut).replace(/^[ \n]/, "");
  }
  if (rest) chunks.push(rest);
  return chunks;
}
