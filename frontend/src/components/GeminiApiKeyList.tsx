// Ordered list of Gemini API keys (primary + backups) with add/test/reorder/remove.
// Fork addition; used by GeminiLiveSettings and the onboarding cloud option.
import { useState } from 'react';
import { toast } from 'sonner';
import { ArrowUp, Eye, EyeOff, Loader2, Plus, Trash2, Wifi } from 'lucide-react';
import { Input } from './ui/input';
import { Button } from './ui/button';
import {
  addGeminiApiKey,
  GeminiLiveSettingsView,
  MAX_API_KEYS,
  moveGeminiApiKey,
  removeGeminiApiKey,
  testGeminiLiveConnection,
} from '@/lib/gemini-live';

interface GeminiApiKeyListProps {
  view: GeminiLiveSettingsView;
  onChange: (view: GeminiLiveSettingsView) => void;
}

export function GeminiApiKeyList({ view, onChange }: GeminiApiKeyListProps) {
  const [newKey, setNewKey] = useState('');
  const [showKey, setShowKey] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);

  const run = async (id: string, action: () => Promise<void>) => {
    setBusy(id);
    try {
      await action();
    } catch (error) {
      toast.error('Gemini API key', { description: String(error) });
    } finally {
      setBusy(null);
    }
  };

  const test = (index: number | null) =>
    run(`test-${index ?? 'new'}`, async () => {
      const message = await testGeminiLiveConnection(index === null ? newKey.trim() : null, index);
      toast.success('Key works', { description: message });
    });

  const add = () =>
    run('add', async () => {
      // Check the key before saving it so typos are caught right away.
      await testGeminiLiveConnection(newKey.trim());
      onChange(await addGeminiApiKey(newKey.trim()));
      setNewKey('');
      toast.success('Key tested and saved');
    });

  const keys = view.apiKeyHints;
  const icon = (id: string, Icon: typeof Wifi) =>
    busy === id ? <Loader2 className="h-4 w-4 animate-spin" /> : <Icon className="h-4 w-4" />;

  return (
    <div className="space-y-2">
      {keys.length > 0 && (
        <ul className="divide-y rounded-md border mx-1">
          {keys.map((hint, i) => (
            <li key={`${hint}-${i}`} className="flex items-center gap-2 px-3 py-2 text-sm">
              <span className="w-20 text-xs font-medium text-gray-500">{i === 0 ? 'Primary' : `Backup ${i}`}</span>
              <span className="flex-1 font-mono text-gray-800">••••••••{hint}</span>
              <Button type="button" variant="ghost" size="icon" title="Test this key" disabled={!!busy} onClick={() => test(i)}>
                {icon(`test-${i}`, Wifi)}
              </Button>
              <Button
                type="button"
                variant="ghost"
                size="icon"
                title="Move up"
                disabled={!!busy || i === 0}
                onClick={() => run(`up-${i}`, async () => onChange(await moveGeminiApiKey(i, i - 1)))}
              >
                {icon(`up-${i}`, ArrowUp)}
              </Button>
              <Button
                type="button"
                variant="ghost"
                size="icon"
                title="Remove this key"
                disabled={!!busy}
                onClick={() => run(`remove-${i}`, async () => onChange(await removeGeminiApiKey(i)))}
              >
                {icon(`remove-${i}`, Trash2)}
              </Button>
            </li>
          ))}
        </ul>
      )}

      {keys.length < MAX_API_KEYS && (
        <div className="flex gap-2 mx-1">
          <div className="relative flex-1">
            <Input
              type={showKey ? 'text' : 'password'}
              className="pr-10"
              value={newKey}
              onChange={(e) => setNewKey(e.target.value)}
              placeholder={keys.length === 0 ? 'Paste your Gemini API key' : 'Paste a backup key (another account)'}
            />
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="absolute inset-y-0 right-0"
              onClick={() => setShowKey(!showKey)}
            >
              {showKey ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
            </Button>
          </div>
          <Button type="button" variant="outline" disabled={!!busy || !newKey.trim()} onClick={add}>
            {busy === 'add' ? <Loader2 className="h-4 w-4 mr-2 animate-spin" /> : <Plus className="h-4 w-4 mr-2" />}
            Test &amp; add
          </Button>
        </div>
      )}

      <p className="text-xs text-gray-500 mx-1">
        {keys.length === 0 && view.hasSummaryApiKey
          ? 'No key here yet: the Gemini key from your summary settings will be used. '
          : ''}
        Keys are used in order. If the key in use fails, the next one takes over immediately. A key
        that hits its rate limit or quota is paused for 5 minutes and then used again; an invalid key
        is skipped for the rest of the meeting. Get keys at aistudio.google.com.
      </p>
    </div>
  );
}
