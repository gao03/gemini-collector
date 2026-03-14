pub mod cookies;
mod export;
mod sync;
pub mod gemini_api;
mod import;
pub mod media;
pub mod protocol;
mod search;
pub mod storage;
pub mod turn_parser;
mod worker_host;

use std::path::Path;
use tauri::Manager;
use worker_host::EnqueueJobRequest;

use export::{resolve_account_id_arg, value_to_non_empty_string};

fn read_account_registry_entry(data_dir: &Path, account_id: &str) -> Result<serde_json::Value, String> {
    let accounts_file = data_dir.join("accounts.json");
    if !accounts_file.exists() {
        return Err("accounts.json 不存在".to_string());
    }

    let registry: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&accounts_file).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    let entries = registry
        .get("accounts")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "accounts.json 缺少 accounts 字段".to_string())?;

    for entry in entries {
        if entry
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s == account_id)
            .unwrap_or(false)
        {
            return Ok(entry.clone());
        }
    }

    Err(format!("未找到账号: {}", account_id))
}

fn is_list_sync_pending(data_dir: &Path, data_dir_rel: &str) -> bool {
    let sync_file = data_dir.join(data_dir_rel).join("sync_state.json");
    if !sync_file.exists() {
        return false;
    }

    let content = match std::fs::read_to_string(&sync_file) {
        Ok(s) => s,
        Err(_) => return false,
    };

    let state: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return false,
    };

    let phase = state
        .get("fullSync")
        .and_then(|v| v.get("phase"))
        .and_then(|v| v.as_str());

    matches!(phase, Some(p) if p != "done")
}

fn normalize_conversation_id(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(stripped) = trimmed.strip_prefix("c_") {
        stripped.to_string()
    } else {
        trimmed.to_string()
    }
}

fn conversation_meta_info(jsonl_file: &Path) -> (bool, Option<String>) {
    let raw = match std::fs::read_to_string(jsonl_file) {
        Ok(v) => v,
        Err(_) => return (false, None),
    };
    let has_failed = raw.contains("\"downloadFailed\": true") || raw.contains("\"downloadFailed\":true");
    let created_at = raw.lines().next().and_then(|line| {
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        v.get("createdAt")?.as_str().map(|s| s.to_string())
    });
    (has_failed, created_at)
}

// ============================================================================
// Tauri 命令：账号管理
// ============================================================================

#[tauri::command]
fn delete_conversation(
    app: tauri::AppHandle,
    account_id: String,
    conversation_id: String,
) -> Result<(), String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let account_dir = data_dir.join("accounts").join(&account_id);
    let bare_id = normalize_conversation_id(&conversation_id);

    // 删除 .jsonl 文件
    let conv_file = account_dir.join("conversations").join(format!("{}.jsonl", bare_id));
    if conv_file.exists() {
        std::fs::remove_file(&conv_file).map_err(|e| e.to_string())?;
    }

    // 从 conversations.json 的 items 中移除该条记录，并取得新数量
    let index_file = account_dir.join("conversations.json");
    let new_count: Option<usize> = if index_file.exists() {
        let raw = std::fs::read_to_string(&index_file).map_err(|e| e.to_string())?;
        if let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(items) = parsed.get_mut("items").and_then(|v| v.as_array_mut()) {
                items.retain(|item| {
                    item.get("id").and_then(|v| v.as_str()) != Some(bare_id.as_str())
                });
                let count = items.len();
                let serialized = serde_json::to_string_pretty(&parsed).map_err(|e| e.to_string())?;
                std::fs::write(&index_file, serialized).map_err(|e| e.to_string())?;
                Some(count)
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    // 同步更新 meta.json 中的 conversationCount
    if let Some(count) = new_count {
        let meta_file = account_dir.join("meta.json");
        if meta_file.exists() {
            let raw = std::fs::read_to_string(&meta_file).map_err(|e| e.to_string())?;
            if let Ok(mut meta) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(obj) = meta.as_object_mut() {
                    obj.insert("conversationCount".to_string(), serde_json::json!(count));
                    let serialized = serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?;
                    std::fs::write(&meta_file, serialized).map_err(|e| e.to_string())?;
                }
            }
        }
    }

    // 清理搜索索引
    if let Ok(index) = search::open_or_create_index(&account_dir) {
        let _ = search::remove_conversation(&index, &account_dir, &bare_id);
    }

    Ok(())
}

#[tauri::command]
fn clear_account_data(
    app: tauri::AppHandle,
    account_id: Option<String>,
    #[allow(non_snake_case)] accountId: Option<String>,
) -> Result<String, String> {
    let account_id = account_id
        .or(accountId)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "缺少 account_id/accountId 参数".to_string())?;

    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let account_dir = data_dir.join("accounts").join(&account_id);
    let conversations_dir = account_dir.join("conversations");
    let media_dir = account_dir.join("media");
    let conversations_file = account_dir.join("conversations.json");
    let sync_state_file = account_dir.join("sync_state.json");
    let media_manifest_file = account_dir.join("media_manifest.json");
    let meta_file = account_dir.join("meta.json");

    if conversations_dir.exists() {
        std::fs::remove_dir_all(&conversations_dir).map_err(|e| e.to_string())?;
    }
    if media_dir.exists() {
        std::fs::remove_dir_all(&media_dir).map_err(|e| e.to_string())?;
    }
    if conversations_file.exists() {
        std::fs::remove_file(&conversations_file).map_err(|e| e.to_string())?;
    }
    if sync_state_file.exists() {
        std::fs::remove_file(&sync_state_file).map_err(|e| e.to_string())?;
    }
    if media_manifest_file.exists() {
        std::fs::remove_file(&media_manifest_file).map_err(|e| e.to_string())?;
    }
    // 清理搜索索引
    let search_idx_dir = account_dir.join("search_index");
    if search_idx_dir.exists() {
        let _ = std::fs::remove_dir_all(&search_idx_dir);
    }
    let search_mtimes = account_dir.join("search_mtimes.json");
    if search_mtimes.exists() {
        let _ = std::fs::remove_file(&search_mtimes);
    }

    std::fs::create_dir_all(&conversations_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&media_dir).map_err(|e| e.to_string())?;

    // Keep account mapping while resetting local sync counters in meta.
    let registry_entry = read_account_registry_entry(&data_dir, &account_id).ok();
    let email_from_registry = registry_entry
        .as_ref()
        .and_then(|v| value_to_non_empty_string(v.get("email")));
    let authuser_from_registry = registry_entry
        .as_ref()
        .and_then(|v| value_to_non_empty_string(v.get("authuser")));

    let mut meta_val = if meta_file.exists() {
        let raw = std::fs::read_to_string(&meta_file).map_err(|e| e.to_string())?;
        serde_json::from_str::<serde_json::Value>(&raw).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    if !meta_val.is_object() {
        meta_val = serde_json::json!({});
    }
    let obj = meta_val
        .as_object_mut()
        .ok_or_else(|| "meta.json 格式错误".to_string())?;

    let email = obj
        .get("email")
        .and_then(|v| value_to_non_empty_string(Some(v)))
        .or(email_from_registry)
        .unwrap_or_default();
    let name = obj
        .get("name")
        .and_then(|v| value_to_non_empty_string(Some(v)))
        .unwrap_or_else(|| {
            if email.is_empty() {
                account_id.clone()
            } else {
                email.split('@').next().unwrap_or(&account_id).to_string()
            }
        });
    let avatar_text = obj
        .get("avatarText")
        .and_then(|v| value_to_non_empty_string(Some(v)))
        .unwrap_or_else(|| {
            name.chars()
                .next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_else(|| "?".to_string())
        });
    let avatar_color = obj
        .get("avatarColor")
        .and_then(|v| value_to_non_empty_string(Some(v)))
        .unwrap_or_else(|| "#667eea".to_string());
    let authuser = obj
        .get("authuser")
        .and_then(|v| value_to_non_empty_string(Some(v)))
        .or(authuser_from_registry);

    obj.insert("version".to_string(), serde_json::json!(1));
    obj.insert("id".to_string(), serde_json::json!(account_id));
    obj.insert("name".to_string(), serde_json::json!(name));
    obj.insert("email".to_string(), serde_json::json!(email));
    obj.insert("avatarText".to_string(), serde_json::json!(avatar_text));
    obj.insert("avatarColor".to_string(), serde_json::json!(avatar_color));
    obj.insert("conversationCount".to_string(), serde_json::json!(0));
    obj.insert("remoteConversationCount".to_string(), serde_json::Value::Null);
    obj.insert("lastSyncAt".to_string(), serde_json::Value::Null);
    obj.insert("lastSyncResult".to_string(), serde_json::Value::Null);
    obj.insert(
        "authuser".to_string(),
        authuser
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null),
    );

    let serialized = serde_json::to_string_pretty(&meta_val).map_err(|e| e.to_string())?;
    std::fs::write(&meta_file, serialized).map_err(|e| e.to_string())?;

    Ok("{\"status\":\"ok\"}".to_string())
}

/// Read accounts.json + each account's meta.json from app data dir.
/// Returns a JSON array of Account objects (matches AccountMeta schema), or "[]".
#[tauri::command]
fn load_accounts(app: tauri::AppHandle) -> Result<String, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let accounts_file = data_dir.join("accounts.json");

    if !accounts_file.exists() {
        return Ok("[]".to_string());
    }

    let registry: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&accounts_file).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    let entries = match registry.get("accounts").and_then(|v| v.as_array()) {
        Some(a) => a.clone(),
        None => return Ok("[]".to_string()),
    };

    let mut result: Vec<serde_json::Value> = Vec::new();
    for entry in &entries {
        let data_dir_rel = entry
            .get("dataDir")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let list_sync_pending = is_list_sync_pending(&data_dir, data_dir_rel);
        let meta_file = data_dir.join(data_dir_rel).join("meta.json");

        if meta_file.exists() {
            if let Ok(s) = std::fs::read_to_string(&meta_file) {
                if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&s) {
                    if let Some(obj) = v.as_object_mut() {
                        obj.insert(
                            "listSyncPending".to_string(),
                            serde_json::Value::Bool(list_sync_pending),
                        );
                    }
                    result.push(v);
                    continue;
                }
            }
        }

        // meta.json missing — build minimal entry from registry
        let id = entry
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let email = entry.get("email").and_then(|v| v.as_str()).unwrap_or("");
        let authuser = entry.get("authuser").and_then(|v| v.as_str());
        let name = email.split('@').next().unwrap_or(id);
        let avatar = name
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".to_string());
        result.push(serde_json::json!({
            "id": id,
            "name": name,
            "email": email,
            "avatarText": avatar,
            "avatarColor": "#667eea",
            "conversationCount": 0,
            "remoteConversationCount": null,
            "lastSyncAt": null,
            "lastSyncResult": null,
            "authuser": authuser,
            "listSyncPending": list_sync_pending,
        }));
    }

    serde_json::to_string(&result).map_err(|e| e.to_string())
}

// ============================================================================
// Tauri 命令：Worker
// ============================================================================

/// 从本机浏览器读取 cookies，发现所有 Gemini 账号并写入 accounts.json。
#[tauri::command]
async fn run_accounts_import(app: tauri::AppHandle) -> Result<String, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;

    // 读取浏览器 cookies
    let all_cookies = tokio::task::spawn_blocking(|| {
        cookies::get_cookies_from_local_browser()
    })
    .await
    .map_err(|e| format!("cookies 读取任务失败: {}", e))?
    .map_err(|e| format!("cookies 读取失败: {}", e))?;

    if all_cookies.is_empty() {
        return Err("未能从浏览器读取到 cookies，请确保 Chrome 已登录 Gemini".to_string());
    }

    // 发现 email ↔ authuser 映射
    let mappings =
        cookies::list_accounts::discover_email_authuser_mapping(&all_cookies)
            .await
            .map_err(|e| format!("账号发现失败: {}", e))?;

    if mappings.is_empty() {
        return Err("未发现已登录的 Gemini 账号".to_string());
    }

    // 逐个写入 accounts.json + meta.json
    let mut imported_ids: Vec<String> = Vec::new();
    for m in &mappings {
        let account_id = protocol::email_to_account_id(&m.email);
        let account_dir = data_dir.join("accounts").join(&account_id);
        std::fs::create_dir_all(account_dir.join("conversations")).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(account_dir.join("media")).map_err(|e| e.to_string())?;

        let name = m.email.split('@').next().unwrap_or(&account_id).to_string();
        let avatar_text = name
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".to_string());

        let info = serde_json::json!({
            "version": 1,
            "id": account_id,
            "email": m.email,
            "name": name,
            "avatarText": avatar_text,
            "avatarColor": "#667eea",
            "conversationCount": 0,
            "remoteConversationCount": serde_json::Value::Null,
            "lastSyncAt": serde_json::Value::Null,
            "lastSyncResult": serde_json::Value::Null,
            "authuser": m.authuser,
        });

        storage::write_accounts_json(&data_dir, &info).map_err(|e| e.to_string())?;
        storage::write_account_meta(&account_dir, &info).map_err(|e| e.to_string())?;
        imported_ids.push(account_id);
    }

    Ok(serde_json::json!({
        "status": "ok",
        "imported": imported_ids.len(),
        "accounts": imported_ids,
    })
    .to_string())
}

#[tauri::command]
async fn enqueue_job(req: EnqueueJobRequest) -> Result<String, String> {
    worker_host::enqueue_job_async(req).await
}

#[tauri::command]
async fn cancel_job(
    account_id: Option<String>,
    #[allow(non_snake_case)] accountId: Option<String>,
) -> Result<(), String> {
    let account_id = resolve_account_id_arg(account_id, accountId)?;
    worker_host::cancel_job_async(&account_id).await
}

// ============================================================================
// Tauri 命令：对话读取
// ============================================================================

/// Read `accounts/{id}/conversations.json` and return the `items` array as JSON string.
#[tauri::command]
fn load_conversation_summaries(app: tauri::AppHandle, account_id: String) -> Result<String, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let account_dir = data_dir.join("accounts").join(&account_id);
    let conv_file = data_dir
        .join("accounts")
        .join(&account_id)
        .join("conversations.json");

    if !conv_file.exists() {
        return Ok("[]".to_string());
    }

    let raw = std::fs::read_to_string(&conv_file).map_err(|e| e.to_string())?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let mut items = parsed
        .get("items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let conversations_dir = account_dir.join("conversations");
    for item in &mut items {
        let Some(obj) = item.as_object_mut() else {
            continue;
        };
        let status = obj
            .get("status")
            .and_then(|v| v.as_str())
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .unwrap_or("normal")
            .to_string();
        obj.insert(
            "status".to_string(),
            serde_json::Value::String(status),
        );

        let cid = obj
            .get("id")
            .and_then(|v| v.as_str())
            .map(|v| v.trim())
            .unwrap_or("");
        if cid.is_empty() {
            obj.insert("hasFailedData".to_string(), serde_json::Value::Bool(false));
            continue;
        }

        let (has_failed_data, created_at) = conversation_meta_info(&conversations_dir.join(format!("{}.jsonl", cid)));
        obj.insert(
            "hasFailedData".to_string(),
            serde_json::Value::Bool(has_failed_data),
        );
        if let Some(ca) = created_at {
            obj.insert("createdAt".to_string(), serde_json::Value::String(ca));
        }
    }

    serde_json::to_string(&items).map_err(|e| e.to_string())
}

/// Return absolute media directory path for an account: `accounts/{id}/media`.
#[tauri::command]
fn get_account_media_dir(app: tauri::AppHandle, account_id: String) -> Result<String, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let media_dir = data_dir
        .join("accounts")
        .join(account_id)
        .join("media");
    Ok(media_dir.to_string_lossy().to_string())
}

// ── 全文搜索 ──────────────────────────────────────────────────────────

#[tauri::command]
fn update_search_index(
    app: tauri::AppHandle,
    account_id: String,
    conversation_ids: Vec<String>,
) -> Result<String, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let account_dir = data_dir.join("accounts").join(&account_id);
    let conversations_dir = account_dir.join("conversations");

    let index = search::open_or_create_index(&account_dir)?;
    let mut indexed = 0u32;
    for cid in &conversation_ids {
        let bare = normalize_conversation_id(cid);
        let jsonl = conversations_dir.join(format!("{}.jsonl", bare));
        if jsonl.exists() {
            search::index_conversation(&index, &account_dir, &bare, &jsonl)?;
            indexed += 1;
        }
    }
    Ok(serde_json::json!({ "indexed": indexed }).to_string())
}

#[tauri::command]
fn search_conversations(
    app: tauri::AppHandle,
    account_id: String,
    query: String,
    limit: Option<u32>,
) -> Result<String, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let account_dir = data_dir.join("accounts").join(&account_id);

    let index = search::open_or_create_index(&account_dir)?;
    let results = search::search_messages(&index, &query, limit.unwrap_or(50))?;
    serde_json::to_string(&results).map_err(|e| e.to_string())
}

#[tauri::command]
fn rebuild_search_index(app: tauri::AppHandle, account_id: String) -> Result<String, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let account_dir = data_dir.join("accounts").join(&account_id);
    let conversations_dir = account_dir.join("conversations");

    // 删除旧索引强制重建
    let search_idx_dir = account_dir.join("search_index");
    if search_idx_dir.exists() {
        std::fs::remove_dir_all(&search_idx_dir).map_err(|e| e.to_string())?;
    }
    let search_mtimes = account_dir.join("search_mtimes.json");
    if search_mtimes.exists() {
        let _ = std::fs::remove_file(&search_mtimes);
    }

    let index = search::open_or_create_index(&account_dir)?;
    let count = search::index_all(&index, &account_dir, &conversations_dir)?;
    let _ = search::merge_segments(&index);
    Ok(serde_json::json!({ "indexed": count }).to_string())
}

/// Read one conversation JSONL detail file and return a Conversation object JSON or `null`.
#[tauri::command]
fn load_conversation_detail(
    app: tauri::AppHandle,
    account_id: String,
    conversation_id: String,
) -> Result<String, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let bare_id = normalize_conversation_id(&conversation_id);
    if bare_id.is_empty() {
        return Ok("null".to_string());
    }

    let jsonl_file = data_dir
        .join("accounts")
        .join(&account_id)
        .join("conversations")
        .join(format!("{}.jsonl", bare_id));

    if !jsonl_file.exists() {
        return Ok("null".to_string());
    }

    let raw = std::fs::read_to_string(&jsonl_file).map_err(|e| e.to_string())?;
    let mut meta: Option<serde_json::Value> = None;
    let mut messages: Vec<serde_json::Value> = Vec::new();
    let mut parse_error_count: usize = 0;
    let mut parse_error_lines: Vec<usize> = Vec::new();

    for (idx, line) in raw.lines().enumerate() {
        let s = line.trim();
        if s.is_empty() {
            continue;
        }
        let row: serde_json::Value = match serde_json::from_str(s) {
            Ok(v) => v,
            Err(_) => {
                parse_error_count += 1;
                if parse_error_lines.len() < 5 {
                    parse_error_lines.push(idx + 1);
                }
                continue;
            }
        };
        match row.get("type").and_then(|v| v.as_str()) {
            Some("meta") => {
                if meta.is_none() {
                    meta = Some(row);
                }
            }
            Some("message") => messages.push(row),
            _ => {}
        }
    }

    let parse_warning = if parse_error_count > 0 {
        let sample_line_str = if parse_error_lines.is_empty() {
            String::new()
        } else {
            format!(
                "（示例行: {}）",
                parse_error_lines
                    .iter()
                    .map(|n| n.to_string())
                    .collect::<Vec<String>>()
                    .join(", ")
            )
        };
        let warning = format!(
            "本地会话数据有 {} 行解析失败{}，已跳过。建议点击该会话右侧同步按钮修复。",
            parse_error_count, sample_line_str
        );
        eprintln!(
            "[load_conversation_detail] account={} conversation={} parse_errors={} lines={:?}",
            account_id, bare_id, parse_error_count, parse_error_lines
        );
        Some(warning)
    } else {
        None
    };

    // 为每个 attachment 注入 size 字段（从 media 目录查找文件大小）
    let media_dir = data_dir
        .join("accounts")
        .join(&account_id)
        .join("media");
    for msg in messages.iter_mut() {
        if let Some(atts) = msg.get_mut("attachments").and_then(|v| v.as_array_mut()) {
            for att in atts.iter_mut() {
                if let Some(obj) = att.as_object_mut() {
                    if !obj.contains_key("size") {
                        if let Some(media_id) = obj.get("mediaId").and_then(|v| v.as_str()) {
                            if !media_id.is_empty() {
                                let file_path = media_dir.join(media_id);
                                if let Ok(meta_fs) = std::fs::metadata(&file_path) {
                                    obj.insert("size".to_string(), serde_json::Value::Number(meta_fs.len().into()));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let meta_val = meta.unwrap_or_else(|| serde_json::json!({}));
    let title = meta_val
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let created_at = meta_val
        .get("createdAt")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let updated_at = meta_val
        .get("updatedAt")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let remote_hash = meta_val.get("remoteHash").cloned().unwrap_or(serde_json::Value::Null);
    let account_id_meta = meta_val
        .get("accountId")
        .and_then(|v| v.as_str())
        .unwrap_or(&account_id)
        .to_string();

    let conversation = serde_json::json!({
        "id": bare_id,
        "accountId": account_id_meta,
        "title": title,
        "createdAt": created_at,
        "updatedAt": updated_at,
        "remoteHash": remote_hash,
        "parseWarning": parse_warning,
        "messages": messages,
    });

    serde_json::to_string(&conversation).map_err(|e| e.to_string())
}

// ============================================================================
// 应用入口
// ============================================================================

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| -> Result<(), Box<dyn std::error::Error>> {
            let app_handle = app.handle().clone();
            let output_dir = app_handle.path().app_data_dir()?;

            worker_host::init_worker_host(app_handle, output_dir)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            load_accounts,
            run_accounts_import,
            enqueue_job,
            cancel_job,
            export::get_account_export_stats,
            export::get_account_range_bytes,
            export::export_account_zip,
            export::export_account_kelivo,
            export::export_account_kelivo_split,
            import::import_account_zip,
            clear_account_data,
            delete_conversation,
            load_conversation_summaries,
            get_account_media_dir,
            load_conversation_detail,
            search_conversations,
            rebuild_search_index,
            update_search_index
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|_app, event| {
        if let tauri::RunEvent::ExitRequested { .. } = event {
            worker_host::shutdown_worker_host();
        }
    });
}
