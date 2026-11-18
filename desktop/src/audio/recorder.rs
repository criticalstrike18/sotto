// Copyright (c) 2026 Sotto
// In-Memory RAM Audio Recorder (cpal + 16kHz Mono Resampler + Zero Disk I/O)
//
// Three capture strategies (see config::CaptureMode):
//
//   AlwaysOn       - legacy: the default (often Bluetooth) stream is pre-built
//                    and paused between takes. Fastest repeat takes, but the
//                    headset stays in HFP/SCO, so A2DP playback is degraded.
//   DualMicSplice  - A2DP-safe: an onboard/USB mic runs a rolling pre-roll ring
//                    while idle. On trigger the headset stream opens in
//                    parallel, and once it delivers real speech the two takes
//                    are correlation-aligned and spliced. The headset stream is
//                    dropped on stop, so Windows returns it to A2DP.
//   OnboardOnly    - never open the headset capture endpoint.
//
// The splice decisions live in audio::splice (pure, unit-tested); this file
// only handles devices, buffers, and timing.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, SupportedStreamConfig};

use crate::audio::chunker::{self, SplitReason};
use crate::audio::filter::{
    apply_digital_gain, compute_peak, compute_rms, trim_trailing_silence, validate_audio, AudioValidation, BiquadHighPass,
    MIN_RECORDING_MS, SILENCE_RMS_THRESHOLD,
};
use crate::audio::resample::{
    downmix_interleaved_to_mono, float_to_pcm16, resample_linear, TARGET_SAMPLE_RATE,
};
use crate::audio::splice::{self, SpliceConfig, DEFAULT_SPEECH_RMS};
use crate::audio::timing::{self, CaptureTiming, SourceClock};
use crate::audio::wav::encode_pcm16_to_wav;
use crate::config::CaptureMode;

/// Audio kept before the trigger so the first syllable is not clipped.
const PREROLL_LEAD_MS: u32 = 120;
/// Rolling ring depth. Also the maximum pre-roll if the app was busy.
const PREROLL_RING_MS: u32 = 1500;

#[derive(Debug, Clone)]
pub struct AudioPayload {
    pub wav_bytes: Vec<u8>,
    pub pcm_samples: Vec<i16>,
    pub duration_ms: u32,
    pub rms: f32,
    pub peak: f32,
    pub digital_gain: f32,
}

/// A sealed chunk that has been removed from the live recorder buffers. The
/// caller turns it into a WAV outside the recorder lock, so sealing a chunk
/// never stalls the audio callback for the duration of the filter/AGC pass.
#[derive(Debug, Clone)]
pub struct SealedAudioChunk {
    pub pcm16k: Vec<i16>,
    pub duration_ms: u32,
}

/// Incremental 16 kHz mono view of one take.
///
/// The raw capture buffers are source-rate and source-channel-count dependent
/// (often 48 kHz stereo float32). Draining them into this normalized stream on
/// every pump keeps resident memory proportional to the 16 kHz upload payload
/// instead of the much larger raw device format.
#[derive(Default)]
struct LongCaptureState {
    active: bool,
    /// Finalized mono stream after the dual-mic handshake (or the only stream
    /// for single-mic capture).
    pcm16k: Vec<i16>,
    /// Dual-mic inputs are kept separate until the handshake is proven.
    headset16k: Vec<i16>,
    onboard16k: Vec<i16>,
    dual_spliced: bool,
    primary_total_ms: u64,
    headset_total_ms: u64,
    onboard_total_ms: u64,
    sealed_ms: u64,
    measured_lag_ms: Option<f32>,
    lag_confidence: Option<f32>,
    splice_fallback: Option<String>,
    splice_processing_ms: u64,
}


#[derive(Clone)]
struct Meters {
    energy: Arc<AtomicU32>,
    low: Arc<AtomicU32>,
    mid: Arc<AtomicU32>,
    high: Arc<AtomicU32>,
}

/// Everything an input callback needs. Kept small and cloneable so the same
/// code path serves the legacy stream, the onboard ring, and the headset take.
#[derive(Clone)]
struct StreamSink {
    is_recording: Arc<AtomicBool>,
    buffer: Arc<Mutex<Vec<f32>>>,
    ring: Option<Arc<Mutex<VecDeque<f32>>>>,
    ring_capacity: usize,
    /// true = append mono samples (onboard bridge); false = interleaved (legacy).
    mono_take: bool,
    channels: u16,
    sample_rate: u32,
    clock: Arc<SourceClock>,
    meters: Option<Meters>,
    first_chunk_ms: Arc<AtomicU32>,
    speech_rms: f32,
}

struct OpenedStream {
    name: String,
    sample_rate: u32,
    channels: u16,
    stream: Stream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrimaryState {
    /// No attempt yet (or the attempt failed).
    NotOpened,
    /// A distinct headset/default stream is capturing the take.
    Opened,
    /// The default endpoint is the same physical onboard mic; there is no
    /// second source to splice.
    SameAsOnboard,
    /// The system currently has no default input device.
    NoDevice,
}

/// Idle pre-roll source. The stream stays playing while Sotto is idle so a
/// trigger can splice backwards in time; this never touches Bluetooth.
struct OnboardBridge {
    device_name: String,
    sample_rate: u32,
    stream: Stream,
    ring: Arc<Mutex<VecDeque<f32>>>,
    take: Arc<Mutex<Vec<f32>>>,
    clock: Arc<SourceClock>,
    stream_error: Arc<AtomicBool>,
}

impl OnboardBridge {
    /// Atomically copies the ring tail into the take and arms recording while
    /// holding both locks. The callback uses the same ring→take lock order, so
    /// no sample can slip between the seed and the first live frame.
    fn seed_take(&self, lead_ms: u32, is_recording: &AtomicBool) -> u64 {
        let lead_samples = (self.sample_rate as u64 * lead_ms as u64 / 1000) as usize;
        let Ok(ring) = self.ring.lock() else {
            return 0;
        };
        let Ok(mut take) = self.take.lock() else {
            return 0;
        };
        take.clear();
        let n = ring.len().min(lead_samples);
        if n > 0 {
            take.extend(ring.iter().skip(ring.len() - n).copied());
        }
        is_recording.store(true, Ordering::SeqCst);
        if n == 0 {
            0
        } else {
            (n as u64 * 1000 / self.sample_rate as u64).max(1)
        }
    }

    fn take_audio(&self) -> Vec<f32> {
        self.take
            .lock()
            .map(|mut t| std::mem::take(&mut *t))
            .unwrap_or_default()
    }
}

pub struct AudioRecorder {
    is_recording: Arc<AtomicBool>,
    buffer: Arc<Mutex<Vec<f32>>>,
    live_energy_raw: Arc<AtomicU32>,
    live_low_raw: Arc<AtomicU32>,
    live_mid_raw: Arc<AtomicU32>,
    live_high_raw: Arc<AtomicU32>,
    device_invalidated: Arc<AtomicBool>,
    active_stream: Option<Stream>,
    source_sample_rate: u32,
    source_channels: u16,
    device_name: String,

    mode: CaptureMode,
    onboard: Option<OnboardBridge>,
    primary_clock: Arc<SourceClock>,
    primary_first_chunk_ms: Arc<AtomicU32>,
    primary_state: PrimaryState,
    pending_play_ms: Option<u64>,
    current_lead_ms: u64,
    take_counter: u64,
    last_timing: Option<CaptureTiming>,
    long: LongCaptureState,
    /// Upload budgets for one chunk. Gemini inlines base64 audio (4/3
    /// expansion), so the session lowers these when Gemini is active.
    chunk_soft_bytes: usize,
    chunk_hard_bytes: usize,
}

unsafe impl Send for AudioRecorder {}
unsafe impl Sync for AudioRecorder {}

#[inline]
fn update_live_energy(live_energy: &AtomicU32, chunk_rms: f32) {
    let chunk_raw = (chunk_rms * 10_000.0) as u32;
    let prev = live_energy.load(Ordering::Relaxed);
    // Instant attack on speech onset, fast responsive carry
    let new_val = if chunk_raw > prev {
        chunk_raw
    } else {
        ((prev as f32) * 0.70 + (chunk_raw as f32) * 0.30) as u32
    };
    live_energy.store(new_val, Ordering::Relaxed);
}

fn band_rms(mono: &[f32]) -> (f32, f32, f32) {
    let mut lp_low: f32 = 0.0;
    let mut lp_mid: f32 = 0.0;
    let mut sum_low: f32 = 0.0;
    let mut sum_mid: f32 = 0.0;
    let mut sum_high: f32 = 0.0;
    for &s in mono {
        lp_low += (s - lp_low) * 0.08;
        lp_mid += (s - lp_mid) * 0.35;
        let b_low = lp_low;
        let b_mid = lp_mid - lp_low;
        let b_high = s - lp_mid;
        sum_low += b_low * b_low;
        sum_mid += b_mid * b_mid;
        sum_high += b_high * b_high;
    }
    let n = mono.len().max(1) as f32;
    (
        (sum_low / n).sqrt(),
        (sum_mid / n).sqrt(),
        (sum_high / n).sqrt(),
    )
}

/// Confidence that an endpoint is a Bluetooth/HFP microphone. Wired USB
/// headsets are deliberately not treated as Bluetooth even if their name
/// contains "Headset".
fn bluetooth_endpoint_score(name: &str) -> i32 {
    let n = name.to_lowercase();
    if n.contains("bluetooth") || n.contains("hands-free") || n.contains("hands free") {
        return 30;
    }
    const BRANDS: &[&str] = &[
        "airpods",
        "buds",
        "wh-1000",
        "wf-1000",
        "jabra",
        "bose",
        "beats",
        "soundcore",
        "sennheiser",
        "momentum",
        "quietcomfort",
        "liberty",
        "freebuds",
    ];
    if BRANDS.iter().any(|t| n.contains(t)) {
        return 25;
    }
    if n.contains("headset") && !n.contains("usb") {
        return 15;
    }
    0
}

fn is_bluetooth_endpoint_name(name: &str) -> bool {
    bluetooth_endpoint_score(name) > 0
}

fn is_virtual_endpoint_name(name: &str) -> bool {
    let n = name.to_lowercase();
    const TOKENS: &[&str] = &[
        "stereo mix",
        "sound mapper",
        "wave out",
        "what u hear",
        "loopback",
        "voicemeeter",
        "vb-audio",
        "cable output",
        "virtual",
        "spdif",
        "primary sound capture",
    ];
    TOKENS.iter().any(|t| n.contains(t))
}

/// Higher is a better onboard candidate. Bluetooth and virtual endpoints are
/// excluded outright; the built-in array / Realtek / Intel names win.
fn onboard_device_score(name: &str) -> i32 {
    if is_bluetooth_endpoint_name(name) || is_virtual_endpoint_name(name) {
        return -100;
    }
    let n = name.to_lowercase();
    let mut score = 0;
    if n.contains("microphone array") || n.contains("mic array") {
        score += 12;
    } else if n.contains("array") {
        score += 7;
    }
    if n.contains("realtek") || n.contains("intel") || n.contains("amd") || n.contains("cirrus") {
        score += 6;
    }
    if n.contains("internal") || n.contains("built-in") || n.contains("built in") {
        score += 5;
    }
    if n.contains("microphone") || n.contains("mic") {
        score += 3;
    }
    if score == 0 && n.contains("usb") {
        score += 1;
    }
    score
}

fn find_onboard_device(host: &cpal::Host) -> Option<cpal::Device> {
    let devices = host.input_devices().ok()?;
    let mut best: Option<(i32, cpal::Device)> = None;
    for device in devices {
        let Ok(name) = device.name() else {
            continue;
        };
        let score = onboard_device_score(&name);
        if score <= 0 {
            continue;
        }
        if best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
            best = Some((score, device));
        }
    }
    best.map(|(_, device)| device)
}

/// Chooses the headset side of the bridge.
///
/// The system default wins when it is distinct from the onboard mic (that is
/// what the legacy path would have recorded). If Windows left the default on
/// the onboard array while a Bluetooth headset is still connected, we look for
/// a distinct Bluetooth/Hands-Free endpoint before giving up.
fn find_dual_primary(host: &cpal::Host, onboard_name: Option<&str>) -> Option<cpal::Device> {
    if let Some(default) = host.default_input_device() {
        let same = onboard_name
            .and_then(|n| default.name().ok().map(|dn| dn == n))
            .unwrap_or(false);
        if !same {
            return Some(default);
        }
    }

    let devices = host.input_devices().ok()?;
    let mut best: Option<(i32, cpal::Device)> = None;
    for device in devices {
        let Ok(name) = device.name() else {
            continue;
        };
        if onboard_name.map(|n| n == name).unwrap_or(false) || is_virtual_endpoint_name(&name) {
            continue;
        }
        let score = bluetooth_endpoint_score(&name);
        if score > 0 && best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
            best = Some((score, device));
        }
    }
    best.map(|(_, device)| device)
}

fn channels_of(channels: u16) -> usize {
    channels.max(1) as usize
}

fn duration_ms(samples: usize, channels: u16, sample_rate: u32) -> u64 {
    if sample_rate == 0 {
        return 0;
    }
    let frames = samples / channels_of(channels);
    frames as u64 * 1000 / sample_rate as u64
}

fn handle_frames(sink: &StreamSink, interleaved: &[f32], mono: &[f32]) {
    if mono.is_empty() {
        return;
    }
    let rms = splice::frame_rms(mono);

    if sink.clock.elapsed_ms().is_some() {
        sink.clock.record_callback();
        if sink.is_recording.load(Ordering::Relaxed) && rms >= sink.speech_rms {
            sink.clock.record_speech();
        }
    }

    let chunk_ms = (mono.len() as u64 * 1000 / sink.sample_rate.max(1) as u64) as u32;
    if chunk_ms > 0 {
        let _ = sink.first_chunk_ms.compare_exchange(
            0,
            chunk_ms,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }

    if sink.mono_take {
        // Onboard bridge: the ring and the take share one critical section, in
        // the same ring→take order as seed_take(). That makes the pre-roll to
        // live-audio hand-boundary atomic (no dropped or duplicated frame).
        let mut recorded = false;
        if let Some(ring) = &sink.ring {
            if let Ok(mut ring) = ring.lock() {
                if let Ok(mut buf) = sink.buffer.lock() {
                    // Must be read under the ring lock: seed_take() arms
                    // recording while holding it.
                    let recording = sink.is_recording.load(Ordering::Relaxed);
                    ring.extend(mono.iter().copied());
                    let excess = ring.len().saturating_sub(sink.ring_capacity);
                    for _ in 0..excess {
                        ring.pop_front();
                    }
                    if recording {
                        buf.extend_from_slice(mono);
                        recorded = true;
                    }
                }
            }
        }
        if recorded {
            if let Some(meters) = &sink.meters {
                let (low, mid, high) = band_rms(mono);
                update_live_energy(&meters.energy, rms);
                update_live_energy(&meters.low, low);
                update_live_energy(&meters.mid, mid);
                update_live_energy(&meters.high, high);
            }
        }
    } else if sink.is_recording.load(Ordering::Relaxed) {
        if let Ok(mut buf) = sink.buffer.lock() {
            buf.extend_from_slice(interleaved);
        }
        if let Some(meters) = &sink.meters {
            let (low, mid, high) = band_rms(mono);
            update_live_energy(&meters.energy, rms);
            update_live_energy(&meters.low, low);
            update_live_energy(&meters.mid, mid);
            update_live_energy(&meters.high, high);
        }
    }
}

fn build_stream(
    device: &cpal::Device,
    config: &SupportedStreamConfig,
    sink: StreamSink,
    err_flag: Arc<AtomicBool>,
) -> Result<Stream, String> {
    let sample_format = config.sample_format();
    let stream_config = config.clone().into();

    let err_fn = move |err| {
        eprintln!(
            "[Audio Ingest Error] Audio stream error: {:?} (Triggering auto-reconnect)",
            err
        );
        err_flag.store(true, Ordering::SeqCst);
    };

    match sample_format {
        SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _| {
                if data.is_empty() {
                    return;
                }
                let mono = downmix_interleaved_to_mono(data, sink.channels);
                handle_frames(&sink, data, &mono);
            },
            err_fn,
            None,
        ),
        SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            move |data: &[i16], _| {
                if data.is_empty() {
                    return;
                }
                let mut interleaved = Vec::with_capacity(data.len());
                for &sample in data {
                    interleaved.push(sample as f32 / 32768.0);
                }
                let mono = downmix_interleaved_to_mono(&interleaved, sink.channels);
                handle_frames(&sink, &interleaved, &mono);
            },
            err_fn,
            None,
        ),
        SampleFormat::U16 => device.build_input_stream(
            &stream_config,
            move |data: &[u16], _| {
                if data.is_empty() {
                    return;
                }
                let mut interleaved = Vec::with_capacity(data.len());
                for &sample in data {
                    interleaved.push((sample as f32 - 32768.0) / 32768.0);
                }
                let mono = downmix_interleaved_to_mono(&interleaved, sink.channels);
                handle_frames(&sink, &interleaved, &mono);
            },
            err_fn,
            None,
        ),
        _ => {
            return Err(format!(
                "Unsupported audio sample format: {:?}",
                sample_format
            ))
        }
    }
    .map_err(|e| format!("Failed to build input stream: {}", e))
}

#[allow(clippy::too_many_arguments)]
fn open_stream(
    device: &cpal::Device,
    is_recording: Arc<AtomicBool>,
    buffer: Arc<Mutex<Vec<f32>>>,
    ring: Option<Arc<Mutex<VecDeque<f32>>>>,
    ring_ms: u32,
    mono_take: bool,
    clock: Arc<SourceClock>,
    meters: Option<Meters>,
    first_chunk_ms: Arc<AtomicU32>,
    err_flag: Arc<AtomicBool>,
) -> Result<OpenedStream, String> {
    let name = device
        .name()
        .unwrap_or_else(|_| "Default Audio Device".to_string());
    let config = device
        .default_input_config()
        .map_err(|e| format!("Failed to query default input config for '{}': {}", name, e))?;
    let sample_rate = config.sample_rate().0;
    let channels = config.channels();
    let ring_capacity = if ring.is_some() {
        (sample_rate as u64 * ring_ms as u64 / 1000) as usize
    } else {
        0
    };

    let sink = StreamSink {
        is_recording,
        buffer,
        ring,
        ring_capacity,
        mono_take,
        channels,
        sample_rate,
        clock,
        meters,
        first_chunk_ms,
        speech_rms: DEFAULT_SPEECH_RMS,
    };

    let stream = build_stream(device, &config, sink, err_flag)?;
    Ok(OpenedStream {
        name,
        sample_rate,
        channels,
        stream,
    })
}

impl AudioRecorder {
    /// Legacy constructor. Preserves the historical always-on default for
    /// tests and callers that do not load user config.
    pub fn new() -> Result<Self, String> {
        Self::new_with_mode(CaptureMode::AlwaysOn)
    }

    /// Creates a recorder for the requested capture strategy.
    pub fn new_with_mode(mode: CaptureMode) -> Result<Self, String> {
        let host = cpal::default_host();
        let is_recording = Arc::new(AtomicBool::new(false));
        let buffer = Arc::new(Mutex::new(Vec::with_capacity(48_000 * 10)));
        let live_energy_raw = Arc::new(AtomicU32::new(0));
        let live_low_raw = Arc::new(AtomicU32::new(0));
        let live_mid_raw = Arc::new(AtomicU32::new(0));
        let live_high_raw = Arc::new(AtomicU32::new(0));
        let device_invalidated = Arc::new(AtomicBool::new(false));
        let primary_clock = Arc::new(SourceClock::new("primary"));
        let primary_first_chunk_ms = Arc::new(AtomicU32::new(0));

        let mut recorder = Self {
            is_recording,
            buffer,
            live_energy_raw,
            live_low_raw,
            live_mid_raw,
            live_high_raw,
            device_invalidated,
            active_stream: None,
            source_sample_rate: 0,
            source_channels: 0,
            device_name: "No input device".to_string(),
            mode,
            onboard: None,
            primary_clock,
            primary_first_chunk_ms,
            primary_state: PrimaryState::NotOpened,
            pending_play_ms: None,
            current_lead_ms: 0,
            take_counter: 0,
            last_timing: None,
            long: LongCaptureState::default(),
            chunk_soft_bytes: chunker::SOFT_REQUEST_BYTES,
            chunk_hard_bytes: chunker::HARD_REQUEST_BYTES,
        };

        match mode {
            CaptureMode::AlwaysOn => recorder.install_primary_paused(None)?,
            CaptureMode::OnboardOnly => recorder.install_primary_paused(Some(true))?,
            CaptureMode::DualMicSplice => {
                if let Err(e) = recorder.install_onboard_bridge() {
                    // No usable onboard endpoint: keep the app alive and let
                    // start() open the default device on trigger.
                    println!(
                        "[Audio Bridge] No onboard microphone available ({}); bridge disabled, default mic will open on trigger",
                        e
                    );
                    if host.default_input_device().is_none() {
                        return Err("No audio input device detected".to_string());
                    }
                }
            }
        }

        println!(
            "[Audio Ingest] Capture mode '{}' ready (device: '{}')",
            mode.label(),
            recorder.device_name
        );
        Ok(recorder)
    }

    fn meters(&self) -> Meters {
        Meters {
            energy: Arc::clone(&self.live_energy_raw),
            low: Arc::clone(&self.live_low_raw),
            mid: Arc::clone(&self.live_mid_raw),
            high: Arc::clone(&self.live_high_raw),
        }
    }

    fn install_primary_paused(&mut self, onboard_preference: Option<bool>) -> Result<(), String> {
        let host = cpal::default_host();
        let device = match onboard_preference {
            // Onboard-only must never latch a Bluetooth headset into HFP.
            Some(true) => find_onboard_device(&host).ok_or_else(|| {
                "No onboard/USB microphone detected (Onboard-only mode will not open a Bluetooth headset)"
                    .to_string()
            })?,
            _ => host
                .default_input_device()
                .ok_or_else(|| "No default audio input device (microphone) detected".to_string())?,
        };

        let opened = open_stream(
            &device,
            Arc::clone(&self.is_recording),
            Arc::clone(&self.buffer),
            None,
            0,
            false,
            Arc::clone(&self.primary_clock),
            Some(self.meters()),
            Arc::clone(&self.primary_first_chunk_ms),
            Arc::clone(&self.device_invalidated),
        )?;

        self.device_name = opened.name;
        self.source_sample_rate = opened.sample_rate;
        self.source_channels = opened.channels;
        self.active_stream = Some(opened.stream);
        Ok(())
    }

    fn install_onboard_bridge(&mut self) -> Result<(), String> {
        let host = cpal::default_host();
        let device =
            find_onboard_device(&host).ok_or_else(|| "No onboard microphone detected".to_string())?;
        let take = Arc::new(Mutex::new(Vec::new()));
        let ring = Arc::new(Mutex::new(VecDeque::new()));
        let clock = Arc::new(SourceClock::new("onboard"));
        let stream_error = Arc::new(AtomicBool::new(false));

        let opened = open_stream(
            &device,
            Arc::clone(&self.is_recording),
            Arc::clone(&take),
            Some(Arc::clone(&ring)),
            PREROLL_RING_MS,
            true,
            Arc::clone(&clock),
            Some(self.meters()),
            Arc::new(AtomicU32::new(0)),
            Arc::clone(&stream_error),
        )?;

        opened
            .stream
            .play()
            .map_err(|e| format!("Failed to start onboard pre-roll stream: {}", e))?;

        self.device_name = opened.name.clone();
        self.onboard = Some(OnboardBridge {
            device_name: opened.name,
            sample_rate: opened.sample_rate,
            stream: opened.stream,
            ring,
            take,
            clock,
            stream_error,
        });
        println!(
            "[Audio Bridge] Pre-roll armed on '{}' ({} ms); headset stays on A2DP while idle",
            self.device_name, PREROLL_RING_MS
        );
        Ok(())
    }

    /// Rebuilds idle streams after a mode change. Never call while recording.
    fn rebuild_idle(&mut self) -> Result<(), String> {
        if let Some(stream) = self.active_stream.take() {
            let _ = stream.pause();
            drop(stream);
        }
        self.onboard = None;
        match self.mode {
            CaptureMode::AlwaysOn => self.install_primary_paused(None),
            CaptureMode::OnboardOnly => self.install_primary_paused(Some(true)),
            CaptureMode::DualMicSplice => {
                if let Err(e) = self.install_onboard_bridge() {
                    println!("[Audio Bridge] Bridge unavailable: {}", e);
                }
                Ok(())
            }
        }
    }

    pub fn set_capture_mode(&mut self, mode: CaptureMode) -> Result<(), String> {
        if self.is_recording.load(Ordering::SeqCst) {
            return Err("Cannot change capture mode while recording".to_string());
        }
        if self.mode == mode {
            return Ok(());
        }
        println!(
            "[Audio Recorder] Capture mode: {} -> {}",
            self.mode.label(),
            mode.label()
        );
        self.mode = mode;
        self.last_timing = None;
        self.rebuild_idle()
    }

    pub fn last_timing(&self) -> Option<&CaptureTiming> {
        self.last_timing.as_ref()
    }

    fn ensure_connected(&mut self) -> Result<(), String> {
        let is_inv = self.device_invalidated.load(Ordering::SeqCst);
        let host = cpal::default_host();
        let cur_device = host.default_input_device();

        let need_reconnect = is_inv || self.active_stream.is_none() || match &cur_device {
            Some(dev) => dev.name().map(|n| n != self.device_name).unwrap_or(false),
            None => true,
        };

        if !need_reconnect {
            return Ok(());
        }

        println!("[Audio Recorder] Audio device transition detected. Re-enumerating input hardware...");

        if let Some(stream) = self.active_stream.take() {
            let _ = stream.pause();
            drop(stream);
        }

        let device = cur_device.ok_or_else(|| {
            self.device_invalidated.store(true, Ordering::SeqCst);
            "No audio input device currently available on system".to_string()
        })?;

        self.install_primary_paused_for(&device)?;
        self.device_invalidated.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn install_primary_paused_for(&mut self, device: &cpal::Device) -> Result<(), String> {
        let opened = open_stream(
            device,
            Arc::clone(&self.is_recording),
            Arc::clone(&self.buffer),
            None,
            0,
            false,
            Arc::clone(&self.primary_clock),
            Some(self.meters()),
            Arc::clone(&self.primary_first_chunk_ms),
            Arc::clone(&self.device_invalidated),
        )?;
        println!(
            "[Audio Recorder] Successfully auto-switched to '{}' [{}Hz, {} channels]",
            opened.name, opened.sample_rate, opened.channels
        );
        self.device_name = opened.name;
        self.source_sample_rate = opened.sample_rate;
        self.source_channels = opened.channels;
        self.active_stream = Some(opened.stream);
        Ok(())
    }

    fn prepare_take(&mut self) {
        self.is_recording.store(false, Ordering::SeqCst);
        if let Ok(mut buf) = self.buffer.lock() {
            buf.clear();
        }
        if let Some(bridge) = &self.onboard {
            if let Ok(mut take) = bridge.take.lock() {
                take.clear();
            }
        }
        self.live_energy_raw.store(0, Ordering::Relaxed);
        self.live_low_raw.store(0, Ordering::Relaxed);
        self.live_mid_raw.store(0, Ordering::Relaxed);
        self.live_high_raw.store(0, Ordering::Relaxed);
        self.primary_first_chunk_ms.store(0, Ordering::Relaxed);
        self.current_lead_ms = 0;
        self.pending_play_ms = None;
        self.long = LongCaptureState {
            active: true,
            ..Default::default()
        };
    }

    fn arm_at(&self, trigger: Instant) {
        self.primary_clock.arm_at(trigger);
        if let Some(bridge) = &self.onboard {
            bridge.clock.arm_at(trigger);
        }
    }

    /// Starts capturing live microphone audio directly into RAM.
    pub fn start(&mut self) -> Result<(), String> {
        match self.mode {
            CaptureMode::AlwaysOn => self.start_single(false),
            CaptureMode::OnboardOnly => self.start_single(true),
            CaptureMode::DualMicSplice => self.start_dual(),
        }
    }

    fn start_single(&mut self, onboard_only: bool) -> Result<(), String> {
        if onboard_only && self.active_stream.is_none() {
            self.install_primary_paused(Some(true))?;
        } else if !onboard_only {
            self.ensure_connected()?;
        }

        self.prepare_take();
        let trigger = Instant::now();
        self.arm_at(trigger);
        self.is_recording.store(true, Ordering::SeqCst);

        let play_started = Instant::now();
        if let Some(stream) = &self.active_stream {
            if let Err(e) = stream.play() {
                eprintln!(
                    "[Audio Recorder] Stream play error ({}), attempting auto-recovery...",
                    e
                );
                self.device_invalidated.store(true, Ordering::SeqCst);
                if onboard_only {
                    self.install_primary_paused(Some(true))?;
                } else {
                    self.ensure_connected()?;
                }
                if let Some(st) = &self.active_stream {
                    st.play()
                        .map_err(|err| format!("Stream playback failed after reconnect: {}", err))?;
                }
            }
        }
        self.pending_play_ms = Some(play_started.elapsed().as_millis() as u64);

        println!(
            "[Audio Recorder] Recording started on '{}' ({} mode)...",
            self.device_name,
            self.mode.label()
        );
        Ok(())
    }

    fn start_dual(&mut self) -> Result<(), String> {
        // Rebuild the bridge if the onboard endpoint disappeared or errored.
        if self
            .onboard
            .as_ref()
            .map(|b| b.stream_error.load(Ordering::SeqCst))
            .unwrap_or(true)
        {
            if self.onboard.is_some() {
                println!("[Audio Bridge] Onboard stream was invalidated; re-arming...");
                self.onboard = None;
            }
            let _ = self.install_onboard_bridge();
        }

        self.prepare_take();
        self.primary_state = PrimaryState::NotOpened;
        let trigger = Instant::now();
        self.arm_at(trigger);

        // Seed the take with audio from before the keypress, then open the
        // headset stream. is_recording stays false until the seed is complete
        // so the callback cannot append twice.
        if let Some(bridge) = &self.onboard {
            self.current_lead_ms = bridge.seed_take(PREROLL_LEAD_MS, &self.is_recording);
            let _ = bridge.stream.play();
        }

        self.is_recording.store(true, Ordering::SeqCst);

        let play_started = Instant::now();
        let opened_primary = self.open_primary_for_dual();
        self.pending_play_ms = Some(play_started.elapsed().as_millis() as u64);

        if self.onboard.is_none() && self.active_stream.is_none() {
            self.is_recording.store(false, Ordering::SeqCst);
            return Err("No audio input device available for dual-mic capture".to_string());
        }
        match opened_primary {
            Ok(state) => self.primary_state = state,
            Err(e) => {
                // The onboard pre-roll still covers the whole take; losing the
                // headset half is a quality fallback, not a hard failure.
                println!("[Audio Bridge] Headset stream unavailable ({}); this take will finish on the onboard mic", e);
            }
        }
        if self.primary_state == PrimaryState::SameAsOnboard {
            println!(
                "[Audio Bridge] Default capture endpoint is the onboard mic; using it for the whole take"
            );
        }

        println!(
            "[Audio Recorder] Recording started (dual-mic splice, lead {} ms)...",
            self.current_lead_ms
        );
        Ok(())
    }

    /// Opens the default (headset) stream for a dual-mic take. Skips the second
    /// stream if the default endpoint *is* the onboard endpoint.
    fn open_primary_for_dual(&mut self) -> Result<PrimaryState, String> {
        let host = cpal::default_host();
        let onboard_name = self.onboard.as_ref().map(|b| b.device_name.clone());
        let Some(device) = find_dual_primary(&host, onboard_name.as_deref()) else {
            return Ok(if self.onboard.is_some() {
                PrimaryState::SameAsOnboard
            } else {
                PrimaryState::NoDevice
            });
        };

        let opened = open_stream(
            &device,
            Arc::clone(&self.is_recording),
            Arc::clone(&self.buffer),
            None,
            0,
            false,
            Arc::clone(&self.primary_clock),
            Some(self.meters()),
            Arc::clone(&self.primary_first_chunk_ms),
            Arc::clone(&self.device_invalidated),
        )?;

        if let Err(e) = opened.stream.play() {
            return Err(format!("Failed to start headset stream: {}", e));
        }

        self.device_name = opened.name;
        self.source_sample_rate = opened.sample_rate;
        self.source_channels = opened.channels;
        self.active_stream = Some(opened.stream);
        Ok(PrimaryState::Opened)
    }

    /// Moves newly captured device audio into the normalized 16 kHz stream.
    /// Called by the long-form pump while recording and once more during stop.
    fn drain_long_capture(&mut self) -> Result<(), String> {
        if !self.long.active {
            return Ok(());
        }

        match self.mode {
            CaptureMode::DualMicSplice => {
                let headset_raw = {
                    let mut buf = self
                        .buffer
                        .lock()
                        .map_err(|_| "Failed to acquire audio buffer lock".to_string())?;
                    std::mem::take(&mut *buf)
                };
                if !headset_raw.is_empty() && self.source_sample_rate > 0 {
                    let mono = downmix_interleaved_to_mono(&headset_raw, self.source_channels);
                    let resampled =
                        resample_linear(&mono, self.source_sample_rate, TARGET_SAMPLE_RATE);
                    let pcm = float_to_pcm16(&resampled);
                    self.long.headset_total_ms +=
                        (pcm.len() as u64 * 1000 / TARGET_SAMPLE_RATE as u64) as u64;
                    if self.long.dual_spliced {
                        self.long.pcm16k.extend_from_slice(&pcm);
                    } else {
                        self.long.headset16k.extend_from_slice(&pcm);
                    }
                }

                if let Some(bridge) = &self.onboard {
                    let onboard_raw = bridge.take_audio();
                    if !onboard_raw.is_empty() && bridge.sample_rate > 0 {
                        let resampled = resample_linear(
                            &onboard_raw,
                            bridge.sample_rate,
                            TARGET_SAMPLE_RATE,
                        );
                        let pcm = float_to_pcm16(&resampled);
                        self.long.onboard_total_ms +=
                            (pcm.len() as u64 * 1000 / TARGET_SAMPLE_RATE as u64) as u64;
                        // After the handshake the headset owns the timeline; the
                        // onboard copy is redundant and is intentionally dropped.
                        if !self.long.dual_spliced {
                            self.long.onboard16k.extend_from_slice(&pcm);
                        }
                    }
                }

                let _ = self.try_finalize_dual_splice();
            }
            _ => {
                let raw = {
                    let mut buf = self
                        .buffer
                        .lock()
                        .map_err(|_| "Failed to acquire audio buffer lock".to_string())?;
                    std::mem::take(&mut *buf)
                };
                if !raw.is_empty() && self.source_sample_rate > 0 {
                    let mono = downmix_interleaved_to_mono(&raw, self.source_channels);
                    let resampled =
                        resample_linear(&mono, self.source_sample_rate, TARGET_SAMPLE_RATE);
                    let pcm = float_to_pcm16(&resampled);
                    self.long.primary_total_ms +=
                        (pcm.len() as u64 * 1000 / TARGET_SAMPLE_RATE as u64) as u64;
                    self.long.pcm16k.extend_from_slice(&pcm);
                }
            }
        }

        Ok(())
    }

    /// Once the headset finally delivers speech, align it with the onboard
    /// pre-roll and commit one continuous stream. From this point on new
    /// headset audio is appended directly to `long.pcm16k`.
    fn try_finalize_dual_splice(&mut self) -> bool {
        if self.mode != CaptureMode::DualMicSplice
            || self.long.dual_spliced
            || self.long.headset16k.is_empty()
            || self.long.onboard16k.is_empty()
        {
            return self.long.dual_spliced;
        }

        let bt: Vec<f32> = self
            .long
            .headset16k
            .iter()
            .map(|&s| s as f32 / 32768.0)
            .collect();
        let Some(bt_speech) = splice::first_speech_index(
            &bt,
            TARGET_SAMPLE_RATE,
            DEFAULT_SPEECH_RMS,
            25,
        ) else {
            return false;
        };

        let onboard: Vec<f32> = self
            .long
            .onboard16k
            .iter()
            .map(|&s| s as f32 / 32768.0)
            .collect();

        let lead_samples =
            (self.current_lead_ms as u64 * TARGET_SAMPLE_RATE as u64 / 1000) as i32;
        let first_chunk = self.primary_first_chunk_ms.load(Ordering::Relaxed) as u64;
        let bt_start_ms = self
            .primary_clock
            .first_callback_ms()
            .map(|ms| ms.saturating_sub(first_chunk))
            .unwrap_or(0);
        let predicted_lag =
            lead_samples + (bt_start_ms * TARGET_SAMPLE_RATE as u64 / 1000) as i32;

        let splice_started = Instant::now();
        let outcome = splice::splice_takes(
            &onboard,
            &bt,
            bt_speech,
            predicted_lag,
            SpliceConfig::default(),
        );
        self.long.splice_processing_ms = splice_started.elapsed().as_millis() as u64;
        self.long.measured_lag_ms = outcome
            .lag_samples
            .map(|lag| lag as f32 * 1000.0 / TARGET_SAMPLE_RATE as f32);
        self.long.lag_confidence = Some(outcome.confidence);
        self.long.splice_fallback = outcome.fallback.map(|f| format!("{:?}", f));

        self.long.pcm16k = float_to_pcm16(&outcome.samples);
        self.long.headset16k.clear();
        self.long.onboard16k.clear();
        self.long.dual_spliced = true;

        println!(
            "[Chunker] Dual-mic handshake spliced (lag {:.1} ms, confidence {:.2}); long-form stream is now continuous",
            self.long.measured_lag_ms.unwrap_or(0.0),
            outcome.confidence
        );
        true
    }

    /// Seals one upload-sized chunk when the live stream has crossed the soft
    /// budget and contains a good pause. The pump thread calls this repeatedly;
    /// returning `Ok(None)` simply means "keep recording".
    pub fn poll_chunk(&mut self) -> Result<Option<SealedAudioChunk>, String> {
        if !self.long.active || !self.is_recording.load(Ordering::SeqCst) {
            return Ok(None);
        }

        self.drain_long_capture()?;

        let (source_is_onboard, source_is_headset, plan, split, buffered_after) = {
            let mut onboard = false;
            let mut headset = false;
            let source: &[i16] = if self.mode == CaptureMode::DualMicSplice
                && !self.long.dual_spliced
            {
                if !self.long.onboard16k.is_empty() {
                    onboard = true;
                    &self.long.onboard16k
                } else {
                    headset = true;
                    &self.long.headset16k
                }
            } else {
                &self.long.pcm16k
            };

            let Some(plan) = chunker::plan_chunk_split(
                source,
                self.chunk_soft_bytes,
                self.chunk_hard_bytes,
                chunker::MIN_PAUSE_MS,
                chunker::RELAXED_PAUSE_MS,
                chunker::MIN_CHUNK_MS,
            ) else {
                return Ok(None);
            };

            let split = plan.sample_index.min(source.len());
            if split == 0 {
                return Ok(None);
            }
            (onboard, headset, plan, split, source.len() - split)
        };

        let chunk_pcm = if source_is_onboard {
            // No usable headset speech before the budget. Commit this take to
            // the onboard mic so later chunks no longer keep two copies.
            let chunk: Vec<i16> = self.long.onboard16k.drain(..split).collect();
            self.long.pcm16k = std::mem::take(&mut self.long.onboard16k);
            self.long.headset16k.clear();
            self.long.dual_spliced = true;
            chunk
        } else if source_is_headset {
            let chunk: Vec<i16> = self.long.headset16k.drain(..split).collect();
            self.long.pcm16k = std::mem::take(&mut self.long.headset16k);
            self.long.onboard16k.clear();
            self.long.dual_spliced = true;
            chunk
        } else {
            self.long.pcm16k.drain(..split).collect()
        };

        let duration_ms = (chunk_pcm.len() as u64 * 1000 / TARGET_SAMPLE_RATE as u64) as u32;
        self.long.sealed_ms += duration_ms as u64;
        match plan.reason {
            SplitReason::Pause => println!(
                "[Chunker] Sealed {} ms chunk at a {} ms pause ({} ms still buffered)",
                duration_ms,
                plan.pause_ms,
                (buffered_after as u64 * 1000 / TARGET_SAMPLE_RATE as u64)
            ),
            SplitReason::HardLimit => println!(
                "[Chunker] Sealed {} ms chunk at the hard upload ceiling (no pause found)",
                duration_ms
            ),
        }

        Ok(Some(SealedAudioChunk {
            pcm16k: chunk_pcm,
            duration_ms,
        }))
    }


    /// Stops audio capture, merges sources when needed, downmixes/resamples,
    /// runs the ghost-tap and silence filter, applies gain, and packages WAV.
    pub fn stop(&mut self) -> Result<Option<AudioPayload>, String> {
        let finalize_start = Instant::now();
        self.is_recording.store(false, Ordering::SeqCst);
        self.live_energy_raw.store(0, Ordering::Relaxed);
        self.live_low_raw.store(0, Ordering::Relaxed);
        self.live_mid_raw.store(0, Ordering::Relaxed);
        self.live_high_raw.store(0, Ordering::Relaxed);
        self.take_counter = self.take_counter.saturating_add(1);

        let mut timing = CaptureTiming {
            mode: self.mode.label().to_string(),
            take: self.take_counter,
            trigger_to_primary_play_ms: self.pending_play_ms,
            ..Default::default()
        };

        match self.mode {
            CaptureMode::DualMicSplice => self.stop_dual(&mut timing, finalize_start),
            _ => self.stop_single(&mut timing, finalize_start),
        }
    }

    /// Overrides the per-chunk upload budget for this take. Called by the
    /// long-form session before capture starts; values are clamped so the soft
    /// search window can never collapse past the hard ceiling.
    pub fn set_chunk_limits(&mut self, soft_bytes: usize, hard_bytes: usize) {
        let hard = hard_bytes.max(chunker::WAV_HEADER_BYTES + 2 * 16000);
        let soft = soft_bytes.min(hard.saturating_sub(1));
        self.chunk_soft_bytes = soft.max(1);
        self.chunk_hard_bytes = hard;
    }

    /// True while the recorder is accumulating the normalized long-form stream.
    pub fn long_form_active(&self) -> bool {
        self.long.active
    }

    /// Duration already sealed into upload chunks during this take.
    pub fn sealed_duration_ms(&self) -> u64 {
        self.long.sealed_ms
    }

    fn stop_single(
        &mut self,
        timing: &mut CaptureTiming,
        finalize_start: Instant,
    ) -> Result<Option<AudioPayload>, String> {
        if let Some(stream) = &self.active_stream {
            let _ = stream.pause();
        }

        // Pull the audio captured since the last pump into the normalized
        // stream. The pump runs on its own thread, so this is usually a small
        // tail (a few hundred milliseconds), not the whole take.
        if let Err(e) = self.drain_long_capture() {
            eprintln!("[Audio Recorder] Final drain failed: {}", e);
        }

        timing.trigger_to_primary_first_sample_ms = self.primary_clock.first_callback_ms();
        timing.trigger_to_primary_first_speech_ms = self.primary_clock.first_speech_ms();
        timing.handshake_ms = self.primary_clock.first_speech_ms();

        if self.long.active {
            timing.headset_duration_ms = self.long.primary_total_ms;
            let pcm16k = std::mem::take(&mut self.long.pcm16k);
            if pcm16k.is_empty() {
                if self.long.sealed_ms == 0 {
                    println!("[Audio Recorder] Discarded: Buffer is empty");
                }
                self.finish_timing(timing, finalize_start);
                return Ok(None);
            }
            let mono_f32: Vec<f32> = pcm16k
                .iter()
                .map(|&s| s as f32 / 32768.0)
                .collect();
            return self.finalize_mono(mono_f32, timing, finalize_start);
        }

        // Legacy path: no long-form state was armed.
        let raw_samples = {
            let mut buf = self
                .buffer
                .lock()
                .map_err(|_| "Failed to acquire audio buffer lock".to_string())?;
            std::mem::take(&mut *buf)
        };
        timing.headset_duration_ms = duration_ms(
            raw_samples.len(),
            self.source_channels,
            self.source_sample_rate,
        );
        if raw_samples.is_empty() {
            println!("[Audio Recorder] Discarded: Buffer is empty");
            self.finish_timing(timing, finalize_start);
            return Ok(None);
        }
        let mono_f32 = downmix_interleaved_to_mono(&raw_samples, self.source_channels);
        let resampled = resample_linear(&mono_f32, self.source_sample_rate, TARGET_SAMPLE_RATE);
        self.finalize_mono(resampled, timing, finalize_start)
    }

    fn stop_dual(
        &mut self,
        timing: &mut CaptureTiming,
        finalize_start: Instant,
    ) -> Result<Option<AudioPayload>, String> {
        // Release the headset endpoint immediately so Windows can return the
        // device to A2DP. Pausing alone keeps IAudioClient alive.
        if let Some(stream) = self.active_stream.take() {
            let _ = stream.pause();
            drop(stream);
        }

        // The pump usually consumed everything a few hundred milliseconds ago;
        // this drains only the final tail into the normalized streams.
        if let Err(e) = self.drain_long_capture() {
            eprintln!("[Audio Recorder] Final drain failed: {}", e);
        }

        timing.onboard_lead_ms = self.current_lead_ms;
        timing.trigger_to_primary_first_sample_ms = self.primary_clock.first_callback_ms();
        timing.trigger_to_primary_first_speech_ms = self.primary_clock.first_speech_ms();
        timing.handshake_ms = self.primary_clock.first_speech_ms();
        timing.onboard_first_speech_ms =
            self.onboard.as_ref().and_then(|b| b.clock.first_speech_ms());

        if self.long.active {
            timing.onboard_duration_ms = self.long.onboard_total_ms;
            timing.headset_duration_ms = self.long.headset_total_ms;
            timing.splice_processing_ms = self.long.splice_processing_ms;
            timing.measured_lag_ms = self.long.measured_lag_ms;
            timing.lag_confidence = self.long.lag_confidence;
            timing.fallback = self.long.splice_fallback.clone();

            // After the handshake (or a chunk commit) `pcm16k` is the single
            // continuous stream. If the handshake never happened, try it now
            // with the complete takes before choosing a fallback source.
            let spliced = self.long.dual_spliced || self.try_finalize_dual_splice();
            if spliced {
                let pcm16k = std::mem::take(&mut self.long.pcm16k);
                if pcm16k.is_empty() {
                    if self.long.sealed_ms == 0 {
                        println!("[Audio Recorder] Discarded: Buffer is empty");
                    }
                    self.finish_timing(timing, finalize_start);
                    return Ok(None);
                }
                let mono_f32: Vec<f32> = pcm16k
                    .iter()
                    .map(|&s| s as f32 / 32768.0)
                    .collect();
                return self.finalize_mono(mono_f32, timing, finalize_start);
            }

            // No measurable handshake. The remaining tail still belongs to one
            // source, so pick it with the same precedence as the old stop path.
            let headset16k = std::mem::take(&mut self.long.headset16k);
            let onboard16k = std::mem::take(&mut self.long.onboard16k);
            let headset_speech = if headset16k.is_empty() {
                None
            } else {
                let floats: Vec<f32> = headset16k
                    .iter()
                    .map(|&s| s as f32 / 32768.0)
                    .collect();
                splice::first_speech_index(
                    &floats,
                    TARGET_SAMPLE_RATE,
                    DEFAULT_SPEECH_RMS,
                    25,
                )
            };

            if headset_speech.is_some() && !onboard16k.is_empty() {
                // Both sources exist and the headset did speak, but the splice
                // candidate search failed. The onboard take covers the whole
                // history; keep it rather than dropping the handshake window.
                timing.fallback = Some("NoOverlap".to_string());
                println!(
                    "[Audio Bridge] Headset never overlapped the onboard take; finishing on the onboard mic"
                );
                return self.finalize_dual_pcm(onboard16k, timing, finalize_start);
            }

            if headset_speech.is_some() {
                // No onboard bridge (single-mic machine or the bridge failed):
                // the headset take is still valid, it just lost the handshake.
                timing.fallback = Some("NoOnboardAudio".to_string());
                println!("[Audio Bridge] No onboard microphone; using the headset take");
                return self.finalize_dual_pcm(headset16k, timing, finalize_start);
            }

            let (fallback, note) = match self.primary_state {
                PrimaryState::SameAsOnboard => ("SameDeviceAsOnboard", ""),
                PrimaryState::NoDevice => (
                    "NoHeadsetDevice",
                    "No default headset endpoint; using the onboard mic",
                ),
                _ if headset16k.is_empty() => (
                    "NoHeadsetAudio",
                    "Headset delivered no audio; finishing take on the onboard mic",
                ),
                _ => (
                    "NoHeadsetSpeech",
                    "Headset delivered no usable speech; finishing take on the onboard mic",
                ),
            };
            timing.fallback = Some(fallback.to_string());
            if !note.is_empty() {
                println!("[Audio Bridge] {}", note);
            }
            if !onboard16k.is_empty() {
                return self.finalize_dual_pcm(onboard16k, timing, finalize_start);
            }
            self.finish_timing(timing, finalize_start);
            return Ok(None);
        }

        // Legacy path: no long-form state was armed (should only happen for a
        // recorder constructed before the normalized stream existed).
        let bt_raw = {
            let mut buf = self
                .buffer
                .lock()
                .map_err(|_| "Failed to acquire audio buffer lock".to_string())?;
            std::mem::take(&mut *buf)
        };
        let (onboard_raw, onboard_rate) = match &self.onboard {
            Some(bridge) => (bridge.take_audio(), bridge.sample_rate),
            None => (Vec::new(), 0),
        };

        timing.onboard_duration_ms = duration_ms(onboard_raw.len(), 1, onboard_rate);
        timing.headset_duration_ms =
            duration_ms(bt_raw.len(), self.source_channels, self.source_sample_rate);

        let onboard_16k = if onboard_raw.is_empty() {
            Vec::new()
        } else {
            resample_linear(&onboard_raw, onboard_rate, TARGET_SAMPLE_RATE)
        };
        let bt_16k = if bt_raw.is_empty() {
            Vec::new()
        } else {
            let mono = downmix_interleaved_to_mono(&bt_raw, self.source_channels);
            resample_linear(&mono, self.source_sample_rate, TARGET_SAMPLE_RATE)
        };

        let bt_speech = splice::first_speech_index(
            &bt_16k,
            TARGET_SAMPLE_RATE,
            DEFAULT_SPEECH_RMS,
            25,
        );

        match bt_speech {
            Some(bt_speech) if !onboard_16k.is_empty() => {
                let lead_samples = (self.current_lead_ms as u64 * TARGET_SAMPLE_RATE as u64 / 1000)
                    as i32;
                let first_chunk = self.primary_first_chunk_ms.load(Ordering::Relaxed) as u64;
                let bt_start_ms = self
                    .primary_clock
                    .first_callback_ms()
                    .map(|ms| ms.saturating_sub(first_chunk))
                    .unwrap_or(0);
                let predicted_lag = lead_samples + (bt_start_ms * TARGET_SAMPLE_RATE as u64 / 1000) as i32;

                let splice_started = Instant::now();
                let outcome = splice::splice_takes(
                    &onboard_16k,
                    &bt_16k,
                    bt_speech,
                    predicted_lag,
                    SpliceConfig::default(),
                );
                timing.splice_processing_ms = splice_started.elapsed().as_millis() as u64;
                timing.measured_lag_ms = outcome
                    .lag_samples
                    .map(|lag| lag as f32 * 1000.0 / TARGET_SAMPLE_RATE as f32);
                timing.lag_confidence = Some(outcome.confidence);
                timing.fallback = outcome.fallback.map(|f| format!("{:?}", f));

                println!(
                    "[Audio Bridge] Splice: onboard {} ms + headset {} ms | lag {:.1} ms (conf {:.2})",
                    timing.onboard_duration_ms,
                    timing.headset_duration_ms,
                    timing.measured_lag_ms.unwrap_or(0.0),
                    outcome.confidence
                );
                self.finalize_mono(outcome.samples, timing, finalize_start)
            }
            Some(_) if onboard_16k.is_empty() => {
                // No onboard bridge (single-mic machine or the bridge failed):
                // the headset take is still valid, it just lost the handshake.
                timing.fallback = Some("NoOnboardAudio".to_string());
                println!("[Audio Bridge] No onboard microphone; using the headset take");
                self.finalize_mono(bt_16k, timing, finalize_start)
            }
            _ => {
                let (fallback, note) = match self.primary_state {
                    PrimaryState::SameAsOnboard => ("SameDeviceAsOnboard", ""),
                    PrimaryState::NoDevice => (
                        "NoHeadsetDevice",
                        "No default headset endpoint; using the onboard mic",
                    ),
                    _ if bt_16k.is_empty() => (
                        "NoHeadsetAudio",
                        "Headset delivered no audio; finishing take on the onboard mic",
                    ),
                    _ => (
                        "NoHeadsetSpeech",
                        "Headset delivered no usable speech; finishing take on the onboard mic",
                    ),
                };
                timing.fallback = Some(fallback.to_string());
                if !note.is_empty() {
                    println!("[Audio Bridge] {}", note);
                }
                if !onboard_16k.is_empty() {
                    self.finalize_mono(onboard_16k, timing, finalize_start)
                } else {
                    self.finish_timing(timing, finalize_start);
                    Ok(None)
                }
            }
        }
    }

    /// Converts a normalized fallback stream and runs the common finalizer.
    fn finalize_dual_pcm(
        &mut self,
        pcm16k: Vec<i16>,
        timing: &mut CaptureTiming,
        finalize_start: Instant,
    ) -> Result<Option<AudioPayload>, String> {
        if pcm16k.is_empty() {
            self.finish_timing(timing, finalize_start);
            return Ok(None);
        }
        let mono_f32: Vec<f32> = pcm16k
            .iter()
            .map(|&s| s as f32 / 32768.0)
            .collect();
        self.finalize_mono(mono_f32, timing, finalize_start)
    }

    fn finalize_mono(
        &mut self,
        mut mono16: Vec<f32>,
        timing: &mut CaptureTiming,
        finalize_start: Instant,
    ) -> Result<Option<AudioPayload>, String> {
        if mono16.is_empty() {
            self.finish_timing(timing, finalize_start);
            return Ok(None);
        }

        // Use the pre-trigger part of the take as the noise floor when we have
        // one, so steady room noise is not reported as "speech at 0 ms". This
        // keeps the first-syllable metric honest in a non-silent room.
        let pre_roll_samples = (timing.onboard_lead_ms as u64 * TARGET_SAMPLE_RATE as u64 / 1000)
            .min(mono16.len() as u64) as usize;
        let noise_floor = if pre_roll_samples >= TARGET_SAMPLE_RATE as usize / 20 {
            splice::frame_rms(&mono16[..pre_roll_samples])
        } else {
            0.0
        };
        let speech_threshold = (noise_floor * 2.0).max(SILENCE_RMS_THRESHOLD);
        timing.first_speech_offset_ms = splice::first_speech_index(
            &mono16,
            TARGET_SAMPLE_RATE,
            speech_threshold,
            25,
        )
        .map(|idx| idx as u64 * 1000 / TARGET_SAMPLE_RATE as u64);

        // 80 Hz Butterworth high-pass: DC, HVAC hum, desk rumble.
        let mut hp = BiquadHighPass::new(80.0, TARGET_SAMPLE_RATE as f32);
        hp.process_slice(&mut mono16);

        let mut pcm16 = float_to_pcm16(&mono16);
        let trimmed_samples = trim_trailing_silence(&mut pcm16, TARGET_SAMPLE_RATE, 0.003, 100);
        if trimmed_samples > 0 {
            let ms = trimmed_samples * 1000 / TARGET_SAMPLE_RATE as usize;
            println!("[Audio Filter] Trimmed {}ms trailing silence to prevent hallucination", ms);
        }
        // Chunks sealed during recording are already transcribed separately; this
        // payload is only the tail. Telemetry reports the whole take, but the
        // payload (and its retry segment) must describe only the tail so the
        // session can add it to the sealed duration without double-counting.
        let sealed_ms = self.long.sealed_ms;
        let tail_ms = pcm16.len() as u64 * 1000 / TARGET_SAMPLE_RATE as u64;
        timing.output_duration_ms = sealed_ms + tail_ms;

        // In long-form mode the tail can legitimately be a short fragment
        // between two sealed chunks, so the ghost-tap gate does not apply. Pure
        // silence still does: uploading it would only invite a hallucination.
        let validation = if sealed_ms > 0 {
            let rms = compute_rms(&pcm16);
            if rms < SILENCE_RMS_THRESHOLD {
                AudioValidation::DiscardSilence { rms }
            } else {
                AudioValidation::Valid {
                    duration_ms: tail_ms.min(u32::MAX as u64) as u32,
                    rms,
                    peak: compute_peak(&pcm16),
                }
            }
        } else {
            validate_audio(
                &pcm16,
                TARGET_SAMPLE_RATE,
                MIN_RECORDING_MS,
                SILENCE_RMS_THRESHOLD,
            )
        };

        match validation {
            AudioValidation::DiscardGhostTap { duration_ms } => {
                println!(
                    "[Audio Filter] Discarded ghost tap: {}ms (minimum: {}ms)",
                    duration_ms, MIN_RECORDING_MS
                );
                self.finish_timing(timing, finalize_start);
                Ok(None)
            }
            AudioValidation::DiscardSilence { rms } => {
                println!(
                    "[Audio Filter] Discarded silent audio: RMS {:.5} (threshold: {:.5})",
                    rms, SILENCE_RMS_THRESHOLD
                );
                self.finish_timing(timing, finalize_start);
                Ok(None)
            }
            AudioValidation::Valid {
                duration_ms,
                rms,
                peak,
            } => {
                let digital_gain = apply_digital_gain(&mut pcm16, 0.72, 16.0);
                let wav_bytes = encode_pcm16_to_wav(&pcm16, TARGET_SAMPLE_RATE);

                println!(
                    "[Audio Ingest Complete] Duration: {}ms | Samples: {} | RMS: {:.4} | Peak: {:.4} | Gain: {:.2}x | WAV: {} bytes",
                    duration_ms,
                    pcm16.len(),
                    rms,
                    peak,
                    digital_gain,
                    wav_bytes.len()
                );

                self.finish_timing(timing, finalize_start);
                Ok(Some(AudioPayload {
                    wav_bytes,
                    pcm_samples: pcm16,
                    duration_ms,
                    rms,
                    peak,
                    digital_gain,
                }))
            }
        }
    }

    fn finish_timing(&mut self, timing: &mut CaptureTiming, finalize_start: Instant) {
        timing.finalize_ms = finalize_start.elapsed().as_millis() as u64;
        timing.first_speech_after_trigger_ms = timing
            .first_speech_offset_ms
            .map(|v| v as i64 - timing.onboard_lead_ms as i64);
        println!("{}", timing.log_line());
        timing::append_capture_timing(timing);
        self.last_timing = Some(timing.clone());
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }

    /// Returns the live RMS energy in [0.0, 1.0] for real-time waveform UI meters (lock-free).
    pub fn live_rms(&self) -> f32 {
        if !self.is_recording.load(Ordering::Relaxed) {
            return 0.0;
        }

        let raw = self.live_energy_raw.load(Ordering::Relaxed);
        ((raw as f32) / 10_000.0).min(1.0)
    }

    /// Returns the live acoustic vocal formant bands: (total_rms, low_pitch_rms, mid_vowel_formant_rms, high_consonant_rms)
    pub fn live_formants(&self) -> (f32, f32, f32, f32) {
        if !self.is_recording.load(Ordering::Relaxed) {
            return (0.0, 0.0, 0.0, 0.0);
        }

        let total = ((self.live_energy_raw.load(Ordering::Relaxed) as f32) / 10_000.0).min(1.0);
        let low = ((self.live_low_raw.load(Ordering::Relaxed) as f32) / 10_000.0).min(1.0);
        let mid = ((self.live_mid_raw.load(Ordering::Relaxed) as f32) / 10_000.0).min(1.0);
        let high = ((self.live_high_raw.load(Ordering::Relaxed) as f32) / 10_000.0).min(1.0);
        (total, low, mid, high)
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    pub fn is_recording(&self) -> bool {
        self.is_recording.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bluetooth_scoring_distinguishes_wireless_from_usb_headsets() {
        assert!(bluetooth_endpoint_score("Headset (OnePlus Buds Hands-Free AG Audio)") > 0);
        assert!(bluetooth_endpoint_score("Bluetooth Hands-Free Audio") > 0);
        assert_eq!(bluetooth_endpoint_score("USB Headset Microphone"), 0);
        assert_eq!(bluetooth_endpoint_score("Microphone Array (Realtek(R) Audio)"), 0);
    }

    #[test]
    fn onboard_selection_prefers_builtin_and_allows_usb_mics() {
        let array = onboard_device_score("Microphone Array (Realtek(R) Audio)");
        let usb = onboard_device_score("USB Headset Microphone");
        assert!(array > usb);
        assert!(usb > 0);
        assert!(onboard_device_score("Headset (WH-1000XM4 Hands-Free AG Audio)") < 0);
        assert!(onboard_device_score("Stereo Mix (Realtek(R) Audio)") < 0);
    }
}
