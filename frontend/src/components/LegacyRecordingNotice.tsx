'use client'

import { invoke } from '@tauri-apps/api/core'
import { useEffect, useState } from 'react'
import { useAuth } from '@/contexts/AuthContext'
import { useRecordingState } from '@/contexts/RecordingStateContext'

export function LegacyRecordingNotice() {
  const { session } = useAuth()
  const [count, setCount] = useState(0)
  const [message, setMessage] = useState<string | null>(null)
  const [recovering, setRecovering] = useState(false)
  const { isRecording, isStopping, isProcessing, isSaving } = useRecordingState()
  const locked = isRecording || isStopping || isProcessing || isSaving

  const recover = async () => {
    setRecovering(true)
    setMessage(null)
    try {
      const recovered = await invoke<number>('api_recover_legacy_recordings')
      setCount(await invoke<number>('api_legacy_recording_count'))
      setMessage(`${recovered} ${recovered === 1 ? 'recording recovered' : 'recordings recovered'} for the original workspace.`)
    } catch (error) {
      setMessage(String(error))
    } finally {
      setRecovering(false)
    }
  }

  useEffect(() => {
    let active = true
    void invoke<number>('api_legacy_recording_count').then(value => {
      if (active) setCount(value)
    }).catch(() => undefined)
    return () => { active = false }
  }, [session])

  if (count === 0 && !message) return null

  return (
    <details className="fixed bottom-4 right-4 z-50 max-w-sm rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-950 shadow-sm">
      <summary className="cursor-pointer font-medium">
        {count > 0 ? `${count} older ${count === 1 ? 'recording needs' : 'recordings need'} ownership recovery` : 'Recording recovery complete'}
      </summary>
      <p className="mt-2">
        Your original recordings are preserved on this computer. They stay local until
        Connections verifies their original user and workspace. Switching workspaces
        does not assign these recordings to the selected workspace.
      </p>
      <button className="mt-3 rounded border border-amber-300 px-3 py-1.5 disabled:opacity-50"
        disabled={!session || locked || recovering || count === 0} onClick={() => void recover()}>
        {recovering ? 'Verifying original ownership…' : 'Verify and recover'}
      </button>
      {!session && <p className="mt-2">Sign in to the original workspace to verify ownership.</p>}
      {message && <p className="mt-2" role="status">{message}</p>}
    </details>
  )
}
