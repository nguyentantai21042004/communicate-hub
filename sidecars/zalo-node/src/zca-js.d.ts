declare module "zca-js" {
  export interface Credentials {
    imei: string;
    cookie: unknown;
    userAgent: string;
    language?: string;
  }

  export enum LoginQRCallbackEventType {
    QRCodeGenerated = 0,
    QRCodeExpired = 1,
    QRCodeScanned = 2,
    QRCodeDeclined = 3,
    GotLoginInfo = 4,
  }

  interface QrActions {
    retry(): unknown;
    abort(): unknown;
  }

  export type LoginQRCallbackEvent =
    | {
        type: LoginQRCallbackEventType.QRCodeGenerated;
        data: { image: string };
        actions: QrActions;
      }
    | { type: LoginQRCallbackEventType.QRCodeExpired; data: null; actions: QrActions }
    | {
        type: LoginQRCallbackEventType.QRCodeScanned;
        data: { display_name: string };
        actions: QrActions;
      }
    | { type: LoginQRCallbackEventType.QRCodeDeclined; data: { code: string }; actions: QrActions }
    | {
        type: LoginQRCallbackEventType.GotLoginInfo;
        data: { cookie: unknown; imei: string; userAgent: string };
        actions: null;
      };

  export class Zalo {
    constructor(options?: {
      selfListen?: boolean;
      checkUpdate?: boolean;
      logging?: boolean;
      imageMetadataGetter?: (filePath: string) => Promise<{ width: number; height: number; size: number } | null>;
    });
    login(credentials: Credentials): Promise<unknown>;
    loginQR(
      options?: { userAgent?: string; language?: string; qrPath?: string },
      callback?: (event: LoginQRCallbackEvent) => unknown,
    ): Promise<unknown>;
  }
}
