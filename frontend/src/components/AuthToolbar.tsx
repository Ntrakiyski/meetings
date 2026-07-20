'use client'

import { useAuth } from '@/contexts/AuthContext'
import { useRecordingState } from '@/contexts/RecordingStateContext'

export function AuthToolbar() {
  const { session, signIn, signOut, openProfile, openOrganization } = useAuth()
  const { isRecording, isStopping, isProcessing, isSaving } = useRecordingState()
  const organizationLocked = isRecording || isStopping || isProcessing || isSaving
  if (!session) return null

  return (
    <div className="fixed right-4 top-3 z-50 flex items-center gap-2 rounded-lg border border-gray-200 bg-white/95 p-1.5 text-xs shadow-sm">
      <span className="max-w-36 truncate px-2 text-gray-500" title={session.clerkOrgId}>{session.clerkOrgId}</span>
      <button className="rounded px-2 py-1 hover:bg-gray-100" onClick={() => void openProfile()}>Profile</button>
      <button className="rounded px-2 py-1 hover:bg-gray-100 disabled:opacity-40" disabled={organizationLocked} title={organizationLocked ? 'Finish saving before managing organizations' : undefined} onClick={() => void openOrganization()}>Organization</button>
      <button className="rounded px-2 py-1 hover:bg-gray-100 disabled:opacity-40" disabled={organizationLocked} title={organizationLocked ? 'Finish saving before switching organizations' : undefined} onClick={() => void signIn()}>Switch</button>
      <button className="rounded px-2 py-1 hover:bg-gray-100 disabled:opacity-40" disabled={organizationLocked} onClick={() => void signOut()}>Sign out</button>
    </div>
  )
}
