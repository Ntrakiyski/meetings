'use client'

import { useAuth } from '@/contexts/AuthContext'

export function AuthGate({ children }: { children: React.ReactNode }) {
  const { session, loading, error, signIn } = useAuth()
  if (loading) return <main className="grid h-screen place-items-center bg-gray-50 text-gray-600">Checking session…</main>
  if (!session) {
    return (
      <main className="grid h-screen place-items-center bg-gray-50 p-6">
        <section className="w-full max-w-md rounded-2xl bg-white p-8 text-center shadow-sm">
          <h1 className="text-2xl font-semibold text-gray-900">Sign in to Meetily</h1>
          <p className="mt-3 text-sm text-gray-600">A Clerk account and organization are required to record and sync meetings.</p>
          {error && <p className="mt-4 rounded-lg bg-red-50 p-3 text-sm text-red-700">{error}</p>}
          <button className="mt-6 rounded-lg bg-gray-900 px-5 py-2.5 text-sm font-medium text-white" onClick={() => void signIn()}>
            Continue with Clerk
          </button>
        </section>
      </main>
    )
  }
  return children
}
