use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

use eframe::egui::{self, Color32, CursorIcon, PointerButton, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use serde::{Deserialize, Serialize};

use crate::audio::{self, fmt_time, parse_time, AudioData, Player};
use crate::clean::clean_filename;
use crate::tools;

// ───────────────────────── Configuración persistente ─────────────────────────

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub ytdlp_path: String,
    pub output_dir: String,
    pub quality: String,
    pub open_cut_after: bool,
}

impl Default for Config {
    fn default() -> Self {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_string_lossy().into_owned()))
            .unwrap_or_else(|| ".".into());
        Self {
            ytdlp_path: String::new(),
            output_dir: exe_dir,
            quality: "192".into(),
            open_cut_after: true,
        }
    }
}

// ───────────────────────── Mensajes de los hilos ─────────────────────────

pub enum Msg {
    Log(String),
    Progress(f32),
    DownloadDone(Result<PathBuf, String>),
    ToolDone,
    AudioLoaded(Result<AudioData, String>),
    CutDone(Result<PathBuf, String>),
}

#[derive(Clone)]
pub struct Notifier {
    tx: Sender<Msg>,
    ctx: egui::Context,
}

impl Notifier {
    #[cfg(test)]
    pub fn for_test(tx: Sender<Msg>) -> Self {
        Self { tx, ctx: egui::Context::default() }
    }

    pub fn send(&self, m: Msg) {
        let _ = self.tx.send(m);
        self.ctx.request_repaint();
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Download,
    Cut,
}

#[derive(Clone, Copy)]
enum Drag {
    None,
    Start,
    End,
    Select { anchor: f64, prev: (f64, f64) },
}

// ───────────────────────── Aplicación ─────────────────────────

pub struct App {
    cfg: Config,
    tab: Tab,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,

    // Descarga
    url: String,
    log: String,
    progress: Option<f32>,
    downloading: bool,
    tool_busy: bool,

    // Corte
    audio: Option<AudioData>,
    loading: bool,
    cutting: bool,
    cut_status: String,
    sel_start: f64,
    sel_end: f64,
    cursor: f64,
    view_start: f64,
    view_end: f64,
    drag: Drag,
    fade_in: f64,
    fade_out: f64,
    out_name: String,
    player: Option<Player>,
}

const QUALITIES: [&str; 5] = ["128", "160", "192", "256", "320"];

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let cfg: Config = cc.storage.and_then(|s| eframe::get_value(s, eframe::APP_KEY)).unwrap_or_default();
        let (tx, rx) = channel();
        let mut app = Self {
            cfg,
            tab: Tab::Download,
            tx,
            rx,
            url: String::new(),
            log: String::new(),
            progress: None,
            downloading: false,
            tool_busy: false,
            audio: None,
            loading: false,
            cutting: false,
            cut_status: String::new(),
            sel_start: 0.0,
            sel_end: 0.0,
            cursor: 0.0,
            view_start: 0.0,
            view_end: 1.0,
            drag: Drag::None,
            fade_in: 0.0,
            fade_out: 0.0,
            out_name: String::new(),
            player: None,
        };
        // Extraer yt-dlp incrustado (solo la primera vez o si cambia la versión)
        {
            let n = app.notifier(&cc.egui_ctx);
            std::thread::spawn(move || {
                let e = tools::init_embedded();
                if let Some(err) = &e.error {
                    n.send(Msg::Log(format!("❌ {err}")));
                }
                n.send(Msg::Log(tools::embedded_info()));
            });
        }
        // "Abrir con…" / arrastrar sobre el .exe: abre el archivo directamente en el editor de corte
        if let Some(arg) = std::env::args().nth(1).map(PathBuf::from).filter(|p| p.is_file()) {
            app.load_audio(&cc.egui_ctx, arg);
            app.tab = Tab::Cut;
        }
        app
    }

    fn notifier(&self, ctx: &egui::Context) -> Notifier {
        Notifier { tx: self.tx.clone(), ctx: ctx.clone() }
    }

    fn log(&mut self, s: impl AsRef<str>) {
        self.log.push_str(s.as_ref());
        self.log.push('\n');
    }

    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Log(s) => self.log(s),
                Msg::Progress(p) => self.progress = Some(p),
                Msg::ToolDone => self.tool_busy = false,
                Msg::DownloadDone(r) => {
                    self.downloading = false;
                    match r {
                        Ok(p) => {
                            self.progress = Some(1.0);
                            self.log(format!("\n✅ ¡ÉXITO! Guardado como: {}\n", p.display()));
                            if self.cfg.open_cut_after {
                                self.load_audio(ctx, p);
                                self.tab = Tab::Cut;
                            }
                        }
                        Err(e) => {
                            self.progress = None;
                            self.log(format!("\n❌ ERROR: {e}\n"));
                        }
                    }
                }
                Msg::AudioLoaded(r) => {
                    self.loading = false;
                    match r {
                        Ok(a) => {
                            self.sel_start = 0.0;
                            self.sel_end = a.duration;
                            self.cursor = 0.0;
                            self.view_start = 0.0;
                            self.view_end = a.duration;
                            let stem = a.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                            self.out_name = format!("{stem} - corte");
                            self.cut_status = format!("Cargado: {} ({})", a.path.display(), fmt_time(a.duration));
                            self.audio = Some(a);
                        }
                        Err(e) => self.cut_status = format!("❌ {e}"),
                    }
                }
                Msg::CutDone(r) => {
                    self.cutting = false;
                    self.cut_status = match r {
                        Ok(p) => format!("✅ Guardado: {}", p.display()),
                        Err(e) => format!("❌ {e}"),
                    };
                }
            }
        }
    }

    fn start_download(&mut self, ctx: &egui::Context) {
        let url = self.url.trim().to_string();
        if url.is_empty() || self.downloading {
            return;
        }
        let out_dir = PathBuf::from(self.cfg.output_dir.trim());
        if let Err(e) = std::fs::create_dir_all(&out_dir) {
            self.log(format!("❌ No se pudo crear la carpeta {}: {e}", out_dir.display()));
            return;
        }
        self.downloading = true;
        self.progress = Some(0.0);
        self.log(format!("── {url}"));
        let job = tools::DownloadJob {
            url,
            out_dir,
            ytdlp: tools::ytdlp_exe(&self.cfg.ytdlp_path),
            quality: self.cfg.quality.clone(),
        };
        let n = self.notifier(ctx);
        std::thread::spawn(move || {
            let r = tools::download(job, &n);
            n.send(Msg::DownloadDone(r));
        });
    }

    fn load_audio(&mut self, ctx: &egui::Context, path: PathBuf) {
        if let Some(p) = &mut self.player {
            p.stop();
        }
        self.loading = true;
        self.audio = None;
        self.cut_status = format!("Cargando {}…", path.display());
        let n = self.notifier(ctx);
        std::thread::spawn(move || {
            let r = audio::load(&path);
            n.send(Msg::AudioLoaded(r));
        });
    }

    fn play(&mut self, from: f64, to: f64, fades: bool) {
        let Some(a) = &self.audio else { return };
        if self.player.is_none() {
            match Player::new() {
                Ok(p) => self.player = Some(p),
                Err(e) => {
                    self.cut_status = format!("❌ {e}");
                    return;
                }
            }
        }
        let (fi, fo) = if fades { (self.fade_in, self.fade_out) } else { (0.0, 0.0) };
        if let Some(p) = &mut self.player {
            if let Err(e) = p.play(a, from, to, fi, fo) {
                self.cut_status = format!("❌ {e}");
            }
        }
    }

    fn stop(&mut self) {
        if let Some(p) = &mut self.player {
            p.stop();
        }
    }

    fn is_playing(&self) -> bool {
        self.player.as_ref().is_some_and(|p| p.is_playing())
    }

    fn start_cut(&mut self, ctx: &egui::Context) {
        let Some(a) = &self.audio else { return };
        let dir = a.path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        let base = clean_filename(&self.out_name);
        let mut output = dir.join(format!("{base}.mp3"));
        let mut i = 2;
        while output.exists() {
            output = dir.join(format!("{base} {i}.mp3"));
            i += 1;
        }
        let job = tools::CutJob {
            samples: a.samples.clone(),
            rate: a.rate,
            input: a.path.clone(),
            output,
            start: self.sel_start,
            end: self.sel_end,
            fade_in: self.fade_in,
            fade_out: self.fade_out,
            quality: self.cfg.quality.clone(),
        };
        self.cutting = true;
        self.cut_status = "Cortando…".into();
        let n = self.notifier(ctx);
        std::thread::spawn(move || {
            let r = tools::cut(job);
            n.send(Msg::CutDone(r));
        });
    }

    // ───────────────────────── Pestaña Descargar ─────────────────────────

    fn ui_download(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.heading("Descargar audio de YouTube a MP3");
        ui.add_space(6.0);

        egui::Grid::new("dl_grid").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
            ui.label("URL:");
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.url)
                    .hint_text("https://www.youtube.com/watch?v=…")
                    .desired_width(f32::INFINITY),
            );
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                self.start_download(&ctx);
            }
            ui.end_row();

            ui.label("Carpeta:");
            ui.horizontal(|ui| {
                if ui.button("📁").on_hover_text("Elegir carpeta").clicked() {
                    if let Some(d) = rfd::FileDialog::new().set_directory(&self.cfg.output_dir).pick_folder() {
                        self.cfg.output_dir = d.to_string_lossy().into_owned();
                    }
                }
                ui.add(egui::TextEdit::singleline(&mut self.cfg.output_dir).desired_width(f32::INFINITY));
            });
            ui.end_row();

            ui.label("Calidad:");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("quality")
                    .selected_text(format!("{} kbps", self.cfg.quality))
                    .show_ui(ui, |ui| {
                        for q in QUALITIES {
                            ui.selectable_value(&mut self.cfg.quality, q.to_string(), format!("{q} kbps"));
                        }
                    });
                ui.checkbox(&mut self.cfg.open_cut_after, "Abrir en el editor de corte al terminar");
            });
            ui.end_row();
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let can = !self.downloading && !self.url.trim().is_empty();
            let btn = egui::Button::new(RichText::new("⬇  Descargar MP3").size(16.0)).min_size(Vec2::new(180.0, 32.0));
            if ui.add_enabled(can, btn).clicked() {
                self.start_download(&ctx);
            }
            if self.downloading {
                ui.spinner();
            }
            if let Some(p) = self.progress {
                ui.add(egui::ProgressBar::new(p).show_percentage().desired_width(ui.available_width()));
            }
        });

        ui.add_space(6.0);
        egui::CollapsingHeader::new("⚙ Herramientas").show(ui, |ui| {
            egui::Grid::new("tools_grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("yt-dlp:");
                ui.horizontal(|ui| {
                    if ui.button("📁").clicked() {
                        if let Some(f) = rfd::FileDialog::new().add_filter("yt-dlp", &["exe"]).pick_file() {
                            self.cfg.ytdlp_path = f.to_string_lossy().into_owned();
                        }
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut self.cfg.ytdlp_path)
                            .hint_text("vacío = el incluido en la app")
                            .desired_width(f32::INFINITY),
                    );
                });
                ui.end_row();
            });
            ui.horizontal(|ui| {
                let enabled = !self.tool_busy && !self.downloading;
                if ui.add_enabled(enabled, egui::Button::new("🔎 Comprobar herramientas")).clicked() {
                    self.tool_busy = true;
                    let (y, n) = (tools::ytdlp_exe(&self.cfg.ytdlp_path), self.notifier(&ctx));
                    std::thread::spawn(move || {
                        tools::check_tools(&y, &n);
                        n.send(Msg::ToolDone);
                    });
                }
                if ui
                    .add_enabled(enabled, egui::Button::new("⟳ Actualizar yt-dlp"))
                    .on_hover_text("Ejecuta yt-dlp -U (útil cuando YouTube cambia algo y deja de descargar)")
                    .clicked()
                {
                    self.tool_busy = true;
                    let (y, n) = (tools::ytdlp_exe(&self.cfg.ytdlp_path), self.notifier(&ctx));
                    std::thread::spawn(move || {
                        if let Err(e) = tools::update_ytdlp(&y, &n) {
                            n.send(Msg::Log(format!("❌ {e}")));
                        }
                        n.send(Msg::ToolDone);
                    });
                }
                if self.tool_busy {
                    ui.spinner();
                }
            });
        });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Log").strong());
            if ui.small_button("Limpiar").clicked() {
                self.log.clear();
            }
        });
        egui::Frame::group(ui.style()).show(ui, |ui| {
            egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.log.as_str())
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY),
                );
            });
        });
    }

    // ───────────────────────── Pestaña Cortar ─────────────────────────

    fn ui_cut(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            ui.heading("Cortar canción");
            ui.add_space(12.0);
            if ui.add_enabled(!self.loading, egui::Button::new("📂 Abrir audio…")).clicked() {
                let mut dlg = rfd::FileDialog::new().add_filter("Audio", &["mp3", "m4a", "mp4", "wav", "flac", "ogg"]);
                if !self.cfg.output_dir.is_empty() {
                    dlg = dlg.set_directory(&self.cfg.output_dir);
                }
                if let Some(f) = dlg.pick_file() {
                    self.load_audio(&ctx, f);
                }
            }
            if self.loading {
                ui.spinner();
            }
        });

        if self.audio.is_none() {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("Abre un archivo de audio o arrástralo a la ventana").size(18.0).weak());
                ui.add_space(6.0);
                ui.label(RichText::new(&self.cut_status).weak());
            });
            return;
        }

        // Atajos de teclado (si no se está escribiendo en un campo)
        if !ctx.wants_keyboard_input() {
            let (space, key_i, key_o) = ctx.input(|i| (i.key_pressed(egui::Key::Space), i.key_pressed(egui::Key::I), i.key_pressed(egui::Key::O)));
            if space {
                if self.is_playing() { self.stop() } else { self.play(self.sel_start, self.sel_end, true) }
            }
            if key_i {
                self.sel_start = self.cursor.min(self.sel_end - 0.01).max(0.0);
            }
            if key_o {
                self.sel_end = self.cursor.max(self.sel_start + 0.01);
            }
        }

        let duration = self.audio.as_ref().map(|a| a.duration).unwrap_or(1.0);
        ui.add_space(4.0);
        self.waveform(ui, duration);

        // Barra de zoom / desplazamiento
        ui.horizontal(|ui| {
            if ui.button("🔍 +").clicked() { self.zoom(0.5, None, duration); }
            if ui.button("🔍 −").clicked() { self.zoom(2.0, None, duration); }
            if ui.button("Ver todo").clicked() { self.view_start = 0.0; self.view_end = duration; }
            if ui.button("Ajustar a selección").clicked() {
                let pad = ((self.sel_end - self.sel_start) * 0.05).max(0.2);
                self.view_start = (self.sel_start - pad).max(0.0);
                self.view_end = (self.sel_end + pad).min(duration);
            }
            let span = self.view_end - self.view_start;
            if span < duration - 1e-6 {
                let mut pos = self.view_start;
                ui.spacing_mut().slider_width = ui.available_width() - 10.0;
                if ui.add(egui::Slider::new(&mut pos, 0.0..=(duration - span)).show_value(false)).changed() {
                    self.view_start = pos;
                    self.view_end = pos + span;
                }
            }
        });
        ui.label(
            RichText::new("Arrastra sobre la onda para seleccionar · arrastra las marcas verde/roja para ajustar · clic = mover cursor · rueda = zoom · botón derecho = desplazar · Espacio = play/stop · I / O = inicio / fin en el cursor")
                .small()
                .weak(),
        );
        ui.separator();

        // Controles de selección
        egui::Grid::new("cut_grid").num_columns(4).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label(RichText::new("Inicio").color(Color32::from_rgb(90, 210, 120)));
            ui.horizontal(|ui| {
                let max = self.sel_end - 0.01;
                ui.add(time_dv(&mut self.sel_start, max));
                if ui.small_button("⏱ = cursor").clicked() { self.sel_start = self.cursor.min(max); }
                if ui.small_button("⏮ 0").clicked() { self.sel_start = 0.0; }
            });
            ui.label("Fade in (s)");
            ui.add(egui::DragValue::new(&mut self.fade_in).speed(0.05).range(0.0..=30.0).fixed_decimals(1));
            ui.end_row();

            ui.label(RichText::new("Fin").color(Color32::from_rgb(235, 90, 90)));
            ui.horizontal(|ui| {
                let min = self.sel_start + 0.01;
                ui.add(time_dv(&mut self.sel_end, duration));
                if self.sel_end < min { self.sel_end = min.min(duration); }
                if ui.small_button("⏱ = cursor").clicked() { self.sel_end = self.cursor.max(min); }
                if ui.small_button("⏭ final").clicked() { self.sel_end = duration; }
            });
            ui.label("Fade out (s)");
            ui.add(egui::DragValue::new(&mut self.fade_out).speed(0.05).range(0.0..=30.0).fixed_decimals(1));
            ui.end_row();

            ui.label("Duración");
            ui.label(RichText::new(fmt_time(self.sel_end - self.sel_start)).monospace().strong());
            ui.label("Cursor");
            ui.label(RichText::new(fmt_time(self.cursor)).monospace());
            ui.end_row();
        });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("▶ Selección").clicked() { self.play(self.sel_start, self.sel_end, true); }
            if ui.button("▶ Desde cursor").clicked() { self.play(self.cursor, duration, false); }
            if ui.button("▶ Primeros 3 s").clicked() { self.play(self.sel_start, (self.sel_start + 3.0).min(self.sel_end), true); }
            if ui.button("▶ Últimos 3 s").clicked() {
                // Se reproduce el tramo final con el fade out aplicado si lo hay
                let from = (self.sel_end - 3.0).max(self.sel_start);
                self.play(from, self.sel_end, false);
            }
            if ui.button("■ Parar").clicked() { self.stop(); }
        });

        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Nombre de salida:");
            ui.add(egui::TextEdit::singleline(&mut self.out_name).desired_width(320.0));
            ui.label(".mp3");
        });
        let cleaned = clean_filename(&self.out_name);
        if cleaned != self.out_name.trim() {
            ui.label(RichText::new(format!("Se guardará como: {cleaned}.mp3")).small().weak());
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let btn = egui::Button::new(RichText::new("✂  Cortar y guardar MP3").size(16.0)).min_size(Vec2::new(220.0, 32.0));
            if ui.add_enabled(!self.cutting && self.sel_end > self.sel_start, btn).clicked() {
                self.start_cut(&ctx);
            }
            if self.cutting {
                ui.spinner();
            }
            ui.label(&self.cut_status);
        });
    }

    fn zoom(&mut self, factor: f64, around: Option<f64>, duration: f64) {
        let span = self.view_end - self.view_start;
        let new_span = (span * factor).clamp(0.05, duration);
        let c = around.unwrap_or((self.view_start + self.view_end) / 2.0);
        let rel = if span > 0.0 { (c - self.view_start) / span } else { 0.5 };
        let mut s = c - rel * new_span;
        s = s.clamp(0.0, (duration - new_span).max(0.0));
        self.view_start = s;
        self.view_end = s + new_span;
    }

    fn waveform(&mut self, ui: &mut egui::Ui, duration: f64) {
        let size = Vec2::new(ui.available_width(), 190.0);
        let (outer, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
        // margen lateral para que las marcas I/O no queden cortadas en los extremos
        let rect = outer.shrink2(Vec2::new(8.0, 0.0));
        let vs = self.view_start;
        let span = (self.view_end - self.view_start).max(1e-6);
        let t_to_x = |t: f64| rect.left() + ((t - vs) / span) as f32 * rect.width();
        let x_to_t = |x: f32| (vs + ((x - rect.left()) / rect.width()) as f64 * span).clamp(0.0, duration);

        // ── Interacción ──
        let hover = resp.hover_pos();
        if let Some(h) = hover {
            let near = |t: f64| (t_to_x(t) - h.x).abs() < 7.0;
            if near(self.sel_start) || near(self.sel_end) {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            }
            let scroll = ui.input(|i| i.smooth_scroll_delta.y + i.smooth_scroll_delta.x);
            if scroll != 0.0 {
                self.zoom((-(scroll as f64) * 0.004).exp(), Some(x_to_t(h.x)), duration);
            }
        }
        if resp.dragged_by(PointerButton::Secondary) || resp.dragged_by(PointerButton::Middle) {
            let dt = -(resp.drag_delta().x / rect.width()) as f64 * span;
            let s = (self.view_start + dt).clamp(0.0, (duration - span).max(0.0));
            self.view_start = s;
            self.view_end = s + span;
        }
        if resp.drag_started_by(PointerButton::Primary) {
            let origin = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos());
            if let Some(o) = origin {
                let ds = (t_to_x(self.sel_start) - o.x).abs();
                let de = (t_to_x(self.sel_end) - o.x).abs();
                self.drag = if ds < 7.0 && ds <= de {
                    Drag::Start
                } else if de < 7.0 {
                    Drag::End
                } else {
                    Drag::Select { anchor: x_to_t(o.x), prev: (self.sel_start, self.sel_end) }
                };
            }
        }
        if resp.dragged_by(PointerButton::Primary) {
            if let Some(p) = resp.interact_pointer_pos() {
                let t = x_to_t(p.x);
                match self.drag {
                    Drag::Start => self.sel_start = t.min(self.sel_end - 0.01).max(0.0),
                    Drag::End => self.sel_end = t.max(self.sel_start + 0.01).min(duration),
                    Drag::Select { anchor, .. } => {
                        self.sel_start = anchor.min(t);
                        self.sel_end = anchor.max(t);
                    }
                    Drag::None => {}
                }
            }
        }
        if resp.drag_stopped() {
            if let Drag::Select { anchor, prev } = self.drag {
                if self.sel_end - self.sel_start < 0.05 {
                    (self.sel_start, self.sel_end) = prev;
                    self.cursor = anchor;
                }
            }
            self.drag = Drag::None;
        }
        if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                self.cursor = x_to_t(p.x);
                if self.is_playing() {
                    self.play(self.cursor, duration, false);
                }
            }
        }

        // ── Dibujo ──
        let painter = ui.painter_at(outer);
        let bg = if ui.visuals().dark_mode { Color32::from_rgb(22, 24, 30) } else { Color32::from_rgb(235, 237, 242) };
        painter.rect_filled(outer, 4.0, bg);
        let ruler_h = 16.0;
        let wave_top = rect.top() + ruler_h;

        let xs = t_to_x(self.sel_start).max(rect.left());
        let xe = t_to_x(self.sel_end).min(rect.right());
        if xe > xs {
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(xs, wave_top), Pos2::new(xe, rect.bottom())),
                0.0,
                Color32::from_rgba_unmultiplied(70, 140, 255, 40),
            );
        }

        let mid = (wave_top + rect.bottom()) / 2.0;
        let half = (rect.bottom() - wave_top) / 2.0 - 4.0;
        if let Some(a) = &self.audio {
            let pps = audio::PEAKS_PER_SEC;
            let col_in = Color32::from_rgb(90, 160, 255);
            let col_out = Color32::from_rgb(95, 105, 125);
            let w = rect.width() as usize;
            for px in 0..w {
                let x = rect.left() + px as f32;
                let t0 = x_to_t(x);
                let t1 = x_to_t(x + 1.0);
                let i0 = (t0 * pps) as usize;
                let i1 = ((t1 * pps) as usize).max(i0 + 1).min(a.peaks.len());
                if i0 >= a.peaks.len() {
                    break;
                }
                let amp = a.peaks[i0..i1].iter().cloned().fold(0.0f32, f32::max);
                let hgt = (amp * half).max(0.5);
                let col = if t0 >= self.sel_start && t0 <= self.sel_end { col_in } else { col_out };
                painter.line_segment([Pos2::new(x, mid - hgt), Pos2::new(x, mid + hgt)], Stroke::new(1.0_f32, col));
            }
        }

        // Regla de tiempos
        let text_col = ui.visuals().weak_text_color();
        let steps = [0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0];
        let max_ticks = (rect.width() / 90.0).max(2.0) as f64;
        let step = steps.iter().cloned().find(|s| span / s <= max_ticks).unwrap_or(600.0);
        let mut t = (vs / step).ceil() * step;
        while t <= self.view_end {
            let x = t_to_x(t);
            painter.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.top() + 5.0)], Stroke::new(1.0_f32, text_col));
            let label = if step < 1.0 { fmt_time(t) } else { fmt_time(t).trim_end_matches(".000").to_string() };
            painter.text(Pos2::new(x + 3.0, rect.top() + 2.0), egui::Align2::LEFT_TOP, label, egui::FontId::monospace(10.0), text_col);
            t += step;
        }

        // Marcas de inicio / fin
        for (tm, col, txt) in [
            (self.sel_start, Color32::from_rgb(90, 210, 120), "I"),
            (self.sel_end, Color32::from_rgb(235, 90, 90), "O"),
        ] {
            let x = t_to_x(tm);
            if x >= rect.left() - 1.0 && x <= rect.right() + 1.0 {
                painter.line_segment([Pos2::new(x, wave_top), Pos2::new(x, rect.bottom())], Stroke::new(2.0_f32, col));
                let handle = Rect::from_center_size(Pos2::new(x, wave_top + 7.0), Vec2::new(14.0, 14.0));
                painter.rect_filled(handle, 3.0, col);
                painter.text(handle.center(), egui::Align2::CENTER_CENTER, txt, egui::FontId::proportional(10.0), Color32::BLACK);
            }
        }

        // Cursor y cabezal de reproducción
        let xc = t_to_x(self.cursor);
        if xc >= rect.left() && xc <= rect.right() {
            painter.line_segment([Pos2::new(xc, wave_top), Pos2::new(xc, rect.bottom())], Stroke::new(1.0_f32, Color32::from_rgb(240, 200, 60)));
        }
        if let Some(pos) = self.player.as_ref().and_then(|p| p.position()) {
            let xp = t_to_x(pos);
            if xp >= rect.left() && xp <= rect.right() {
                painter.line_segment([Pos2::new(xp, rect.top()), Pos2::new(xp, rect.bottom())], Stroke::new(2.0_f32, Color32::WHITE));
            }
            ui.ctx().request_repaint();
        }
    }
}

fn time_dv(v: &mut f64, max: f64) -> egui::DragValue<'_> {
    egui::DragValue::new(v)
        .speed(0.02)
        .range(0.0..=max)
        .custom_formatter(|n, _| fmt_time(n))
        .custom_parser(parse_time)
        .min_decimals(3)
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);

        // Arrastrar y soltar un archivo de audio → editor de corte
        let dropped: Option<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone()));
        if let Some(p) = dropped {
            self.load_audio(ctx, p);
            self.tab = Tab::Cut;
        }

        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Download, RichText::new("⬇ Descargar").size(15.0));
                ui.selectable_value(&mut self.tab, Tab::Cut, RichText::new("✂ Cortar").size(15.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    egui::widgets::global_theme_preference_switch(ui);
                });
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Download => self.ui_download(ui),
            Tab::Cut => self.ui_cut(ui),
        });
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.cfg);
    }
}
