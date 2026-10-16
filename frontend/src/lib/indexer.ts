/**
 * Client for the Trellis indexer/relay service (indexer/ at the repo root).
 *
 * The relay polls Soroban RPC once server-side and fans updates out over
 * Server-Sent Events, and its SQLite store keeps history past the RPC node's
 * retention window. Hooks use it when VITE_INDEXER_URL is set and fall back
 * to polling RPC directly whenever it is unset or unreachable.
 */

import { getIndexerUrl } from './config'

/** How long to wait for the stream to (re)open before giving up on the relay. */
const CONNECT_TIMEOUT_MS = 5_000

export type StreamHandlers = Record<string, (data: unknown) => void>

/**
 * Opens an SSE stream on the indexer and dispatches each named event to its
 * handler with the parsed JSON payload.
 *
 * `onUnavailable` fires at most once, when the relay is not configured, the
 * browser has no EventSource, or the stream fails to open (or re-open after a
 * drop) within CONNECT_TIMEOUT_MS. The caller should switch to direct RPC
 * polling at that point. Returns a cleanup function that closes the stream.
 */
export function openIndexerStream(
  path: string,
  handlers: StreamHandlers,
  onUnavailable: () => void,
): () => void {
  const baseUrl = getIndexerUrl()
  if (!baseUrl || typeof EventSource === 'undefined') {
    onUnavailable()
    return () => {}
  }

  let closed = false
  const source = new EventSource(`${baseUrl}${path}`)

  const giveUp = () => {
    if (closed) return
    closed = true
    clearTimeout(connectTimer)
    source.close()
    console.warn(`[indexer] relay unavailable at ${baseUrl}; falling back to direct RPC polling`)
    onUnavailable()
  }

  let connectTimer = setTimeout(giveUp, CONNECT_TIMEOUT_MS)

  source.onopen = () => clearTimeout(connectTimer)
  source.onerror = () => {
    if (source.readyState === EventSource.CLOSED) {
      giveUp()
      return
    }
    // EventSource retries on its own; give it one window to reconnect.
    clearTimeout(connectTimer)
    connectTimer = setTimeout(giveUp, CONNECT_TIMEOUT_MS)
  }

  for (const [name, handler] of Object.entries(handlers)) {
    source.addEventListener(name, (e) => {
      try {
        handler(JSON.parse((e as MessageEvent<string>).data))
      } catch (err) {
        console.error(`[indexer] bad "${name}" payload:`, err)
      }
    })
  }

  return () => {
    closed = true
    clearTimeout(connectTimer)
    source.close()
  }
}

/** Fetches stored history for one agreement from the indexer's query API. */
export async function fetchIndexedEvents(
  agreementId: string,
  signal: AbortSignal,
): Promise<unknown[]> {
  const baseUrl = getIndexerUrl()
  if (!baseUrl) throw new Error('VITE_INDEXER_URL is not set')
  const response = await fetch(
    `${baseUrl}/events?agreementId=${encodeURIComponent(agreementId)}`,
    { signal },
  )
  if (!response.ok) throw new Error(`Indexer HTTP ${response.status}`)
  const json = (await response.json()) as { events?: unknown[] }
  return json.events ?? []
}
