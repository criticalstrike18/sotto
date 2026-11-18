# Known Audio Issues & Architectural Solutions

## Issue: Bluetooth Audio Profile Switching & Background Audio Degradation During Trigger

### 1. Problem Description
When triggering dictation (e.g. via keyboard hotkey or mouse trigger) on systems where a Bluetooth audio device (such as OnePlus Buds, Bullets Wireless Z2, or other consumer Bluetooth headsets) is active:
- Background audio, media, and game sounds suddenly sound noticeably louder, harsher, and stripped of bass/depth ("like a layer of equalization went away").
- In the legacy Always-On capture mode, Sotto keeps this state active even when the application is sitting idle in the background or minimized to the tray. The default Dual-Mic Splice mode does not.

### 2. Root Cause Analysis
1. **Bluetooth BR/EDR Physical Radio Bandwidth Limitation:**
   - **A2DP (Advanced Audio Distribution Profile):** Used for music playback. Delivers high-bitrate stereo audio (44.1/48 kHz) with manufacturer DSP equalization, bass boost, and ANC. The microphone is unpowered.
   - **HFP / SCO (Hands-Free Profile / Synchronous Connection-Oriented):** Used for two-way voice communications. Due to Bluetooth RF bandwidth constraints, both playback and capture are forced into low-bitrate mono (8/16 kHz mSBC/CVSD).
   - In HFP mode, the headset hardware disables music equalization and injects an aggressive telephone speech boost into mid-range frequencies (~1 kHz – 3.5 kHz). The human ear perceives this boost as sudden loudness and harshness.
2. **Legacy Stream Lifecycle (Always-On mode):**
   - `AudioRecorder::new()` can initialise a WASAPI capture stream on application startup and retain it permanently.
   - `AudioRecorder::stop()` only calls `stream.pause()`. Under Windows WASAPI, pausing a stream does not release the underlying COM `IAudioClient` session or Bluetooth Audio Gateway lock.
   - Consequently, the headset remains trapped in low-fidelity HFP/SCO mode 24/7 as long as Sotto is running. This is now isolated to the explicit Always-On fallback.

---

## 3. Full Architectural Solution

The optimal design provides high-fidelity A2DP playback when idle, clean Bluetooth microphone capture during dictation, zero dropped speech syllables, and zero driver thrashing.

### Component A: Ephemeral Stream Lifecycle
- **Idle State:** Sotto holds zero handles to the audio capture device (`active_stream: None`). Windows keeps the Bluetooth headset in pristine, high-bitrate A2DP stereo with complete dynamic equalization.
- **On Trigger:** Sotto dynamically instantiates and starts the input stream.
- **On Stop:** Sotto releases the stream, allowing Windows to return the headset to A2DP.

### Component B: Overcoming Bluetooth RF Link Latency (~200ms–400ms Handshake)
When Windows commands a Bluetooth device to activate its SCO channel, the physical radio handshake requires ~250ms to negotiate. To avoid losing the first syllable of speech:

1. **Dual-Device Instant Bridge (Recommended):**
   - Keep a lightweight, rolling 500ms circular RAM buffer active on the secondary hardware device (e.g. onboard `Microphone Array (Realtek(R) Audio)` or USB mic).
   - On trigger, record from millisecond 0 using the onboard array buffer while concurrently initiating the Bluetooth SCO handshake in the background.
   - When the Bluetooth stream delivers its first valid audio buffer, smoothly cross-fade from the onboard array to the Bluetooth headset microphone.
   - **Outcome:** Exact millisecond-zero speech capture with zero truncated syllables, while retaining close-mic Bluetooth clarity for the rest of dictation.

2. **Cooldown Teardown (Graceful Tail):**
   - On `stop_dictation()`, immediately finish the transcription payload, but maintain the active hardware stream handle for an idle grace period (e.g. 5–8 seconds) before releasing the WASAPI client.
   - If the user re-triggers dictation within that window, latency is 0ms (no Bluetooth RF re-negotiation).
   - If no trigger occurs within the grace period, cleanly drop the stream and release the device back to A2DP stereo.

3. **Predictive Pre-Warming:**
   - Begin the asynchronous Bluetooth wake routine on initial modifier down (e.g. `Ctrl` or `F8` down event), completing the RF handshake before the user's vocalization starts.

### Component C: Programmatic Ducking Suppression
- On Windows 10 (Build 20348+) and Windows 11, query `IAudioClientDuckingControl` on the WASAPI `IAudioClient`:
  ```rust
  ducking_control.SetDuckingOptionsForCurrentStream(
      AUDIO_DUCKING_OPTIONS_DO_NOT_DUCK_OTHER_STREAMS
  );
  ```
- Explicitly prevents Windows Audio Service from attenuating other applications' audio sessions.

### Component D: Call-Aware Media Auto-Pause / Resume
- **Do not send `VK_MEDIA_PLAY_PAUSE`.** A blind media key targets whichever session Windows considers current and can mute a meeting or toggle the wrong player.
- Query Windows SMTC (`GlobalSystemMediaTransportControlsSessionManager`) for the session that is actually in `Playing` state, then call `TryPauseAsync()` on that exact session.
- Refuse sessions whose app or title looks like a call (Teams, Zoom, Webex, Discord, Slack, Skype, WhatsApp, Telegram, FaceTime, Google Meet, "in a call", etc.). Unknown apps are also left untouched; only known media players and browsers are eligible.
- Remember the paused session and resume **only that session** with `TryPlayAsync()`. If its COM object expires during a long transcription, fall back by matching the same app ID, never the global current session.
- Run all COM/WinRT work on one dedicated MTA thread. The trigger path only enqueues a command, so a stalled media app cannot delay the recorder or the hotkey.
- Pause before the microphone opens, keep media paused through transcription and text injection, then resume 140 ms after injection to give the audio route a short settle before playback returns.
- Cancel, empty-transcript, transcription-error, recorder-start-error, and settings-disable paths resume without the 140 ms settle. Application exit asks the worker to resume and waits briefly for acknowledgement before terminating.

### Component E: Start / Stop Chimes
- Synthesize two 300 ms mono WAV cues in memory at 48 kHz: a rising G4 → C5 perfect fourth on start and the same interval falling on stop.
- The character comes from the reference cue's measured signature: two nearly pure sine bells (harmonics around −53 dBc), a ~4 ms raised-cosine attack, and a smooth exponential ring. The reference measured 394 Hz → 527 Hz with a ~106 ms decay constant; Sotto uses equal-tempered G4/C5, a 68 ms decay constant, and a 300 ms buffer so the cue stays out of the first spoken syllable.
- Normalize to a 0.16 full-scale peak and close the shortened tail with a 25 ms smooth fade. No hard beeps, no partial stacks, and no system `MessageBeep` sound.
- Play via `PlaySoundW(SND_MEMORY | SND_ASYNC | SND_NODEFAULT)` after capture is armed, so the cue follows the route the take is using. The stop cue plays after the recorder drops the headset stream.
- The cue is generated in code rather than bundled as an MP3: no decoder or codec dependency, no asset download, no sample-rate conversion at playback, deterministic output, and zero added binary size. The source MP3 was analyzed only to reproduce its envelope and interval; it is not loaded at runtime.
- The start cue is deliberately short and quiet because it can be picked up by the microphone. Users who find it intrusive can disable cues under Customization → Feedback & Media.

### Component F: Floating Pill Placement
- Resolve the monitor under the pill's center and clamp the window origin to that monitor's `rcWork` on startup, during every drag, and periodically while the app runs.
- Intersect the work area with the actual visible taskbar rectangle when the taskbar is not auto-hidden. This covers shell mods such as Windhawk that draw a taller taskbar without updating the reserved work area; a normal docked taskbar already ends at `rcWork`, so the intersection is a no-op.
- A saved position that is now under the taskbar is repaired before the first paint. Dragging stops at the usable edge instead of disappearing underneath it.
- The periodic check only moves the pill when it is actually outside the usable area, so it does not fight a drag and does not require a z-order war with the taskbar.
- Auto-hidden taskbars are intentionally not reserved: the pill may use the full screen height, and the taskbar can temporarily cover it while revealed.

---

## 4. Implementation Status (2026-10-23)

`CaptureMode` is stored in `AppConfig.capture_mode` and selectable in **Customization → Microphone Capture**:

| Mode | Idle behaviour | A2DP while idle | First-syllable coverage |
| --- | --- | --- | --- |
| `DualMicSplice` (default) | Onboard/USB pre-roll ring stays warm; no headset handle | Yes | Onboard pre-roll from before the trigger |
| `AlwaysOn` (legacy) | Default/headset stream pre-built and paused between takes | No (HFP stays latched) | Whatever the headset delivers after resume |
| `OnboardOnly` | Onboard/USB stream only; headset endpoint never opened | Yes | Onboard only (no headset handshake) |

The bridge lives in `desktop/src/audio/recorder.rs` and `desktop/src/audio/splice.rs`:

1. On trigger, the last `120 ms` of the onboard ring is copied into the take and the headset stream is opened in parallel.
2. The take keeps both sources until `stop()`, so a failed headset stream can still finish on the onboard mic.
3. At stop, both takes are downmixed/resampled to 16 kHz mono; normalized cross-correlation finds the true lag around the wall-clock prior; the hand-off is placed at a low-energy point with a 50 ms equal-power crossfade and ±6 dB level match.
4. The headset `cpal::Stream` is **dropped** (not merely paused) on stop, releasing `IAudioClient` and returning the device to A2DP.

Every take writes a JSONL record to `%APPDATA%\Sotto\capture_timing.jsonl` with the handshake window, measured lag/confidence, splice processing time, finalize time, and first-speech-after-trigger position. Run the live comparison with:

```powershell
cargo run -- --capture-bench --capture-mode=dual-mic --takes=3 --seconds=3
cargo run -- --capture-bench --capture-mode=always-on --takes=3 --seconds=3
cargo run -- --capture-bench --capture-mode=onboard-only --takes=3 --seconds=3
```

The pure splice path is covered by synthetic unit tests in `audio/splice.rs` (known 250–300 ms delays, timeline preservation, silence fallback, and a processing-time budget).

### Media and feedback status

- `desktop/src/media.rs` implements the SMTC worker and the conservative call/unknown-app policy. The policy is unit-tested; the WinRT transport is exercised at runtime only when a media app is playing.
- `desktop/src/audio/chime.rs` synthesizes and validates both WAV cues: 300 ms, G4 → C5 / C5 → G4, pure sine bells with a soft attack and exponential ring. `chimes_enabled` and `media_pause_enabled` default to `true` in `AppConfig`, including for configs written before these fields existed.
- Customization → **Feedback & Media** exposes both toggles. They apply immediately and are persisted with the rest of the preset.
- Resume timing: success/empty/error paths use a 140 ms delayed resume after injection or failure; cancel and disable paths resume immediately.
- Floating pill placement is clamped per monitor at startup, while dragging, and by a 2 s guard. The geometry is unit-tested for bottom/top/side taskbars, a taller-than-`rcWork` taskbar, multi-monitor isolation, and off-screen positions.
- **Verification gap:** this machine has no distinct Bluetooth capture endpoint, so real SCO handshake and A2DP-return timings could not be measured. The SMTC path was smoke-tested against a local Chrome media session (pause and resume both acknowledged), and chime playback produced no winmm error; audible cue quality still needs a human check on the target headset. The taskbar path was checked against the live 2560×1440 desktop (48 px taskbar exactly at the `rcWork` edge); a Windhawk mod that changes the taskbar height still needs a manual run with the user's active mod configuration.

### Repository cleanup (2026-10-25)

The dictation work was followed by a conservative prune: the direct `arboard` dependency was replaced by the existing Win32 clipboard helper (`inject::copy_text_to_clipboard`, same retry and history/cloud exclusion formats), unused Tokio features were removed, `windows-sys` moved to 0.60 with four unused feature groups dropped, two unreferenced SVG icons were deleted, and callerless helpers were removed. A second audit moved the test-only UI spec mirror into `desktop/tests/support/`, deleted abandoned scroll-window helpers and unused scroll/test-only APIs, and removed five further uncalled helpers. All existing dictation, media, chime, capture-mode, and pill-placement behavior is unchanged. `cargo check --all-targets` is clean, the full suite passes (269 passed, 0 failed, 3 ignored), and the optimized release build succeeds at 56.58 MiB (rebuilt after the second audit).
