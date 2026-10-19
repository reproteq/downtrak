//! Audio 100 % Rust: decodificación (symphonia), codificación MP3 (LAME
//! compilado dentro del exe), etiquetas ID3 y reproducción (rodio).

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rodio::{OutputStream, OutputStreamHandle, Sink, Source};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub const CHANNELS: u16 = 2;
pub const PEAKS_PER_SEC: f64 = 200.0;

pub struct AudioData {
    pub path: PathBuf,
    pub samples: Arc<Vec<i16>>, // intercalado estéreo
    pub rate: u32,
    pub duration: f64,
    pub peaks: Vec<f32>, // amplitud máx. (0..1) cada 1/PEAKS_PER_SEC s
}

/// Decodifica MP3 / M4A(AAC, ALAC) / FLAC / WAV / OGG Vorbis a PCM i16 estéreo intercalado.
pub fn decode(path: &Path) -> Result<(Vec<i16>, u32), String> {
    let file = File::open(path).map_err(|e| format!("No se pudo abrir {}: {e}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let fmt_opts = FormatOptions { enable_gapless: true, ..Default::default() };
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &fmt_opts, &MetadataOptions::default())
        .map_err(|e| format!("Formato no soportado ({e}). Formatos válidos: mp3, m4a, flac, wav, ogg"))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or("El archivo no tiene pista de audio")?;
    let track_id = track.id;
    let mut rate = track.codec_params.sample_rate.unwrap_or(44_100);
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("Códec no soportado ({e}). Opus/WebM no se pueden abrir"))?;

    let mut out: Vec<i16> = Vec::new();
    let mut sbuf: Option<SampleBuffer<i16>> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(format!("Error leyendo audio: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymError::DecodeError(_)) => continue, // paquete dañado: se salta
            Err(e) => return Err(format!("Error decodificando: {e}")),
        };
        let spec = *decoded.spec();
        rate = spec.rate;
        let ch = spec.channels.count();
        let need = decoded.capacity() as u64;
        if sbuf.as_ref().map_or(true, |b| (b.capacity() as u64) < need * ch as u64) {
            sbuf = Some(SampleBuffer::<i16>::new(need, spec));
        }
        let b = sbuf.as_mut().unwrap();
        b.copy_interleaved_ref(decoded);
        let s = b.samples();
        match ch {
            1 => out.extend(s.iter().flat_map(|&v| [v, v])),
            2 => out.extend_from_slice(s),
            n => {
                // más de 2 canales: nos quedamos con los dos primeros
                for f in s.chunks_exact(n) {
                    out.extend_from_slice(&f[..2]);
                }
            }
        }
    }
    if out.is_empty() {
        return Err("El archivo no contiene audio".into());
    }
    Ok((out, rate))
}

pub fn load(path: &Path) -> Result<AudioData, String> {
    let (samples, rate) = decode(path)?;
    let frames = samples.len() / CHANNELS as usize;
    let duration = frames as f64 / rate as f64;
    let bucket = ((rate as f64 / PEAKS_PER_SEC) as usize).max(1) * CHANNELS as usize;
    let peaks = samples
        .chunks(bucket)
        .map(|c| c.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0) as f32 / 32768.0)
        .collect();
    Ok(AudioData { path: path.to_path_buf(), samples: Arc::new(samples), rate, duration, peaks })
}

fn bitrate(kbps: &str) -> mp3lame_encoder::Bitrate {
    use mp3lame_encoder::Bitrate::*;
    match kbps.trim() {
        "128" => Kbps128,
        "160" => Kbps160,
        "256" => Kbps256,
        "320" => Kbps320,
        _ => Kbps192,
    }
}

/// Codifica PCM i16 estéreo intercalado a MP3 CBR con LAME.
pub fn encode_mp3(pcm: &[i16], rate: u32, kbps: &str) -> Result<Vec<u8>, String> {
    use mp3lame_encoder::{Builder, FlushNoGap, InterleavedPcm, Mode, Quality};
    let e = |x: &dyn std::fmt::Display| format!("LAME: {x}");
    let mut b = Builder::new().ok_or("LAME: no se pudo inicializar")?;
    b.set_num_channels(2).map_err(|x| e(&x))?;
    b.set_sample_rate(rate).map_err(|x| e(&x))?;
    b.set_brate(bitrate(kbps)).map_err(|x| e(&x))?;
    b.set_mode(Mode::JointStereo).map_err(|x| e(&x))?;
    b.set_quality(Quality::NearBest).map_err(|x| e(&x))?;
    let mut enc = b.build().map_err(|x| e(&x))?;

    let mut out = Vec::with_capacity(pcm.len() / 4);
    const CHUNK_FRAMES: usize = 1152 * 32;
    for chunk in pcm.chunks(CHUNK_FRAMES * 2) {
        out.reserve(mp3lame_encoder::max_required_buffer_size(chunk.len() / 2));
        enc.encode_to_vec(InterleavedPcm(chunk), &mut out).map_err(|x| e(&x))?;
    }
    out.reserve(7200);
    enc.flush_to_vec::<FlushNoGap>(&mut out).map_err(|x| e(&x))?;
    Ok(out)
}

/// Aplica fundidos (en segundos) sobre un tramo estéreo intercalado.
pub fn apply_fades(pcm: &mut [i16], rate: u32, fade_in: f64, fade_out: f64) {
    let frames = pcm.len() / 2;
    let fi = ((fade_in * rate as f64) as usize).min(frames);
    let fo = ((fade_out * rate as f64) as usize).min(frames);
    for f in 0..frames {
        let mut g = 1.0f32;
        if fi > 0 && f < fi {
            g *= f as f32 / fi as f32;
        }
        let rem = frames - f;
        if fo > 0 && rem < fo {
            g *= rem as f32 / fo as f32;
        }
        if g < 1.0 {
            pcm[2 * f] = (pcm[2 * f] as f32 * g) as i16;
            pcm[2 * f + 1] = (pcm[2 * f + 1] as f32 * g) as i16;
        }
    }
}

/// Escribe el MP3 y sus etiquetas ID3v2.3 (como hacía '-id3v2_version 3').
pub fn write_mp3(path: &Path, data: &[u8], tag: Option<id3::Tag>) -> Result<(), String> {
    std::fs::write(path, data).map_err(|e| format!("No se pudo escribir {}: {e}", path.display()))?;
    if let Some(t) = tag {
        t.write_to_path(path, id3::Version::Id3v23).map_err(|e| format!("Error escribiendo etiquetas: {e}"))?;
    }
    Ok(())
}

pub fn make_tag(title: &str, artist: &str) -> id3::Tag {
    use id3::TagLike;
    let mut t = id3::Tag::new();
    if !title.is_empty() {
        t.set_title(title);
    }
    if !artist.is_empty() {
        t.set_artist(artist);
    }
    t
}

/// Fuente rodio sobre el buffer compartido (sin copiar), con fundidos opcionales.
struct Segment {
    data: Arc<Vec<i16>>,
    pos: usize,
    start: usize,
    end: usize,
    fade_in: usize,  // en frames
    fade_out: usize, // en frames
    rate: u32,
}

impl Iterator for Segment {
    type Item = i16;
    fn next(&mut self) -> Option<i16> {
        if self.pos >= self.end {
            return None;
        }
        let s = self.data[self.pos];
        let ch = CHANNELS as usize;
        let frame = (self.pos - self.start) / ch;
        let total = (self.end - self.start) / ch;
        self.pos += 1;
        let mut g = 1.0f32;
        if self.fade_in > 0 && frame < self.fade_in {
            g *= frame as f32 / self.fade_in as f32;
        }
        let rem = total.saturating_sub(frame);
        if self.fade_out > 0 && rem < self.fade_out {
            g *= rem as f32 / self.fade_out as f32;
        }
        Some(if g >= 1.0 { s } else { (s as f32 * g) as i16 })
    }
}

impl Source for Segment {
    fn current_frame_len(&self) -> Option<usize> { None }
    fn channels(&self) -> u16 { CHANNELS }
    fn sample_rate(&self) -> u32 { self.rate }
    fn total_duration(&self) -> Option<Duration> { None }
}

pub struct Player {
    _stream: OutputStream,
    handle: OutputStreamHandle,
    sink: Option<Sink>,
    started: Instant,
    from: f64,
    to: f64,
}

impl Player {
    pub fn new() -> Result<Self, String> {
        let (stream, handle) = OutputStream::try_default().map_err(|e| format!("Sin salida de audio: {e}"))?;
        Ok(Self { _stream: stream, handle, sink: None, started: Instant::now(), from: 0.0, to: 0.0 })
    }

    pub fn play(&mut self, audio: &AudioData, from: f64, to: f64, fade_in: f64, fade_out: f64) -> Result<(), String> {
        self.stop();
        let ch = CHANNELS as usize;
        let total_frames = audio.samples.len() / ch;
        let rate = audio.rate as f64;
        let f0 = ((from * rate) as usize).min(total_frames);
        let f1 = ((to * rate) as usize).min(total_frames);
        if f1 <= f0 {
            return Ok(());
        }
        let sink = Sink::try_new(&self.handle).map_err(|e| e.to_string())?;
        sink.append(Segment {
            data: audio.samples.clone(),
            pos: f0 * ch,
            start: f0 * ch,
            end: f1 * ch,
            fade_in: (fade_in * rate) as usize,
            fade_out: (fade_out * rate) as usize,
            rate: audio.rate,
        });
        self.sink = Some(sink);
        self.started = Instant::now();
        self.from = from;
        self.to = to;
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(s) = self.sink.take() {
            s.stop();
        }
    }

    pub fn is_playing(&self) -> bool {
        self.sink.as_ref().is_some_and(|s| !s.empty())
    }

    /// Posición actual en segundos (None si no suena).
    pub fn position(&self) -> Option<f64> {
        self.is_playing().then(|| (self.from + self.started.elapsed().as_secs_f64()).min(self.to))
    }
}

pub fn fmt_time(t: f64) -> String {
    let t = t.max(0.0);
    let ms = (t * 1000.0).round() as u64;
    let (h, m, s, ms) = (ms / 3_600_000, (ms / 60_000) % 60, (ms / 1000) % 60, ms % 1000);
    if h > 0 { format!("{h}:{m:02}:{s:02}.{ms:03}") } else { format!("{m:02}:{s:02}.{ms:03}") }
}

/// Acepta "ss.mmm", "m:ss.mmm" o "h:mm:ss.mmm" (coma o punto decimal).
pub fn parse_time(s: &str) -> Option<f64> {
    let s = s.trim().replace(',', ".");
    let mut total = 0.0;
    for part in s.split(':') {
        total = total * 60.0 + part.trim().parse::<f64>().ok()?;
    }
    (total >= 0.0).then_some(total)
}
