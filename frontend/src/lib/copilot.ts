// Meeting copilot (fork addition): live answers during a meeting from the web and the user's files.
import { invoke } from '@tauri-apps/api/core';

export interface CopilotSource {
  title: string;
  url: string;
}

export interface CopilotFile {
  name: string;
  path: string;
}

export interface CopilotCard {
  topic: string;
  answer: string;
  sources: CopilotSource[];
  /** Files from the allowed folders that were read for this answer. */
  files: CopilotFile[];
  /** Google's Search Suggestions widget (Gemini engine); its terms require showing it with the answer. */
  suggestionsHtml: string | null;
}

/** 'gemini': Gemini + Google Search (Gemini Live keys). 'custom': the summary model's OpenAI-compatible endpoint + Tavily. */
export type CopilotEngine = 'gemini' | 'custom';

export interface CopilotPrefs {
  enabled: boolean;
  /** Research topics automatically while recording (otherwise only the Ask box). */
  auto: boolean;
  /** Seconds between automatic checks for new speech. */
  autoIntervalSeconds: number;
  /** Let the model search the web (Google Search or Tavily). The Tavily key stays saved either way. */
  webSearch: boolean;
  engine: CopilotEngine;
  /** Gemini model for the 'gemini' engine. */
  model: string;
  /** Model for the 'custom' engine; '' means the model configured for summaries. */
  customModel: string;
}

export interface CopilotSettings {
  /** Last 4 characters of the saved Tavily key. */
  tavilyKeyHint: string | null;
  /** Model configured for summaries on the custom endpoint (null if none is set up). */
  customDefaultModel: string | null;
}

export const DEFAULT_COPILOT_MODEL = 'gemini-3.5-flash-lite';
export const AUTO_INTERVAL_CHOICES = [5, 10, 15, 20, 30, 45, 60];
const DEFAULT_PREFS: CopilotPrefs = {
  enabled: false,
  auto: true,
  autoIntervalSeconds: 10,
  webSearch: true,
  engine: 'gemini',
  model: DEFAULT_COPILOT_MODEL,
  customModel: '',
};
const STORAGE_KEY = 'meetingCopilot';

export function loadCopilotPrefs(): CopilotPrefs {
  try {
    return { ...DEFAULT_PREFS, ...JSON.parse(localStorage.getItem(STORAGE_KEY) || '{}') };
  } catch {
    return DEFAULT_PREFS;
  }
}

export function saveCopilotPrefs(prefs: CopilotPrefs) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(prefs));
  } catch {
    // Preferences are a convenience; the copilot works without them.
  }
}

/** Resolves to null when nothing in the transcript is worth researching (auto mode only). */
export const copilotResearch = (args: {
  engine: CopilotEngine;
  model: string;
  webSearch: boolean;
  transcript: string;
  covered: string[];
  question?: string;
}) => invoke<CopilotCard | null>('copilot_research', args);

export const getCopilotSettings = () => invoke<CopilotSettings>('copilot_get_settings');
/** Saves the Tavily key; an empty string removes it. The key stays in the backend. */
export const setTavilyKey = (apiKey: string) => invoke<CopilotSettings>('copilot_set_tavily_key', { apiKey });
/** Models offered by the custom OpenAI-compatible endpoint used for summaries. */
export const listCopilotModels = () => invoke<string[]>('copilot_list_models');

/** Folders the copilot may search. Rust owns this list and checks every file access against it. */
export const getCopilotFolders = () => invoke<string[]>('copilot_get_folders');
/** Opens a folder picker; resolves to the updated list (unchanged if cancelled). */
export const addCopilotFolder = () => invoke<string[]>('copilot_add_folder');
export const removeCopilotFolder = (folder: string) => invoke<string[]>('copilot_remove_folder', { folder });
export const openCopilotFile = (path: string) => invoke<void>('copilot_open_file', { path });
