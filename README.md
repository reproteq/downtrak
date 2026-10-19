# YT-MP3 — YouTube → MP3 + cortador (Rust / egui) — un solo exe

Todo va dentro de un único `YT-MP3.exe`:
- **Audio en Rust puro**: decodificación (symphonia), MP3 con LAME compilado
  dentro del exe y etiquetas ID3v2.3. **No necesita ffmpeg.**
- **yt-dlp (y deno) incrustados**: se extraen solos la primera vez a
  `%LOCALAPPDATA%\yt_mp3_gui\bin`. El botón "Actualizar yt-dlp" actualiza esa copia.

## Compilar en Windows
1. Instala Rust: https://rustup.rs (toolchain MSVC, con "Desktop development with C++").
2. En esta carpeta:
   `powershell -ExecutionPolicy Bypass -File .\preparar.ps1`
   (añade `-SinDeno` para un exe más pequeño, aunque YouTube puede fallar más sin deno)
3. Resultado: `YT-MP3.exe`. Cópialo donde quieras; no necesita nada más.

Si compilas a mano (`cargo build --release`) sin poner nada en `embed/`, el exe
buscará yt-dlp junto a él o en el PATH.

## Uso
- **Descargar**: pega la URL → Descargar MP3. Nombre limpio como el script Python
  (sin tildes ni símbolos), con título y autor en las etiquetas.
- **Cortar**: abre o arrastra un audio (mp3, m4a, flac, wav, ogg). Arrastra sobre la
  onda para seleccionar, ajusta las marcas I/O, previsualiza, fade in/out y guarda.
  Atajos: Espacio = play/stop, I / O = inicio / fin en el cursor, rueda = zoom,
  botón derecho = desplazar.
