import { renderHook } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { nativeToScVal } from '@stellar/stellar-sdk'

const readMock = vi.fn()
vi.mock('./useContractRead', () => ({
  useContractRead: (...args: unknown[]) => readMock(...args),
}))

import { useTotalAmount } from './useTotalAmount'

const ID = 'ab'.repeat(32)

function readResult(data: unknown) {
  return { data, status: data ? 'success' : 'idle', loading: false, error: null, refetch: vi.fn() }
}

describe('useTotalAmount', () => {
  beforeEach(() => {
    readMock.mockReset()
  })

  it('calls get_total_amount and is disabled without an agreement ID', () => {
    readMock.mockReturnValue(readResult(null))
    const { result } = renderHook(() => useTotalAmount(null))

    expect(readMock).toHaveBeenCalledWith('get_total_amount', [], false)
    expect(result.current.totalAmount).toBeNull()
  })

  it('passes the agreement ID as a single bytes argument when enabled', () => {
    readMock.mockReturnValue(readResult(null))
    renderHook(() => useTotalAmount(ID))

    const [method, args, enabled] = readMock.mock.calls[0]
    expect(method).toBe('get_total_amount')
    expect(args).toHaveLength(1)
    expect(enabled).toBe(true)
  })

  it('decodes an i128 result into a lossless decimal string', () => {
    const big = 170141183460469231731687303715884105727n
    readMock.mockReturnValue(readResult(nativeToScVal(big, { type: 'i128' })))
    const { result } = renderHook(() => useTotalAmount(ID))

    expect(result.current.totalAmount).toBe(big.toString())
  })

  it('returns null for an unexpected (non-numeric) result', () => {
    readMock.mockReturnValue(readResult(nativeToScVal('oops', { type: 'string' })))
    const { result } = renderHook(() => useTotalAmount(ID))

    expect(result.current.totalAmount).toBeNull()
  })
})
