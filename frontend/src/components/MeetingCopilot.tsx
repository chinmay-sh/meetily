'use client';

// Meeting copilot (fork addition): a side panel that researches topics raised in the
// live meeting (web and allowed folders), plus an Ask box for direct questions.

import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import ReactMarkdown from 'react-markdown';
import { FileText, Folder, FolderPlus, Loader2, Send, Settings2, Sparkles, X } from 'lucide-react';
import { useTranscripts } from '@/contexts/TranscriptContext';
import { useRecordingState } from '@/contexts/RecordingStateContext';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Switch } from '@/components/ui/switch';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Transcript } from '@/types';
import {
  CopilotCard,
  CopilotEngine,
  CopilotPrefs,
  CopilotSettings,
  DEFAULT_COPILOT_MODEL,
  addCopilotFolder,
  copilotResearch,
  getCopilotFolders,
  getCopilotSettings,
  listCopilotModels,
  loadCopilotPrefs,
  openCopilotFile,
  removeCopilotFolder,
  saveCopilotPrefs,
  setTavilyKey,
} from '@/lib/copilot';

const AUTO_INTERVAL_MS = 20_000;
const AUTO_CONTEXT_CHARS = 4_000; // roughly the last 3 minutes of speech
const ASK_CONTEXT_CHARS = 12_000;
/** Select item for "use the summary model" (Radix Select items cannot have an empty value). */
const SUMMARY_MODEL_ITEM = '__summary_model__';

type Entry = CopilotCard & { id: number; question?: string };
type ReportError = (message: string | null) => void;

const recentText = (transcripts: Transcript[], maxChars: number) =>
  transcripts.map(t => t.text.trim()).filter(Boolean).join('\n').slice(-maxChars);

const openUrl = (url: string) =>
  invoke('open_external_url', { url }).catch(err => console.error('Failed to open link:', err));

const modelFor = (prefs: CopilotPrefs) =>
  prefs.engine === 'custom' ? prefs.customModel : prefs.model || DEFAULT_COPILOT_MODEL;

export function MeetingCopilot() {
  const { transcripts } = useTranscripts();
  const { isRecording } = useRecordingState();
  const [prefs, setPrefs] = useState<CopilotPrefs | null>(null);
  const [entries, setEntries] = useState<Entry[]>([]);
  const [researching, setResearching] = useState(false);
  const [asking, setAsking] = useState(false);
  const [question, setQuestion] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [showSettings, setShowSettings] = useState(false);

  const transcriptsRef = useRef(transcripts);
  transcriptsRef.current = transcripts;
  const entriesRef = useRef(entries);
  entriesRef.current = entries;
  const lastCheckedCount = useRef(0);
  const nextId = useRef(0);

  // Loaded after mount: localStorage does not exist while Next pre-renders the page.
  useEffect(() => setPrefs(loadCopilotPrefs()), []);

  const updatePrefs = (change: Partial<CopilotPrefs>) =>
    setPrefs(prev => {
      const next = { ...(prev ?? loadCopilotPrefs()), ...change };
      saveCopilotPrefs(next);
      return next;
    });

  // A new recording starts with a clean panel (results are never stored).
  useEffect(() => {
    if (isRecording) {
      setEntries([]);
      setError(null);
      lastCheckedCount.current = 0;
    }
  }, [isRecording]);

  const enabled = prefs?.enabled ?? false;
  const auto = prefs?.auto ?? false;
  const engine: CopilotEngine = prefs?.engine ?? 'gemini';
  const model = prefs ? modelFor(prefs) : DEFAULT_COPILOT_MODEL;

  // Auto mode: every interval, if new speech arrived, ask whether anything is worth looking up.
  useEffect(() => {
    if (!enabled || !auto || !isRecording) return;
    let busy = false;
    const timer = setInterval(async () => {
      const count = transcriptsRef.current.length;
      if (busy || count === lastCheckedCount.current) return;
      busy = true;
      lastCheckedCount.current = count;
      setResearching(true);
      try {
        const card = await copilotResearch({
          engine,
          model,
          transcript: recentText(transcriptsRef.current, AUTO_CONTEXT_CHARS),
          covered: entriesRef.current.map(e => e.topic).filter(Boolean),
        });
        if (card) {
          setEntries(prev => [{ ...card, id: nextId.current++ }, ...prev]);
        }
        setError(null);
      } catch (e) {
        setError(String(e));
      } finally {
        busy = false;
        setResearching(false);
      }
    }, AUTO_INTERVAL_MS);
    return () => clearInterval(timer);
  }, [enabled, auto, engine, model, isRecording]);

  const ask = async () => {
    const q = question.trim();
    if (!q || asking) return;
    setAsking(true);
    setError(null);
    try {
      const card = await copilotResearch({
        engine,
        model,
        transcript: recentText(transcriptsRef.current, ASK_CONTEXT_CHARS),
        covered: [],
        question: q,
      });
      if (card) setEntries(prev => [{ ...card, id: nextId.current++, question: q }, ...prev]);
      setQuestion('');
    } catch (e) {
      setError(String(e));
    } finally {
      setAsking(false);
    }
  };

  if (!prefs) return null;

  if (!prefs.enabled) {
    return (
      <div className="w-12 shrink-0 border-l border-gray-200 bg-white flex justify-center pt-4">
        <Button
          variant="ghost"
          size="icon"
          title="Meeting copilot: live research from the web and your files"
          aria-label="Open meeting copilot"
          onClick={() => updatePrefs({ enabled: true })}
        >
          <Sparkles />
        </Button>
      </div>
    );
  }

  return (
    <aside className="w-96 shrink-0 border-l border-gray-200 bg-white flex flex-col" aria-label="Meeting copilot">
      <div className="p-4 border-b border-gray-200 space-y-3 max-h-[70%] overflow-y-auto">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2 font-medium text-gray-900">
            <Sparkles className="h-4 w-4" />
            Copilot
            {(researching || asking) && (
              <Loader2 className="h-4 w-4 animate-spin text-gray-400" aria-label="Researching" />
            )}
          </div>
          <div className="flex items-center">
            <Button
              variant={showSettings ? 'secondary' : 'ghost'}
              size="icon"
              title="Copilot settings"
              aria-label="Copilot settings"
              aria-expanded={showSettings}
              onClick={() => setShowSettings(v => !v)}
            >
              <Settings2 />
            </Button>
            <Button
              variant="ghost"
              size="icon"
              title="Turn off copilot"
              aria-label="Turn off copilot"
              onClick={() => updatePrefs({ enabled: false })}
            >
              <X />
            </Button>
          </div>
        </div>
        <label className="flex items-center justify-between gap-3 text-sm text-gray-700">
          Research topics automatically while recording
          <Switch checked={prefs.auto} onCheckedChange={value => updatePrefs({ auto: value })} />
        </label>
        <form
          className="flex gap-2"
          onSubmit={e => {
            e.preventDefault();
            ask();
          }}
        >
          <Input
            value={question}
            onChange={e => setQuestion(e.target.value)}
            placeholder="Ask anything…"
            aria-label="Ask the copilot"
            disabled={asking}
          />
          <Button type="submit" size="icon" title="Ask" aria-label="Ask" disabled={asking || !question.trim()}>
            <Send />
          </Button>
        </form>
        <p className="text-xs text-gray-500">
          {prefs.engine === 'custom'
            ? 'Sends the recent transcript, and text from any allowed files it reads, to your custom model endpoint, and search queries to Tavily.'
            : 'Sends the recent transcript, and text from any allowed files it reads, to Google Gemini using your Gemini API keys.'}
        </p>
        {error && <p className="text-xs text-red-600 break-words">{error}</p>}
        {showSettings && <SettingsSection prefs={prefs} updatePrefs={updatePrefs} onError={setError} />}
      </div>

      <div className="flex-1 overflow-y-auto p-4 pb-32 space-y-3">
        {entries.length === 0 && (
          <p className="text-sm text-gray-500 text-center mt-8">
            {prefs.auto
              ? 'Answers appear here when the meeting touches something worth looking up.'
              : 'Ask a question above.'}
          </p>
        )}
        {entries.map(entry => (
          <CopilotEntry key={entry.id} entry={entry} />
        ))}
      </div>
    </aside>
  );
}

function SettingsSection({
  prefs,
  updatePrefs,
  onError,
}: {
  prefs: CopilotPrefs;
  updatePrefs: (change: Partial<CopilotPrefs>) => void;
  onError: ReportError;
}) {
  const [settings, setSettings] = useState<CopilotSettings | null>(null);
  const [models, setModels] = useState<string[]>([]);
  const [folders, setFolders] = useState<string[]>([]);
  const [tavilyInput, setTavilyInput] = useState('');

  useEffect(() => {
    getCopilotSettings().then(setSettings).catch(e => onError(String(e)));
    getCopilotFolders().then(setFolders).catch(e => onError(String(e)));
  }, [onError]);

  useEffect(() => {
    if (prefs.engine === 'custom') listCopilotModels().then(setModels).catch(e => onError(String(e)));
  }, [prefs.engine, onError]);

  const changeFolders = (change: Promise<string[]>) =>
    change
      .then(list => {
        setFolders(list);
        onError(null);
      })
      .catch(e => onError(String(e)));

  const saveTavilyKey = (key: string) =>
    setTavilyKey(key)
      .then(next => {
        setSettings(next);
        setTavilyInput('');
        onError(null);
      })
      .catch(e => onError(String(e)));

  // Keep a saved model selectable even if the endpoint's list could not be loaded.
  const modelOptions =
    prefs.customModel && !models.includes(prefs.customModel) ? [prefs.customModel, ...models] : models;

  return (
    <div className="space-y-3 border-t border-gray-200 pt-3 text-xs text-gray-600">
      <div className="space-y-1">
        <span className="font-medium text-gray-700">Engine</span>
        <Select value={prefs.engine} onValueChange={value => updatePrefs({ engine: value as CopilotEngine })}>
          <SelectTrigger className="h-8 text-xs" aria-label="Copilot engine">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="gemini">Gemini + Google Search</SelectItem>
            <SelectItem value="custom">Custom model (summary endpoint)</SelectItem>
          </SelectContent>
        </Select>
      </div>

      {prefs.engine === 'gemini' ? (
        <div className="flex items-center gap-2">
          Model
          <Input
            className="h-7 text-xs"
            defaultValue={prefs.model}
            aria-label="Copilot Gemini model"
            onBlur={e => updatePrefs({ model: e.target.value.trim() || DEFAULT_COPILOT_MODEL })}
          />
        </div>
      ) : (
        <>
          {settings && !settings.customDefaultModel && (
            <p className="text-amber-700">Set up a custom OpenAI-compatible model in Settings → Summary first.</p>
          )}
          <div className="space-y-1">
            <span className="font-medium text-gray-700">Model</span>
            <Select
              value={prefs.customModel || SUMMARY_MODEL_ITEM}
              onValueChange={value => updatePrefs({ customModel: value === SUMMARY_MODEL_ITEM ? '' : value })}
            >
              <SelectTrigger className="h-8 text-xs" aria-label="Copilot model">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value={SUMMARY_MODEL_ITEM}>
                  Same as summaries{settings?.customDefaultModel ? ` (${settings.customDefaultModel})` : ''}
                </SelectItem>
                {modelOptions.map(m => (
                  <SelectItem key={m} value={m}>
                    {m}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="space-y-1">
            <span className="font-medium text-gray-700">Web search (Tavily)</span>
            {settings?.tavilyKeyHint ? (
              <div className="flex items-center justify-between">
                <span>Key …{settings.tavilyKeyHint} saved</span>
                <Button variant="ghost" size="sm" className="h-7 px-2 text-xs" onClick={() => saveTavilyKey('')}>
                  Remove
                </Button>
              </div>
            ) : (
              <>
                <form
                  className="flex gap-2"
                  onSubmit={e => {
                    e.preventDefault();
                    if (tavilyInput.trim()) saveTavilyKey(tavilyInput);
                  }}
                >
                  <Input
                    type="password"
                    className="h-7 text-xs"
                    placeholder="tvly-…"
                    autoComplete="off"
                    aria-label="Tavily API key"
                    value={tavilyInput}
                    onChange={e => setTavilyInput(e.target.value)}
                  />
                  <Button type="submit" size="sm" className="h-7 px-2 text-xs" disabled={!tavilyInput.trim()}>
                    Save
                  </Button>
                </form>
                <p>
                  Without a key the custom model can&apos;t search the web. Free keys at{' '}
                  <button type="button" className="text-blue-600 hover:underline" onClick={() => openUrl('https://tavily.com')}>
                    tavily.com
                  </button>{' '}
                  include 1,000 searches a month.
                </p>
              </>
            )}
          </div>
        </>
      )}

      <div className="space-y-1">
        <div className="flex items-center justify-between">
          <span className="font-medium text-gray-700">Folders it may read</span>
          <Button variant="ghost" size="sm" className="h-7 px-2 text-xs" onClick={() => changeFolders(addCopilotFolder())}>
            <FolderPlus className="h-3.5 w-3.5" />
            Add folder
          </Button>
        </div>
        {folders.length === 0 ? (
          <p>None. Add a folder to let it search your documents and code.</p>
        ) : (
          <ul className="space-y-1">
            {folders.map(folder => (
              <li key={folder} className="flex items-center gap-1.5">
                <Folder className="h-3.5 w-3.5 shrink-0" />
                <span className="truncate flex-1" title={folder}>
                  {folder}
                </span>
                <button
                  type="button"
                  className="shrink-0 text-gray-400 hover:text-gray-700"
                  title={`Stop reading ${folder}`}
                  aria-label={`Remove folder ${folder}`}
                  onClick={() => changeFolders(removeCopilotFolder(folder))}
                >
                  <X className="h-3.5 w-3.5" />
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}

function CopilotEntry({ entry }: { entry: Entry }) {
  return (
    <div className="rounded-lg border border-gray-200 p-3 text-sm space-y-2">
      {entry.question && <p className="text-xs text-gray-500">You asked: {entry.question}</p>}
      {entry.topic && <p className="font-medium text-gray-900">{entry.topic}</p>}
      <div className="text-gray-700 space-y-1 [&_ul]:list-disc [&_ul]:pl-4 [&_ol]:list-decimal [&_ol]:pl-4">
        <ReactMarkdown
          components={{
            // Links must open in the system browser, never inside the app window.
            a: ({ href, children }) => (
              <button type="button" className="text-blue-600 hover:underline" onClick={() => href && openUrl(href)}>
                {children}
              </button>
            ),
          }}
        >
          {entry.answer}
        </ReactMarkdown>
      </div>
      {entry.sources.length > 0 && (
        <div className="flex flex-wrap gap-x-3 gap-y-1">
          {entry.sources.map(source => (
            <button
              key={source.url}
              type="button"
              className="text-xs text-blue-600 hover:underline truncate max-w-full"
              title={source.url}
              onClick={() => openUrl(source.url)}
            >
              {source.title}
            </button>
          ))}
        </div>
      )}
      {entry.files.length > 0 && (
        <div className="flex flex-wrap gap-x-3 gap-y-1">
          {entry.files.map(file => (
            <button
              key={file.path}
              type="button"
              className="flex items-center gap-1 text-xs text-gray-700 hover:underline max-w-full"
              title={file.path}
              onClick={() => openCopilotFile(file.path).catch(err => console.error('Failed to open file:', err))}
            >
              <FileText className="h-3.5 w-3.5 shrink-0" />
              <span className="truncate">{file.name}</span>
            </button>
          ))}
        </div>
      )}
      {entry.suggestionsHtml && <SearchSuggestions html={entry.suggestionsHtml} />}
    </div>
  );
}

/** Google's Search Suggestions widget, isolated in a shadow root so its CSS cannot leak. */
function SearchSuggestions({ html }: { html: string }) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const host = ref.current;
    if (!host) return;
    const root = host.shadowRoot ?? host.attachShadow({ mode: 'open' });
    const doc = new DOMParser().parseFromString(html, 'text/html');
    doc.querySelectorAll('script, iframe, object, embed').forEach(node => node.remove());
    doc.querySelectorAll('*').forEach(el =>
      Array.from(el.attributes)
        .filter(attr => attr.name.toLowerCase().startsWith('on'))
        .forEach(attr => el.removeAttribute(attr.name))
    );
    root.innerHTML = doc.head.innerHTML + doc.body.innerHTML;

    const onClick = (event: Event) => {
      const link = event
        .composedPath()
        .find((node): node is HTMLAnchorElement => node instanceof HTMLAnchorElement && !!node.href);
      if (!link) return;
      event.preventDefault();
      openUrl(link.href);
    };
    root.addEventListener('click', onClick);
    return () => root.removeEventListener('click', onClick);
  }, [html]);

  return <div ref={ref} />;
}
