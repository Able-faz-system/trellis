import { useMemo } from 'react'
import { nativeToScVal, scValToNative, xdr } from '@stellar/stellar-sdk'
import { useContractRead } from './useContractRead'
import { hexToBytes } from '../lib/format'

/**
 * Hook to fetch an agreement's total value via the `get_total_amount`
 * entrypoint — the pre-computed sum of every milestone amount.
 *
 * Returns the total as a decimal string (the contract type is `i128`, which
 * would lose precision as a JS number), or null while loading / on error.
 * Failures are deliberately not surfaced: the total is supplementary context
 * next to the milestone list, and the agreement lookup already reports an
 * unknown ID.
 */
export function useTotalAmount(agreementId: string | null) {
  const args = useMemo(() => {
    if (!agreementId) return []
    return [nativeToScVal(hexToBytes(agreementId), { type: 'bytes' })]
  }, [agreementId])

  const { data, loading, refetch } = useContractRead<xdr.ScVal>(
    'get_total_amount',
    args,
    !!agreementId,
  )

  const totalAmount = useMemo<string | null>(() => {
    if (!data) return null
    try {
      const native = scValToNative(data) as unknown
      if (typeof native === 'bigint' || typeof native === 'number') return native.toString()
      return null
    } catch (err) {
      console.error('[useTotalAmount] Failed to parse total amount:', err)
      return null
    }
  }, [data])

  return { totalAmount, isLoading: loading, refetch }
}
