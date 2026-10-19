//! yt-dlp (incrustado en el exe) + conversión/corte en Rust puro.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};

use crate::app::{Msg, Notifier};
use crate::audio;
use crate::clean::clean_filename;

// ───────────────────────── Binarios incrustados ─────────────────────────
// build.rs copia embed/yt-dlp.exe y embed/deno.exe (si existen) a OUT_DIR.
// Si no existen se incrusta un archivo vacío y se busca la herramienta fuera.

static YTDLP_BIN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/embed_ytdlp.bin"));
static DENO_BIN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/embed_deno.bin"));
const YTDLP_HASH: &str = env!("EMBED_YTDLP_HASH");
const DENO_HASH: &str = env!("EMBED_DENO_HASH");

#[derive(Default)]
pub struct Embedded {
    pub ytdlp: Option<PathBuf>,
    pub deno: Option<PathBuf>,
    pub error: Option<String>,
}

static EMBEDDED: OnceLock<Embedded> = OnceLock::new();

fn exe_name(base: &str) -> String {
    if cfg!(windows) { format!("{base}.exe") } else { base.to_string() }
}

/// Carpeta donde se extraen las herramientas: %LOCALAPPDATA%\yt_mp3_gui\bin
pub fn tools_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("yt_mp3_gui").join("bin")
}

/// Extrae un binario si falta o si este exe trae una versión distinta.
/// (Si el usuario actualiza yt-dlp con -U, no se pisa mientras no cambie el exe.)
fn extract(bytes: &[u8], hash: &str, name: &str) -> Result<Option<PathBuf>, String> {
    if bytes.is_empty() {
        return Ok(None);
    }
    let dir = tools_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("No se pudo crear {}: {e}", dir.display()))?;
    let exe = dir.join(exe_name(name));
    let stamp = dir.join(format!("{name}.stamp"));
    let current = std::fs::read_to_string(&stamp).unwrap_or_default();
    if !exe.is_file() || current.trim() != hash {
        let tmp = dir.join(format!("{name}.tmp"));
        std::fs::write(&tmp, bytes).map_err(|e| format!("No se pudo extraer {name}: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
        }
        let _ = std::fs::remove_file(&exe);
        std::fs::rename(&tmp, &exe).map_err(|e| format!("No se pudo extraer {name}: {e}"))?;
        let _ = std::fs::write(&stamp, hash);
    }
    Ok(Some(exe))
}

/// Se llama una vez al arrancar.
pub fn init_embedded() -> &'static Embedded {
    EMBEDDED.get_or_init(|| {
        let mut e = Embedded::default();
        match extract(YTDLP_BIN, YTDLP_HASH, "yt-dlp") {
            Ok(p) => e.ytdlp = p,
            Err(x) => e.error = Some(x),
        }
        match extract(DENO_BIN, DENO_HASH, "deno") {
            Ok(p) => e.deno = p,
            Err(x) => e.error = Some(x),
        }
        e
    })
}

pub fn embedded_info() -> String {
    let mb = |b: &[u8]| b.len() as f64 / 1_048_576.0;
    let mut s = String::new();
    s += &if YTDLP_BIN.is_empty() { "yt-dlp: no incrustado".to_string() } else { format!("yt-dlp incrustado ({:.1} MB)", mb(YTDLP_BIN)) };
    if !DENO_BIN.is_empty() {
        s += &format!(" · deno incrustado ({:.1} MB)", mb(DENO_BIN));
    }
    s
}

/// Crea un Command sin abrir ventana de consola en Windows.
pub fn command(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// Busca un ejecutable junto al .exe de esta aplicación.
fn next_to_app(base: &str) -> Option<String> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let cand = dir.join(exe_name(base));
    cand.is_file().then(|| cand.to_string_lossy().into_owned())
}

/// yt-dlp: ruta configurada > incrustado > junto a la app > PATH.
pub fn ytdlp_exe(cfg_path: &str) -> String {
    let p = cfg_path.trim();
    if !p.is_empty() {
        return p.to_string();
    }
    if let Some(e) = init_embedded().ytdlp.as_ref() {
        return e.to_string_lossy().into_owned();
    }
    next_to_app("yt-dlp").unwrap_or_else(|| "yt-dlp".into())
}

fn deno_exe() -> Option<String> {
    init_embedded()
        .deno
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .or_else(|| next_to_app("deno"))
}

fn parse_percent(line: &str) -> Option<f32> {
    if !line.starts_with("[download]") {
        return None;
    }
    line.split_whitespace().find(|t| t.ends_with('%')).and_then(|t| t.trim_end_matches('%').parse().ok())
}

/// Ejecuta un comando enviando cada línea al log (y el % de descarga a la barra).
fn run_streaming(mut cmd: Command, n: &Notifier) -> Result<bool, String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("No se pudo ejecutar {:?}: {e}", cmd.get_program()))?;

    fn pump(r: impl std::io::Read, n: &Notifier) {
        let mut r = BufReader::new(r);
        let mut buf = Vec::new();
        while r.read_until(b'\n', &mut buf).unwrap_or(0) > 0 {
            let line = String::from_utf8_lossy(&buf).trim_end().to_string();
            buf.clear();
            if line.is_empty() {
                continue;
            }
            match parse_percent(&line) {
                Some(p) => n.send(Msg::Progress(p / 100.0)),
                None => n.send(Msg::Log(line)),
            }
        }
    }

    let stderr = child.stderr.take().unwrap();
    let n2 = n.clone();
    let t = std::thread::spawn(move || pump(stderr, &n2));
    pump(child.stdout.take().unwrap(), n);
    let _ = t.join();
    let status = child.wait().map_err(|e| e.to_string())?;
    Ok(status.success())
}

fn base_ytdlp_args(cmd: &mut Command) {
    cmd.args(["--no-playlist", "--encoding", "utf-8"]);
    if let Some(d) = deno_exe() {
        cmd.args(["--js-runtimes", &format!("deno:{d}")]);
    }
}

pub struct DownloadJob {
    pub url: String,
    pub out_dir: PathBuf,
    pub ytdlp: String,
    pub quality: String,
}

/// Equivale a download_youtube_audio() del script Python, sin ffmpeg:
/// yt-dlp baja el audio original (m4a/AAC) y lo convertimos a MP3 en Rust.
pub fn download(job: DownloadJob, n: &Notifier) -> Result<PathBuf, String> {
    // 1. Título y autor sin descargar
    n.send(Msg::Log("Obteniendo información del vídeo…".into()));
    let mut cmd = command(&job.ytdlp);
    base_ytdlp_args(&mut cmd);
    let out = cmd
        .args(["--no-warnings", "--skip-download", "--print", "title", "--print", "uploader"])
        .arg(&job.url)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("No se pudo ejecutar yt-dlp ({}): {e}", job.ytdlp))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let mut lines = text.lines();
    let original = lines.next().unwrap_or("audio").trim().to_string();
    let uploader = lines.next().map(str::trim).filter(|u| *u != "NA").unwrap_or("").to_string();

    // 2. Nombre limpio
    let safe = clean_filename(&original);
    n.send(Msg::Log(format!("🎵 Nombre original: {original}")));
    n.send(Msg::Log(format!("✨ Nombre limpio:   {safe}.mp3")));

    // 3. Descargar audio original (sin conversión → no necesita ffmpeg)
    let tmp_prefix = format!("{safe}.descarga-tmp");
    let template = job.out_dir.join(format!("{tmp_prefix}.%(ext)s"));
    let mut cmd = command(&job.ytdlp);
    base_ytdlp_args(&mut cmd);
    cmd.args(["--newline", "--no-part", "--no-mtime", "--no-write-thumbnail"])
        .args(["-f", "bestaudio[ext=m4a]/bestaudio[acodec^=mp4a]/bestaudio[ext=mp3]/bestaudio"])
        .arg("-o")
        .arg(&template)
        .arg(&job.url);
    if !run_streaming(cmd, n)? {
        return Err("yt-dlp terminó con error (mira el log)".into());
    }
    let tmp = std::fs::read_dir(&job.out_dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.file_name().and_then(|f| f.to_str()).is_some_and(|f| f.starts_with(&tmp_prefix)))
        .ok_or("No se encontró el audio descargado")?;

    // 4. Convertir a MP3 en Rust (symphonia + LAME) con etiquetas ID3v2.3
    n.send(Msg::Log(format!("Convirtiendo a MP3 {} kbps…", job.quality)));
    let result = (|| {
        let (pcm, rate) = audio::decode(&tmp)?;
        let mp3 = audio::encode_mp3(&pcm, rate, &job.quality)?;
        let dest = job.out_dir.join(format!("{safe}.mp3"));
        audio::write_mp3(&dest, &mp3, Some(audio::make_tag(&original, &uploader)))?;
        Ok::<_, String>(dest)
    })();
    let _ = std::fs::remove_file(&tmp);
    result
}

pub fn update_ytdlp(ytdlp: &str, n: &Notifier) -> Result<(), String> {
    let mut cmd = command(ytdlp);
    cmd.arg("-U");
    if run_streaming(cmd, n)? { Ok(()) } else { Err("La actualización falló".into()) }
}

pub fn check_tools(ytdlp: &str, n: &Notifier) {
    n.send(Msg::Log(embedded_info()));
    match command(ytdlp).arg("--version").stdin(Stdio::null()).output() {
        Ok(o) if o.status.success() => {
            let v = String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").to_string();
            n.send(Msg::Log(format!("✅ yt-dlp {v} ({ytdlp})")));
        }
        Ok(o) => n.send(Msg::Log(format!("❌ yt-dlp ({ytdlp}): {}", String::from_utf8_lossy(&o.stderr).trim()))),
        Err(e) => n.send(Msg::Log(format!("❌ yt-dlp no encontrado ({ytdlp}): {e}"))),
    }
    match deno_exe() {
        Some(d) => n.send(Msg::Log(format!("✅ deno: {d}"))),
        None => n.send(Msg::Log("ℹ deno no incrustado: yt-dlp usará el del PATH si existe".into())),
    }
    n.send(Msg::Log(format!("✅ MP3: LAME {} integrado", mp3lame_encoder::mp3lame_version())));
}

pub struct CutJob {
    pub samples: Arc<Vec<i16>>,
    pub rate: u32,
    pub input: PathBuf,
    pub output: PathBuf,
    pub start: f64,
    pub end: f64,
    pub fade_in: f64,
    pub fade_out: f64,
    pub quality: String,
}

/// Corta [start, end] del audio ya cargado en memoria y lo guarda como MP3.
pub fn cut(job: CutJob) -> Result<PathBuf, String> {
    let frames = job.samples.len() / 2;
    let f0 = ((job.start * job.rate as f64) as usize).min(frames);
    let f1 = ((job.end * job.rate as f64) as usize).min(frames);
    if f1 <= f0 {
        return Err("Selección vacía".into());
    }
    let mut pcm = job.samples[f0 * 2..f1 * 2].to_vec();
    audio::apply_fades(&mut pcm, job.rate, job.fade_in, job.fade_out);
    let mp3 = audio::encode_mp3(&pcm, job.rate, &job.quality)?;
    // Conservar las etiquetas del original si las tiene
    let tag = id3::Tag::read_from_path(&job.input).ok().or_else(|| {
        let stem = job.input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        Some(audio::make_tag(&stem, ""))
    });
    audio::write_mp3(&job.output, &mp3, tag)?;
    Ok(job.output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codifica_carga_y_corta() {
        let dir = std::env::temp_dir().join("ytmp3_test");
        std::fs::create_dir_all(&dir).unwrap();
        // 10 s de seno 440 Hz estéreo a 44.1 kHz
        let rate = 44_100u32;
        let pcm: Vec<i16> = (0..rate as usize * 10)
            .flat_map(|i| {
                let v = ((i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 8000.0) as i16;
                [v, v]
            })
            .collect();
        let src = dir.join("tono.mp3");
        let mp3 = audio::encode_mp3(&pcm, rate, "192").unwrap();
        audio::write_mp3(&src, &mp3, Some(audio::make_tag("Canción de prueba", "Autor"))).unwrap();

        let a = audio::load(&src).unwrap();
        assert!((a.duration - 10.0).abs() < 0.1, "dur {}", a.duration);

        let out = dir.join("corte.mp3");
        let _ = std::fs::remove_file(&out);
        cut(CutJob {
            samples: a.samples.clone(),
            rate: a.rate,
            input: src,
            output: out.clone(),
            start: 2.5,
            end: 6.0,
            fade_in: 0.5,
            fade_out: 1.0,
            quality: "192".into(),
        })
        .unwrap();
        let b = audio::load(&out).unwrap();
        assert!((b.duration - 3.5).abs() < 0.06, "dur corte {}", b.duration);
        use id3::TagLike;
        let t = id3::Tag::read_from_path(&out).unwrap();
        assert_eq!(t.title(), Some("Canción de prueba"));
        assert_eq!(t.version(), id3::Version::Id3v23);
    }
}

#[cfg(test)]
mod tests_descarga {
    use super::*;
    #[test]
    fn descarga_con_ytdlp_incrustado() {
        let e = init_embedded();
        let Some(y) = e.ytdlp.clone() else {
            eprintln!("(sin embed/yt-dlp: prueba omitida)");
            return;
        };
        assert!(y.is_file());
        let (tx, rx) = std::sync::mpsc::channel();
        let n = Notifier::for_test(tx);
        let dir = std::env::temp_dir().join("ytmp3_dl");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let r = download(DownloadJob { url: "x".into(), out_dir: dir.clone(), ytdlp: ytdlp_exe(""), quality: "192".into() }, &n).unwrap();
        let logs: Vec<String> = rx.try_iter().filter_map(|m| if let Msg::Log(s) = m { Some(s) } else { None }).collect();
        println!("{logs:#?}");
        assert_eq!(r.file_name().unwrap(), "Cancion - Prueba Official Video.mp3");
        let a = audio::load(&r).unwrap();
        assert!((a.duration - 5.0).abs() < 0.1);
        use id3::TagLike;
        let t = id3::Tag::read_from_path(&r).unwrap();
        assert_eq!(t.artist(), Some("Autor Falso"));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "debe borrar el temporal");
    }
}
