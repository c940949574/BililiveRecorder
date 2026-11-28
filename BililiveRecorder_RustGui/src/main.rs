mod api;
mod launcher;
mod series;

use api::{ApiClient, Room};
use eframe::egui;
use egui_plot::{Line, Plot, Polygon};
use launcher::Launcher;
use serde::{Deserialize, Serialize};
use series::{Sample, Series};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const COLOR_UP: egui::Color32 = egui::Color32::from_rgb(226, 75, 74);
const COLOR_UP_SOFT: egui::Color32 = egui::Color32::from_rgb(240, 149, 149);
const COLOR_SHORT: egui::Color32 = egui::Color32::from_rgb(239, 159, 39);
const COLOR_LONG: egui::Color32 = egui::Color32::from_rgb(85, 183, 235);
const COLOR_OK: egui::Color32 = egui::Color32::from_rgb(96, 190, 120);
const COLOR_WARN: egui::Color32 = egui::Color32::from_rgb(239, 159, 39);
const COLOR_BAD: egui::Color32 = egui::Color32::from_rgb(226, 75, 74);
const COLOR_MUTED: egui::Color32 = egui::Color32::from_rgb(0x8A, 0x8D, 0x93);
const COLOR_TEXT: egui::Color32 = egui::Color32::from_rgb(0xF2, 0xF3, 0xF5);
const BG_APP: egui::Color32 = egui::Color32::from_rgb(0x17, 0x18, 0x1C);
const BG_CARD: egui::Color32 = egui::Color32::from_rgb(0x1E, 0x20, 0x25);
const BG_HOVER: egui::Color32 = egui::Color32::from_rgb(0x2A, 0x2D, 0x33);
const BG_INPUT: egui::Color32 = egui::Color32::from_rgb(0x10, 0x11, 0x14);
const COLOR_LINE: egui::Color32 = egui::Color32::from_rgba_premultiplied(26, 26, 26, 26);
const DEFAULT_PORT: u16 = 2356;

/// 持久化到 data/gui-settings.json 的设置
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Config {
    base_url: String,
    username: String,
    password: String,
    poll_ms: u64,
    max_points: usize,
    ma_short: usize,
    ma_long: usize,
    auto_launch: bool,
    /// 播放器程序路径（ffplay/mpv/PotPlayer 等）；空 = 系统默认关联程序
    player_path: String,
    /// 录制文件保存目录；空 = 程序同目录下的「录播文件」
    work_dir: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base_url: format!("http://localhost:{DEFAULT_PORT}"),
            username: String::new(),
            password: String::new(),
            poll_ms: 2000,
            max_points: 180,
            ma_short: 3,
            ma_long: 10,
            auto_launch: true,
            player_path: String::new(),
            work_dir: String::new(),
        }
    }
}

struct App {
    cfg: Config,
    client: ApiClient,
    launcher: Launcher,
    exe_dir: PathBuf,
    work_dir_str: String,
    rooms: Vec<Room>,
    series: HashMap<String, Series>,
    last_poll: Instant,
    /// 最近一次拉取房间列表失败的原因；成功后清空
    poll_error: Option<String>,
    status: String,
    add_room_input: String,
    show_settings: bool,
    show_logs: bool,
    logs: Vec<api::LogEntry>,
    elapsed: f64,
}

impl App {
    fn new() -> Self {
        let cfg = Self::load_settings();
        let client = ApiClient::new(cfg.base_url.clone(), None, None);
        let exe_dir = std::env::current_exe().ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        let launcher = Launcher::new(cfg.base_url.clone(), api::build_auth_header(&cfg.username, &cfg.password));
        let work_dir_str = cfg.work_dir.clone();
        Self {
            cfg, client, launcher, exe_dir, work_dir_str,
            rooms: Vec::new(), series: HashMap::new(), poll_error: None,
            last_poll: Instant::now() - Duration::from_secs(10),
            status: String::new(), add_room_input: String::new(),
            show_settings: false, show_logs: false, logs: Vec::new(),
            elapsed: 0.0,
        }
    }

    // ---------- 路径 ----------

    fn data_dir(&self) -> PathBuf { self.exe_dir.join("data") }
    fn settings_file(&self) -> PathBuf { self.data_dir().join("gui-settings.json") }
    fn core_config_file(&self) -> PathBuf { self.data_dir().join("config.json") }
    fn cli_log_file(&self) -> PathBuf { self.data_dir().join("cli.log") }
    fn gui_log_file(&self) -> PathBuf { self.data_dir().join("gui.log") }

    /// 把 GUI 自身的错误追加到 data/gui.log（写不进去也不影响运行）
    fn gui_log(&self, msg: &str) {
        use std::io::Write;
        let file = self.gui_log_file();
        let res = std::fs::create_dir_all(self.data_dir())
            .and_then(|_| OpenOptions::new().create(true).append(true).open(&file))
            .and_then(|mut f| writeln!(f, "[{}] {msg}", launcher::now_string()));
        if let Err(e) = res {
            eprintln!("写 {} 失败：{e}", file.display());
        }
    }

    /// 录制文件保存目录（相对路径按程序目录解析）
    fn work_dir(&self) -> PathBuf {
        let raw = self.cfg.work_dir.trim_matches('"').trim();
        if raw.is_empty() { return self.exe_dir.join("录播文件"); }
        let p = PathBuf::from(raw);
        if p.is_absolute() { p } else { self.exe_dir.join(p) }
    }

    fn load_settings() -> Config {
        let exe_dir = std::env::current_exe().ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        let file = exe_dir.join("data").join("gui-settings.json");
        std::fs::read_to_string(file)
            .ok()
            .and_then(|s| serde_json::from_str::<Config>(&s).ok())
            .unwrap_or_default()
    }

    fn save_settings(&self) -> Result<(), String> {
        let dir = self.data_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}：{e}", dir.display()))?;
        let mut cfg = self.cfg.clone();
        cfg.work_dir = self.work_dir_str.trim_matches('"').trim().to_string();
        let json = serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
        std::fs::write(self.settings_file(), json).map_err(|e| format!("{}：{e}", self.settings_file().display()))
    }

    // ---------- 录制核心 ----------

    fn bootstrap(&mut self) {
        let cfg_port = port_of(&self.cfg.base_url).unwrap_or(DEFAULT_PORT);
        let cli = launcher::find_cli(&self.exe_dir);
        let config_file = self.core_config_file();
        let work_dir = self.work_dir();
        let log_file = self.cli_log_file();
        match self.launcher.bootstrap(
            self.cfg.auto_launch, cli, cfg_port, &config_file, &work_dir, &log_file,
        ) {
            Ok(port) if port != 0 && port != cfg_port => {
                // 端口被占用，录制核心换了端口
                self.cfg.base_url = format!("http://localhost:{port}");
                self.reconnect();
            }
            Ok(_) => {}
            Err(e) => self.status = e,
        }
    }

    fn reconnect(&mut self) {
        let u = if self.cfg.username.is_empty() { None } else { Some(self.cfg.username.clone()) };
        let p = if self.cfg.password.is_empty() { None } else { Some(self.cfg.password.clone()) };
        self.client = ApiClient::new(self.cfg.base_url.clone(), u, p);
        let auth = api::build_auth_header(&self.cfg.username, &self.cfg.password);
        self.launcher.set_endpoint(&self.cfg.base_url, auth);
        self.status = "已重连".into();
        self.last_poll = Instant::now() - Duration::from_secs(10);
    }

    fn poll(&mut self) {
        match self.client.get_rooms() {
            Ok(rooms) => {
                if self.poll_error.is_some() { self.poll_error = None; }
                self.rooms = rooms;
                for r in &self.rooms {
                    let s = self.series.entry(r.key()).or_insert_with(|| Series::new(self.cfg.max_points));
                    if r.recording {
                        s.push(Sample {
                            time: self.elapsed,
                            mbps: r.io_stats.network_mbps,
                            disk_mbps: r.io_stats.disk_mbps,
                        });
                    } else {
                        s.clear();
                    }
                }
                let keys: Vec<String> = self.series.keys().cloned().collect();
                for k in keys {
                    if !self.rooms.iter().any(|r| r.key() == k) { self.series.remove(&k); }
                }
            }
            Err(e) => {
                self.rooms.clear();
                // 同一个错误只记一次，避免日志刷屏
                if self.poll_error.as_deref() != Some(e.as_str()) {
                    self.gui_log(&format!("拉取房间列表失败：{e}"));
                }
                self.poll_error = Some(e);
            }
        }
        if self.show_logs {
            if let Ok(logs) = self.client.fetch_logs() { self.logs = logs; }
        }
    }

    fn act(&mut self, key: &str, action: &str) {
        let result = match action {
            "start" => self.client.start(key),
            "stop" => self.client.stop(key),
            "split" => self.client.split(key),
            "refresh" => self.client.refresh(key),
            "delete" => self.client.delete(key),
            _ => Ok(()),
        };
        let label = match action {
            "start" => "开始录制", "stop" => "停止录制", "split" => "切片",
            "refresh" => "刷新", "delete" => "删除房间", _ => action,
        };
        self.status = match result {
            Ok(()) => format!("{label}成功"),
            Err(e) => format!("{label}失败：{e}"),
        };
        self.last_poll = Instant::now() - Duration::from_secs(10);
    }

    fn ma_last(&self, s: &Series, n: usize) -> f64 {
        s.last_n_avg(n)
    }

    // ---------- 播放正在录制的文件 ----------

    /// 在保存目录里找该房间正在写入的 FLV（30 秒内有改动）；没有则取最新的一个
    fn find_recording_file(&self, room: &Room) -> Option<PathBuf> {
        let root = self.work_dir();
        let prefixes = [format!("{}-", room.room_id), format!("{}-", room.short_id)];
        let exact = [room.room_id.to_string(), room.short_id.to_string()];
        let now = std::time::SystemTime::now();
        let mut found: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
        for top in std::fs::read_dir(&root).ok()?.flatten() {
            let path = top.path();
            if !path.is_dir() { continue; }
            let name = top.file_name().to_string_lossy().to_string();
            if !(prefixes.iter().any(|p| name.starts_with(p)) || exact.contains(&name)) { continue; }
            for entry in std::fs::read_dir(&path).ok()?.flatten() {
                let f = entry.path();
                if f.extension().map(|e| e.eq_ignore_ascii_case("flv")).unwrap_or(false) {
                    if let Ok(meta) = entry.metadata() {
                        if let Ok(mtime) = meta.modified() { found.push((mtime, f)); }
                    }
                }
            }
        }
        found.sort_by(|a, b| b.0.cmp(&a.0));
        found.iter()
            .find(|(mtime, _)| now.duration_since(*mtime).map(|d| d.as_secs() < 30).unwrap_or(false))
            .or_else(|| found.first())
            .map(|(_, p)| p.clone())
    }

    fn play_current(&mut self, room: &Room) {
        match self.find_recording_file(room) {
            Some(path) => {
                let shown = path.display().to_string();
                let title = format!("{} · 正在录制", room.display_name());
                let choice = self.resolve_player();
                match launcher::launch_player(&choice, &path, &title) {
                    Ok(()) => self.status = format!("正在播放：{shown}"),
                    Err(e) => self.status = e,
                }
            }
            None => self.status = format!("保存目录里没有房间 {} 的录制文件：{}", room.room_id, self.work_dir().display()),
        }
    }

    /// 播放器解析顺序：设置里填的路径 → 程序同目录的 ffplay.exe → 系统默认关联
    fn resolve_player(&self) -> launcher::PlayerChoice {
        let p = self.cfg.player_path.trim_matches('"').trim();
        if !p.is_empty() {
            let path = PathBuf::from(p);
            if path.is_file() {
                return launcher::PlayerChoice::for_path(path);
            }
        }
        let local = self.exe_dir.join("ffplay.exe");
        if local.is_file() {
            return launcher::PlayerChoice::Ffplay(local);
        }
        launcher::PlayerChoice::System
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = Instant::now();
        self.elapsed += ctx.input(|i| i.unstable_dt as f64);

        // 录制核心：首次检查 → 自动拉起 → 等待就绪
        if self.launcher.state == launcher::CoreState::Pending { self.bootstrap(); }
        self.launcher.tick();

        let dt = now.duration_since(self.last_poll).as_millis() as u64;
        if dt >= self.cfg.poll_ms {
            self.last_poll = now;
            self.poll();
        }

        let (core_text, core_color) = match self.launcher.state {
            launcher::CoreState::Ready => (self.launcher.state_text(), COLOR_OK),
            launcher::CoreState::Starting | launcher::CoreState::Pending => (self.launcher.state_text(), COLOR_WARN),
            _ => (self.launcher.state_text(), COLOR_BAD),
        };
        let status = self.status.clone();
        let rooms_len = self.rooms.len();

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("录播姬");
                ui.separator();
                ui.colored_label(core_color, &core_text);
                if rooms_len > 0 { ui.label(format!("{rooms_len} 个房间")); }
                if !status.is_empty() {
                    ui.separator();
                    ui.label(weak_text(&status));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("设置").clicked() { self.show_settings ^= true; }
                    if ui.button("日志").clicked() {
                        self.show_logs ^= true;
                        if self.show_logs {
                            if let Ok(logs) = self.client.fetch_logs() { self.logs = logs; }
                        }
                    }
                });
            });
        });

        if self.show_settings { self.render_settings(ctx); }
        if self.show_logs { self.render_logs(ctx); }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("添加房间:");
                ui.text_edit_singleline(&mut self.add_room_input);
                if ui.button("添加").clicked() {
                    if let Ok(id) = self.add_room_input.trim().parse::<i64>() {
                        match self.client.add_room(id, true) {
                            Ok(()) => { self.status = format!("已添加房间 {id}"); self.add_room_input.clear(); }
                            Err(e) => self.status = format!("添加失败：{e}"),
                        }
                    } else { self.status = "房间号无效".into(); }
                    self.last_poll = Instant::now() - Duration::from_secs(10);
                }
            });
            ui.separator();

            egui::ScrollArea::vertical().show(ui, |ui| {
                let rooms = self.rooms.clone();
                // 自适应网格：按可用宽度决定列数（每格最小 360px，最多 4 列）。
                // 用 Grid 而不是手动 horizontal 排布，保证同行卡片顶部对齐。
                let avail = ui.available_width();
                let gap = 10.0;
                let min_card = 360.0;
                let cols = ((avail / min_card).floor() as usize).clamp(1, 4);
                let col_w = ((avail - gap * (cols as f32 - 1.0)) / cols as f32).max(240.0);
                egui::Grid::new("room_grid")
                    .num_columns(cols)
                    .min_col_width(col_w)
                    .spacing(egui::vec2(gap, 6.0))
                    .show(ui, |ui| {
                        for (i, room) in rooms.iter().enumerate() {
                            ui.allocate_ui_with_layout(
                                egui::vec2(col_w, 0.0),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| self.render_room_card(ui, room),
                            );
                            if (i + 1) % cols == 0 {
                                ui.end_row();
                            }
                        }
                        ui.end_row();
                    });
                if rooms.is_empty() {
                    self.render_empty_hint(ui);
                }
            });
        });

        ctx.request_repaint_after(Duration::from_millis(300));
    }
}

/// 富文本小工具：弱化文字
fn weak_text(s: &str) -> egui::RichText {
    egui::RichText::new(s.to_string()).weak()
}

/// 有限数值保留 n 位小数；NaN / Infinity 显示为 "—"
fn fmt_finite(x: f64, digits: usize) -> String {
    if x.is_finite() { format!("{x:.digits$}") } else { "—".to_string() }
}

impl App {
    fn render_empty_hint(&mut self, ui: &mut egui::Ui) {
        use launcher::CoreState;
        let data_dir = self.data_dir();
        let can_retry = matches!(self.launcher.state, CoreState::NotFound(_) | CoreState::Failed(_));
        ui.add_space(12.0);
        match &self.launcher.state {
            CoreState::NotFound(msg) | CoreState::Failed(msg) => {
                ui.colored_label(COLOR_BAD, msg);
                ui.label(weak_text(&format!("录制核心会读取：{}", self.core_config_file().display())));
                ui.label(weak_text(&format!("录制文件保存到：{}", self.work_dir().display())));
                if self.launcher.log_path.is_some() {
                    ui.label(weak_text(&format!("运行日志：{}", self.cli_log_file().display())));
                }
            }
            CoreState::Disabled => {
                ui.label("未连接录制核心。到「设置」里开启自动启动，或手动运行 CLI：");
                ui.label(egui::RichText::new(format!(
                    "BililiveRecorder.Cli run --bind {} <工作目录>",
                    port_of(&self.cfg.base_url).unwrap_or(DEFAULT_PORT)
                )).monospace().small());
            }
            _ => {
                if let Some(e) = &self.poll_error {
                    ui.colored_label(COLOR_BAD, format!("拉取房间列表失败：{e}"));
                    ui.label(weak_text(&format!("详见 {}", self.gui_log_file().display())));
                } else {
                    ui.label("还没有房间，在上方输入房间号后点「添加」。");
                }
            }
        }
        if can_retry {
            ui.horizontal(|ui| {
                if ui.button("重试").clicked() { self.launcher.reset(); }
                if ui.button("打开数据目录").clicked() {
                    let _ = std::fs::create_dir_all(&data_dir);
                    match launcher::open_with_default(&data_dir) {
                        Ok(()) => self.status = format!("已打开 {}", data_dir.display()),
                        Err(e) => self.status = e,
                    }
                }
            });
        }
    }

    fn render_room_card(&mut self, ui: &mut egui::Ui, room: &Room) {
        let key = room.key();
        let card = egui::Frame::new()
            .fill(BG_CARD)
            .stroke(egui::Stroke::new(0.5, COLOR_LINE))
            .corner_radius(10.0)
            .inner_margin(egui::Margin::symmetric(14, 10));

        card.show(ui, |ui| {
            // 标题行：名字 + 状态胶囊 + 播放 + 更多菜单
            ui.horizontal(|ui| {
                ui.strong(egui::RichText::new(room.display_name()).size(15.0));
                self.status_pill(ui, room);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.menu_button("…", |ui| {
                        if ui.button("刷新房间信息").clicked() { self.act(&key, "refresh"); ui.close_menu(); }
                        if ui.button("切片").clicked() { self.act(&key, "split"); ui.close_menu(); }
                        ui.separator();
                        if ui.button(egui::RichText::new("删除房间").color(COLOR_BAD)).clicked() { self.act(&key, "delete"); ui.close_menu(); }
                    });
                    if ui.add_enabled(room.recording, egui::Button::new("⏵ 播放")).clicked() {
                        let r = room.clone();
                        self.play_current(&r);
                    }
                });
            });
            ui.label(egui::RichText::new(room.display_title()).small().color(COLOR_MUTED));
            ui.label(egui::RichText::new(format!("{} · 房间 {}", room.display_area(), room.room_id)).small().weak());

            // 码率行：大数字 + 单位，右侧录制控制
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                let mbps = room.io_stats.network_mbps;
                ui.add(egui::Label::new(
                    egui::RichText::new(if mbps.is_finite() { format!("{mbps:.2}") } else { "—".to_string() })
                        .size(22.0).strong().color(if room.recording { COLOR_UP_SOFT } else { COLOR_MUTED }),
                ));
                ui.label(egui::RichText::new("Mbps").small().color(COLOR_MUTED));
                ui.label(egui::RichText::new(format!("写入 {:.2} MB/s", room.io_stats.disk_mbps)).small().weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if room.recording {
                        if ui.small_button("停止录制").clicked() { self.act(&key, "stop"); }
                    } else if ui.small_button("开始录制").clicked() { self.act(&key, "start"); }
                });
            });

            // 录制中：码率折线（面积填充 + MA 均线）
            if room.recording {
                let s = self.series.get(&key).cloned().unwrap_or_else(|| Series::new(self.cfg.max_points));
                let plot_id = egui::Id::new(format!("spark_{}", key));
                let plot = Plot::new(plot_id)
                    .height(56.0)
                    .show_axes(false)
                    .show_grid(false)
                    .allow_zoom(false)
                    .allow_drag(false)
                    .allow_scroll(false);
                let response = plot.show(ui, |plot_ui| {
                    let area = s.area_points();
                    if !area.is_empty() {
                        plot_ui.polygon(
                            Polygon::new("面积", area)
                                .fill_color(egui::Color32::from_rgba_unmultiplied(226, 75, 74, 36))
                                .stroke(egui::Stroke::NONE),
                        );
                    }
                    plot_ui.line(Line::new("码率", s.points()).color(COLOR_UP).width(1.8_f32));
                    if s.len() >= self.cfg.ma_long {
                        plot_ui.line(Line::new(format!("MA{}", self.cfg.ma_short), s.ma(self.cfg.ma_short)).color(COLOR_SHORT).width(1.2_f32));
                        plot_ui.line(Line::new(format!("MA{}", self.cfg.ma_long), s.ma(self.cfg.ma_long)).color(COLOR_LONG).width(1.2_f32));
                    }
                }).response;
                let (avg, max, _min) = s.stats();
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("━ 码率").small().color(COLOR_UP));
                    ui.label(egui::RichText::new(format!("╌ MA{}", self.cfg.ma_long)).small().color(COLOR_LONG));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(format!(
                            "均值 {:.2} · 峰值 {:.2} · 速度比 {}",
                            avg, max, fmt_finite(room.recording_stats.duration_ratio, 2),
                        )).small().weak());
                    });
                });
                if response.hovered() {
                    let tip_pos = response.hover_pos().unwrap_or(response.rect.center());
                    let (short, long) = (self.cfg.ma_short, self.cfg.ma_long);
                    let (ma_s, ma_l) = (self.ma_last(&s, short), self.ma_last(&s, long));
                    egui::show_tooltip_at(ui.ctx(), ui.layer_id(), plot_id.with("tip"), tip_pos, |ui| {
                        ui.label(egui::RichText::new("码率详情").strong());
                        ui.label(format!("当前 {:.2} Mbps", room.io_stats.network_mbps));
                        ui.label(format!("均值 {:.2} / 峰值 {:.2} / 谷值 {:.2}", avg, max, _min));
                        ui.colored_label(COLOR_SHORT, format!("MA{short} {ma_s:.2}"));
                        ui.colored_label(COLOR_LONG, format!("MA{long} {ma_l:.2}"));
                        ui.label(format!("写入 {:.2} MB/s · 总写入 {:.1} MB",
                            room.io_stats.disk_mbps, room.recording_stats.total_output_bytes as f64 / 1048576.0));
                    });
                }
            }
        });
    }

    /// 状态胶囊：录制中 / 直播中 / 空闲
    fn status_pill(&self, ui: &mut egui::Ui, room: &Room) {
        let (text, color) = if room.recording {
            ("● 录制中", COLOR_UP)
        } else if room.streaming {
            ("○ 直播中", COLOR_WARN)
        } else {
            ("空闲", COLOR_MUTED)
        };
        egui::Frame::new()
            .fill(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 14))
            .stroke(egui::Stroke::new(0.5, color.gamma_multiply(0.55)))
            .corner_radius(99.0)
            .inner_margin(egui::Margin::symmetric(8, 2))
            .show(ui, |ui| {
                ui.colored_label(color, egui::RichText::new(text).size(11.0));
            });
    }

    fn render_settings(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings;
        let work_dir = self.work_dir();
        let data_dir = self.data_dir();
        egui::Window::new("设置").open(&mut open).resizable(false).show(ctx, |ui| {
            egui::ScrollArea::vertical().max_height(520.0).show(ui, |ui| {
                ui.label("连接");
                ui.label("API 地址");
                ui.text_edit_singleline(&mut self.cfg.base_url);
                ui.horizontal(|ui| {
                    ui.label("用户名");
                    ui.text_edit_singleline(&mut self.cfg.username);
                    ui.label("密码");
                    ui.add(egui::TextEdit::singleline(&mut self.cfg.password).password(true));
                });

                ui.separator();
                ui.label("录制核心");
                ui.checkbox(&mut self.cfg.auto_launch, "连不上时自动启动 cli 文件夹里的录制核心");
                ui.label("录制文件保存目录（重启后生效）");
                ui.text_edit_singleline(&mut self.work_dir_str);
                ui.horizontal(|ui| {
                    if ui.small_button("打开保存目录").clicked() {
                        let _ = std::fs::create_dir_all(&work_dir);
                        match launcher::open_with_default(&work_dir) {
                            Ok(()) => self.status = format!("已打开 {}", work_dir.display()),
                            Err(e) => self.status = e,
                        }
                    }
                    if ui.small_button("打开数据目录").clicked() {
                        let _ = std::fs::create_dir_all(&data_dir);
                        match launcher::open_with_default(&data_dir) {
                            Ok(()) => self.status = format!("已打开 {}", data_dir.display()),
                            Err(e) => self.status = e,
                        }
                    }
                    if self.launcher.pid().is_some() && ui.small_button("重启录制核心").clicked() {
                        self.launcher.stop();
                        self.launcher.reset();
                    }
                });

                ui.separator();
                ui.label("折线图");
                ui.label("轮询间隔（毫秒）");
                ui.add(egui::Slider::new(&mut self.cfg.poll_ms, 500..=10000));
                ui.label("折线图最大点数");
                ui.add(egui::Slider::new(&mut self.cfg.max_points, 30..=600));
                ui.horizontal(|ui| {
                    ui.label("短期均线");
                    ui.add(egui::DragValue::new(&mut self.cfg.ma_short).range(2..=30));
                    ui.label("长期均线");
                    ui.add(egui::DragValue::new(&mut self.cfg.ma_long).range(3..=60));
                });

                ui.separator();
                ui.label("播放");
                ui.label("播放器程序路径（ffplay/mpv/PotPlayer 等，留空用系统默认；ffplay 可直接看正在写的文件）");
                ui.text_edit_singleline(&mut self.cfg.player_path);

                ui.separator();
                if ui.button("保存并重连").clicked() {
                    self.cfg.work_dir = self.work_dir_str.clone();
                    match self.save_settings() {
                        Ok(()) => self.reconnect(),
                        Err(e) => self.status = e,
                    }
                }
            });
        });
        self.show_settings = open;
    }

    fn render_logs(&mut self, ctx: &egui::Context) {
        let mut open = self.show_logs;
        egui::Window::new("日志").open(&mut open).resizable(true).default_size(egui::vec2(700.0, 400.0)).show(ctx, |ui| {
            if ui.button("刷新").clicked() {
                if let Ok(logs) = self.client.fetch_logs() { self.logs = logs; }
            }
            egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                for log in self.logs.iter().rev().take(150) {
                    ui.label(egui::RichText::new(&log.message).small().monospace());
                }
            });
        });
        self.show_logs = open;
    }
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = self.save_settings();
    }
}

fn port_of(url: &str) -> Option<u16> {
    url.rsplit(':').next()?.trim_end_matches('/').parse::<u16>().ok()
}

/// 暗色主题：黑底红光的科技感配色，所有控件的底色/描边/文字统一走这套
fn apply_dark_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    let v = &mut style.visuals;
    v.dark_mode = true;
    v.panel_fill = BG_APP;
    v.window_fill = BG_CARD;
    v.extreme_bg_color = BG_INPUT;
    v.faint_bg_color = BG_CARD;
    v.override_text_color = Some(COLOR_TEXT);
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(0.5, COLOR_LINE);
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, COLOR_TEXT);
    v.widgets.inactive.bg_fill = BG_CARD;
    v.widgets.inactive.weak_bg_fill = BG_CARD;
    v.widgets.inactive.bg_stroke = egui::Stroke::new(0.5, COLOR_LINE);
    v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, COLOR_TEXT);
    v.widgets.hovered.bg_fill = BG_HOVER;
    v.widgets.hovered.weak_bg_fill = BG_HOVER;
    v.widgets.hovered.bg_stroke = egui::Stroke::new(0.5, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 60));
    v.widgets.active.bg_fill = BG_HOVER;
    v.widgets.active.weak_bg_fill = BG_HOVER;
    v.selection.bg_fill = egui::Color32::from_rgba_unmultiplied(226, 75, 74, 90);
    v.selection.stroke = egui::Stroke::new(1.0, COLOR_UP_SOFT);
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 4.0);
    // 输入框：深底 + 明显描边，避免在暗色卡片上隐形
    style.visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgba_premultiplied(60, 62, 68, 160));
    ctx.set_style(style);
}

fn install_chinese_font(ctx: &egui::Context) {    let candidates = [
        r"C:\Users\94094\AppData\Roaming\kingsoft\office6\docerFonts\MiSans Bold.ttf",
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Users\94094\AppData\Local\Microsoft\Windows\Fonts\MiSans VF.ttf",
    ];
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert(
                "chinese".to_owned(),
                egui::FontData::from_owned(bytes).into(),
            );
            fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "chinese".to_owned());
            fonts.families.entry(egui::FontFamily::Monospace).or_default().insert(0, "chinese".to_owned());
            ctx.set_fonts(fonts);
            return;
        }
    }
}

fn main() -> eframe::Result<()> {
    if std::env::args().any(|a| a == "-h" || a == "--help") {
        println!("录播姬 Rust GUI");
        println!("  --minimized   启动后最小化到任务栏（录制核心照常在后台运行）");
        println!("环境变量：BLR_CLI_PATH 指定录制核心 CLI 的路径（默认在程序同目录的 cli 文件夹里找）");
        return Ok(());
    }
    let minimized = std::env::args().any(|a| a == "--minimized");
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([1100.0, 720.0])
        .with_title("录播姬 Rust GUI");
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native(
        "录播姬 Rust GUI",
        options,
        Box::new(|cc| {
            install_chinese_font(&cc.egui_ctx);
            apply_dark_theme(&cc.egui_ctx);
            if minimized {
                cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
            Ok(Box::new(App::new()))
        }),
    )
}
