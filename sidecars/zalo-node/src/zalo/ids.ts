/** Zalo thread type on the wire. User is 0, group is 1. */
export const ZALO_THREAD_USER = 0;
export const ZALO_THREAD_GROUP = 1;

const ZALO_EMPTY_ID = "0";

export function conversationId(threadId: string, threadType: number): string {
  const kind = threadType === ZALO_THREAD_GROUP ? "group" : "dm";
  return `zalo:${kind}:${threadId}`;
}

export function parseConversation(value: string): { threadId: string; threadType: number } | undefined {
  const prefixDm = "zalo:dm:";
  const prefixGroup = "zalo:group:";
  if (value.startsWith(prefixGroup)) {
    const threadId = value.slice(prefixGroup.length);
    if (!threadId) return undefined;
    return { threadId, threadType: ZALO_THREAD_GROUP };
  }
  if (value.startsWith(prefixDm)) {
    const threadId = value.slice(prefixDm.length);
    if (!threadId) return undefined;
    return { threadId, threadType: ZALO_THREAD_USER };
  }
  return undefined;
}

/** Drop Zalo's `"0"` sentinel and blanks. */
export function realId(value: string | number | undefined | null): string | undefined {
  if (value === undefined || value === null) return undefined;
  const id = String(value).trim();
  if (!id || id === ZALO_EMPTY_ID) return undefined;
  return id;
}

/** Unix seconds. Zalo sends milliseconds. A value that is already seconds is kept. */
export function unixSeconds(ts: string | number | undefined): number {
  const n = typeof ts === "number" ? ts : Number(ts);
  if (!Number.isFinite(n) || n <= 0) return 0;
  if (n >= 1e11) return Math.floor(n / 1000);
  return Math.floor(n);
}
