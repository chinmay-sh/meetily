// Meeting copilot (fork addition): Gemini + Google Search answers during a live meeting.
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
  /** Google's Search Suggestions widget; its terms require showing it with the answer. */
  suggestionsHtml: string | null;
}

export interface CopilotPrefs {
  enabled: boolean;
  /** Research topics automatically while recording (otherwise only the Ask box). */
  auto: boolean;
  model: string;
}

export const DEFAULT_COPILOT_MODEL = 'gemini-3.5-flash-lite';
const DEFAULT_PREFS: CopilotPrefs = { enabled: false, auto: true, model: DEFAULT_COPILOT_MODEL };
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
  model: string;
  transcript: string;
  covered: string[];
  question?: string;
}) => invoke<CopilotCard | null>('copilot_research', args);

/** Folders the copilot may search. Rust owns this list and checks every file access against it. */
export const getCopilotFolders = () => invoke<string[]>('copilot_get_folders');
/** Opens a folder picker; resolves to the updated list (unchanged if cancelled). */
export const addCopilotFolder = () => invoke<string[]>('copilot_add_folder');
export const removeCopilotFolder = (folder: string) => invoke<string[]>('copilot_remove_folder', { folder });
export const openCopilotFile = (path: string) => invoke<void>('copilot_open_file', { path });
