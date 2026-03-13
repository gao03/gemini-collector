//! HTTP 客户端封装：reqwest Client 构建、cookie 注入、退避/重试/延迟、请求状态持久化。
//!
//! 对应 Python GeminiExporter 中的：
//! - `_create_http_client`
//! - `_before_request` / `_mark_request_success` / `_mark_request_failure`
//! - `_client_get_with_retry`
//! - `_sync_request_state_file` / `_set_request_state_scope`

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use rand::distr::{Distribution, Uniform};
use serde_json::json;

use crate::protocol::{
    request_backoff_seconds, REQUEST_BACKOFF_LIMIT_FAILURES, REQUEST_BACKOFF_MAX_SECONDS,
    REQUEST_DELAY, REQUEST_JITTER_MAX, REQUEST_JITTER_MIN, BROWSER_ACCEPT_LANGUAGE,
    BROWSER_USER_AGENT, ProtocolError,
};
use crate::storage;

use super::GeminiExporter;

// ============================================================================
// Client 构建
// ============================================================================

/// 构建 reqwest 异步客户端，注入 cookie 和默认 headers。
pub fn build_http_client(cookies: &HashMap<String, String>) -> reqwest::Client {
    let cookie_header: String = cookies
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("; ");

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::USER_AGENT,
        reqwest::header::HeaderValue::from_static(BROWSER_USER_AGENT),
    );
    headers.insert(
        reqwest::header::ACCEPT_LANGUAGE,
        reqwest::header::HeaderValue::from_static(BROWSER_ACCEPT_LANGUAGE),
    );
    if !cookie_header.is_empty() {
        if let Ok(val) = reqwest::header::HeaderValue::from_str(&cookie_header) {
            headers.insert(reqwest::header::COOKIE, val);
        }
    }

    reqwest::Client::builder()
        .default_headers(headers)
        .redirect(reqwest::redirect::Policy::limited(10))
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .expect("Failed to build reqwest client")
}

// ============================================================================
// 退避 / 延迟 / 重试
// ============================================================================

/// 三角分布随机抖动（模拟 Python random.triangular）
fn triangular_jitter(min: f64, max: f64, mode: f64) -> f64 {
    let u: f64 = Uniform::new(0.0_f64, 1.0_f64)
        .expect("invalid uniform range")
        .sample(&mut rand::rng());
    let fc = (mode - min) / (max - min);
    if u < fc {
        min + ((max - min) * (mode - min) * u).sqrt()
    } else {
        max - ((max - min) * (max - mode) * (1.0 - u)).sqrt()
    }
}

impl GeminiExporter {
    /// 当前退避毫秒数
    pub fn request_backoff_ms(&self) -> u64 {
        let failures = self.request_consecutive_failures.load(Ordering::Relaxed);
        (request_backoff_seconds(failures) * 1000.0).round() as u64
    }

    /// 当前请求状态快照（用于持久化到 sync_state.json）
    pub fn current_request_state(&self) -> serde_json::Value {
        let now_iso = chrono::Utc::now().to_rfc3339();
        json!({
            "consecutiveFailures": self.request_consecutive_failures.load(Ordering::Relaxed),
            "backoffMs": self.request_backoff_ms(),
            "updatedAt": now_iso,
        })
    }

    /// 将请求状态写入 sync_state.json
    pub async fn sync_request_state_file(&self) {
        let account_dir = self.request_state_account_dir.lock().await;
        let account_dir = match account_dir.as_ref() {
            Some(dir) => dir.clone(),
            None => return,
        };
        drop(account_dir); // release lock early

        let dir = self.request_state_account_dir.lock().await;
        let dir = match dir.as_ref() {
            Some(d) => d.clone(),
            None => return,
        };

        let mut state = storage::load_sync_state(&dir);
        let now_iso = chrono::Utc::now().to_rfc3339();

        if !state.is_object() {
            state = json!({});
        }
        let obj = state.as_object_mut().unwrap();
        obj.entry("version")
            .or_insert(json!(1));
        obj.entry("accountId")
            .or_insert_with(|| {
                json!(dir.file_name().and_then(|s| s.to_str()).unwrap_or(""))
            });
        obj.insert("updatedAt".to_string(), json!(now_iso));
        obj.insert("requestState".to_string(), self.current_request_state());

        let _ = storage::write_sync_state(&dir, &state);
    }

    /// 设置请求状态作用域，从 sync_state 恢复失败计数
    pub async fn set_request_state_scope(&self, account_dir: PathBuf) {
        self.request_started.store(false, Ordering::Relaxed);

        let state = storage::load_sync_state(&account_dir);
        if let Some(obj) = state.as_object() {
            // 优先从 requestState 恢复
            if let Some(request_state) = obj.get("requestState").and_then(|v| v.as_object()) {
                if let Some(count) = request_state
                    .get("consecutiveFailures")
                    .and_then(|v| v.as_u64())
                {
                    self.request_consecutive_failures.store(
                        (count as u32).min(REQUEST_BACKOFF_LIMIT_FAILURES),
                        Ordering::Relaxed,
                    );
                    *self.request_state_account_dir.lock().await = Some(account_dir);
                    return;
                }
            }
            // 兼容旧格式
            if let Some(full_sync) = obj.get("fullSync").and_then(|v| v.as_object()) {
                if let Some(count) = full_sync
                    .get("listingConsecutiveFailures")
                    .and_then(|v| v.as_u64())
                {
                    self.request_consecutive_failures.store(
                        (count as u32).min(REQUEST_BACKOFF_LIMIT_FAILURES),
                        Ordering::Relaxed,
                    );
                }
            }
        }

        *self.request_state_account_dir.lock().await = Some(account_dir);
    }

    /// 请求前等待：延迟 + 退避 + 退避上限探测。
    ///
    /// 返回 Err 表示退避上限已达、应中止。
    pub async fn before_request(&self, label: &str) -> Result<(), ProtocolError> {
        // 检查取消
        if self.cancelled.load(Ordering::Relaxed) {
            return Err(ProtocolError::RequestBackoffLimitReached);
        }

        let failures = self.request_consecutive_failures.load(Ordering::Relaxed);
        let backoff_sec = request_backoff_seconds(failures);

        if backoff_sec >= REQUEST_BACKOFF_MAX_SECONDS {
            let started = self.request_started.load(Ordering::Relaxed);
            let probe_consumed = self.limit_probe_consumed.load(Ordering::Relaxed);
            if !started && !probe_consumed {
                // 首次放行探测请求
                self.limit_probe_consumed.store(true, Ordering::Relaxed);
                eprintln!(
                    "  [backoff] 连续失败达到上限，放行一次启动探测请求: failures={}, op={}",
                    failures, label
                );
            } else {
                self.sync_request_state_file().await;
                return Err(ProtocolError::RequestBackoffLimitReached);
            }
        }

        // 请求间延迟
        if self.request_started.load(Ordering::Relaxed) {
            let jitter = triangular_jitter(
                REQUEST_JITTER_MIN,
                REQUEST_JITTER_MAX,
                crate::protocol::REQUEST_JITTER_MODE,
            );
            let delay_sec = REQUEST_DELAY + jitter;
            *self.last_delay_sec.lock().await = delay_sec;
            tokio::time::sleep(std::time::Duration::from_secs_f64(delay_sec)).await;
        }

        // 退避等待
        if backoff_sec > 0.0 && backoff_sec < REQUEST_BACKOFF_MAX_SECONDS {
            self.sync_request_state_file().await;
            eprintln!(
                "  [backoff] 连续失败退避等待: failures={}, wait={:.2}s, op={}",
                failures, backoff_sec, label
            );
            tokio::time::sleep(std::time::Duration::from_secs_f64(backoff_sec)).await;
        }

        self.request_started.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// 标记请求成功，重置失败计数
    pub async fn mark_request_success(&self) {
        if self.request_consecutive_failures.load(Ordering::Relaxed) == 0 {
            return;
        }
        self.request_consecutive_failures.store(0, Ordering::Relaxed);
        self.limit_probe_consumed.store(false, Ordering::Relaxed);
        self.sync_request_state_file().await;
    }

    /// 标记请求失败，递增失败计数
    pub async fn mark_request_failure(&self) {
        let current = self.request_consecutive_failures.load(Ordering::Relaxed);
        self.request_consecutive_failures.store(
            (current + 1).min(REQUEST_BACKOFF_LIMIT_FAILURES),
            Ordering::Relaxed,
        );
        self.sync_request_state_file().await;
    }

    /// GET 请求 + 自动重试（最多 attempts 次）。
    ///
    /// `count_as_business_request`：是否计入业务请求成功/失败统计。
    /// init_auth 等不计入。
    pub async fn client_get_with_retry(
        &self,
        url: &str,
        params: &[(&str, String)],
        attempts: u32,
        count_as_business_request: bool,
    ) -> Result<reqwest::Response, String> {
        let mut last_err = String::new();
        for _ in 0..attempts {
            self.before_request("http_get")
                .await
                .map_err(|e| e.to_string())?;

            let mut req = self.client.get(url);
            if !params.is_empty() {
                req = req.query(params);
            }

            match req.send().await {
                Ok(resp) => {
                    if count_as_business_request {
                        self.mark_request_success().await;
                    }
                    return Ok(resp);
                }
                Err(e) => {
                    if count_as_business_request {
                        self.mark_request_failure().await;
                    }
                    last_err = e.to_string();
                }
            }
        }
        Err(last_err)
    }
}
