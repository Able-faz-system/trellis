import { EventEmitter } from 'node:events'
import { decodeBytesHex, decodeSymbol } from './scval.js'

const PAGE_LIMIT = 100
const RPC_TIMEOUT_MS = 30_000

async function rpcCall(rpcUrl, method, params) {
  const response = await fetch(rpcUrl, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
    signal: AbortSignal.timeout(RPC_TIMEOUT_MS),
  })
  if (!response.ok) throw new Error(`RPC HTTP ${response.status}`)
  const json = await response.json()
  if (json.error) {
    const err = new Error(`RPC error ${json.error.code}: ${json.error.message}`)
    err.rpcCode = json.error.code
    throw err
  }
  return json.result
}

function normalize(raw, contractId) {
  const topic = Array.isArray(raw.topic) ? raw.topic : []
  return {
    id: raw.id ?? raw.pagingToken,
    ledger: raw.ledger,
    ledgerClosedAt: raw.ledgerClosedAt,
    txHash: raw.txHash,
    contractId: raw.contractId ?? contractId,
    eventType: topic[0] ? decodeSymbol(topic[0]) : null,
    agreementId: topic[1] ? decodeBytesHex(topic[1]) : null,
    topic,
    valueXdr: raw.value?.xdr ?? raw.value ?? null,
  }
}

/** The event as the frontend expects it — the RPC `getEvents` shape. */
export function toRpcEvent(e) {
  return {
    id: e.id,
    ledger: e.ledger,
    ledgerClosedAt: e.ledgerClosedAt,
    txHash: e.txHash,
    contractId: e.contractId,
    topic: e.topic,
    value: { xdr: e.valueXdr },
  }
}

/**
 * Polls Soroban RPC `getEvents` on a single schedule and persists every
 * contract event to the store.
 *
 * Emits:
 *   - `events` (normalizedEvents[]) — newly stored events after each poll
 *   - `gap` ({ cursor, resumedAtLedger, detectedAt }) — the stored cursor fell outside the
 *     RPC node's retention window; events in that range are unrecoverable
 *     from this node and must be backfilled from another source.
 */
export class Ingester extends EventEmitter {
  constructor({ store, rpcUrl, contractId, pollIntervalMs, log = console }) {
    super()
    this.store = store
    this.rpcUrl = rpcUrl
    this.contractId = contractId
    this.pollIntervalMs = pollIntervalMs
    this.log = log
    this.timer = null
    this.stopped = false
    this.rpcLatestLedger = null
    this.lastPollAt = null
    this.lastError = null
  }

  start() {
    this.stopped = false
    const loop = async () => {
      await this.pollOnce()
      if (!this.stopped) this.timer = setTimeout(loop, this.pollIntervalMs)
    }
    void loop()
  }

  stop() {
    this.stopped = true
    if (this.timer) clearTimeout(this.timer)
  }

  async pollOnce() {
    try {
      let fresh = []
      // Drain every available page so a long outage catches up in one cycle.
      for (;;) {
        const page = await this.fetchPage()
        const normalized = page.events.map((raw) => normalize(raw, this.contractId))
        fresh = fresh.concat(this.store.insertMany(normalized))
        if (page.cursor) this.store.setMeta('cursor', page.cursor)
        if (page.events.length < PAGE_LIMIT) break
      }
      this.lastPollAt = new Date().toISOString()
      this.lastError = null
      if (fresh.length > 0) this.emit('events', fresh)
    } catch (err) {
      this.lastError = err instanceof Error ? err.message : String(err)
      this.log.error('[indexer] poll failed:', this.lastError)
    }
  }

  async fetchPage() {
    const filters = [{ type: 'contract', contractIds: [this.contractId] }]
    const cursor = this.store.getMeta('cursor')

    if (cursor) {
      try {
        const result = await rpcCall(this.rpcUrl, 'getEvents', {
          filters,
          pagination: { cursor, limit: PAGE_LIMIT },
        })
        return this.pageFrom(result)
      } catch (err) {
        // A cursor older than the node's retention window is rejected with
        // -32600 (invalid request). Restart from the oldest ledger the node
        // still has and record the gap rather than stalling forever.
        if (err.rpcCode !== -32600) throw err
        const { oldestLedger } = await rpcCall(this.rpcUrl, 'getHealth')
        this.log.warn(`[indexer] cursor ${cursor} is outside RPC retention; resuming at ledger ${oldestLedger}`)
        const gap = { cursor, resumedAtLedger: oldestLedger, detectedAt: new Date().toISOString() }
        this.emit('gap', gap)
        this.store.setMeta('last_gap', JSON.stringify(gap))
        return this.firstPage(filters, oldestLedger)
      }
    }

    const configured = this.store.getMeta('start_ledger')
    const startLedger = configured
      ? Number(configured)
      : (await rpcCall(this.rpcUrl, 'getHealth')).oldestLedger
    return this.firstPage(filters, startLedger)
  }

  async firstPage(filters, startLedger) {
    const result = await rpcCall(this.rpcUrl, 'getEvents', {
      startLedger,
      filters,
      pagination: { limit: PAGE_LIMIT },
    })
    return this.pageFrom(result)
  }

  pageFrom(result) {
    const events = result?.events ?? []
    this.rpcLatestLedger = result?.latestLedger ?? this.rpcLatestLedger
    // Newer RPC versions return a top-level cursor; older ones only expose
    // per-event paging tokens.
    const last = events[events.length - 1]
    const cursor = result?.cursor ?? last?.id ?? last?.pagingToken ?? null
    return { events, cursor }
  }
}
