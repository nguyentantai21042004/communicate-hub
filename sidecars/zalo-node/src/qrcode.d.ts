declare module "qrcode" {
  export function toBuffer(
    text: string,
    options?: { type?: "png"; width?: number; margin?: number },
  ): Promise<Buffer>;
}
