import assert from 'node:assert/strict'
import test from 'node:test'

import { recordingCallbackRequestsSave, recordingStopDisposition, retryRecordingStop, runNativeStopBeforeProcessing, stopRecordingAfterTranscriptionError } from './recordingRecovery.mjs'

test('an incomplete native transcription remains recoverable', () => {
  assert.equal(recordingStopDisposition(true, false), 'retry')
})

test('an explicit discard may release the operation identity', () => {
  assert.equal(recordingStopDisposition(false, false), 'discard')
})

test('a completed native transcription proceeds to durable save', () => {
  assert.equal(recordingStopDisposition(true, true), 'save')
})

test('native failures retry while only an explicit user discard skips save', () => {
  assert.equal(recordingCallbackRequestsSave('native-stop-failure'), true)
  assert.equal(recordingCallbackRequestsSave('transcript-error'), true)
  assert.equal(recordingCallbackRequestsSave('transcription-error'), true)
  assert.equal(recordingCallbackRequestsSave('user-discard'), false)
})

test('visible recording retry action calls the retained save path', async () => {
  const calls = []
  await retryRecordingStop(async saveRequested => { calls.push(saveRequested) })
  assert.deepEqual(calls, [true])
})

test('native stop retry succeeds before final processing begins', async () => {
  const events = []
  let attempts = 0
  const stopNative = async () => {
    events.push(`native-${++attempts}`)
    if (attempts === 1) throw new Error('device busy')
  }
  const processFinal = async saveRequested => events.push(`process-${saveRequested}`)

  await assert.rejects(runNativeStopBeforeProcessing(stopNative, processFinal))
  assert.deepEqual(events, ['native-1'])
  await runNativeStopBeforeProcessing(stopNative, processFinal)
  assert.deepEqual(events, ['native-1', 'native-2', 'process-true'])
})

test('a pre-start transcription error does not stop or process a recording', async () => {
  const events = []

  await stopRecordingAfterTranscriptionError(
    async () => false,
    async () => { events.push('stop') },
  )

  assert.deepEqual(events, [])
})

test('native recording state wins while React recording state is still false', async () => {
  const events = []

  await stopRecordingAfterTranscriptionError(async () => true, () => runNativeStopBeforeProcessing(
    async () => { events.push('native-stop') },
    async () => { events.push('process-final') },
  ))

  assert.deepEqual(events, ['native-stop', 'process-final'])
})

test('native recording state query failures are reported without processing', async () => {
  const events = []

  const stopped = await stopRecordingAfterTranscriptionError(
    async () => { throw new Error('native unavailable') },
    async () => { events.push('stop') },
    error => { events.push(`error-${error.message}`) },
  )

  assert.equal(stopped, false)
  assert.deepEqual(events, ['error-native unavailable'])
})
