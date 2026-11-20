export const CODEPLUSE_WS_PROTOCOL = "codepluse-events"
const CODEPLUSE_WS_TOKEN_PROTOCOL_PREFIX = "codepluse-token."

function base64UrlEncode(value: string): string {
  const bytes = new TextEncoder().encode(value)
  let binary = ""
  for (const byte of bytes) {
    binary += String.fromCharCode(byte)
  }
  return btoa(binary)
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/g, "")
}

export function buildCodepluseWebSocketProtocols(token: string): string[] {
  const trimmed = token.trim()
  if (!trimmed) return [CODEPLUSE_WS_PROTOCOL]
  return [
    CODEPLUSE_WS_PROTOCOL,
    `${CODEPLUSE_WS_TOKEN_PROTOCOL_PREFIX}${base64UrlEncode(trimmed)}`,
  ]
}
