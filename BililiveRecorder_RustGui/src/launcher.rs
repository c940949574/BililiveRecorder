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

/// 当前 UTC 时刻 HH:MM:SS，只用于在 cli.log 里分隔每次启动
fn now_string() -> String {
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
