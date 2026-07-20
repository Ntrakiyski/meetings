export function recordingStopDisposition(saveRequested, transcriptionComplete) {
  if (!saveRequested) return 'discard'
  return transcriptionComplete ? 'save' : 'retry'
}

export function recordingCallbackRequestsSave(source) {
  return source !== 'user-discard'
}

export function retryRecordingStop(handleRecordingStop) {
  return handleRecordingStop(true)
}

export async function runNativeStopBeforeProcessing(stopNative, processFinal) {
  const result = await stopNative()
  await processFinal(true)
  return result
}

export async function stopRecordingAfterTranscriptionError(getIsRecording, stopRecording, reportError = console.error) {
  let isRecording
  try {
    isRecording = await getIsRecording()
  } catch (error) {
    reportError(error)
    return false
  }
  if (!isRecording) return false
  await stopRecording()
  return true
}
