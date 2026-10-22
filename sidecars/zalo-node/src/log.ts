/**
 * Stderr logs. Stdout is reserved for protocol frames.
 * A line that looks like a credential is replaced before it is written.
 */

const SECRET = /cookie|imei|userAgent|zpw_enk|credentials/i;

export function logInfo(message: string): void {
  write("info", message);
}

export function logWarn(message: string): void {
  write("warn", message);
}

export function logError(message: string): void {
  write("error", message);
}

function write(level: string, message: string): void {
  const safe = SECRET.test(message) ? "redacted" : message.replace(/[\r\n]+/g, " ");
  process.stderr.write(`${level} ${safe}\n`);
}
