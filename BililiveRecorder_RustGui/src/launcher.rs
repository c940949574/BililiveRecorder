//! 录制核心（C# CLI）的自动拉起与生命周期管理。
//!
//! 职责：
//! 1. 在 exe 同目录下查找 CLI（`cli/` 目录或环境变量 `BLR_CLI_PATH`）
//! 2. 连不上 API 时自动启动 CLI，后台无窗口运行，输出写入 data/cli.log
//! 3. GUI 退出时先通过 API 停止所有录制（让 FLV 正常收尾），再结束 CLI 进程

use std::{
    fs::{self, OpenOptions},
    io::Write,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, PartialEq)]
pub enum CoreState {
    /// 尚未检查
    Pending,
    /// CLI 已拉起，等待 API 就绪
    Starting,
    /// API 可用
    Ready,
    /// 用户关闭了自动启动
    Disabled,
    /// 找不到 CLI 可执行文件
    NotFound(String),
    /// CLI 启动后超时或崩溃
    Failed(String),
}

pub struct Launcher {
    pub child: Option<Child>,
    pub cli_path: Option<PathBuf>,
    pub state: CoreState,
    since: Option<Instant>,
    /// 上一次确认"外部录制核心仍在运行"的时间（避免每帧发请求）
    last_ready_check: Option<Instant>,
    pub log_path: Option<PathBuf>,
    url: String,
    auth_header: Option<String>,
}

impl Launcher {
    pub fn new(url: String, auth_header: Option<String>) -> Self {
        Self { child: None, cli_path: None, state: CoreState::Pending, since: None, last_ready_check: None, log_path: None, url, auth_header }
    }

    /// 已拉起的 CLI 进程号
    pub fn pid(&self) -> Option<u32> { self.child.as_ref().map(|c| c.id()) }

    /// GUI 重连时同步 API 地址与认证头
    pub fn set_endpoint(&mut self, url: &str, auth_header: Option<String>) {
        self.url = url.trim_end_matches('/').to_string();
        self.auth_header = auth_header;
    }

    /// 回到待检查状态，下一帧重新检查并按需拉起
    pub fn reset(&mut self) { self.set_state(CoreState::Pending); }

    /// 立即结束自己拉起的 CLI（不碰外部启动的）
    pub fn stop(&mut self) {
        self.shutdown();
        // 给系统一点时间释放端口，避免重新拉起时误判端口被占用
        std::thread::sleep(Duration::from_millis(400));
    }

    pub fn is_starting(&self) -> bool { self.state == CoreState::Starting }

    /// 距离进入当前状态经过的毫秒数
    pub fn elapsed_ms(&self) -> u128 { self.since.map(|t| t.elapsed().as_millis()).unwrap_or(0) }

    fn set_state(&mut self, s: CoreState) { self.state = s; self.since = Some(Instant::now()); }

    /// 检查 API；不可用时自动拉起 CLI。
    pub fn bootstrap(&mut self, auto_launch: bool, cli: Option<PathBuf>, bind_port: u16,
                      config_file: &Path, work_dir: &Path, log_file: &Path) -> Result<u16, String> {
        if self.child.is_some() {
            self.set_state(CoreState::Starting);
            return Ok(0);
        }
        if api_healthy(&self.url, &self.auth_header) {
            self.set_state(CoreState::Ready);
            return Ok(0);
        }
        if !auto_launch {
            self.set_state(CoreState::Disabled);
            return Ok(0);
        }
        let cli = match cli {
            Some(p) => p,
            None => {
                let msg = "未找到录制核心 CLI。请把 CLI 放到程序同目录的 cli 文件夹（BililiveRecorder.Cli.exe）".to_string();
                self.set_state(CoreState::NotFound(msg));
                return Ok(0);
            }
        };
        for d in [config_file.parent().unwrap_or(Path::new(".")), work_dir] {
            fs::create_dir_all(d).map_err(|e| format!("无法创建目录 {}：{e}", d.display()))?;
        }
        let mut log = OpenOptions::new().create(true).append(true).open(log_file)
            .map_err(|e| format!("无法创建日志 {}：{e}", log_file.display()))?;
        let err = log.try_clone().map_err(|e| e.to_string())?;
        let _ = writeln!(log, "\n===== {} 启动录制核心 =====", now_string());

        // 目标端口被别的程序占用时换一个空闲端口
        let port = if port_in_use(bind_port) { free_port() } else { bind_port };
        self.url = format!("http://localhost:{port}");
        self.set_state(CoreState::Starting);

        let mut cmd = Command::new(&cli);
        cmd.arg("run")
            .arg("--bind").arg(port.to_string())
            .arg("--config-override").arg(config_file)
            .arg(work_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err));
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        match cmd.spawn() {
            Ok(child) => {
                self.child = Some(child);
                self.cli_path = Some(cli);
                self.log_path = Some(log_file.to_path_buf());
                Ok(port)
            }
            Err(e) => {
                self.set_state(CoreState::Failed(format!("启动录制核心失败：{e}")));
                Err(format!("启动录制核心失败：{e}"))
            }
        }
    }

    /// 每帧调用：等待 API 就绪、处理超时。
    pub fn tick(&mut self) -> bool {
        match &self.state {
            CoreState::Starting => {
                if self.child.as_mut().map(|c| c.try_wait().ok().flatten().is_some()).unwrap_or(false) {
                    self.set_state(CoreState::Failed("录制核心已退出，请查看 data/cli.log".into()));
                    return true;
                }
                if api_healthy(&self.url, &self.auth_header) {
                    self.last_ready_check = Some(Instant::now());
                    self.set_state(CoreState::Ready);
                    return true;
                }
                if self.elapsed_ms() > 45_000 {
                    self.set_state(CoreState::Failed("录制核心 45 秒内未就绪，请查看 data/cli.log".into()));
                    return true;
                }
                false
            }
            CoreState::Ready => {
                // 之前连接的是外部已启动的 CLI，若它退出了，回到待检查状态以便重新拉起。每 5 秒确认一次。
                if self.child.is_some() { return false; }
                let due = self.last_ready_check.map(|t| t.elapsed() > Duration::from_secs(5)).unwrap_or(true);
                if !due { return false; }
                self.last_ready_check = Some(Instant::now());
                if api_healthy(&self.url, &self.auth_header) {
                    false
                } else {
                    self.set_state(CoreState::Pending);
                    true
                }
            }
            _ => false,
        }
    }

    pub fn state_text(&self) -> String {
        let pid = self.pid().map(|p| format!(" · PID {p}")).unwrap_or_default();
        match &self.state {
            CoreState::Pending => "正在检查录制核心…".into(),
            CoreState::Starting => "正在启动录制核心…".into(),
            CoreState::Ready => {
                if self.child.is_some() { format!("录制核心已就绪{pid}") } else { "已连接到已运行的录制核心".into() }
            }
            CoreState::Disabled => "未连接（自动启动录制核心已关闭）".into(),
            CoreState::NotFound(m) | CoreState::Failed(m) => m.clone(),
        }
    }

    /// 关闭前收尾：先停所有录制（FLV 正常写完），再结束 CLI。
    fn shutdown(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = self.stop_all_recordings();
            let _ = child.kill();
            let _ = child.wait();
        }
        self.state = CoreState::Disabled;
    }

    /// 通过 API 停止所有正在录制的房间，最多等 5 秒。
    fn stop_all_recordings(&self) -> Result<(), String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| e.to_string())?;
        let req = |method: reqwest::Method, path: &str| {
            let mut r = client.request(method, format!("{}{}", self.url, path));
            if let Some(h) = &self.auth_header {
                r = r.header("Authorization", h);
            }
            r.send().map_err(|e| e.to_string())
        };
        let rooms: Vec<crate::api::Room> = req(reqwest::Method::GET, "/api/room")?
            .error_for_status().map_err(|e| e.to_string())?
            .json().map_err(|e| e.to_string())?;
        let recording: Vec<String> = rooms.iter().filter(|r| r.recording).map(|r| r.key()).collect();
        if recording.is_empty() {
            return Ok(());
        }
        for key in &recording {
            let _ = req(reqwest::Method::POST, &format!("/api/room/{key}/stop"));
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let still: Vec<crate::api::Room> = req(reqwest::Method::GET, "/api/room")?
                .error_for_status().map_err(|e| e.to_string())?
                .json().map_err(|e| e.to_string())?;
            if !still.iter().any(|r| r.recording) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Ok(())
    }
}

impl Drop for Launcher {
    fn drop(&mut self) { self.shutdown(); }
}

/// 当前 UTC 时刻 HH:MM:SS，用于在日志里标注时间
pub fn now_string() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{:02}:{:02}:{:02} UTC", (secs / 3600) % 24, (secs / 60) % 60, secs % 60)
}

fn api_healthy(url: &str, auth_header: &Option<String>) -> bool {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(1500))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new());
    let mut r = client.get(format!("{}/api/version", url.trim_end_matches('/')));
    if let Some(h) = auth_header {
        r = r.header("Authorization", h);
    }
    r.send().map(|s| s.status().is_success()).unwrap_or(false)
}

fn port_in_use(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_err() || TcpListener::bind(("[::1]", port)).is_err()
}

pub fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0)).ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
        .unwrap_or(2356)
}

/// 在 exe 同目录查找 CLI：环境变量 > exe 同级 > exe/cli/（最多下探 3 层）
pub fn find_cli(exe_dir: &Path) -> Option<PathBuf> {
    let exe_name = if cfg!(windows) { "BililiveRecorder.Cli.exe" } else { "BililiveRecorder.Cli" };
    if let Ok(p) = std::env::var("BLR_CLI_PATH") {
        let p = PathBuf::from(p);
        if p.is_file() { return Some(p); }
    }
    let direct = exe_dir.join(exe_name);
    if direct.is_file() { return Some(direct); }
    let cli_dir = exe_dir.join("cli");
    if cli_dir.is_dir() {
        if let Some(p) = search(&cli_dir, exe_name, 3) { return Some(p); }
    }
    None
}

fn search(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
    let entries = match fs::read_dir(dir) { Ok(e) => e, Err(_) => return None };
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            dirs.push(p);
        } else if p.file_name().map(|n| n == name).unwrap_or(false) {
            return Some(p);
        }
    }
    if depth == 0 { return None; }
    dirs.iter().find_map(|d| search(d, name, depth - 1))
}

/// 播放器的选择结果
pub enum PlayerChoice {
    /// ffplay：用 FFmpeg 自带的简易播放器，支持跟随正在写入的文件
    Ffplay(PathBuf),
    /// 其它播放器程序（mpv / PotPlayer 等）：直接把文件路径作为第一个参数
    Generic(PathBuf),
    /// 没有指定播放器：交给系统默认关联程序
    System,
}

impl PlayerChoice {
    /// 根据程序路径判断类型：文件名以 ffplay 开头的按 ffplay 处理
    pub fn for_path(p: PathBuf) -> Self {
        let is_ffplay = p.file_stem().map(|s| s.to_string_lossy().to_lowercase().starts_with("ffplay")).unwrap_or(false);
        if is_ffplay { PlayerChoice::Ffplay(p) } else { PlayerChoice::Generic(p) }
    }
}

/// 探测正在写入的 FLV 的最后视频时间戳（秒）。用 ffprobe 扫最后一个包，
/// 比 format 元数据可靠（头部 duration 在增长中的文件里是假值）。
fn probe_last_pts(ffplay: &Path, file: &Path) -> Option<f64> {
    let ffprobe = ffplay.with_file_name("ffprobe.exe");
    if !ffprobe.is_file() { return None; }
    let mut cmd = Command::new(&ffprobe);
    cmd.args(["-v", "error", "-select_streams", "v",
              "-show_entries", "packet=pts_time", "-of", "csv=p=0"])
        .arg(file);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let out = cmd.output().ok()?;
    let last = String::from_utf8_lossy(&out.stdout)
        .lines().rev().find(|l| !l.trim().is_empty())?.trim().to_string();
    last.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0)
}

/// 启动外部播放器播放文件。非阻塞：不等播放器退出。
/// geometry = Some([左,上,宽,高]) 时按此打开窗口（ffplay）。
/// 成功返回播放器进程号（System 方式返回 None），供窗口位置记忆用。
pub fn launch_player(choice: &PlayerChoice, file: &Path, title: &str,
                     geometry: Option<[i32; 4]>) -> Result<Option<u32>, String> {
    let (exe, args): (&Path, Vec<String>) = match choice {
        PlayerChoice::System => {
            open_with_default(file)?;
            return Ok(None);
        }
        PlayerChoice::Ffplay(exe) => {
            let mut a = vec![
                "-autoexit".into(),
                "-loglevel".into(), "error".into(),
                "-window_title".into(), title.into(),
            ];
            if let Some([left, top, w, h]) = geometry {
                a.push("-left".into()); a.push(left.to_string());
                a.push("-top".into()); a.push(top.to_string());
                a.push("-x".into()); a.push(w.to_string());
                a.push("-y".into()); a.push(h.to_string());
            }
            // 增长中的文件从头播会落后直播：快进到最后时间戳前 ~1.5 秒，直接对上实时
            if let Some(pts) = probe_last_pts(exe, file) {
                let ss = (pts - 1.5).max(0.0);
                a.push("-ss".into());
                a.push(format!("{ss:.3}"));
            }
            a.push(file.display().to_string());
            (exe, a)
        }
        PlayerChoice::Generic(exe) => (exe, vec![file.display().to_string()]),
    };
    let mut cmd = Command::new(exe);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let child = cmd.spawn().map_err(|e| format!("启动播放器 {} 失败：{e}", exe.display()))?;
    Ok(Some(child.id()))
}

/// 盯着播放器的窗口，退出时把最后一次的 [左,上,宽,高] 存回设置文件，下次按此打开。
#[cfg(windows)]
pub fn watch_player_geometry(pid: u32, settings_path: PathBuf) {
    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel::<[i32; 4]>();
    // 采样线程：500ms 一次，窗口消失后把最后一次矩形交给保存线程
    std::thread::spawn(move || unsafe {
        let mut last: Option<[i32; 4]> = None;
        let mut seen = false;
        // 上限 8 小时，防播放器永不退出时线程常驻
        for _ in 0..(8 * 3600 * 2) {
            let rect = find_player_window(pid);
            match rect {
                Some(r) => { seen = true; last = Some(r); }
                None if seen => { break; }
                None => {}
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        let _ = tx.send(last);
    });
    // 保存线程：读设置 JSON → 更新 playerGeometry → 写回
    std::thread::spawn(move || {
        if let Ok(rect) = rx.recv() {
            if let Some(rect) = rect {
                if let Some(mut v) = read_settings_value(&settings_path) {
                    v["playerGeometry"] = serde_json::json!(rect);
                    let _ = std::fs::write(&settings_path,
                        serde_json::to_string_pretty(&v).unwrap_or_default());
                }
            }
        }
    });
}

#[cfg(windows)]
#[allow(non_snake_case)]
fn find_player_window(pid: u32) -> Option<[i32; 4]> {
    use winapi::shared::minwindef::LPARAM;
    use winapi::um::winuser::{EnumWindows, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible};

    struct Ctx { pid: u32, rect: Option<[i32; 4]> }
    unsafe extern "system" fn cb(hwnd: winapi::shared::windef::HWND, lparam: LPARAM) -> i32 {
        let ctx = &mut *(lparam.0 as *mut Ctx);
        let mut wpid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut wpid);
        if wpid == ctx.pid && IsWindowVisible(hwnd) != 0 {
            let mut r = winapi::shared::windef::RECT::default();
            if GetWindowRect(hwnd, &mut r) != 0 && r.right - r.left > 80 && r.bottom - r.top > 60 {
                ctx.rect = Some([r.left, r.top, r.right - r.left, r.bottom - r.top]);
            }
        }
        1
    }
    let mut ctx = Ctx { pid, rect: None };
    unsafe { EnumWindows(Some(cb), LPARAM(&mut ctx as *mut Ctx as isize)); }
    ctx.rect
}

#[cfg(not(windows))]
pub fn watch_player_geometry(_pid: u32, _settings_path: PathBuf) {}

#[cfg(windows)]
fn read_settings_value(p: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}
#[cfg(not(windows))]
fn read_settings_value(_p: &Path) -> Option<serde_json::Value> { None }

/// 用系统默认程序打开文件或文件夹
pub fn open_with_default(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("路径不存在：{}", path.display()));
    }
    let mut cmd;
    #[cfg(windows)]
    {
        cmd = Command::new("rundll32");
        cmd.arg("url.dll,FileProtocolHandler").arg(path);
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        cmd = Command::new("xdg-open");
        cmd.arg(path);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    cmd.status().map(|_| ()).map_err(|e| format!("打开失败：{e}"))
}
