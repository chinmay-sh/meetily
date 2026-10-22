// Onboarding option to use Gemini Live (cloud) instead of downloading local models.
// Fork addition.
import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'sonner';
import { Cloud, Loader2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { GeminiApiKeyList } from '@/components/GeminiApiKeyList';
import { useOnboarding } from '@/contexts/OnboardingContext';
import {
  CLOUD_ONLY,
  GeminiLiveSettingsView,
  getGeminiLiveSettings,
  saveGeminiLiveSettings,
} from '@/lib/gemini-live';

const PARAKEET_MODEL = 'parakeet-tdt-0.6b-v3-int8';

export function GeminiCloudSetup() {
  const { cloudTranscription, setCloudTranscription, selectedSummaryModel } = useOnboarding();
  const [open, setOpen] = useState(false);
  const [view, setView] = useState<GeminiLiveSettingsView | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (open && !view) {
      getGeminiLiveSettings()
        .then(setView)
        .catch((error) => toast.error('Could not load Gemini settings', { description: String(error) }));
    }
  }, [open, view]);

  if (cloudTranscription) {
    return (
      <div className="w-full max-w-lg rounded-xl border border-green-200 bg-green-50 p-5 text-sm text-green-900">
        <p className="font-medium flex items-center gap-2">
          <Cloud className="w-4 h-4" /> Gemini cloud transcription selected
        </p>
        <p className="mt-1">
          No models will be downloaded. For meeting summaries, pick a cloud provider (Claude, OpenAI,
          Groq or OpenRouter) or download the built-in model later in Settings → Summary.
        </p>
      </div>
    );
  }

  if (!open) {
    return (
      <button type="button" className="text-sm text-blue-600 hover:underline" onClick={() => setOpen(true)}>
        Don&apos;t want to download models? Use Gemini cloud transcription instead
      </button>
    );
  }

  const useCloud = async () => {
    if (!view) return;
    setSaving(true);
    try {
      await saveGeminiLiveSettings({
        languageMode: view.languageMode,
        customVocabulary: view.customVocabulary,
        transcriptionMode: view.transcriptionMode,
        fallbackProvider: CLOUD_ONLY,
      });
      // Stop the local model downloads started by this step.
      await invoke('parakeet_cancel_download', { modelName: PARAKEET_MODEL }).catch(() => {});
      if (selectedSummaryModel) {
        await invoke('builtin_ai_cancel_download', { modelName: selectedSummaryModel }).catch(() => {});
      }
      setCloudTranscription(true);
    } catch (error) {
      toast.error('Could not switch to Gemini', { description: String(error) });
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="w-full max-w-lg rounded-xl border border-gray-200 bg-white p-5 space-y-3">
      <div>
        <p className="font-medium text-gray-900 flex items-center gap-2">
          <Cloud className="w-4 h-4" /> Gemini Live cloud transcription
        </p>
        <p className="text-sm text-gray-600 mt-1">
          Hindi + English transcription by Google Gemini. Meeting audio is sent to Google. Add one
          key, or several from different accounts as backups.
        </p>
      </div>
      {view ? <GeminiApiKeyList view={view} onChange={setView} /> : <Loader2 className="w-4 h-4 animate-spin" />}
      <div className="flex items-center gap-3">
        <Button type="button" onClick={useCloud} disabled={saving || !view || view.apiKeyHints.length === 0}>
          {saving && <Loader2 className="w-4 h-4 mr-2 animate-spin" />}
          Use Gemini and skip downloads
        </Button>
        <button type="button" className="text-sm text-gray-500 hover:underline" onClick={() => setOpen(false)}>
          Cancel
        </button>
      </div>
    </div>
  );
}
