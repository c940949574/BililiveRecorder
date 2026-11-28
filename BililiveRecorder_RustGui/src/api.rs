use base64::Engine;
use serde::{Deserialize, Deserializer, Serialize};

/// 录播核心用 Newtonsoft 序列化，数值为 NaN / Infinity 时会输出成字符串 "NaN" / "Infinity"。
/// 这里两种形式都接受，避免整个房间列表因为一个字段解析失败而拉不到。
fn de_f64<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Value {
        Number(f64),
        Text(String),
        Null,
    }
    Ok(match Value::deserialize(d)? {
        Value::Number(x) => x,
        Value::Null => 0.0,
        Value::Text(s) => match s.trim() {
            "NaN" => f64::NAN,
            "Infinity" => f64::INFINITY,
            "-Infinity" => f64::NEG_INFINITY,
            other => other.parse().unwrap_or(0.0),
        },
    })
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct IoStats {
    pub stream_host: Option<String>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    #[serde(deserialize_with = "de_f64")]
    pub duration: f64,
    pub network_bytes_downloaded: i64,
    #[serde(deserialize_with = "de_f64")]
    pub network_mbps: f64,
    #[serde(deserialize_with = "de_f64")]
    pub disk_write_duration: f64,
    pub disk_bytes_written: i64,
    #[serde(deserialize_with = "de_f64")]
    pub disk_mbps: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct RecordingStats {
    #[serde(deserialize_with = "de_f64")]
    pub session_duration: f64,
    pub total_input_bytes: i64,
    pub total_output_bytes: i64,
    pub current_file_size: i64,
    pub session_max_timestamp: i64,
    pub file_max_timestamp: i64,
    #[serde(deserialize_with = "de_f64")]
    pub added_duration: f64,
    #[serde(deserialize_with = "de_f64")]
    pub passed_time: f64,
    #[serde(deserialize_with = "de_f64")]
    pub duration_ratio: f64,
    pub input_video_bytes: i64,
    pub input_audio_bytes: i64,
    pub output_video_frames: i64,
    pub output_audio_frames: i64,
    pub output_video_bytes: i64,
    pub output_audio_bytes: i64,
    pub total_input_video_bytes: i64,
    pub total_input_audio_bytes: i64,
    pub total_output_video_frames: i64,
    pub total_output_audio_frames: i64,
    pub total_output_video_bytes: i64,
    pub total_output_audio_bytes: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Room {
    pub object_id: String,
    pub room_id: i64,
    pub auto_record: bool,
    pub short_id: i64,
    pub name: Option<String>,
    pub uid: i64,
    pub title: Option<String>,
    pub area_name_parent: Option<String>,
    pub area_name_child: Option<String>,
    pub recording: bool,
    pub streaming: bool,
    pub danmaku_connected: bool,
    pub auto_record_for_this_session: bool,
    pub recording_stats: RecordingStats,
    pub io_stats: IoStats,
}

impl Room {
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or("(未加载)")
    }
    pub fn display_title(&self) -> &str {
        self.title.as_deref().unwrap_or("")
    }
    pub fn display_area(&self) -> String {
        match (&self.area_name_parent, &self.area_name_child) {
            (Some(a), Some(b)) => format!("{} · {}", a, b),
            (Some(a), None) => a.clone(),
            (None, Some(b)) => b.clone(),
            _ => String::new(),
        }
    }
    pub fn key(&self) -> String {
        if !self.object_id.is_empty() && self.object_id != "00000000-0000-0000-0000-000000000000" {
            self.object_id.clone()
        } else {
            self.short_id.to_string()
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub level: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub room_id: Option<i64>,
}

#[derive(Clone)]
pub struct ApiClient {
    base: String,
    auth_header: Option<String>,
    client: reqwest::blocking::Client,
}

/// 生成 Basic Auth 请求头；用户名或密码为空则不使用认证
pub fn build_auth_header(username: &str, password: &str) -> Option<String> {
    if username.is_empty() || password.is_empty() {
        return None;
    }
    let raw = format!("{}:{}", username, password);
    let encoded = base64::engine::general_purpose::STANDARD.encode(raw.as_bytes());
    Some(format!("Basic {}", encoded))
}

impl ApiClient {
    pub fn new(base: String, username: Option<String>, password: Option<String>) -> Self {
        let auth_header = match (username, password) {
            (Some(u), Some(p)) => build_auth_header(&u, &p),
            _ => None,
        };
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self { base, auth_header, client }
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::blocking::RequestBuilder {
        let url = format!("{}{}", self.base.trim_end_matches('/'), path);
        let mut r = self.client.request(method, &url);
        if let Some(h) = &self.auth_header {
            r = r.header("Authorization", h);
        }
        r
    }

    pub fn get_rooms(&self) -> Result<Vec<Room>, String> {
        let resp = self.req(reqwest::Method::GET, "/api/room").send().map_err(|e| e.to_string())?;
        if !resp.status().is_success() { return Err(format!("HTTP {}", resp.status())); }
        resp.json::<Vec<Room>>().map_err(|e| e.to_string())
    }

    /// 录制核心的录播文件工作目录（核心实际使用的，可能与 GUI 本地推断不同）
    pub fn get_workdir(&self) -> Result<String, String> {
        let resp = self.req(reqwest::Method::GET, "/api/misc/workDirectory").send().map_err(|e| e.to_string())?;
        if !resp.status().is_success() { return Err(format!("HTTP {}", resp.status())); }
        resp.text().map(|s| s.trim_matches('"').trim().to_string()).map_err(|e| e.to_string())
    }

    pub fn add_room(&self, room_id: i64, auto_record: bool) -> Result<(), String> {
        #[derive(Serialize)]
        struct Body { room_id: i64, auto_record: bool }
        let resp = self.req(reqwest::Method::POST, "/api/room")
            .json(&Body { room_id, auto_record })
            .send().map_err(|e| e.to_string())?;
        if resp.status().is_success() { Ok(()) } else { Err(format!("HTTP {}", resp.status())) }
    }

    fn action(&self, key: &str, action: &str) -> Result<(), String> {
        let path = format!("/api/room/{}/{}", key, action);
        let resp = self.req(reqwest::Method::POST, &path).send().map_err(|e| e.to_string())?;
        if resp.status().is_success() { Ok(()) } else { Err(format!("HTTP {}", resp.status())) }
    }

    pub fn start(&self, key: &str) -> Result<(), String> { self.action(key, "start") }
    pub fn stop(&self, key: &str) -> Result<(), String> { self.action(key, "stop") }
    pub fn split(&self, key: &str) -> Result<(), String> { self.action(key, "split") }
    pub fn refresh(&self, key: &str) -> Result<(), String> { self.action(key, "refresh") }
    pub fn delete(&self, key: &str) -> Result<(), String> {
        let path = format!("/api/room/{}", key);
        let resp = self.req(reqwest::Method::DELETE, &path).send().map_err(|e| e.to_string())?;
        if resp.status().is_success() { Ok(()) } else { Err(format!("HTTP {}", resp.status())) }
    }

    pub fn fetch_logs(&self) -> Result<Vec<LogEntry>, String> {
        let resp = self.req(reqwest::Method::GET, "/api/log/fetch").send().map_err(|e| e.to_string())?;
        if !resp.status().is_success() { return Err(format!("HTTP {}", resp.status())); }
        let text = resp.text().map_err(|e| e.to_string())?;
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        if text.trim_start().starts_with('[') {
            return serde_json::from_str(&text).map_err(|e| e.to_string());
        }
        Ok(lines.into_iter().map(|l| LogEntry {
            timestamp: String::new(), level: String::new(), message: l.to_string(), room_id: None,
        }).collect())
    }

    pub fn health(&self) -> bool {
        self.req(reqwest::Method::GET, "/api/version").send()
            .map(|r| r.status().is_success()).unwrap_or(false)
    }
}
