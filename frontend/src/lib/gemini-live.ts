// Gemini Live (cloud) transcription: Tauri command wrappers. Fork addition.
import { invoke } from '@tauri-apps/api/core';

export const GEMINI_LIVE_PROVIDER = 'geminiLive';
export const GEMINI_LIVE_MODEL = 'gemini-3.5-transcribe-live';
/** Backup engine value for cloud-only mode (hold audio and retry, no local model). */
export const CLOUD_ONLY = 'none';

export type GeminiLanguageMode = 'roman' | 'devanagari' | 'auto';
export type GeminiTranscriptionMode = 'VERBATIM' | 'SMART';
export type GeminiFallbackProvider = typeof CLOUD_ONLY | 'parakeet' | 'localWhisper';

export interface GeminiLiveSettings {
  languageMode: GeminiLanguageMode;
  customVocabulary: string[];
  transcriptionMode: GeminiTranscriptionMode;
  fallbackProvider: GeminiFallbackProvider;
}

export interface GeminiLiveSettingsView extends GeminiLiveSettings {
  /** Last 4 characters of each saved key, primary first. */
  apiKeyHints: string[];
  hasSummaryApiKey: boolean;
}

export interface GeminiLiveStatus {
  kind: 'key_switch' | 'outage' | 'recovered' | 'gave_up' | 'skipped' | 'fallback' | 'resumed' | 'disabled';
  message: string;
}

export const MAX_VOCABULARY_TERMS = 1000;
export const MAX_API_KEYS = 10;

export const getGeminiLiveSettings = () =>
  invoke<GeminiLiveSettingsView>('gemini_live_get_settings');

export const saveGeminiLiveSettings = (settings: GeminiLiveSettings) =>
  invoke<GeminiLiveSettingsView>('gemini_live_save_settings', { settings });

export const addGeminiApiKey = (apiKey: string) =>
  invoke<GeminiLiveSettingsView>('gemini_live_add_api_key', { apiKey });

export const removeGeminiApiKey = (index: number) =>
  invoke<GeminiLiveSettingsView>('gemini_live_remove_api_key', { index });

export const moveGeminiApiKey = (index: number, toIndex: number) =>
  invoke<GeminiLiveSettingsView>('gemini_live_move_api_key', { index, toIndex });

/** Test a pasted key, the saved key at `index`, or (neither) the key that would be used. */
export const testGeminiLiveConnection = (apiKey: string | null, index: number | null = null) =>
  invoke<string>('gemini_live_test_connection', { apiKey, index });

/** Open the folder with gemini-live.log; resolves to the log file path. */
export const openGeminiLiveLogFolder = () => invoke<string>('gemini_live_open_log_folder');

/** Make Gemini Live the active transcription engine. */
export const activateGeminiLive = () =>
  invoke('api_save_transcript_config', {
    provider: GEMINI_LIVE_PROVIDER,
    model: GEMINI_LIVE_MODEL,
    apiKey: null,
  });

/**
 * Recording readiness checks use the local backup engine when one is configured; in
 * cloud-only mode there is no local model to check (returns CLOUD_ONLY).
 */
export async function getGeminiLiveFallbackProvider(): Promise<GeminiFallbackProvider> {
  try {
    return (await getGeminiLiveSettings()).fallbackProvider;
  } catch (error) {
    console.error('Failed to load Gemini Live settings:', error);
    return CLOUD_ONLY;
  }
}
