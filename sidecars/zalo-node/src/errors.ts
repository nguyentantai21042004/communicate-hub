import { ERROR_MESSAGE, type ErrorCode } from "./contract.js";

/** Failure the RPC layer turns into `{ ok: false, error }`. */
export class SidecarError extends Error {
  readonly code: ErrorCode;
  readonly retryAfterSecs: number | undefined;

  constructor(code: ErrorCode, retryAfterSecs?: number) {
    super(ERROR_MESSAGE[code]);
    this.name = "SidecarError";
    this.code = code;
    this.retryAfterSecs = retryAfterSecs;
  }
}

export const RATE_LIMIT_CODE = 221;
export const DEFAULT_RETRY_AFTER_SECS = 30;

export function notLoggedIn(): SidecarError {
  return new SidecarError("auth_expired");
}

/** Map a zca-js failure onto the closed error list. The raw message is not kept. */
export function classifyError(err: unknown): SidecarError {
  if (err instanceof SidecarError) return err;
  const code = numericCode(err);
  if (code === RATE_LIMIT_CODE) return new SidecarError("rate_limited", DEFAULT_RETRY_AFTER_SECS);
  if (code === 404) return new SidecarError("not_found");
  const name = err instanceof Error ? err.name : "";
  if (name === "ZaloApiLoginQRDeclined" || name === "ZaloApiLoginQRAborted") {
    return new SidecarError("auth_expired");
  }
  return new SidecarError("internal");
}

function numericCode(err: unknown): number | undefined {
  if (!err || typeof err !== "object" || !("code" in err)) return undefined;
  const code = err.code;
  return typeof code === "number" ? code : undefined;
}
