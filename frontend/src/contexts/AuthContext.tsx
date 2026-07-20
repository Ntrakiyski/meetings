'use client'

import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { createContext, useCallback, useContext, useEffect, useMemo, useState } from 'react'
import { createAuthSessionRecovery } from './authRecovery.mjs'

export interface AuthSession {
  userId: string
  clerkOrgId: string
  expiresAt: number
}

interface AuthContextValue {
  session: AuthSession | null
  loading: boolean
  error: string | null
  signIn(): Promise<void>
  signOut(): Promise<void>
  openProfile(): Promise<void>
  openOrganization(): Promise<void>
}

const AuthContext = createContext<AuthContextValue | null>(null)

export function AuthProvider({ children }: { children: React.ReactNode }) {
  const [session, setSession] = useState<AuthSession | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const sessionRecovery = useMemo(
    () => createAuthSessionRecovery(command => invoke(command)),
    [],
  )

  const acceptSession = useCallback((nextSession: AuthSession | null) => {
    setSession(nextSession)
    if (nextSession) void invoke<number>('api_retry_meeting_sync').catch(() => undefined)
  }, [])

  useEffect(() => {
    const listeners = Promise.all([
      listen<AuthSession | null>('auth-changed', event => {
        acceptSession(event.payload)
        setError(null)
        setLoading(false)
      }),
      listen<string>('auth-error', event => {
        setError(event.payload)
        setLoading(false)
      }),
    ])
    sessionRecovery.refresh()
      .then(session => setSession(session))
      .catch(reason => setError(String(reason)))
      .finally(() => setLoading(false))
    return () => { void listeners.then(unlisten => unlisten.forEach(stop => stop())) }
  }, [acceptSession, sessionRecovery])

  useEffect(() => {
    const recoverWhenOnline = () => {
      void sessionRecovery.refresh()
        .then(session => setSession(session))
        .catch(() => undefined)
    }
    window.addEventListener('online', recoverWhenOnline)
    return () => window.removeEventListener('online', recoverWhenOnline)
  }, [sessionRecovery])

  const signIn = useCallback(async () => {
    setError(null)
    await invoke('auth_start_sign_in').catch(reason => {
      setError(String(reason))
      throw reason
    })
  }, [])
  const signOut = useCallback(async () => {
    await invoke('auth_sign_out')
    setSession(null)
  }, [])
  const openProfile = useCallback(() => invoke<void>('auth_open_profile'), [])
  const openOrganization = useCallback(() => invoke<void>('auth_open_organization'), [])
  const value = useMemo(
    () => ({ session, loading, error, signIn, signOut, openProfile, openOrganization }),
    [session, loading, error, signIn, signOut, openProfile, openOrganization],
  )
  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>
}

export function useAuth(): AuthContextValue {
  const value = useContext(AuthContext)
  if (!value) throw new Error('useAuth must be used inside AuthProvider')
  return value
}
