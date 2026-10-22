import { classifyError, DEFAULT_RETRY_AFTER_SECS } from "../errors.js";
import type { DirectoryEntry } from "./members.js";

/** How long a friends and groups snapshot is reused. */
export const DIRECTORY_TTL_MS = 24 * 60 * 60 * 1000;

export interface DirectoryBookSource {
  listFriends(): Promise<DirectoryEntry[]>;
  listGroups(): Promise<DirectoryEntry[]>;
}

/**
 * Friends and groups fetched once per start, then at most once per TTL, plus one
 * extra fetch when an event names an id the snapshot does not have.
 * No timer. Callers invoke {@link DirectoryBook.lookup} from an event.
 */
export class DirectoryBook {
  private friends = new Map<string, DirectoryEntry>();
  private groups = new Map<string, DirectoryEntry>();
  private fetchedAt = 0;
  private backoffUntil = 0;
  private unknownUsed = false;
  private readonly misses = new Set<string>();
  private inFlight: Promise<boolean> | undefined;

  constructor(
    private readonly source: DirectoryBookSource,
    private readonly ttlMs: number,
    private readonly now: () => number,
  ) {}

  /** Starts the once-per-process fetch. Failures stay inside the book. */
  warm(): void {
    void this.refresh();
  }

  async lookup(kind: "dm" | "group", id: string): Promise<DirectoryEntry | undefined> {
    if (this.now() < this.backoffUntil) return this.get(kind, id);
    const stale = this.fetchedAt === 0 || this.now() - this.fetchedAt >= this.ttlMs;
    if (stale) {
      await this.refresh();
      return this.get(kind, id);
    }
    const hit = this.get(kind, id);
    if (hit) return hit;
    const key = `${kind}:${id}`;
    if (this.misses.has(key) || this.unknownUsed) return undefined;
    const ok = await this.refresh();
    this.unknownUsed = true;
    const found = this.get(kind, id);
    if (ok && !found) this.misses.add(key);
    return found;
  }

  private get(kind: "dm" | "group", id: string): DirectoryEntry | undefined {
    return (kind === "dm" ? this.friends : this.groups).get(id);
  }

  private refresh(): Promise<boolean> {
    if (this.inFlight) return this.inFlight;
    const run = this.fetch().finally(() => {
      this.inFlight = undefined;
    });
    this.inFlight = run;
    return run;
  }

  private async fetch(): Promise<boolean> {
    let friends: DirectoryEntry[] | undefined;
    try {
      friends = await this.source.listFriends();
      const groups = await this.source.listGroups();
      this.friends = index(friends);
      this.groups = index(groups);
      this.fetchedAt = this.now();
      this.unknownUsed = false;
      this.misses.clear();
      return true;
    } catch (err) {
      if (friends) this.friends = index(friends);
      const classified = classifyError(err);
      const secs =
        classified.code === "rate_limited"
          ? (classified.retryAfterSecs ?? DEFAULT_RETRY_AFTER_SECS)
          : DEFAULT_RETRY_AFTER_SECS;
      this.backoffUntil = this.now() + secs * 1000;
      return false;
    }
  }
}

function index(entries: DirectoryEntry[]): Map<string, DirectoryEntry> {
  const map = new Map<string, DirectoryEntry>();
  for (const entry of entries) {
    if (entry.id) map.set(entry.id, entry);
  }
  return map;
}
