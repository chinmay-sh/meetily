// Settings panel for the Gemini Live cloud transcription engine. Fork addition.
import { useEffect, useState } from 'react';
import { toast } from 'sonner';
import { Loader2 } from 'lucide-react';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from './ui/select';
import { Textarea } from './ui/textarea';
import { Button } from './ui/button';
import { Label } from './ui/label';
import { GeminiApiKeyList } from './GeminiApiKeyList';
import {
  activateGeminiLive,
  CLOUD_ONLY,
  GeminiLiveSettings as Settings,
  GeminiLiveSettingsView,
  getGeminiLiveSettings,
  MAX_VOCABULARY_TERMS,
  saveGeminiLiveSettings,
} from '@/lib/gemini-live';

interface GeminiLiveSettingsProps {
  isActive: boolean;
  onActivated: () => void;
}

const parseVocabulary = (text: string) =>
  text.split(/[\n,]/).map((t) => t.trim()).filter(Boolean);

const toSettings = (view: GeminiLiveSettingsView): Settings => ({
  languageMode: view.languageMode,
  customVocabulary: view.customVocabulary,
  transcriptionMode: view.transcriptionMode,
  fallbackProvider: view.fallbackProvider,
});

export function GeminiLiveSettings({ isActive, onActivated }: GeminiLiveSettingsProps) {
  const [view, setView] = useState<GeminiLiveSettingsView | null>(null);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [vocabularyText, setVocabularyText] = useState('');
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    getGeminiLiveSettings()
      .then((loaded) => {
        setView(loaded);
        setSettings(toSettings(loaded));
        setVocabularyText(loaded.customVocabulary.join('\n'));
      })
      .catch((error) => {
        console.error('Failed to load Gemini Live settings:', error);
        toast.error('Could not load Gemini Live settings', { description: String(error) });
      });
  }, []);

  if (!settings || !view) {
    return <div className="text-sm text-gray-500 mt-4">Loading Gemini Live settings…</div>;
  }

  const vocabulary = parseVocabulary(vocabularyText);
  const hasKey = view.apiKeyHints.length > 0 || view.hasSummaryApiKey;

  const handleSave = async () => {
    if (!hasKey) {
      toast.error('Add a Gemini API key first');
      return;
    }
    setSaving(true);
    try {
      const saved = await saveGeminiLiveSettings({ ...settings, customVocabulary: vocabulary });
      setView(saved);
      await activateGeminiLive();
      onActivated();
      toast.success(isActive ? 'Gemini Live settings saved' : 'Gemini Live is now your transcription engine');
    } catch (error) {
      toast.error('Failed to save Gemini Live settings', { description: String(error) });
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="mt-6 space-y-5">
      <div className="rounded-md border border-amber-200 bg-amber-50 p-3 text-sm text-amber-900">
        Cloud engine: meeting audio is sent to Google for transcription.
      </div>

      <div>
        <Label className="block text-sm font-medium text-gray-700 mb-1">Gemini API keys</Label>
        <GeminiApiKeyList view={view} onChange={setView} />
      </div>

      <div>
        <Label className="block text-sm font-medium text-gray-700 mb-1">Hindi output script</Label>
        <div className="mx-1">
          <Select
            value={settings.languageMode}
            onValueChange={(v) => setSettings({ ...settings, languageMode: v as Settings['languageMode'] })}
          >
            <SelectTrigger><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value="roman">Roman / Hinglish (“chaliye start karte hain”)</SelectItem>
              <SelectItem value="devanagari">Devanagari (“चलिए स्टार्ट करते हैं”)</SelectItem>
              <SelectItem value="auto">Automatic (may mix both scripts)</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>

      <div>
        <Label className="block text-sm font-medium text-gray-700 mb-1">Transcript style</Label>
        <div className="mx-1">
          <Select
            value={settings.transcriptionMode}
            onValueChange={(v) => setSettings({ ...settings, transcriptionMode: v as Settings['transcriptionMode'] })}
          >
            <SelectTrigger><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value="VERBATIM">Verbatim (every word, including fillers)</SelectItem>
              <SelectItem value="SMART">Smart (removes fillers, cleans formatting)</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>

      <div>
        <Label className="block text-sm font-medium text-gray-700 mb-1">Custom vocabulary</Label>
        <div className="mx-1">
          <Textarea
            rows={5}
            value={vocabularyText}
            onChange={(e) => setVocabularyText(e.target.value)}
            placeholder={'One term per line, e.g.\nMeetily\nKubernetes\nnames of colleagues and clients'}
          />
          <p className={`mt-1 text-xs ${vocabulary.length > MAX_VOCABULARY_TERMS ? 'text-red-600' : 'text-gray-500'}`}>
            {vocabulary.length} / {MAX_VOCABULARY_TERMS} terms. Names, products and jargon work best; keep
            the list short (under ~100) for best accuracy.
          </p>
        </div>
      </div>

      <div>
        <Label className="block text-sm font-medium text-gray-700 mb-1">When all keys fail</Label>
        <div className="mx-1">
          <Select
            value={settings.fallbackProvider}
            onValueChange={(v) => setSettings({ ...settings, fallbackProvider: v as Settings['fallbackProvider'] })}
          >
            <SelectTrigger><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value={CLOUD_ONLY}>Cloud only: hold the audio and keep retrying (no download)</SelectItem>
              <SelectItem value="parakeet">Use Parakeet on this computer (needs ~670 MB model)</SelectItem>
              <SelectItem value="localWhisper">Use Whisper on this computer (needs a Whisper model)</SelectItem>
            </SelectContent>
          </Select>
          <p className="mt-1 text-xs text-gray-500">
            {settings.fallbackProvider === CLOUD_ONLY
              ? 'Untranscribed audio is kept and retried until Gemini responds, so the transcript catches up. If Gemini is still down 3 minutes after you stop recording, the rest is skipped (the audio recording is always saved).'
              : 'The local model must be downloaded: pick that engine in the Transcript Model list above, download a model, then come back here and click Save.'}
          </p>
        </div>
      </div>

      <div className="mx-1 flex items-center gap-3">
        <Button type="button" onClick={handleSave} disabled={saving || vocabulary.length > MAX_VOCABULARY_TERMS}>
          {saving && <Loader2 className="h-4 w-4 mr-2 animate-spin" />}
          {isActive ? 'Save settings' : 'Save and use Gemini Live'}
        </Button>
        {isActive && <span className="text-sm text-green-700">✓ Gemini Live is active</span>}
      </div>
    </div>
  );
}
