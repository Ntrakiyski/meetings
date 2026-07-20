export type RecordingStopDisposition = 'save' | 'discard' | 'retry'

export function recordingStopDisposition(
  saveRequested: boolean,
  transcriptionComplete: boolean,
): RecordingStopDisposition

export type RecordingCallbackSource =
  | 'native-stop-success'
  | 'native-stop-failure'
  | 'transcript-error'
  | 'transcription-error'
  | 'user-discard'

export function recordingCallbackRequestsSave(source: RecordingCallbackSource): boolean

export function retryRecordingStop(
  handleRecordingStop: (saveRequested: boolean) => Promise<void>,
): Promise<void>

export function runNativeStopBeforeProcessing<T>(
  stopNative: () => Promise<T>,
  processFinal: (saveRequested: true) => Promise<void>,
): Promise<T>

export function stopRecordingAfterTranscriptionError(
  getIsRecording: () => Promise<boolean>,
  stopRecording: () => Promise<void> | void,
  reportError?: (error: unknown) => void,
): Promise<boolean>
