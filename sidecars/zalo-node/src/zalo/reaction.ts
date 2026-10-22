/** Emoji Telegram accepts in `setMessageReaction` for a bot. */
export const TELEGRAM_ALLOWED_REACTIONS: ReadonlySet<string> = new Set([
  "👍", "👎", "❤️", "🔥", "🥰", "👏", "😁", "🤔", "🤯", "😱", "🤬", "😢", "🎉", "🤩",
  "🤮", "💩", "🙏", "👌", "🕊", "🤡", "🥱", "🥴", "😍", "🐳", "❤‍🔥", "🌚", "🌭", "💯",
  "🤣", "⚡", "🍌", "🏆", "💔", "🤨", "😐", "🍓", "🍾", "💋", "🖕", "😈", "😴", "😭",
  "🤓", "👻", "👨‍💻", "👀", "🎃", "🙈", "😇", "😨", "🤝", "✍", "🤗", "🫡", "🎅", "🎄",
  "☃", "💅", "🤪", "🗿", "🆒", "💘", "🙉", "🦄", "😘", "💊", "🙊", "😎", "👾", "🤷‍♂",
  "🤷", "🤷‍♀", "😡",
]);

/** Zalo icon code to the closest permitted Telegram emoji. */
const ZALO_ICON_TO_EMOJI: Readonly<Record<string, string>> = {
  "/-heart": "❤️",
  "/-strong": "👍",
  "/-weak": "👎",
  ":>": "😁",
  ":o": "😱",
  ":-((": "😢",
  ":((": "😭",
  ":-h": "😡",
  ":-*": "😘",
  ";')": "🤣",
  "':>": "🤣",
  "/-shit": "💩",
  "/-rose": "🥰",
  "/-break": "💔",
  ";xx": "🥰",
  ";-/": "🤔",
  ";-)": "😁",
  "/-fade": "⚡",
  "/-ok": "👌",
  "/-v": "👌",
  "/-thanks": "🙏",
  "_()_": "🙏",
  "/-punch": "🔥",
  "/-no": "🤷",
  "/-loveu": "🤝",
  "--b": "😐",
  "x-)": "😎",
  "/-bd": "🎉",
};

/** Telegram emoji to the Zalo icon sent on the way back. */
const EMOJI_TO_ZALO_ICON: Readonly<Record<string, string>> = {
  "❤️": "/-heart",
  "❤": "/-heart",
  "🔥": "/-heart",
  "👍": "/-strong",
  "👎": "/-weak",
  "😁": ":>",
  "😱": ":o",
  "🤯": ":o",
  "😢": ":-((",
  "😭": ":((",
  "😡": ":-h",
  "🤬": ":-h",
  "😘": ":-*",
  "🥰": ";xx",
  "😍": ";xx",
  "🤣": ":')",
  "💩": "/-shit",
  "💔": "/-break",
  "🤔": ";-/",
  "🤨": ";-/",
  "👌": "/-ok",
  "🙏": "_()_",
  "🤷": "/-no",
  "🤝": "/-loveu",
  "😐": "--b",
  "😎": "x-)",
  "🤩": "x-)",
  "🎉": "/-bd",
  "💯": "/-strong",
  "👏": "/-strong",
};

/** Emoji to show in Telegram, or undefined when the icon is outside the set. */
export function emojiForZaloIcon(icon: string): string | undefined {
  const emoji = ZALO_ICON_TO_EMOJI[icon];
  return emoji && TELEGRAM_ALLOWED_REACTIONS.has(emoji) ? emoji : undefined;
}

/** Zalo icon for a Telegram reaction, or undefined when Zalo has no match. */
export function zaloIconForEmoji(emoji: string): string | undefined {
  return EMOJI_TO_ZALO_ICON[emoji];
}
