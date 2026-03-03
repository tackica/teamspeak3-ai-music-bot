use anyhow::{Context, Result};
use audiopus::{coder::Encoder, Application, Channels, SampleRate};
use std::process::Command;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tracing::{debug, error, info};
use tsproto_packets::packets::{AudioData, CodecType, OutAudio, OutPacket};
use whatlang::{detect, Lang};

static VOLUME: AtomicU8 = AtomicU8::new(100);
/// When true, TTS is actively playing — music should pause
static DUCKING: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone)]
pub struct TtsConfig {
    pub piper_path: String,
    pub voice_dir: String,
    pub ffmpeg_path: String,
    pub yt_dlp_path: String,
    pub music_start_volume: u8,
}

pub fn set_volume(v: u8) {
    VOLUME.store(v, Ordering::SeqCst);
}

pub fn get_volume() -> u8 {
    VOLUME.load(Ordering::SeqCst)
}

pub fn set_ducking(active: bool) {
    DUCKING.store(active, Ordering::SeqCst);
}

pub fn is_ducking() -> bool {
    DUCKING.load(Ordering::SeqCst)
}

const MAX_OPUS_FRAME_SIZE: usize = 4000;

// 20ms of audio at 48kHz mono = 960 samples
const FRAME_SIZE: usize = 960;
const TTS_MAX_SEGMENT_CHARS: usize = 240;
const TTS_SEGMENT_PAUSE_SAMPLES: usize = (48_000 * 120) / 1000;
const TTS_LEAD_IN_SAMPLES: usize = (48_000 * 50) / 1000;

fn voice_model_path(voice_dir: &str, filename: &str) -> String {
    format!("{}/{}", voice_dir.trim_end_matches('/'), filename)
}

fn piper_voice_for_text(text: &str, voice_dir: &str) -> (Lang, String, Option<&'static str>) {
    let lang = detect(text).map(|i| i.lang()).unwrap_or(Lang::Eng);
    let (filename, speaker_id) = match lang {
        Lang::Eng => ("en_US.onnx", None),
        Lang::Tur => ("tr_TR.onnx", None),
        Lang::Deu => ("de_DE.onnx", None),
        Lang::Fra => ("fr_FR.onnx", None),
        Lang::Spa => ("es_ES.onnx", Some("1")),
        Lang::Hrv | Lang::Srp | Lang::Slv => ("sl_SI.onnx", None),
        _ => ("en_US.onnx", None),
    };

    (lang, voice_model_path(voice_dir, filename), speaker_id)
}

async fn synthesize_piper_segment(
    piper_path: &str,
    model_path: &str,
    speaker_id: Option<&str>,
    text: &str,
) -> Result<Vec<u8>> {
    use tokio::io::AsyncWriteExt;

    let mut args = vec!["--model", model_path, "--output_file", "-"];
    if let Some(sid) = speaker_id {
        args.push("--speaker");
        args.push(sid);
    }

    let mut child = tokio::process::Command::new(piper_path)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("Failed to spawn piper TTS")?;

    let mut stdin = child
        .stdin
        .take()
        .context("Failed to open stdin for piper")?;
    stdin
        .write_all(text.as_bytes())
        .await
        .context("Failed to write text to piper stdin")?;
    drop(stdin);

    let output = child
        .wait_with_output()
        .await
        .context("Failed to read piper output")?;

    if !output.status.success() {
        anyhow::bail!("Piper failed to generate TTS audio");
    }

    Ok(output.stdout)
}

fn split_long_segment_by_words(text: &str, max_chars: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();

    for word in text.split_whitespace() {
        if word.len() > max_chars {
            if !current.is_empty() {
                out.push(current.trim().to_string());
                current.clear();
            }
            out.push(word.to_string());
            continue;
        }

        let next_len = if current.is_empty() {
            word.len()
        } else {
            current.len() + 1 + word.len()
        };

        if next_len > max_chars && !current.is_empty() {
            out.push(current.trim().to_string());
            current.clear();
        }

        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }

    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }

    out
}

fn split_tts_segments(text: &str) -> Vec<String> {
    let normalized = text.replace("\r\n", "\n");
    let mut blocks = normalized
        .split("\n\n")
        .map(|b| b.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|b| !b.is_empty())
        .collect::<Vec<_>>();

    if blocks.is_empty() {
        let compact = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
        if !compact.is_empty() {
            blocks.push(compact);
        }
    }

    let mut packed = Vec::new();
    for block in blocks {
        let mut sentences = Vec::new();
        let mut current = String::new();
        for ch in block.chars() {
            current.push(ch);
            if matches!(ch, '.' | '!' | '?') {
                let trimmed = current.trim();
                if !trimmed.is_empty() {
                    sentences.push(trimmed.to_string());
                }
                current.clear();
            }
        }

        let remaining = current.trim();
        if !remaining.is_empty() {
            sentences.push(remaining.to_string());
        }

        let mut block_packed = String::new();
        for sentence in sentences {
            for part in split_long_segment_by_words(&sentence, TTS_MAX_SEGMENT_CHARS) {
                if part.is_empty() {
                    continue;
                }

                let next_len = if block_packed.is_empty() {
                    part.len()
                } else {
                    block_packed.len() + 1 + part.len()
                };

                if next_len > TTS_MAX_SEGMENT_CHARS && !block_packed.is_empty() {
                    packed.push(block_packed.trim().to_string());
                    block_packed = part;
                } else {
                    if !block_packed.is_empty() {
                        block_packed.push(' ');
                    }
                    block_packed.push_str(&part);
                }
            }
        }

        if !block_packed.trim().is_empty() {
            packed.push(block_packed.trim().to_string());
        }
    }

    if packed.is_empty() {
        let compact = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
        if !compact.is_empty() {
            packed.push(compact);
        }
    }

    packed
}

/// Fetch TTS using Piper TTS with Multilingual Language Detection
#[allow(dead_code)]
pub async fn fetch_tts(text: &str, tts_config: &TtsConfig) -> Result<Vec<u8>> {
    let (lang, model_path, speaker_id) = piper_voice_for_text(text, &tts_config.voice_dir);

    info!(
        "Detected language: {:?} - Using Piper model: {} (Speaker: {:?})",
        lang, model_path, speaker_id
    );

    synthesize_piper_segment(&tts_config.piper_path, &model_path, speaker_id, text).await
}

pub async fn fetch_tts_pcm(text: &str, tts_config: &TtsConfig) -> Result<Vec<i16>> {
    let (lang, model_path, speaker_id) = piper_voice_for_text(text, &tts_config.voice_dir);
    let segments = split_tts_segments(text);

    if segments.is_empty() {
        anyhow::bail!("No text available for TTS synthesis");
    }

    info!(
        "Detected language: {:?} - Using Piper model: {} (Speaker: {:?}) - TTS segments: {}",
        lang,
        model_path,
        speaker_id,
        segments.len()
    );

    let mut combined_pcm = Vec::new();
    combined_pcm.resize(TTS_LEAD_IN_SAMPLES, 0);

    for (idx, segment) in segments.iter().enumerate() {
        let wav_bytes =
            synthesize_piper_segment(&tts_config.piper_path, &model_path, speaker_id, segment)
                .await
                .with_context(|| format!("Failed to synthesize TTS segment {}", idx + 1))?;
        let mut pcm = convert_to_pcm(&wav_bytes, &tts_config.ffmpeg_path)
            .with_context(|| format!("Failed to convert TTS segment {} to PCM", idx + 1))?;

        if idx > 0 {
            combined_pcm.resize(combined_pcm.len() + TTS_SEGMENT_PAUSE_SAMPLES, 0);
        }

        combined_pcm.append(&mut pcm);
    }

    Ok(combined_pcm)
}

/// Convert mp3 (or any audio format) to raw 48KHz 16-bit mono PCM using ffmpeg
pub fn convert_to_pcm(audio_data: &[u8], ffmpeg_path: &str) -> Result<Vec<i16>> {
    use std::io::Write;

    let mut child = Command::new(ffmpeg_path)
        .args([
            "-i", "pipe:0", "-f", "s16le", "-ar", "48000", "-ac", "1", "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("Failed to spawn ffmpeg")?;

    let mut stdin = child.stdin.take().context("Failed to open stdin")?;
    let data = audio_data.to_vec();

    // Write audio in a background thread to avoid deadlocking with ffmpeg's stdout buffer
    std::thread::spawn(move || {
        let _ = stdin.write_all(&data);
    });

    let output = child
        .wait_with_output()
        .context("Failed to read ffmpeg output")?;

    if !output.status.success() {
        anyhow::bail!("ffmpeg failed to convert audio");
    }

    // Convert bytes to i16
    let mut pcm = Vec::with_capacity(output.stdout.len() / 2);
    for chunk in output.stdout.chunks_exact(2) {
        let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
        pcm.push(sample);
    }

    Ok(pcm)
}

/// Start an audio stream that sends Opus packets to the provided TeamSpeak connection.
/// Uses a mutex to ensure only one TTS plays at a time (prevents garbled overlapping audio).
pub fn stream_to_ts(
    pcm_data: Vec<i16>,
    sender: mpsc::Sender<OutPacket>,
    _connection_id: u16,
) -> Result<()> {
    use parking_lot::Mutex;
    use std::sync::LazyLock;

    // Global TTS lock — only one TTS can play at a time
    static TTS_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    tokio::task::spawn_blocking(move || {
        // Acquire lock — if another TTS is playing, we wait here
        let _guard = TTS_LOCK.lock();

        // Signal music to pause while TTS plays
        set_ducking(true);
        // Give the music consumer a moment to stop sending
        std::thread::sleep(std::time::Duration::from_millis(60));

        let encoder = match Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::Voip) {
            Ok(e) => e,
            Err(e) => {
                error!("Failed to create Opus encoder: {:?}", e);
                set_ducking(false);
                return;
            }
        };

        let mut opus_buf = [0u8; MAX_OPUS_FRAME_SIZE];
        let mut packet_id: u16 = 0;

        let vol = get_volume() as f32 / 100.0;

        for chunk in pcm_data.chunks(FRAME_SIZE) {
            let mut buf = [0i16; FRAME_SIZE];
            for (i, &sample) in chunk.iter().enumerate() {
                buf[i] = (sample as f32 * vol) as i16;
            }

            match encoder.encode(&buf, &mut opus_buf) {
                Ok(len) => {
                    let packet = OutAudio::new(&AudioData::C2S {
                        id: packet_id,
                        codec: CodecType::OpusVoice,
                        data: &opus_buf[..len],
                    });

                    if let Err(e) = sender.blocking_send(packet) {
                        debug!("Audio sender closed: {:?}", e);
                        break;
                    }
                    packet_id = packet_id.wrapping_add(1);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(e) => {
                    error!("Opus encoding failed: {:?}", e);
                    break;
                }
            }
        }
        // Resume music after TTS finishes (guard drops here too, releasing lock)
        set_ducking(false);
    });

    Ok(())
}

/// Check if a URL needs yt-dlp to resolve (YouTube, SoundCloud, etc.)
fn needs_resolver(url: &str) -> bool {
    let patterns = [
        "youtube.com",
        "youtu.be",
        "soundcloud.com",
        "twitch.tv",
        "vimeo.com",
        "dailymotion.com",
        "bandcamp.com",
        "spotify.com",
    ];
    patterns.iter().any(|p| url.contains(p))
}

/// Use yt-dlp to extract the direct audio URL from a service like YouTube
async fn resolve_url(url: &str, yt_dlp_path: &str) -> Result<String> {
    use tracing::info;

    info!("Resolving URL via yt-dlp: {}", url);

    let output = tokio::process::Command::new(yt_dlp_path)
        .args([
            "-f",
            "bestaudio/best", // Best audio, fallback to best combined
            "--no-playlist",  // Don't download full playlists
            "-g",             // Print the direct URL instead of downloading
            url,
        ])
        .output()
        .await
        .context("Failed to run yt-dlp")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("yt-dlp failed for {}: {}", url, stderr.trim());
    }

    let resolved = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if resolved.is_empty() {
        anyhow::bail!("yt-dlp returned empty URL for {}", url);
    }

    info!(
        "yt-dlp resolved to: {}...",
        &resolved[..resolved.len().min(80)]
    );
    Ok(resolved)
}

/// Stream audio from a URL directly to TeamSpeak using FFmpeg
pub async fn stream_url_to_ts(
    url: String,
    sender: mpsc::Sender<OutPacket>,
    stop_flag: Arc<AtomicBool>,
    audio_config: TtsConfig,
) -> Result<()> {
    use parking_lot::Mutex;
    use std::collections::VecDeque;

    // Always start music/radio at configured initial volume
    set_volume(audio_config.music_start_volume.min(100));

    // Resolve URL if it's from YouTube/SoundCloud/etc.
    let resolved_url = if needs_resolver(&url) {
        match resolve_url(&url, &audio_config.yt_dlp_path).await {
            Ok(u) => u,
            Err(e) => {
                error!("Failed to resolve URL {}: {}", url, e);
                return Err(e);
            }
        }
    } else {
        url.clone()
    };

    // Buffer capacity: enough for ~10min of audio at 48kHz mono
    let buffer = Arc::new(Mutex::new(VecDeque::<i16>::with_capacity(960_000)));
    let buffer_clone = buffer.clone();
    let url_for_ffmpeg = resolved_url;
    let ffmpeg_path = audio_config.ffmpeg_path.clone();
    let stop_flag_for_ffmpeg = stop_flag.clone();

    // 1. Spawning Producer (FFmpeg -> Buffer)
    tokio::spawn(async move {
        let mut child = match tokio::process::Command::new(&ffmpeg_path)
            .args([
                "-i",
                &url_for_ffmpeg,
                "-f",
                "s16le",
                "-ar",
                "48000",
                "-ac",
                "1",
                "-loglevel",
                "warning",
                "pipe:1",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                error!("Failed to spawn ffmpeg: {}", e);
                return;
            }
        };

        let mut stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take();
        let url_for_stderr = url_for_ffmpeg.clone();

        // Log stderr in background
        if let Some(mut stderr) = stderr {
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let _ = stderr.read_to_end(&mut buf).await;
                if !buf.is_empty() {
                    let msg = String::from_utf8_lossy(&buf);
                    if !msg.trim().is_empty() {
                        tracing::warn!("FFmpeg stderr for {}: {}", url_for_stderr, msg.trim());
                    }
                }
            });
        }

        let mut pcm_buf = [0u8; 8192]; // Larger read buffer for throughput
        let mut got_data = false;

        loop {
            if stop_flag_for_ffmpeg.load(Ordering::SeqCst) {
                let _ = child.kill().await;
                break;
            }

            match stdout.read(&mut pcm_buf).await {
                Ok(0) => {
                    tracing::info!("FFmpeg EOF for {}", url_for_ffmpeg);
                    break;
                }
                Ok(n) => {
                    if !got_data {
                        tracing::info!(
                            "FFmpeg producing audio for {} ({} bytes)",
                            url_for_ffmpeg,
                            n
                        );
                        got_data = true;
                    }
                    let mut b = buffer_clone.lock();
                    // Only process complete 2-byte samples
                    let usable = n & !1; // round down to even
                    for chunk in pcm_buf[..usable].chunks_exact(2) {
                        b.push_back(i16::from_le_bytes([chunk[0], chunk[1]]));
                    }
                    // Cap at ~10 minutes to prevent unbounded growth
                    if b.len() > 30_000_000 {
                        b.drain(0..5_000_000);
                    }
                }
                Err(e) => {
                    error!("FFmpeg read error for {}: {}", url_for_ffmpeg, e);
                    break;
                }
            }
        }
        debug!("FFmpeg producer for {} finished.", url_for_ffmpeg);
    });

    // 2. Spawning Consumer (Buffer -> Opus -> TeamSpeak)
    tokio::task::spawn(async move {
        let encoder = match Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::Audio) {
            Ok(e) => e,
            Err(e) => {
                error!("Failed to create Opus encoder for music: {:?}", e);
                return;
            }
        };

        let mut opus_buf = [0u8; MAX_OPUS_FRAME_SIZE];
        let mut packet_id: u16 = 0;
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(20));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        // Pre-buffering: Wait until we have at least 1 second of audio
        let mut prebuffered = false;
        let prebuffer_threshold = 48000; // 1 second

        while !stop_flag.load(Ordering::SeqCst) {
            interval.tick().await;

            // If TTS is playing, pause music output (don't consume buffer, just wait)
            if is_ducking() {
                continue;
            }

            let mut samples = [0i16; FRAME_SIZE];
            {
                let mut b = buffer.lock();

                if !prebuffered {
                    if b.len() < prebuffer_threshold {
                        continue; // Keep buffering
                    }
                    prebuffered = true;
                    tracing::info!(
                        "Pre-buffering complete for {}, starting playback ({} samples buffered)",
                        url,
                        b.len()
                    );
                }

                if b.len() < FRAME_SIZE {
                    // Buffer underrun - don't send anything, wait for more data
                    // This prevents the "beeping" (sending empty/silent packets doesn't help TS)
                    continue;
                }

                for sample in samples.iter_mut().take(FRAME_SIZE) {
                    *sample = b.pop_front().unwrap_or(0);
                }
            }

            // Apply volume
            let vol = get_volume() as f32 / 100.0;
            if vol != 1.0 {
                for s in samples.iter_mut() {
                    *s = (*s as f32 * vol) as i16;
                }
            }

            match encoder.encode(&samples, &mut opus_buf) {
                Ok(len) => {
                    let packet = OutAudio::new(&AudioData::C2S {
                        id: packet_id,
                        codec: CodecType::OpusMusic,
                        data: &opus_buf[..len],
                    });

                    if let Err(e) = sender.send(packet).await {
                        debug!("Music audio sender closed: {:?}", e);
                        break;
                    }
                    packet_id = packet_id.wrapping_add(1);
                }
                Err(e) => {
                    error!("Opus music encoding failed: {:?}", e);
                    break;
                }
            }
        }
        debug!("Music stream consumer for {} finished.", url);
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{split_tts_segments, TTS_MAX_SEGMENT_CHARS};

    #[test]
    fn splits_on_paragraphs_and_sentences() {
        let input = "Okay, here is a joke for you!\n\nWhy do programmers prefer dark mode?\n\nBecause light attracts bugs!";
        let parts = split_tts_segments(input);
        assert!(parts.len() >= 2);
        assert!(parts
            .iter()
            .any(|p| p.contains("Okay, here is a joke for you!")));
        assert!(parts
            .iter()
            .any(|p| p.contains("Why do programmers prefer dark mode?")));
        assert!(parts
            .iter()
            .any(|p| p.contains("Because light attracts bugs!")));
    }

    #[test]
    fn splits_long_text_to_safe_lengths() {
        let input = "word ".repeat(TTS_MAX_SEGMENT_CHARS + 120);
        let parts = split_tts_segments(&input);
        assert!(parts.len() >= 2);
        assert!(parts.iter().all(|p| p.len() <= TTS_MAX_SEGMENT_CHARS));
    }

    #[test]
    fn trims_and_ignores_empty_input() {
        assert!(split_tts_segments(" \n\n \n").is_empty());
    }
}
