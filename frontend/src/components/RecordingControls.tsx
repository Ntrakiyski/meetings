'use client';

import { invoke } from '@tauri-apps/api/core';
import { appDataDir } from '@tauri-apps/api/path';
import { useCallback, useEffect, useState, useRef } from 'react';
import { Play, Pause, Square, Mic, AlertCircle, X, Check, Ellipsis } from 'lucide-react';
import { ProcessRequest, SummaryResponse } from '@/types/summary';
import { listen } from '@tauri-apps/api/event';
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from '@/components/ui/tooltip';
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';
import Analytics from '@/lib/analytics';
import { useRecordingState } from '@/contexts/RecordingStateContext';
import { useConfig } from '@/contexts/ConfigContext';
import { writePinnedSummaryLanguageDefault } from '@/lib/summary-language-preferences';
import {
  runNativeStopBeforeProcessing,
  stopRecordingAfterTranscriptionError,
} from '@/hooks/recordingRecovery.mjs';
import { toast } from 'sonner';

const RECORDING_LANGUAGE_OPTIONS = [
  { code: 'bg', label: 'Bulgarian' },
  { code: 'en', label: 'English' },
] as const;

type RecordingLanguage = (typeof RECORDING_LANGUAGE_OPTIONS)[number]['code'];

const BULGARIAN_WHISPER_MODEL = 'large-v3-q5_0';
const PARAKEET_MODEL = 'parakeet-tdt-0.6b-v3-int8';

interface RecordingControlsProps {
  isRecording: boolean;
  barHeights: string[];
  onRecordingStop: (callApi?: boolean) => void;
  onRecordingStart: (language?: string) => Promise<void>;
  onTranscriptReceived: (summary: SummaryResponse) => void;
  onTranscriptionError?: (message: string) => void;
  onStopInitiated?: () => void; // Called immediately when stop button is clicked
  isRecordingDisabled: boolean;
  isParentProcessing: boolean;
  selectedDevices?: {
    micDevice: string | null;
    systemDevice: string | null;
  };
  meetingName?: string;
}

export const RecordingControls: React.FC<RecordingControlsProps> = ({
  isRecording,
  barHeights,
  onRecordingStop,
  onRecordingStart,
  onTranscriptReceived,
  onTranscriptionError,
  onStopInitiated,
  isRecordingDisabled,
  isParentProcessing,
  selectedDevices,
  meetingName,
}) => {
  // Use global recording state context for pause state (syncs with tray operations)
  const recordingState = useRecordingState();
  const isPaused = recordingState.isPaused;
  const { selectedLanguage, setSelectedLanguage, setTranscriptModelConfig } = useConfig();

  const [showPlayback, setShowPlayback] = useState(false);
  const [recordingPath, setRecordingPath] = useState<string | null>(null);
  const [transcript, setTranscript] = useState<string>('');
  const [isProcessing, setIsProcessing] = useState(false);
  const [isStarting, setIsStarting] = useState(false);
  const [isStopping, setIsStopping] = useState(false);
  const [isPausing, setIsPausing] = useState(false);
  const [isResuming, setIsResuming] = useState(false);
  const MIN_RECORDING_DURATION = 2000; // 2 seconds minimum recording time
  const [transcriptionErrors, setTranscriptionErrors] = useState(0);
  const [isValidatingModel, setIsValidatingModel] = useState(false);
  const [speechDetected, setSpeechDetected] = useState(false);
  const stopRecordingActionRef = useRef<(() => Promise<void>) | null>(null);
  const [deviceError, setDeviceError] = useState<{ title: string, message: string } | null>(null);
  const [recordingLanguages, setRecordingLanguages] = useState<RecordingLanguage[]>(() => {
    if (typeof window !== 'undefined') {
      try {
        const saved = JSON.parse(localStorage.getItem('recordingLanguages') ?? '[]');
        if (Array.isArray(saved)) {
          const supported = saved.filter((language): language is RecordingLanguage =>
            language === 'bg' || language === 'en'
          );
          if (supported.length > 0) return supported;
        }
      } catch {
        // A malformed old preference should not block recording.
      }
    }
    return selectedLanguage === 'en' ? ['en'] : ['bg'];
  });

  const transcriptionLanguage = recordingLanguages.length === 1
    ? recordingLanguages[0]
    : 'auto';
  const transcriptionConfig = recordingLanguages.length === 1 && recordingLanguages[0] === 'bg'
    ? { provider: 'localWhisper' as const, model: BULGARIAN_WHISPER_MODEL }
    : { provider: 'parakeet' as const, model: PARAKEET_MODEL };

  useEffect(() => {
    localStorage.setItem('recordingLanguages', JSON.stringify(recordingLanguages));
    if (selectedLanguage !== transcriptionLanguage) {
      setSelectedLanguage(transcriptionLanguage);
    }
    // Summaries have one output language. Bulgarian remains the default for
    // bilingual meetings, while an English-only meeting gets an English summary.
    writePinnedSummaryLanguageDefault(
      recordingLanguages.includes('en') && !recordingLanguages.includes('bg') ? 'en' : 'bg'
    );
  }, [recordingLanguages, selectedLanguage, setSelectedLanguage, transcriptionLanguage]);

  const toggleRecordingLanguage = (language: RecordingLanguage) => {
    setRecordingLanguages((current) => {
      if (current.includes(language)) {
        return current.length === 1 ? current : current.filter((item) => item !== language);
      }
      return [...current, language];
    });
  };

  const currentTime = 0;
  const duration = 0;
  const isPlaying = false;
  const progress = 0;

  const formatTime = (time: number) => {
    const minutes = Math.floor(time / 60);
    const seconds = Math.floor(time % 60);
    return `${minutes}:${seconds.toString().padStart(2, '0')}`;
  };

  useEffect(() => {
    const checkTauri = async () => {
      try {
        const result = await invoke('is_recording');
        console.log('Tauri is initialized and ready, is_recording result:', result);
      } catch (error) {
        console.error('Tauri initialization error:', error);
        alert('Failed to initialize recording. Please check the console for details.');
      }
    };
    checkTauri();
  }, []);

  const handleStartRecording = useCallback(async () => {
    if (isStarting || isValidatingModel) return;
    console.log('Starting recording...');
    console.log('Selected devices:', selectedDevices);
    console.log('Meeting name:', meetingName);
    console.log('Current isRecording state:', isRecording);

    setShowPlayback(false);
    setTranscript(''); // Clear any previous transcript
    setSpeechDetected(false); // Reset speech detection on new recording

    try {
      // Parakeet only auto-detects languages. Bulgarian-only meetings use the
      // existing local Whisper engine with a real `bg` language constraint.
      await invoke('api_save_transcript_config', {
        provider: transcriptionConfig.provider,
        model: transcriptionConfig.model,
        apiKey: null,
      });
      setTranscriptModelConfig({ ...transcriptionConfig, apiKey: null });

      // Call the validation callback which will:
      // 1. Check if model is ready
      // 2. Show appropriate toast/modal
      // 3. Call backend if valid
      // 4. Update UI state
      await onRecordingStart(transcriptionLanguage);
    } catch (error) {
      console.error('Failed to start recording:', error);
      console.error('Error details:', {
        message: error instanceof Error ? error.message : String(error),
        name: error instanceof Error ? error.name : 'Unknown',
        stack: error instanceof Error ? error.stack : undefined
      });

      // Parse error message to provide user-friendly feedback
      const errorMsg = error instanceof Error ? error.message : String(error);

      // Check for device-related errors
      if (errorMsg.includes('microphone') || errorMsg.includes('mic') || errorMsg.includes('input')) {
        setDeviceError({
          title: 'Microphone Not Available',
          message: 'Unable to access your microphone. Please check that:\n• Your microphone is connected\n• The app has microphone permissions\n• No other app is using the microphone'
        });
      } else if (errorMsg.includes('system audio') || errorMsg.includes('speaker') || errorMsg.includes('output')) {
        setDeviceError({
          title: 'System Audio Not Available',
          message: 'Unable to capture system audio. Please check that:\n• A virtual audio device (like BlackHole) is installed\n• The app has screen recording permissions (macOS)\n• System audio is properly configured'
        });
      } else if (errorMsg.includes('permission')) {
        setDeviceError({
          title: 'Permission Required',
          message: 'Recording permissions are required. Please:\n• Grant microphone access in System Settings\n• Grant screen recording access for system audio (macOS)\n• Restart the app after granting permissions'
        });
      } else {
        setDeviceError({
          title: 'Recording Failed',
          message: 'Unable to start recording. Please check your audio device settings and try again.'
        });
      }
    }
  }, [onRecordingStart, isStarting, isValidatingModel, selectedDevices, meetingName, isRecording, transcriptionLanguage, transcriptionConfig, setTranscriptModelConfig]);

  const stopRecordingAction = useCallback(async () => {
    console.log('Executing stop recording...');
    try {
      setIsProcessing(true);
      const dataDir = await appDataDir();
      const timestamp = new Date().toISOString().replace(/[:.]/g, '-');
      const savePath = `${dataDir}/recording-${timestamp}.wav`;
      console.log('Saving recording to:', savePath);
      console.log('About to call stop_recording command');
      const result = await runNativeStopBeforeProcessing(
        () => invoke('stop_recording', {
          args: { save_path: savePath }
        }),
        async saveRequested => {
          setRecordingPath(savePath);
          setIsProcessing(false);
          Analytics.trackTranscriptionSuccess();
          onRecordingStop(saveRequested);
        },
      );
      console.log('stop_recording command completed successfully:', result);
    } catch (error) {
      console.error('Failed to stop recording:', error);
      if (error instanceof Error) {
        console.error('Error details:', {
          message: error.message,
          name: error.name,
          stack: error.stack,
        });
      }
      setIsProcessing(false);
      toast.error('Could not stop recording', {
        description: 'The recording is still active. Retry the native stop before saving.',
        duration: Infinity,
        action: {
          label: 'Retry stop',
          onClick: () => { void stopRecordingActionRef.current?.(); },
        },
      });
    } finally {
      setIsStopping(false);
    }
  }, [onRecordingStop]);

  useEffect(() => {
    stopRecordingActionRef.current = stopRecordingAction;
  }, [stopRecordingAction]);

  const handleStopRecording = useCallback(async () => {
    console.log('handleStopRecording called - isRecording:', isRecording, 'isStarting:', isStarting, 'isStopping:', isStopping);
    if (!isRecording || isStarting || isStopping) {
      console.log('Early return from handleStopRecording due to state check');
      return;
    }

    console.log('Stopping recording...');

    // Notify parent immediately (for UI state updates)
    onStopInitiated?.();

    setIsStopping(true);

    // Immediately trigger the stop action
    await stopRecordingAction();
  }, [isRecording, isStarting, isStopping, stopRecordingAction, onStopInitiated]);

  const handlePauseRecording = useCallback(async () => {
    if (!isRecording || isPaused || isPausing) return;

    console.log('Pausing recording...');
    setIsPausing(true);

    try {
      await invoke('pause_recording');
      // isPaused state now managed by RecordingStateContext via events
      console.log('Recording paused successfully');
    } catch (error) {
      console.error('Failed to pause recording:', error);
      alert('Failed to pause recording. Please check the console for details.');
    } finally {
      setIsPausing(false);
    }
  }, [isRecording, isPaused, isPausing]);

  const handleResumeRecording = useCallback(async () => {
    if (!isRecording || !isPaused || isResuming) return;

    console.log('Resuming recording...');
    setIsResuming(true);

    try {
      await invoke('resume_recording');
      // isPaused state now managed by RecordingStateContext via events
      console.log('Recording resumed successfully');
    } catch (error) {
      console.error('Failed to resume recording:', error);
      alert('Failed to resume recording. Please check the console for details.');
    } finally {
      setIsResuming(false);
    }
  }, [isRecording, isPaused, isResuming]);

  useEffect(() => {
    return () => {
      // Cleanup on unmount if needed
    };
  }, []);

  useEffect(() => {
    console.log('Setting up recording event listeners');
    let unsubscribes: (() => void)[] = [];

    const setupListeners = async () => {
      try {
        // Transcript error listener - handles both regular and actionable errors
        const transcriptErrorUnsubscribe = await listen('transcript-error', (event) => {
          console.log('transcript-error event received:', event);
          console.error('Transcription error received:', event.payload);
          const errorMessage = event.payload as string;

          Analytics.trackTranscriptionError(errorMessage);
          console.log('Tracked transcription error:', errorMessage);

          setTranscriptionErrors(prev => {
            const newCount = prev + 1;
            console.log('Transcription error count incremented:', newCount);
            return newCount;
          });
          console.log('Stopping native recording before recoverable processing due to transcript error');
          void stopRecordingAfterTranscriptionError(
            () => invoke<boolean>('is_recording'),
            () => stopRecordingActionRef.current?.(),
            error => console.error('Failed to check native recording state after transcript error:', error),
          );
          if (onTranscriptionError) {
            onTranscriptionError(errorMessage);
          }
        });

        // Transcription error listener - handles structured error objects with actionable flag
        const transcriptionErrorUnsubscribe = await listen('transcription-error', (event) => {
          console.log('transcription-error event received:', event);
          console.error('Transcription error received:', event.payload);

          let errorMessage: string;
          let isActionable = false;

          if (typeof event.payload === 'object' && event.payload !== null) {
            const payload = event.payload as { error: string, userMessage: string, actionable: boolean };
            errorMessage = payload.userMessage || payload.error;
            isActionable = payload.actionable || false;
          } else {
            errorMessage = String(event.payload);
          }

          Analytics.trackTranscriptionError(errorMessage);
          console.log('Tracked transcription error:', errorMessage);

          setTranscriptionErrors(prev => {
            const newCount = prev + 1;
            console.log('Transcription error count incremented:', newCount);
            return newCount;
          });
          console.log('Stopping native recording before recoverable processing due to transcription error');
          void stopRecordingAfterTranscriptionError(
            () => invoke<boolean>('is_recording'),
            () => stopRecordingActionRef.current?.(),
            error => console.error('Failed to check native recording state after transcription error:', error),
          );

          // For actionable errors (like model loading failures), the main page will handle showing the model selector
          // For regular errors, they are handled by useModalState global listener which shows a toast
          // We don't want to show a modal (via onTranscriptionError) AND a toast, so we skip the callback here
          /* if (onTranscriptionError && !isActionable) {
            onTranscriptionError(errorMessage);
          } */
        });

        // Pause/Resume events are now handled by RecordingStateContext
        // No need for duplicate listeners here

        // Speech detected listener - for UX feedback when VAD detects speech
        const speechDetectedUnsubscribe = await listen('speech-detected', (event) => {
          console.log('speech-detected event received:', event);
          setSpeechDetected(true);
        });

        unsubscribes = [
          transcriptErrorUnsubscribe,
          transcriptionErrorUnsubscribe,
          speechDetectedUnsubscribe
        ];
        console.log('Recording event listeners set up successfully');
      } catch (error) {
        console.error('Failed to set up recording event listeners:', error);
      }
    };

    setupListeners();

    return () => {
      console.log('Cleaning up recording event listeners');
      unsubscribes.forEach(unsubscribe => {
        if (unsubscribe && typeof unsubscribe === 'function') {
          unsubscribe();
        }
      });
    };
  }, [onRecordingStop, onTranscriptionError]);

  return (
    <TooltipProvider>
      <div className="flex flex-col space-y-2">
        <div className="flex items-center space-x-2 bg-white rounded-full shadow-lg px-4 py-2">
          {isProcessing && !isParentProcessing ? (
            <div className="flex items-center space-x-2">
              <div className="animate-spin rounded-full h-5 w-5 border-b-2 border-gray-900"></div>
              <span className="text-sm text-gray-600">Processing recording...</span>
            </div>
          ) : (
            <>
              {showPlayback ? (
                <>
                  <button
                    onClick={handleStartRecording}
                    className="w-10 h-10 flex items-center justify-center bg-red-500 rounded-full text-white hover:bg-red-600 transition-colors"
                  >
                    <Mic size={16} />
                  </button>

                  <div className="w-px h-6 bg-gray-200 mx-1" />

                  <div className="flex items-center space-x-1 mx-2">
                    <div className="text-sm text-gray-600 min-w-[40px]">
                      {formatTime(currentTime)}
                    </div>
                    <div
                      className="relative w-24 h-1 bg-gray-200 rounded-full"
                    >
                      <div
                        className="absolute h-full bg-blue-500 rounded-full"
                        style={{ width: `${progress}%` }}
                      />
                    </div>
                    <div className="text-sm text-gray-600 min-w-[40px]">
                      {formatTime(duration)}
                    </div>
                  </div>

                  <button
                    className="w-10 h-10 flex items-center justify-center bg-gray-300 rounded-full text-white cursor-not-allowed"
                    disabled
                  >
                    <Play size={16} />
                  </button>
                </>
              ) : (
                <>
                  {!isRecording ? (
                    // Start recording button
                    <Tooltip>
                      <TooltipTrigger asChild>
                        <button
                          onClick={() => {
                            Analytics.trackButtonClick('start_recording', 'recording_controls');
                            handleStartRecording();
                          }}
                          disabled={isStarting || isProcessing || isRecordingDisabled || isValidatingModel}
                          className={`w-12 h-12 flex items-center justify-center ${isStarting || isProcessing || isValidatingModel ? 'bg-gray-400' : 'bg-red-500 hover:bg-red-600'
                            } rounded-full text-white transition-colors relative`}
                        >
                          {isValidatingModel ? (
                            <div className="animate-spin rounded-full h-5 w-5 border-b-2 border-white"></div>
                          ) : (
                            <Mic size={20} />
                          )}
                        </button>
                      </TooltipTrigger>
                      <TooltipContent>
                        <p>Start recording</p>
                      </TooltipContent>
                    </Tooltip>
                  ) : (
                    // Recording controls (pause/resume + stop)
                    <>
                      <Tooltip>
                        <TooltipTrigger asChild>
                          <button
                            onClick={() => {
                              if (isPaused) {
                                Analytics.trackButtonClick('resume_recording', 'recording_controls');
                                handleResumeRecording();
                              } else {
                                Analytics.trackButtonClick('pause_recording', 'recording_controls');
                                handlePauseRecording();
                              }
                            }}
                            disabled={isPausing || isResuming || isStopping}
                            className={`w-10 h-10 flex items-center justify-center ${isPausing || isResuming || isStopping
                              ? 'bg-gray-200 border-2 border-gray-300 text-gray-400'
                              : 'bg-white border-2 border-gray-300 text-gray-600 hover:border-gray-400 hover:bg-gray-50'
                              } rounded-full transition-colors relative`}
                          >
                            {isPaused ? <Play size={16} /> : <Pause size={16} />}
                            {(isPausing || isResuming) && (
                              <div className="absolute -top-8 text-gray-600 font-medium text-xs">
                                {isPausing ? 'Pausing...' : 'Resuming...'}
                              </div>
                            )}
                          </button>
                        </TooltipTrigger>
                        <TooltipContent>
                          <p>{isPaused ? 'Resume recording' : 'Pause recording'}</p>
                        </TooltipContent>
                      </Tooltip>

                      <Tooltip>
                        <TooltipTrigger asChild>
                          <button
                            onClick={() => {
                              Analytics.trackButtonClick('stop_recording', 'recording_controls');
                              handleStopRecording();
                            }}
                            disabled={isStopping || isPausing || isResuming}
                            className={`w-10 h-10 flex items-center justify-center ${isStopping || isPausing || isResuming ? 'bg-gray-400' : 'bg-red-500 hover:bg-red-600'
                              } rounded-full text-white transition-colors relative`}
                          >
                            <Square size={16} />
                            {isStopping && (
                              <div className="absolute -top-8 text-gray-600 font-medium text-xs">
                                Stopping...
                              </div>
                            )}
                          </button>
                        </TooltipTrigger>
                        <TooltipContent>
                          <p>Stop recording</p>
                        </TooltipContent>
                      </Tooltip>
                    </>
                  )}

                  {!isRecording ? (
                    <Popover>
                      <PopoverTrigger asChild>
                        <button
                          type="button"
                          aria-label="Choose recording languages"
                          className="ml-2 flex h-10 w-10 items-center justify-center rounded-full text-red-500 transition-colors hover:bg-red-50 focus:outline-none focus:ring-2 focus:ring-red-200"
                        >
                          <Ellipsis size={24} aria-hidden="true" />
                        </button>
                      </PopoverTrigger>
                      <PopoverContent side="top" align="center" className="w-64 p-3">
                        <p className="text-sm font-semibold text-gray-900">Meeting language</p>
                        <p className="mt-1 text-xs leading-5 text-gray-500">
                          {transcriptionConfig.provider === 'localWhisper'
                            ? 'Bulgarian uses Whisper Large V3 (1.1 GB) for the highest local accuracy.'
                            : 'Bilingual meetings use fast automatic language detection.'}
                        </p>
                        <div className="mt-3 space-y-1">
                          {RECORDING_LANGUAGE_OPTIONS.map((language) => {
                            const checked = recordingLanguages.includes(language.code);
                            return (
                              <button
                                key={language.code}
                                type="button"
                                role="checkbox"
                                aria-checked={checked}
                                onClick={() => toggleRecordingLanguage(language.code)}
                                className="flex w-full items-center gap-3 rounded-lg px-2.5 py-2 text-left text-sm text-gray-800 transition-colors hover:bg-gray-50 focus:outline-none focus:ring-2 focus:ring-red-200"
                              >
                                <span
                                  className={`flex h-5 w-5 items-center justify-center rounded border ${
                                    checked
                                      ? 'border-red-500 bg-red-500 text-white'
                                      : 'border-gray-300 bg-white text-transparent'
                                  }`}
                                >
                                  <Check size={14} strokeWidth={3} aria-hidden="true" />
                                </span>
                                {language.label}
                              </button>
                            );
                          })}
                        </div>
                      </PopoverContent>
                    </Popover>
                  ) : (
                    <div className="flex items-center space-x-1 mx-4">
                      {barHeights.map((height, index) => (
                        <div
                          key={index}
                          className={`w-1 rounded-full transition-all duration-200 ${isPaused ? 'bg-orange-500' : 'bg-red-500'
                            }`}
                          style={{
                            height: !isPaused ? height : '4px',
                            opacity: isPaused ? 0.6 : 1,
                          }}
                        />
                      ))}
                    </div>
                  )}
                </>
              )}
            </>
          )}
        </div>

        {/* Show validation status only */}
        {isValidatingModel && (
          <div className="text-xs text-gray-600 text-center mt-2">
            Validating speech recognition...
          </div>
        )}

        {/* Device error alert */}
        {deviceError && (
          <Alert variant="destructive" className="mt-4 border-red-300 bg-red-50">
            <AlertCircle className="h-5 w-5 text-red-600" />
            <button
              onClick={() => setDeviceError(null)}
              className="absolute right-3 top-3 text-red-600 hover:text-red-800 transition-colors"
              aria-label="Close alert"
            >
              <X className="h-4 w-4" />
            </button>
            <AlertTitle className="text-red-800 font-semibold mb-2">
              {deviceError.title}
            </AlertTitle>
            <AlertDescription className="text-red-700">
              {deviceError.message.split('\n').map((line, i) => (
                <div key={i} className={i > 0 ? 'ml-2' : ''}>
                  {line}
                </div>
              ))}
            </AlertDescription>
          </Alert>
        )}

        {/* {showPlayback && recordingPath && (
        <div className="text-sm text-gray-600 px-4">
          Recording saved to: {recordingPath}
        </div>
      )} */}
      </div>
    </TooltipProvider>
  );
};
