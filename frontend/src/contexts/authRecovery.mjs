export async function recoverAuthenticatedSession(invoke) {
  const session = await invoke('auth_get_session')
  if (session) await invoke('api_retry_meeting_sync')
  return session
}

export function createAuthSessionRecovery(invoke) {
  return { refresh: () => recoverAuthenticatedSession(invoke) }
}

export async function retryPendingFinalSync(invoke) {
  const session = await invoke('auth_get_session')
  if (!session) throw new Error('Sign in before retrying meeting sync.')
  return invoke('api_retry_meeting_sync')
}
