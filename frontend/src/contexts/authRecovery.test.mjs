import assert from 'node:assert/strict'
import test from 'node:test'

import { createAuthSessionRecovery, recoverAuthenticatedSession, retryPendingFinalSync } from './authRecovery.mjs'

test('offline startup recovery verifies the restored session before retrying sync', async () => {
  const calls = []
  const session = { userId: 'user-a', clerkOrgId: 'org-a', expiresAt: 1 }
  const invoke = async command => {
    calls.push(command)
    if (command === 'auth_get_session') return session
    return 0
  }

  assert.equal(await recoverAuthenticatedSession(invoke), session)
  assert.deepEqual(calls, ['auth_get_session', 'api_retry_meeting_sync'])
})

test('offline startup recovery does not retry without a verified session', async () => {
  const calls = []
  const invoke = async command => {
    calls.push(command)
    return null
  }

  assert.equal(await recoverAuthenticatedSession(invoke), null)
  assert.deepEqual(calls, ['auth_get_session'])
})

test('initial null session is recovered online before sync retry', async () => {
  const calls = []
  let online = false
  const session = { userId: 'user-a', clerkOrgId: 'org-a', expiresAt: 1 }
  const recovery = createAuthSessionRecovery(async command => {
    calls.push(command)
    if (command === 'auth_get_session') return online ? session : null
    return 0
  })

  assert.equal(await recovery.refresh(), null)
  online = true
  assert.equal(await recovery.refresh(), session)
  assert.deepEqual(calls, [
    'auth_get_session',
    'auth_get_session',
    'api_retry_meeting_sync',
  ])
})

test('import retry action verifies auth and invokes production recovery', async () => {
  const calls = []
  const recovered = await retryPendingFinalSync(async command => {
    calls.push(command)
    if (command === 'auth_get_session') return { userId: 'user-a' }
    return 1
  })
  assert.equal(recovered, 1)
  assert.deepEqual(calls, ['auth_get_session', 'api_retry_meeting_sync'])
})

test('import retry remains failed until queued delivery actually succeeds', async () => {
  let deliverySucceeds = false
  const invoke = async command => {
    if (command === 'auth_get_session') return { userId: 'user-a' }
    if (!deliverySucceeds) throw new Error('offline')
    return 1
  }
  await assert.rejects(retryPendingFinalSync(invoke), /offline/)
  deliverySucceeds = true
  assert.equal(await retryPendingFinalSync(invoke), 1)
})
