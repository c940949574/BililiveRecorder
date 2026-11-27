mod api;
mod series;

use api::{ApiClient, Room};
use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use series::{Sample, Series};
use std::collections::HashMap;
use std::time::{Duration, Instant};

struct Config {
    base_url: String,
    username: String,
    password: String,
    poll_ms: u64,
    max_points: usize,
    ma_short: usize,
    ma_long: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:2356".into(),
            username: String::new(),
            password: String::new(),
            poll_ms: 2000,
            max_points: 180,
            ma_short: 3,
            ma_long: 10,
        }
    }
}

struct App {
    cfg: Config,
    client: ApiClient,
    rooms: Vec<Room>,
    series: HashMap<String, Series>,
    last_poll: Instant,
    status: String,
    add_room_input: String,
    show_settings: bool,
    show_logs: bool,
    logs: Vec<api::LogEntry>,
    elapsed: f64,
}

impl App {
    fn new() -> Self {
        let cfg = Config::default();
        let client = ApiClient::new(cfg.base_url.clone(), None, None);
        Self {
            cfg, client, rooms: Vec::new(), series: HashMap::new(),
            last_poll: Instant::now() - Duration::from_secs(10),
            status: "就绪".into(), add_room_input: String::new(),
            show_settings: false, show_logs: false, logs: Vec::new(),
            elapsed: 0.0,
        }
    }

    fn reconnect(&mut self) {
        let u = if self.cfg.username.is_empty() { None } else { Some(self.cfg.username.clone()) };
        let p = if self.cfg.password.is_empty() { None } else { Some(self.cfg.password.clone()) };
        self.client = ApiClient::new(self.cfg.base_url.clone(), u, p);
        self.status = "已重连".into();
    }

    fn poll(&mut self) {
        match self.client.get_rooms() {
            Ok(rooms) => {
                self.rooms = rooms;
                self.status = format!("已连接 · {} 个房间", self.rooms.len());
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
            Err(_) => self.status = format!("未连接到 {} — 请先启动录播姬 CLI 并开启 HTTP 服务", self.cfg.base_url),
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
        self.status = match result {
            Ok(()) => format!("{} 成功", action),
            Err(e) => format!("{} 失败: {}", action, e),
        };
        self.last_poll = Instant::now() - Duration::from_secs(10);
    }

    fn ma_last(&self, s: &Series, n: usize) -> f64 {
        s.last_n_avg(n)
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_poll).as_millis() as u64;
        self.elapsed += ctx.input(|i| i.unstable_dt as f64);
        if dt >= self.cfg.poll_ms {
            self.last_poll = now;
            self.poll();
        }

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("录播姬 · Rust GUI");
                ui.separator();
                ui.label(&self.status);
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
                            Ok(()) => { self.status = format!("已添加房间 {}", id); self.add_room_input.clear(); }
                            Err(e) => self.status = format!("添加失败: {}", e),
                        }
                    } else { self.status = "房间号无效".into(); }
                    self.last_poll = Instant::now() - Duration::from_secs(10);
                }
            });
            ui.separator();

            egui::ScrollArea::vertical().show(ui, |ui| {
                let rooms = self.rooms.clone();
                for room in &rooms {
                    self.render_room_card(ui, room);
                    ui.add_space(6.0);
                }
                if self.rooms.is_empty() {
                    ui.label("未连接或无房间。检查设置中的 API 地址。");
                }
            });
        });

        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

impl App {
    fn render_room_card(&mut self, ui: &mut egui::Ui, room: &Room) {
        let key = room.key();
        let card = egui::Frame::new()
            .fill(ui.style().visuals.panel_fill)
            .stroke(ui.style().visuals.widgets.noninteractive.bg_stroke)
            .corner_radius(8.0)
            .inner_margin(egui::Margin::symmetric(10, 8));

        card.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong(room.display_name());
                if room.recording {
                    ui.label(egui::RichText::new("● 录制中").color(egui::Color32::from_rgb(226, 75, 74)));
                } else if room.streaming {
                    ui.label(egui::RichText::new("○ 直播中").color(egui::Color32::from_rgb(239, 159, 39)));
                }
            });
            ui.label(egui::RichText::new(room.display_title()).small());
            ui.label(egui::RichText::new(format!("{} · UID {} · 房间 {}", room.display_area(), room.uid, room.room_id)).small().weak());

            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("{:.2} Mbps", room.io_stats.network_mbps)).color(egui::Color32::from_rgb(226, 75, 74)).strong());
                ui.label(egui::RichText::new(format!("写入 {:.2} MB/s", room.io_stats.disk_mbps)).small().weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("删").clicked() { self.act(&key, "delete"); }
                    if ui.small_button("刷").clicked() { self.act(&key, "refresh"); }
                    if ui.small_button("切").clicked() { self.act(&key, "split"); }
                    if ui.small_button("录").clicked() { self.act(&key, "start"); }
                    if ui.small_button("停").clicked() { self.act(&key, "stop"); }
                });
            });

            if room.recording {
                let s = self.series.get(&key).cloned().unwrap_or_else(|| Series::new(self.cfg.max_points));
                let plot_id = egui::Id::new(format!("spark_{}", key));
                let plot = Plot::new(plot_id)
                    .height(48.0)
                    .show_axes(false)
                    .show_grid(false)
                    .allow_zoom(false)
                    .allow_drag(false)
                    .allow_scroll(false);
                let response = plot.show(ui, |plot_ui| {
                    plot_ui.line(Line::new("码率", s.points()).color(egui::Color32::from_rgb(226, 75, 74)).width(2.0));
                    if s.len() >= self.cfg.ma_long {
                        plot_ui.line(Line::new(format!("MA{}", self.cfg.ma_short), s.ma(self.cfg.ma_short)).color(egui::Color32::from_rgb(239, 159, 39)).width(1.5));
                        plot_ui.line(Line::new(format!("MA{}", self.cfg.ma_long), s.ma(self.cfg.ma_long)).color(egui::Color32::from_rgb(85, 183, 235)).width(1.5));
                    }
                }).response;
                if response.hovered() {
                    let (avg, max, min) = s.stats();
                    let tip_pos = response.hover_pos().unwrap_or(response.rect.center());
                    egui::show_tooltip_at(ui.ctx(), ui.layer_id(), plot_id.with("tip"), tip_pos, |ui| {
                        ui.label(egui::RichText::new("码率详情").strong());
                        ui.label(format!("当前 {:.2} Mbps", room.io_stats.network_mbps));
                        ui.label(format!("均值 {:.2} / 峰值 {:.2} / 谷值 {:.2}", avg, max, min));
                        ui.label(format!("MA{} {:.2} · MA{} {:.2}", self.cfg.ma_short, self.ma_last(&s, self.cfg.ma_short), self.cfg.ma_long, self.ma_last(&s, self.cfg.ma_long)));
                        ui.label(format!("写入 {:.2} MB/s · 速度比 {:.2}", room.io_stats.disk_mbps, room.recording_stats.duration_ratio));
                        ui.label(format!("总写入 {:.1} MB", room.recording_stats.total_output_bytes as f64 / 1048576.0));
                    });
                }
            }
        });
    }

    fn render_settings(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings;
        egui::Window::new("设置").open(&mut open).resizable(false).show(ctx, |ui| {
            ui.label("API 地址");
            ui.text_edit_singleline(&mut self.cfg.base_url);
            ui.label("用户名（可选）");
            ui.text_edit_singleline(&mut self.cfg.username);
            ui.label("密码（可选）");
            ui.add(egui::TextEdit::singleline(&mut self.cfg.password).password(true));
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
            if ui.button("重连").clicked() { self.reconnect(); }
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

fn install_chinese_font(ctx: &egui::Context) {
    let candidates = [
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
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 720.0])
            .with_title("录播姬 Rust GUI"),
        ..Default::default()
    };
    eframe::run_native(
        "录播姬 Rust GUI",
        options,
        Box::new(|cc| {
            install_chinese_font(&cc.egui_ctx);
            Ok(Box::new(App::new()))
        }),
    )
}
