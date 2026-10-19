//! Incrusta yt-dlp (y opcionalmente deno) dentro del exe.
//! Pon los binarios en la carpeta `embed/` antes de compilar:
//!   embed/yt-dlp.exe   (obligatorio para que el exe sea autónomo)
//!   embed/deno.exe     (opcional, recomendado por yt-dlp para YouTube)
//! En Linux/macOS se usan embed/yt-dlp y embed/deno.

use std::path::{Path, PathBuf};

fn fnv1a(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in data {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let windows = std::env::var("CARGO_CFG_WINDOWS").is_ok();
    println!("cargo:rerun-if-changed=embed");
    for (name, key) in [("yt-dlp", "YTDLP"), ("deno", "DENO")] {
        let file = if windows { format!("{name}.exe") } else { name.to_string() };
        let src = Path::new("embed").join(&file);
        println!("cargo:rerun-if-changed={}", src.display());
        let data = std::fs::read(&src).unwrap_or_default();
        if data.is_empty() {
            println!("cargo:warning=No se encontró {} — no se incrustará", src.display());
        }
        std::fs::write(out.join(format!("embed_{}.bin", name.replace('-', ""))), &data).unwrap();
        println!("cargo:rustc-env=EMBED_{key}_HASH={:016x}-{}", fnv1a(&data), data.len());
    }

    // Icono / metadatos del exe en Windows: opcional, no requerido.
}
