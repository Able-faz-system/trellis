import { mkdirSync } from 'node:fs'
import { dirname } from 'node:path'
import { DatabaseSync } from 'node:sqlite'

// Event symbols counted by /stats. The contract emits the prefixed `trls_*`
// names; the bare names are accepted too so stats stay correct against a
// contract instance deployed before the prefix was introduced.
const CREATED_SYMBOLS = ['trls_crte', 'created']
const LOCKED_SYMBOLS = ['trls_lckd', 'locked']

/**
 * SQLite-backed event store. Events are keyed by the RPC event `id`, which is
 * globally unique and stable, so re-ingesting an overlapping page is a no-op.
 */
export class EventStore {
  constructor(path) {
    if (path !== ':memory:') mkdirSync(dirname(path), { recursive: true })
    this.db = new DatabaseSync(path)
    this.db.exec(`
      PRAGMA journal_mode = WAL;
      CREATE TABLE IF NOT EXISTS events (
        id               TEXT PRIMARY KEY,
        ledger           INTEGER NOT NULL,
        ledger_closed_at TEXT,
        tx_hash          TEXT,
        contract_id      TEXT NOT NULL,
        event_type       TEXT,
        agreement_id     TEXT,
        topic_json       TEXT NOT NULL,
        value_xdr        TEXT
      );
      CREATE INDEX IF NOT EXISTS events_agreement ON events (agreement_id, ledger);
      CREATE INDEX IF NOT EXISTS events_type ON events (event_type);
      CREATE TABLE IF NOT EXISTS meta (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
      );
    `)

    this.insertStmt = this.db.prepare(`
      INSERT OR IGNORE INTO events
        (id, ledger, ledger_closed_at, tx_hash, contract_id, event_type, agreement_id, topic_json, value_xdr)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
    `)
    this.getMetaStmt = this.db.prepare('SELECT value FROM meta WHERE key = ?')
    this.setMetaStmt = this.db.prepare(
      'INSERT INTO meta (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value',
    )
  }

  /** Inserts events and returns only the ones that were not already stored. */
  insertMany(events) {
    const inserted = []
    this.db.exec('BEGIN')
    try {
      for (const e of events) {
        const { changes } = this.insertStmt.run(
          e.id,
          e.ledger,
          e.ledgerClosedAt ?? null,
          e.txHash ?? null,
          e.contractId,
          e.eventType ?? null,
          e.agreementId ?? null,
          JSON.stringify(e.topic ?? []),
          e.valueXdr ?? null,
        )
        if (changes > 0) inserted.push(e)
      }
      this.db.exec('COMMIT')
    } catch (err) {
      this.db.exec('ROLLBACK')
      throw err
    }
    return inserted
  }

  /** Events for one agreement, newest first, in the RPC `getEvents` shape. */
  eventsForAgreement(agreementId, { limit = 500, beforeLedger } = {}) {
    const rows = beforeLedger
      ? this.db
          .prepare(
            'SELECT * FROM events WHERE agreement_id = ? AND ledger < ? ORDER BY ledger DESC, id DESC LIMIT ?',
          )
          .all(agreementId, beforeLedger, limit)
      : this.db
          .prepare('SELECT * FROM events WHERE agreement_id = ? ORDER BY ledger DESC, id DESC LIMIT ?')
          .all(agreementId, limit)
    return rows.map(toRpcShape)
  }

  stats() {
    const count = (symbols) =>
      this.db
        .prepare(`SELECT COUNT(*) AS n FROM events WHERE event_type IN (${symbols.map(() => '?').join(',')})`)
        .get(...symbols).n
    const { latest } = this.db.prepare('SELECT MAX(ledger) AS latest FROM events').get()
    return {
      agreements: count(CREATED_SYMBOLS),
      milestonesLocked: count(LOCKED_SYMBOLS),
      latestIndexedLedger: latest ?? null,
    }
  }

  getMeta(key) {
    return this.getMetaStmt.get(key)?.value ?? null
  }

  setMeta(key, value) {
    this.setMetaStmt.run(key, String(value))
  }
}

/**
 * Converts a stored row back to the shape Soroban RPC returns from
 * `getEvents`, so the frontend can parse indexer and RPC results with the
 * same code path.
 */
export function toRpcShape(row) {
  return {
    id: row.id,
    ledger: row.ledger,
    ledgerClosedAt: row.ledger_closed_at,
    txHash: row.tx_hash,
    contractId: row.contract_id,
    topic: JSON.parse(row.topic_json),
    value: { xdr: row.value_xdr },
  }
}
