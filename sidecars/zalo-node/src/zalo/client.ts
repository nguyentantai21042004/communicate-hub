import { chmodSync, existsSync, mkdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";

import { LoginQRCallbackEventType, Zalo, type Credentials } from "zca-js";

import { SidecarError } from "../errors.js";
import { logError, logInfo } from "../log.js";
import { imageMetadata } from "./image-size.js";
import type { QrHooks, ZaloApi, ZaloSession } from "./api.js";

const CREDENTIALS_MODE = 0o600;
const DIR_MODE = 0o700;

export interface ZaloClientOptions {
  credentialsPath: string;
  imageMetadataGetter?: (filePath: string) => Promise<{ width: number; height: number; size: number } | null>;
}

/**
 * The only module that imports zca-js. Credentials are written mode 0600 and
 * are never logged.
 */
export class ZaloClient implements ZaloSession {
  private api: ZaloApi | undefined;
  private qrAbort: (() => void) | undefined;
  private qrCancelled = false;

  constructor(private readonly options: ZaloClientOptions) {}

  hasCredentials(): boolean {
    return existsSync(this.options.credentialsPath);
  }

  current(): ZaloApi | undefined {
    return this.api;
  }

  require(): ZaloApi {
    if (!this.api) throw new SidecarError("auth_expired");
    return this.api;
  }

  reset(): void {
    this.stopListener();
    this.api = undefined;
  }

  cancelQr(): void {
    this.qrCancelled = true;
    const abort = this.qrAbort;
    this.qrAbort = undefined;
    abort?.();
  }

  async loginWithSavedCredentials(): Promise<ZaloApi> {
    if (!this.hasCredentials()) throw new SidecarError("auth_expired");
    const credentials = readCredentials(this.options.credentialsPath);
    const zalo = this.newZalo();
    this.api = asApi(await zalo.login(credentials));
    logInfo("logged in with saved credentials");
    return this.api;
  }

  async loginWithQr(hooks: QrHooks): Promise<ZaloApi> {
    this.stopListener();
    this.api = undefined;
    this.qrCancelled = false;
    this.qrAbort = undefined;
    const zalo = this.newZalo();
    const api = await zalo.loginQR({}, (event) => {
      if (this.qrCancelled) {
        if (event.actions) event.actions.abort();
        return;
      }
      if (event.actions) this.qrAbort = () => event.actions?.abort();
      switch (event.type) {
        case LoginQRCallbackEventType.QRCodeGenerated: {
          const image = event.data.image.replace(/^data:image\/png;base64,/, "");
          if (image) void hooks.onQrCode(Buffer.from(image, "base64"));
          break;
        }
        case LoginQRCallbackEventType.QRCodeExpired:
          void hooks.onExpired?.();
          event.actions.retry();
          break;
        case LoginQRCallbackEventType.QRCodeScanned:
          void hooks.onScanned?.(event.data.display_name ?? "");
          break;
        case LoginQRCallbackEventType.QRCodeDeclined:
          void hooks.onDeclined?.();
          event.actions.abort();
          break;
        case LoginQRCallbackEventType.GotLoginInfo:
          this.saveCredentials(event.data);
          break;
        default:
          break;
      }
    });
    this.api = asApi(api);
    logInfo("qr login succeeded");
    return this.api;
  }

  private saveCredentials(data: { imei: string; cookie: unknown; userAgent: string }): void {
    const target = this.options.credentialsPath;
    try {
      mkdirSync(path.dirname(target), { recursive: true, mode: DIR_MODE });
      writeFileSync(target, JSON.stringify({ imei: data.imei, cookie: data.cookie, userAgent: data.userAgent }, null, 2), {
        encoding: "utf8",
        mode: CREDENTIALS_MODE,
      });
      chmodSync(target, CREDENTIALS_MODE);
      logInfo("credentials saved");
    } catch {
      logError("saving credentials failed");
    }
  }

  private stopListener(): void {
    try {
      this.api?.listener.stop();
    } catch {
      // The socket may already be closed.
    }
  }

  private newZalo(): Zalo {
    return new Zalo({
      logging: false,
      checkUpdate: false,
      selfListen: true,
      imageMetadataGetter: this.options.imageMetadataGetter ?? imageMetadata,
    });
  }
}

/**
 * Refuse a credentials file whose mode stays wider than 0600. A file this
 * process owns is tightened before it is read.
 */
export function enforceCredentialsMode(filePath: string): void {
  const info = statSync(filePath);
  if ((info.mode & 0o077) === 0) return;
  const self = typeof process.getuid === "function" ? process.getuid() : undefined;
  if (self !== undefined && info.uid === self) chmodSync(filePath, CREDENTIALS_MODE);
  if ((statSync(filePath).mode & 0o077) !== 0) throw new SidecarError("auth_expired");
}

function readCredentials(filePath: string): Credentials {
  enforceCredentialsMode(filePath);
  const parsed: unknown = JSON.parse(readFileSync(filePath, "utf8"));
  if (!parsed || typeof parsed !== "object") throw new SidecarError("auth_expired");
  const rec = parsed as Record<string, unknown>;
  if (typeof rec["imei"] !== "string" || typeof rec["userAgent"] !== "string" || rec["cookie"] === undefined) {
    throw new SidecarError("auth_expired");
  }
  return {
    imei: rec["imei"],
    userAgent: rec["userAgent"],
    cookie: rec["cookie"] as Credentials["cookie"],
  };
}

function asApi(api: unknown): ZaloApi {
  return api as ZaloApi;
}
