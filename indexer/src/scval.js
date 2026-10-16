// Minimal ScVal XDR helpers — just enough to index Trellis events by
// (event symbol, agreement_id) without pulling in @stellar/stellar-sdk.
//
// Every Trellis event publishes topics `(Symbol, BytesN<32>)` (see
// contracts/trellis_core/src/events.rs), so the indexer only ever needs to
// decode an scvSymbol and an scvBytes. Payloads (`value.xdr`) are stored
// verbatim and decoded by the frontend, which already ships the SDK.

const SCV_BYTES = 13
const SCV_SYMBOL = 15

function readVarOpaque(buf, discriminant) {
  if (buf.length < 8 || buf.readInt32BE(0) !== discriminant) return null
  const len = buf.readUInt32BE(4)
  if (buf.length < 8 + len) return null
  return buf.subarray(8, 8 + len)
}

/** Decodes a base64 scvSymbol topic, or returns null for any other ScVal. */
export function decodeSymbol(base64) {
  try {
    const bytes = readVarOpaque(Buffer.from(base64, 'base64'), SCV_SYMBOL)
    return bytes ? bytes.toString('utf8') : null
  } catch {
    return null
  }
}

/** Decodes a base64 scvBytes topic to lowercase hex, or returns null. */
export function decodeBytesHex(base64) {
  try {
    const bytes = readVarOpaque(Buffer.from(base64, 'base64'), SCV_BYTES)
    return bytes ? bytes.toString('hex') : null
  } catch {
    return null
  }
}
