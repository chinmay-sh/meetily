# Gemini Live transcription (fork addition)

This fork adds **Gemini Live** (`gemini-3.5-transcribe-live`) as a cloud transcription
engine next to Parakeet and Whisper. It is tuned for Hindi + English (code-mixed) meetings.

## Build the Windows installer with GitHub Actions

No local tools are needed; GitHub builds the installer for you.

1. Open the fork on GitHub and click the **Actions** tab.
   - First time only: if GitHub says workflows are disabled for this fork, click
     **"I understand my workflows, go ahead and enable them"**.
2. In the left list, click **Build and Test - Windows**.
3. On the right, click the **Run workflow** dropdown and set:
   | Field | Value |
   |---|---|
   | Use workflow from | the branch to build (e.g. `main`, or the feature branch) |
   | Build type | `release` |
   | Sign the build | **unchecked** (the fork has no code-signing certificate) |
   | Run pre-build signing test | unchecked |
   | Upload build artifacts | checked |
4. Click the green **Run workflow** button. A new run appears after a few seconds
   (refresh if needed). A build takes roughly 45–90 minutes.
5. When the run shows a green check, open it and scroll to **Artifacts** at the bottom.
   Click **meetily-windows-x64-release-&lt;version&gt;** to download a zip.
6. Unzip it. Run `nsis\Meetily_<version>_x64-setup.exe` (the `msi` folder has an
   alternative installer; use one or the other).
   - The installer is not code-signed, so Windows SmartScreen shows a blue warning:
     click **More info → Run anyway**.
   - Installing replaces any official Meetily install (same app ID); settings, models
     and meetings are kept.

If a run fails, open the failed step in the run log; the most common cause is leaving
**Sign the build** checked.

Fork builds check for updates at this fork's GitHub releases, never at upstream's, so
the app will not "update" itself back to official Meetily.

## Turn on Gemini Live in the app

**During first-run setup** (no model downloads): on the "Getting things ready" step, click
**"Don't want to download models? Use Gemini cloud transcription instead"**, paste your key
and click **Test & add** (add more keys from other accounts if you have them), then
**Use Gemini and skip downloads** → **Continue**. Parakeet and the summary model are not
downloaded. For summaries, choose a cloud provider (Claude, OpenAI, Groq or OpenRouter) in
**Settings → Summary**, or download the built-in model there later.

**Later, from Settings:** open **Settings → Transcription**, choose
**☁️ Gemini Live (Cloud - Hindi + English)** in **Transcript Model**, then:

- **Gemini API keys**: paste a key (from aistudio.google.com) and click **Test & add**. Add
  more keys, e.g. from other Google accounts, as backups. The first key is the primary; use the
  ↑ button to reorder, the Wi-Fi button to test a key, and the bin to remove one. If you have no
  key here but saved a Gemini key for summaries, that key is used.
- **Hindi output script**: *Roman / Hinglish* (`chaliye start karte hain`) or *Devanagari*
  (`चलिए स्टार्ट करते हैं`). *Automatic* mixes both scripts.
- **Transcript style**: *Verbatim* keeps every word; *Smart* removes fillers.
- **Custom vocabulary**: names, products and jargon, one per line. Keep it short (under ~100
  terms); very long lists can make the model "hear" those words in short phrases.
- **When all keys fail**: *Cloud only* (default, no downloads) or a local engine (Parakeet or
  Whisper, which needs its model downloaded).

Click **Save and use Gemini Live**.

## How it behaves

- Audio is sent to Google only while recording with Gemini Live selected. Your local
  recording, meeting storage and summaries work as before.
- Speech segments are streamed as 16 kHz PCM. Text appears ~0.5–2.5 s after each sentence
  ends (similar to the local engines).
- Gemini sessions last at most ~10 minutes, so the app opens a fresh session every 8.5 minutes
  between sentences. The new session hears the last 2 seconds again for context, and repeated
  words are removed.
- **Several keys**: if the key in use fails for any reason (rate limit, quota, invalid key,
  server or network error), the same sentence is retried right away with the next key, and the
  app stays on the key that works.
  - A key that hits its **rate limit or quota** is paused for **5 minutes**, then becomes
    available again. If every key is paused, the one whose pause ends soonest is tried first,
    so with two keys a rate-limited second key hands back to the first.
  - Other errors (network, server) pause a key for 1 minute.
  - An **invalid key** is skipped for the rest of the recording.
- **When every key fails**:
  - *Cloud only*: untranscribed audio is held and retried (every 2–30 s) until Gemini answers,
    then the transcript catches up in order with correct timestamps. If Gemini is still
    unreachable 3 minutes after you press Stop, the remaining speech is skipped; the audio
    recording itself is always saved. If every key is rejected (invalid), transcription stops
    for that recording.
  - *Local engine*: the sentence is transcribed by Parakeet/Whisper on your computer; after 3
    failures in a row the local engine is used for 5 minutes, then Gemini is tried again.
- A notification appears for key switches, outages, recovery and fallback.
- Known quirk: in Roman mode, a very short isolated phrase can occasionally come back in
  Devanagari.

## For developers

Code: `frontend/src-tauri/src/audio/transcription/gemini_live/` (engine),
`frontend/src/components/GeminiLiveSettings.tsx`, `GeminiApiKeyList.tsx` and
`onboarding/GeminiCloudSetup.tsx` (UI). Upstream files only gain small
`"geminiLive"` hooks, to keep merges from upstream simple.

```bash
cd frontend/src-tauri
cargo test --lib gemini_live            # unit + mock-server tests
GEMINI_API_KEY=... MEETILY_GEMINI_TEST_PCM=clip_16k_s16le.pcm \
  cargo test --lib gemini_live::tests::real_api -- --ignored --nocapture
```
