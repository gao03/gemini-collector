//! 账号数据导出：原始 ZIP 打包 + Kelivo 格式转换。

use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, FixedOffset, Local};
use tauri::Manager;

use crate::storage;
use crate::str_err::ToStringErr;

// ============================================================================
// 共享工具（从 lib.rs 迁入）
// ============================================================================

pub(crate) fn resolve_account_id_arg(
    account_id: Option<String>,
    account_id_camel: Option<String>,
) -> Result<String, String> {
    account_id
        .or(account_id_camel)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "缺少 account_id/accountId 参数".to_string())
}

pub(crate) fn value_to_non_empty_string(v: Option<&serde_json::Value>) -> Option<String> {
    match v {
        Some(serde_json::Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

// ============================================================================
// 导出统计
// ============================================================================

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct AccountExportStats {
    account_id: String,
    conversation_count: u64,
    conversation_file_count: u64,
    media_file_count: u64,
    total_file_count: u64,
    total_bytes: u64,
    estimated_zip_bytes: u64,
}

fn sanitize_file_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_control() || matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            out.push('_');
            continue;
        }
        if ch.is_whitespace() {
            out.push('_');
            continue;
        }
        out.push(ch);
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "account".to_string()
    } else {
        trimmed.to_string()
    }
}

fn count_files_and_bytes_recursive(root: &Path) -> Result<(u64, u64), String> {
    if !root.exists() {
        return Ok((0, 0));
    }

    let mut files: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).str_err()? {
            let entry = entry.str_err()?;
            let file_type = entry.file_type().str_err()?;
            if file_type.is_dir() {
                stack.push(entry.path());
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            files += 1;
            total_bytes += entry.metadata().str_err()?.len();
        }
    }

    Ok((files, total_bytes))
}

fn account_export_user_label(account_dir: &Path, account_id: &str) -> String {
    let meta_file = account_dir.join("meta.json");
    if meta_file.exists() {
        if let Ok(raw) = std::fs::read_to_string(&meta_file) {
            if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(email) = value_to_non_empty_string(meta.get("email")) {
                    let name = email.split('@').next().unwrap_or("").trim();
                    if !name.is_empty() {
                        return name.to_string();
                    }
                }
                if let Some(name) = value_to_non_empty_string(meta.get("name")) {
                    if !name.is_empty() {
                        return name;
                    }
                }
            }
        }
    }
    account_id.to_string()
}

fn build_account_export_stats(
    account_dir: &Path,
    account_id: &str,
) -> Result<AccountExportStats, String> {
    let conversations_dir = account_dir.join("conversations");
    let media_dir = account_dir.join("media");

    let conversation_file_count = storage::count_jsonl_files(&conversations_dir)?;
    let media_file_count = count_files_and_bytes_recursive(&media_dir)?.0;
    let (total_file_count, total_bytes) = count_files_and_bytes_recursive(account_dir)?;
    let conversation_count =
        storage::conversation_count_from_index(account_dir).unwrap_or(conversation_file_count);
    let estimated_zip_bytes = if total_bytes == 0 {
        0
    } else {
        ((total_bytes as f64) * 0.62).round() as u64
    };

    Ok(AccountExportStats {
        account_id: account_id.to_string(),
        conversation_count,
        conversation_file_count,
        media_file_count,
        total_file_count,
        total_bytes,
        estimated_zip_bytes,
    })
}

// ============================================================================
// ZIP 打包
// ============================================================================

/// 已压缩的媒体扩展名，使用 Stored 避免无效 Deflate
fn should_store(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some(
            "jpg"
                | "jpeg"
                | "png"
                | "gif"
                | "webp"
                | "avif"
                | "heic"
                | "heif"
                | "mp4"
                | "webm"
                | "mov"
                | "avi"
                | "mkv"
                | "mp3"
                | "aac"
                | "ogg"
                | "opus"
                | "flac"
                | "zip"
                | "gz"
                | "zst"
                | "br"
                | "xz"
                | "bz2"
        )
    )
}

fn zip_opts_deflate() -> zip::write::SimpleFileOptions {
    zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)
}

fn zip_opts_stored() -> zip::write::SimpleFileOptions {
    zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)
}

fn zip_account_dir(account_dir: &Path, zip_path: &Path) -> Result<(), String> {
    let folder_name = account_dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "账号目录名称异常".to_string())?;

    let file = std::fs::File::create(zip_path).map_err(|e| format!("创建 zip 文件失败: {}", e))?;
    let mut zip_writer = zip::ZipWriter::new(file);
    let opts_deflate = zip_opts_deflate();
    let opts_stored = zip_opts_stored();

    // 递归收集所有文件
    let mut entries: Vec<std::path::PathBuf> = Vec::new();
    collect_files(account_dir, &mut entries).map_err(|e| format!("遍历目录失败: {}", e))?;

    for entry_path in &entries {
        let rel = entry_path
            .strip_prefix(account_dir)
            .map_err(|e| format!("路径计算失败: {}", e))?;
        // zip 内路径以 folder_name/ 为前缀
        let zip_entry_name = format!(
            "{}/{}",
            folder_name,
            rel.to_string_lossy().replace('\\', "/")
        );

        if entry_path.is_dir() {
            zip_writer
                .add_directory(&zip_entry_name, opts_stored)
                .map_err(|e| format!("添加目录失败: {}", e))?;
        } else {
            let opts = if should_store(entry_path) {
                opts_stored
            } else {
                opts_deflate
            };
            zip_writer
                .start_file(&zip_entry_name, opts)
                .map_err(|e| format!("添加文件失败: {}", e))?;
            let mut f =
                std::fs::File::open(entry_path).map_err(|e| format!("打开文件失败: {}", e))?;
            std::io::copy(&mut f, &mut zip_writer).map_err(|e| format!("写入 zip 失败: {}", e))?;
        }
    }

    zip_writer
        .finish()
        .map_err(|e| format!("zip 完成失败: {}", e))?;
    Ok(())
}

fn collect_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

// ============================================================================
// Obsidian 导出
// ============================================================================

/// 清理文件名，确保跨平台兼容
/// 遵循最严格的交集规则，兼容 Windows、macOS、Linux、Android
fn sanitize_filename(name: &str) -> String {
    // Windows 保留名称（不区分大小写）
    const RESERVED_NAMES: &[&str] = &[
        "CON", "PRN", "AUX", "NUL",
        "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
        "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];

    let mut result = String::with_capacity(name.len());

    for ch in name.chars() {
        match ch {
            // Windows 禁用字符: < > : " / \ | ? *
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => result.push('-'),
            // 中括号、圆括号、花括号（虽然技术上允许，但会导致 Shell 脚本问题）
            '[' | ']' | '(' | ')' | '{' | '}' => result.push('-'),
            // 控制字符（包括 \0）
            c if c.is_control() => result.push('_'),
            // 空格替换为下划线（避免 Shell 脚本问题）
            ' ' => result.push('_'),
            // 其他字符保留（包括中文、数字、字母、下划线、连字符）
            c => result.push(c),
        }
    }

    // 移除首尾的空格、点号、下划线、连字符
    let trimmed = result.trim_matches(|c: char| {
        c.is_whitespace() || c == '.' || c == '_' || c == '-'
    });

    // 检查是否为空
    if trimmed.is_empty() {
        return "untitled".to_string();
    }

    // 检查是否为 Windows 保留名称
    let name_upper = trimmed.to_uppercase();
    let base_name = name_upper.split('.').next().unwrap_or("");
    if RESERVED_NAMES.contains(&base_name) {
        return format!("_{}", trimmed);
    }

    // 限制长度
    // Windows: 255 字符
    // macOS: 255 字符
    // Linux: 255 字节（UTF-8，中文约 85 个字符）
    // 为了兼容性，限制为 100 个字符（留出扩展名空间）
    let max_len = 100;
    if trimmed.chars().count() > max_len {
        trimmed.chars().take(max_len).collect()
    } else {
        trimmed.to_string()
    }
}

/// 转义 Markdown 中的特殊字符（在普通文本中）
#[allow(dead_code)]
fn escape_markdown_text(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

/// 清理标签，只保留 Obsidian 允许的字符
/// Obsidian 标签允许：字母、数字、下划线(_)、连字符(-)、斜杠(/)
/// 不允许：空格、中括号[]、冒号:、井号#、等其他特殊字符
fn sanitize_tag(tag: &str) -> String {
    // 先移除所有不允许的字符
    let cleaned: String = tag.chars()
        .filter(|c| {
            // 只保留字母、数字和明确允许的符号
            c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '/'
        })
        .collect();

    // 去除首尾的特殊字符（不能以这些字符开头或结尾）
    let trimmed = cleaned.trim_matches(|c: char| c == '-' || c == '_' || c == '/');

    // 如果清理后为空，返回空字符串
    if trimmed.is_empty() {
        return String::new();
    }

    trimmed.to_string()
}

/// 从标题生成标签（不包含标题本身，只包含关键词）
fn generate_tags_from_title(title: &str) -> Vec<String> {
    let mut tags = vec!["gemini".to_string(), "conversation".to_string()];

    // 提取可能的关键词作为标签
    let keywords = [
        ("deep research", "deep-research"),
        ("研究", "research"),
        ("代码", "code"),
        ("编程", "programming"),
        ("问答", "qa"),
        ("教程", "tutorial"),
    ];

    let title_lower = title.to_lowercase();
    for (keyword, tag) in keywords.iter() {
        if title_lower.contains(keyword) {
            tags.push(tag.to_string());
        }
    }

    // 不再从标题生成额外的标签，避免特殊字符问题
    tags
}

/// 转义 YAML 值中的特殊字符
fn escape_yaml_value(value: &str) -> String {
    if value.contains(':') || value.contains('#') || value.contains('"') || value.contains('\'') {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        value.to_string()
    }
}

/// 格式化时间为可读格式（用于显示）
fn format_readable_datetime(iso_str: &str) -> String {
    if let Ok(dt) = DateTime::parse_from_rfc3339(iso_str) {
        let local = dt.with_timezone(&Local);
        local.format("%Y-%m-%d %H:%M:%S").to_string()
    } else {
        iso_str.to_string()
    }
}

/// 生成简短的别名（用于 Obsidian aliases）
fn generate_aliases(title: &str) -> Vec<String> {
    let mut aliases = Vec::new();

    // 如果标题太长，生成短标题
    if title.len() > 50 {
        let short_title: String = title.chars().take(50).collect();
        aliases.push(format!("{}...", short_title.trim()));
    }

    // 如果标题包含特定模式，添加简化版本
    if title.to_lowercase().contains("deep research") {
        aliases.push(title.replace("Deep Research - ", "").replace("Deep Research: ", ""));
    }

    aliases
}

/// 生成消息的 block ID（基于消息索引）
fn generate_block_id(_msg_id: &str, index: usize) -> String {
    // 使用消息索引生成简单的 block ID
    format!("msg-{}", index)
}

/// 构建 Obsidian Markdown 内容
fn build_obsidian_content(
    meta: &serde_json::Value,
    messages: &[serde_json::Value],
    account_email: &str,
) -> Result<String, String> {
    let title = meta
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Untitled Conversation");
    let conv_id = meta.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let created_at = meta.get("createdAt").and_then(|v| v.as_str()).unwrap_or("");
    let updated_at = meta.get("updatedAt").and_then(|v| v.as_str()).unwrap_or("");

    // 统计消息数量和是否包含 Deep Research
    let user_msg_count = messages.iter().filter(|m| {
        m.get("role").and_then(|v| v.as_str()) == Some("user") &&
        !m.get("hidden").and_then(|v| v.as_bool()).unwrap_or(false)
    }).count();
    let assistant_msg_count = messages.iter().filter(|m| {
        m.get("role").and_then(|v| v.as_str()) == Some("model") &&
        !m.get("hidden").and_then(|v| v.as_bool()).unwrap_or(false)
    }).count();
    let has_deep_research = messages.iter().any(|m| {
        m.get("deep_research_articles").is_some() || m.get("deep_research_plan").is_some()
    });
    let has_thinking = messages.iter().any(|m| {
        m.get("thinking").and_then(|v| v.as_str()).map(|s| !s.trim().is_empty()).unwrap_or(false)
    });

    let mut content = String::new();

    // YAML Frontmatter
    content.push_str("---\n");

    // 使用 date 字段（Obsidian 常用）
    if !created_at.is_empty() {
        content.push_str(&format!("date: {}\n", created_at));
    }

    content.push_str(&format!("updated: {}\n", updated_at));
    content.push_str(&format!("account: {}\n", escape_yaml_value(account_email)));
    content.push_str(&format!("link: https://gemini.google.com/app/{}\n", conv_id));

    // 添加别名
    let aliases = generate_aliases(title);
    if !aliases.is_empty() {
        content.push_str("aliases:\n");
        for alias in aliases {
            content.push_str(&format!("  - {}\n", escape_yaml_value(&alias)));
        }
    }

    // 标签使用 YAML 数组格式（更标准），并清理标签
    let mut tags = generate_tags_from_title(title);
    if has_deep_research {
        tags.push("deep-research".to_string());
    }
    content.push_str("tags:\n");
    for tag in tags {
        let clean_tag = sanitize_tag(&tag);
        if !clean_tag.is_empty() {
            content.push_str(&format!("  - {}\n", clean_tag));
        }
    }

    content.push_str("---\n\n");

    // 添加元数据注释
    content.push_str("%%\n");
    content.push_str(&format!("Gemini Collector Export\n"));
    content.push_str(&format!("Conversation ID: {}\n", conv_id));
    content.push_str(&format!("Messages: {} user, {} assistant\n", user_msg_count, assistant_msg_count));
    if has_deep_research {
        content.push_str("Contains: Deep Research\n");
    }
    if has_thinking {
        content.push_str("Contains: Thinking Process\n");
    }
    content.push_str("%%\n\n");

    // 标题
    content.push_str(&format!("# {}\n\n", title));

    // 对话记录
    content.push_str("## 📝 对话记录\n\n");

    let mut msg_index = 0;
    for msg in messages {
        // 跳过隐藏消息
        if msg.get("hidden").and_then(|v| v.as_bool()).unwrap_or(false) {
            continue;
        }

        msg_index += 1;
        let _msg_id = msg.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let text = msg.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let timestamp = msg.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
        let model = msg.get("model").and_then(|v| v.as_str());
        let thinking = msg.get("thinking").and_then(|v| v.as_str());

        // 消息头部
        let role_emoji = if role == "model" { "🤖" } else { "👤" };
        let role_name = if role == "model" { "Assistant" } else { "User" };
        let formatted_time = format_readable_datetime(timestamp);

        content.push_str(&format!("### {} {} ({})\n\n", role_emoji, role_name, formatted_time));

        // 模型信息（使用 highlight 语法强调）
        if let Some(model_name) = model {
            content.push_str(&format!("%%\n==Model: {}==%%\n\n", model_name));
        }

        // 思考过程（可折叠，使用 tip callout）
        if let Some(thinking_text) = thinking {
            if !thinking_text.trim().is_empty() {
                content.push_str("> [!tip]- 💭 思考过程\n");
                for line in thinking_text.lines() {
                    content.push_str(&format!("> {}\n", line));
                }
                content.push_str("\n");
            }
        }

        // 消息内容
        content.push_str(text);
        content.push_str("\n\n");

        // 附件
        if let Some(attachments) = msg.get("attachments").and_then(|v| v.as_array()) {
            for att in attachments {
                let media_id = att.get("mediaId").and_then(|v| v.as_str()).unwrap_or("");
                if media_id.is_empty() {
                    continue;
                }
                let mime = att.get("mimeType").and_then(|v| v.as_str()).unwrap_or("");

                if mime.starts_with("image/") {
                    content.push_str(&format!("![[attachments/{}]]\n\n", media_id));
                } else if mime.starts_with("video/") {
                    content.push_str(&format!("![[attachments/{}]]\n\n", media_id));
                } else if mime.starts_with("audio/") {
                    content.push_str(&format!("![[attachments/{}]]\n\n", media_id));
                } else {
                    content.push_str(&format!("[📎 {}](attachments/{})\n\n", media_id, media_id));
                }
            }
        }

        // Deep Research Plan（使用 info callout，可折叠）
        if let Some(plan) = msg.get("deep_research_plan") {
            if !plan.is_null() {
                if let Some(plan_title) = plan.get("title").and_then(|v| v.as_str()) {
                    content.push_str("> [!info]- 🔍 研究方案\n");
                    content.push_str(&format!("> =={}==\n>\n", plan_title));
                    if let Some(steps) = plan.get("steps").and_then(|v| v.as_str()) {
                        for line in steps.lines() {
                            content.push_str(&format!("> {}\n", line));
                        }
                    }
                    content.push_str("\n");
                }
            }
        }

        // Deep Research Articles（使用 abstract callout，可折叠）
        if let Some(articles) = msg.get("deep_research_articles").and_then(|v| v.as_array()) {
            if !articles.is_empty() {
                content.push_str("> [!abstract]- 📚 研究文章\n");
                content.push_str(&format!("> 共 =={}== 篇文章\n\n", articles.len()));

                for (i, article) in articles.iter().enumerate() {
                    if let Some(article_title) = article.get("title").and_then(|v| v.as_str()) {
                        content.push_str(&format!("#### {}. {}\n\n", i + 1, article_title));
                        if let Some(article_content) = article.get("article_markdown").and_then(|v| v.as_str()) {
                            content.push_str(article_content);
                            content.push_str("\n\n");
                        }
                    }
                }
            }
        }

        // 添加 Block ID
        let block_id = generate_block_id("", msg_index);
        content.push_str(&format!("^{}\n\n", block_id));

        content.push_str("---\n\n");
    }

    Ok(content)
}

/// 解析对话并生成 Obsidian 项
struct ObsidianItem {
    #[allow(dead_code)]
    conv_id: String,
    title: String,
    markdown_content: String,
    media_ids: Vec<String>,
    #[allow(dead_code)]
    created_at: String,
    updated_at: String,
}

fn parse_obsidian_jsonl(
    path: &Path,
    account_email: &str,
    after_date: Option<&str>,
) -> Result<Option<ObsidianItem>, String> {
    let raw = std::fs::read_to_string(path).str_err()?;
    let mut meta: Option<serde_json::Value> = None;
    let mut messages: Vec<serde_json::Value> = Vec::new();

    for line in raw.lines() {
        let s = line.trim();
        if s.is_empty() {
            continue;
        }
        let obj: serde_json::Value = serde_json::from_str(s).str_err()?;
        match obj.get("type").and_then(|v| v.as_str()) {
            Some("meta") => {
                if meta.is_none() {
                    meta = Some(obj);
                }
            }
            Some("message") => messages.push(obj),
            _ => {}
        }
    }

    let meta = match meta {
        Some(m) => m,
        None => return Ok(None),
    };

    // 时间过滤
    if let Some(after) = after_date {
        let updated_at = meta.get("updatedAt").and_then(|v| v.as_str()).unwrap_or("");
        if !updated_at.is_empty() && updated_at < after {
            return Ok(None);
        }
    }

    let conv_id = meta.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let title = meta.get("title").and_then(|v| v.as_str()).unwrap_or("Untitled").to_string();
    let created_at = meta.get("createdAt").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let updated_at = meta.get("updatedAt").and_then(|v| v.as_str()).unwrap_or("").to_string();

    // 收集媒体文件
    let mut media_ids_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    for msg in &messages {
        if let Some(attachments) = msg.get("attachments").and_then(|v| v.as_array()) {
            for att in attachments {
                if let Some(mid) = att.get("mediaId").and_then(|v| v.as_str()) {
                    if !mid.is_empty() {
                        media_ids_set.insert(mid.to_string());
                    }
                }
            }
        }
    }

    let markdown_content = build_obsidian_content(&meta, &messages, account_email)?;

    Ok(Some(ObsidianItem {
        conv_id,
        title,
        markdown_content,
        media_ids: media_ids_set.into_iter().collect(),
        created_at,
        updated_at,
    }))
}

/// 执行 Obsidian 导出
fn obsidian_export_impl(
    data_dir: &Path,
    account_id: &str,
    output_dir: &Path,
    after_date: Option<&str>,
) -> Result<String, String> {
    let account_dir = data_dir.join("accounts").join(account_id);
    let conv_dir = account_dir.join("conversations");
    let media_dir = account_dir.join("media");

    if !conv_dir.exists() {
        return Err(format!("对话目录不存在: {}", conv_dir.display()));
    }

    // 获取账号邮箱
    let meta_file = account_dir.join("meta.json");
    let account_email = if meta_file.exists() {
        if let Ok(raw) = std::fs::read_to_string(&meta_file) {
            if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&raw) {
                value_to_non_empty_string(meta.get("email")).unwrap_or_else(|| account_id.to_string())
            } else {
                account_id.to_string()
            }
        } else {
            account_id.to_string()
        }
    } else {
        account_id.to_string()
    };

    // 创建输出目录结构
    let conversations_dir = output_dir.join("conversations");
    let attachments_dir = output_dir.join("attachments");
    std::fs::create_dir_all(&conversations_dir).str_err()?;
    std::fs::create_dir_all(&attachments_dir).str_err()?;

    // 收集所有对话文件
    let mut jsonl_files: Vec<PathBuf> = std::fs::read_dir(&conv_dir)
        .str_err()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| storage::is_jsonl_file(p))
        .collect();
    jsonl_files.sort();

    let mut items: Vec<ObsidianItem> = Vec::new();
    let mut skipped = 0usize;

    // 解析所有对话
    for jsonl_path in &jsonl_files {
        match parse_obsidian_jsonl(jsonl_path, &account_email, after_date) {
            Ok(Some(item)) => items.push(item),
            Ok(None) => skipped += 1,
            Err(_) => skipped += 1,
        }
    }

    // 写入 Markdown 文件
    let mut filename_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut total_media_copied = 0usize;
    let mut total_media_missing = 0usize;

    for item in &items {
        // 生成唯一文件名
        let base_filename = sanitize_filename(&item.title);
        let count = filename_counts.entry(base_filename.clone()).or_insert(0);
        *count += 1;

        let filename = if *count == 1 {
            format!("{}.md", base_filename)
        } else {
            format!("{} {}.md", base_filename, count)
        };

        let md_path = conversations_dir.join(&filename);
        std::fs::write(&md_path, &item.markdown_content).str_err()?;

        // 复制媒体文件
        for media_id in &item.media_ids {
            let src = media_dir.join(media_id);
            let dst = attachments_dir.join(media_id);

            if src.exists() {
                if !dst.exists() {
                    std::fs::copy(&src, &dst).str_err()?;
                    total_media_copied += 1;
                }
            } else {
                total_media_missing += 1;
            }
        }
    }

    // 生成索引文件
    let mut index_content = String::new();
    index_content.push_str("---\n");
    index_content.push_str("title: Gemini 对话索引\n");
    index_content.push_str(&format!("date: {}\n", Local::now().to_rfc3339()));
    index_content.push_str(&format!("account: {}\n", escape_yaml_value(&account_email)));
    index_content.push_str("tags:\n");
    index_content.push_str("  - gemini\n");
    index_content.push_str("  - index\n");
    index_content.push_str("---\n\n");
    index_content.push_str("# Gemini 对话索引\n\n");
    index_content.push_str(&format!("**账号**: {}\n\n", account_email));
    index_content.push_str(&format!("**对话数量**: {}\n\n", items.len()));
    index_content.push_str(&format!("**导出时间**: {}\n\n", Local::now().format("%Y-%m-%d %H:%M:%S")));
    index_content.push_str("## 📋 对话列表\n\n");

    // 按更新时间排序
    let mut sorted_items: Vec<&ObsidianItem> = items.iter().collect();
    sorted_items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    for item in sorted_items {
        let formatted_time = format_readable_datetime(&item.updated_at);
        let base_filename = sanitize_filename(&item.title);
        let count = filename_counts.get(&base_filename).unwrap_or(&1);
        let filename = if *count == 1 {
            format!("{}.md", base_filename)
        } else {
            format!("{} {}.md", base_filename, count)
        };
        index_content.push_str(&format!("- [[conversations/{}|{}]] - {}\n",
            filename.trim_end_matches(".md"), item.title, formatted_time));
    }

    let index_path = output_dir.join("_index.md");
    std::fs::write(&index_path, &index_content).str_err()?;

    // 生成统计信息
    let summary = format!(
        "[信息] 成功导出: {} 个对话，跳过 {}\n\
         [媒体] 复制 {} 个文件，缺失 {} 个\n\
         [完成] 输出目录: {}",
        items.len(),
        skipped,
        total_media_copied,
        total_media_missing,
        output_dir.display()
    );

    Ok(summary)
}

#[tauri::command]
pub async fn export_account_obsidian(
    app: tauri::AppHandle,
    account_id: String,
    output_path: String,
    after_date: Option<String>,
) -> Result<String, String> {
    let data_dir = app.path().app_data_dir().str_err()?;
    let output_dir = PathBuf::from(&output_path);
    let after = after_date.clone();

    tauri::async_runtime::spawn_blocking(move || {
        obsidian_export_impl(&data_dir, &account_id, &output_dir, after.as_deref())
    })
    .await
    .str_err()?
}

// ============================================================================
// Tauri 导出命令
// ============================================================================

#[tauri::command]
pub fn get_account_range_bytes(
    app: tauri::AppHandle,
    account_id: Option<String>,
    #[allow(non_snake_case)] accountId: Option<String>,
    after_date: Option<String>,
    #[allow(non_snake_case)] afterDate: Option<String>,
) -> Result<String, String> {
    let account_id = resolve_account_id_arg(account_id, accountId)?;
    let data_dir = app.path().app_data_dir().str_err()?;
    let account_dir = data_dir.join("accounts").join(&account_id);
    if !account_dir.exists() {
        return Err(format!("账号目录不存在: {}", account_id));
    }
    let conversations_dir = account_dir.join("conversations");
    let media_dir = account_dir.join("media");

    let after = after_date
        .or(afterDate)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    if !conversations_dir.exists() {
        let result = serde_json::json!({ "totalBytes": 0u64 });
        return serde_json::to_string(&result).str_err();
    }

    let mut total_bytes: u64 = 0;

    for entry in std::fs::read_dir(&conversations_dir).str_err()? {
        let entry = entry.str_err()?;
        let path = entry.path();
        if !storage::is_jsonl_file(&path) {
            continue;
        }

        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let mut updated_at: Option<String> = None;
        let mut media_ids: Vec<String> = Vec::new();

        for line in raw.lines() {
            let s = line.trim();
            if s.is_empty() {
                continue;
            }
            let obj: serde_json::Value = match serde_json::from_str(s) {
                Ok(v) => v,
                Err(_) => continue,
            };
            match obj.get("type").and_then(|v| v.as_str()) {
                Some("meta") => {
                    if updated_at.is_none() {
                        updated_at = obj
                            .get("updatedAt")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                    }
                }
                Some("message") => {
                    if let Some(atts) = obj.get("attachments").and_then(|v| v.as_array()) {
                        for att in atts {
                            if let Some(mid) = att.get("mediaId").and_then(|v| v.as_str()) {
                                if !mid.is_empty() {
                                    media_ids.push(mid.to_string());
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        if let Some(ref after_str) = after {
            let conv_updated = updated_at.as_deref().unwrap_or("");
            if !conv_updated.is_empty() && conv_updated < after_str.as_str() {
                continue;
            }
        }

        for mid in &media_ids {
            let file_path = media_dir.join(mid);
            if let Ok(meta_fs) = std::fs::metadata(&file_path) {
                total_bytes += meta_fs.len();
            }
        }
    }

    let result = serde_json::json!({ "totalBytes": total_bytes });
    serde_json::to_string(&result).str_err()
}

#[tauri::command]
pub fn get_account_export_stats(
    app: tauri::AppHandle,
    account_id: Option<String>,
    #[allow(non_snake_case)] accountId: Option<String>,
) -> Result<String, String> {
    let account_id = resolve_account_id_arg(account_id, accountId)?;
    let data_dir = app.path().app_data_dir().str_err()?;
    let account_dir = data_dir.join("accounts").join(&account_id);
    if !account_dir.exists() {
        return Err(format!("账号目录不存在: {}", account_id));
    }
    let stats = build_account_export_stats(&account_dir, &account_id)?;
    serde_json::to_string(&stats).str_err()
}

#[tauri::command]
pub fn export_account_zip(
    app: tauri::AppHandle,
    account_id: Option<String>,
    #[allow(non_snake_case)] accountId: Option<String>,
    output_dir: Option<String>,
    #[allow(non_snake_case)] outputDir: Option<String>,
) -> Result<String, String> {
    let account_id = resolve_account_id_arg(account_id, accountId)?;
    let data_dir = app.path().app_data_dir().str_err()?;
    let account_dir = data_dir.join("accounts").join(&account_id);
    if !account_dir.exists() {
        return Err(format!("账号目录不存在: {}", account_id));
    }

    let stats = build_account_export_stats(&account_dir, &account_id)?;
    let user_label = account_export_user_label(&account_dir, &account_id);
    let timestamp = Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let file_name = format!(
        "gemini-{}-{}.zip",
        sanitize_file_component(&user_label),
        timestamp
    );

    let preferred_export_dir = output_dir
        .or(outputDir)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);

    let export_dir = preferred_export_dir
        .unwrap_or_else(|| dirs::download_dir().unwrap_or_else(|| data_dir.join("exports")));
    if !export_dir.exists() {
        std::fs::create_dir_all(&export_dir).str_err()?;
    }
    if !export_dir.is_dir() {
        return Err(format!("导出目录不可用: {}", export_dir.display()));
    }

    let zip_path = export_dir.join(file_name);
    if zip_path.exists() {
        std::fs::remove_file(&zip_path).str_err()?;
    }

    zip_account_dir(&account_dir, &zip_path)?;
    let zip_size_bytes = std::fs::metadata(&zip_path).str_err()?.len();

    let result = serde_json::json!({
        "accountId": account_id,
        "zipPath": zip_path.to_string_lossy().to_string(),
        "fileName": zip_path.file_name().and_then(|s| s.to_str()).unwrap_or("export.zip"),
        "zipSizeBytes": zip_size_bytes,
        "conversationCount": stats.conversation_count,
        "conversationFileCount": stats.conversation_file_count,
        "mediaFileCount": stats.media_file_count,
        "totalFileCount": stats.total_file_count,
        "totalBytes": stats.total_bytes,
        "estimatedZipBytes": stats.estimated_zip_bytes,
    });
    serde_json::to_string(&result).str_err()
}

// ============================================================================
// Kelivo 导出
// ============================================================================

fn to_cst(utc_str: &str) -> String {
    let cst = FixedOffset::east_opt(8 * 3600).unwrap();
    if let Ok(dt) = DateTime::parse_from_rfc3339(utc_str) {
        let cst_dt = dt.with_timezone(&cst);
        return format!("{}+00:00", cst_dt.format("%Y-%m-%dT%H:%M:%S"));
    }
    utc_str.to_string()
}

fn to_cst_value(v: &serde_json::Value) -> serde_json::Value {
    match v.as_str() {
        Some(s) => serde_json::Value::String(to_cst(s)),
        None => v.clone(),
    }
}

fn parse_size(s: &str) -> Result<u64, String> {
    let upper = s.trim().to_uppercase();
    for (suffix, mult) in &[
        ("GB", 1u64 << 30),
        ("MB", 1u64 << 20),
        ("KB", 1u64 << 10),
        ("B", 1u64),
    ] {
        if upper.ends_with(suffix) {
            let num_str = upper[..upper.len() - suffix.len()].trim();
            let val: f64 = num_str
                .parse()
                .map_err(|_| format!("无法解析大小: {}", s))?;
            return Ok((val * (*mult as f64)).round() as u64);
        }
    }
    s.trim()
        .parse::<u64>()
        .map_err(|_| format!("无法解析大小: {}", s))
}

fn idx_to_label(mut n: usize) -> String {
    let mut label = String::new();
    n += 1;
    while n > 0 {
        let r = (n - 1) % 26;
        label.insert(0, (b'a' + r as u8) as char);
        n = (n - 1) / 26;
    }
    label
}

fn build_kelivo_content(text: &str, attachments: &[serde_json::Value], msg: &serde_json::Value) -> String {
    let mut parts: Vec<String> = vec![text.to_string()];

    // 添加附件
    for att in attachments {
        let media_id = att.get("mediaId").and_then(|v| v.as_str()).unwrap_or("");
        if media_id.is_empty() {
            continue;
        }
        let mime = att
            .get("mimeType")
            .and_then(|v| v.as_str())
            .unwrap_or("application/octet-stream");
        if mime.starts_with("image/") {
            parts.push(format!("[image:/upload/{}]", media_id));
        } else {
            parts.push(format!(
                "[file:/upload/{mid}|{mid}|{mime}]",
                mid = media_id,
                mime = mime
            ));
        }
    }

    // 添加 Deep Research Plan（研究方案）
    if let Some(plan) = msg.get("deep_research_plan") {
        if !plan.is_null() {
            if let Some(title) = plan.get("title").and_then(|v| v.as_str()) {
                parts.push(format!("\n---\n## 🔍 研究方案: {}\n", title));
                if let Some(steps) = plan.get("steps").and_then(|v| v.as_str()) {
                    parts.push(steps.to_string());
                }
            }
        }
    }

    // 添加 Deep Research Articles（研究文章）
    if let Some(articles) = msg.get("deep_research_articles") {
        if let Some(arr) = articles.as_array() {
            if !arr.is_empty() {
                parts.push(format!("\n---\n## 📚 研究文章 ({} 篇)\n", arr.len()));
                for (i, article) in arr.iter().enumerate() {
                    if let Some(title) = article.get("title").and_then(|v| v.as_str()) {
                        parts.push(format!("\n### {}. {}\n", i + 1, title));
                        if let Some(content) = article.get("article_markdown").and_then(|v| v.as_str()) {
                            parts.push(content.to_string());
                        }
                    }
                }
            }
        }
    }

    parts.join("\n")
}

struct KelivoItem {
    #[allow(dead_code)]
    conv_id: String,
    kelivo_conv: serde_json::Value,
    kelivo_msgs: Vec<serde_json::Value>,
    media_ids: Vec<String>,
    json_bytes: u64,
    media_bytes: u64,
}

fn parse_kelivo_jsonl(
    path: &Path,
    media_dir: &Path,
    after_date: Option<&str>,
) -> Result<Option<KelivoItem>, String> {
    let raw = std::fs::read_to_string(path).str_err()?;
    let mut meta: Option<serde_json::Value> = None;
    let mut messages: Vec<serde_json::Value> = Vec::new();

    for line in raw.lines() {
        let s = line.trim();
        if s.is_empty() {
            continue;
        }
        let obj: serde_json::Value = serde_json::from_str(s).str_err()?;
        match obj.get("type").and_then(|v| v.as_str()) {
            Some("meta") => {
                if meta.is_none() {
                    meta = Some(obj);
                }
            }
            Some("message") => messages.push(obj),
            _ => {}
        }
    }

    let meta = match meta {
        Some(m) => m,
        None => return Ok(None),
    };

    if let Some(after) = after_date {
        let updated_at = meta.get("updatedAt").and_then(|v| v.as_str()).unwrap_or("");
        if !updated_at.is_empty() && updated_at < after {
            return Ok(None);
        }
    }

    let conv_id = meta
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let title = meta
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let created_at = to_cst_value(
        &meta
            .get("createdAt")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    );
    let updated_at = to_cst_value(
        &meta
            .get("updatedAt")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    );

    let mut kelivo_msgs: Vec<serde_json::Value> = Vec::new();
    let mut message_ids: Vec<serde_json::Value> = Vec::new();
    let mut media_ids_set: std::collections::HashSet<String> = std::collections::HashSet::new();

    for msg in &messages {
        if msg.get("hidden").and_then(|v| v.as_bool()).unwrap_or(false) {
            continue;
        }
        let text = msg.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let msg_id = msg
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let role_raw = msg.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let role = if role_raw == "model" {
            "assistant"
        } else {
            "user"
        };
        let attachments = msg
            .get("attachments")
            .and_then(|v| v.as_array())
            .map(|a| a.as_slice())
            .unwrap_or(&[]);
        let content = build_kelivo_content(text, attachments, msg);
        let timestamp = to_cst_value(
            &msg.get("timestamp")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        );

        for att in attachments {
            if let Some(mid) = att.get("mediaId").and_then(|v| v.as_str()) {
                if !mid.is_empty() {
                    media_ids_set.insert(mid.to_string());
                }
            }
        }

        message_ids.push(serde_json::Value::String(msg_id.clone()));
        kelivo_msgs.push(serde_json::json!({
            "id": msg_id,
            "role": role,
            "content": content,
            "timestamp": timestamp,
            "modelId": msg.get("model").cloned().unwrap_or(serde_json::Value::Null),
            "providerId": "google",
            "totalTokens": null,
            "conversationId": conv_id,
            "isStreaming": false,
            "reasoningText": msg.get("thinking").cloned().unwrap_or(serde_json::Value::Null),
            "reasoningStartAt": null,
            "reasoningFinishedAt": null,
            "translation": null,
            "reasoningSegmentsJson": null,
            "groupId": null,
            "version": 0,
        }));
    }

    let kelivo_conv = serde_json::json!({
        "id": conv_id,
        "title": title,
        "createdAt": created_at,
        "updatedAt": updated_at,
        "messageIds": message_ids,
        "isPinned": false,
        "mcpServerIds": [],
        "assistantId": null,
        "truncateIndex": -1,
        "versionSelections": {},
        "summary": null,
        "lastSummarizedMessageCount": 0,
    });

    let conv_bytes = serde_json::to_string(&kelivo_conv)
        .unwrap_or_default()
        .len() as u64;
    let msgs_bytes: u64 = kelivo_msgs
        .iter()
        .map(|m| serde_json::to_string(m).unwrap_or_default().len() as u64)
        .sum();
    let json_bytes = conv_bytes + msgs_bytes;

    let media_ids: Vec<String> = media_ids_set.into_iter().collect();

    let media_bytes: u64 = media_ids
        .iter()
        .filter_map(|mid| {
            let p = media_dir.join(mid);
            p.metadata().ok().map(|m| m.len())
        })
        .sum();

    Ok(Some(KelivoItem {
        conv_id,
        kelivo_conv,
        kelivo_msgs,
        media_ids,
        json_bytes,
        media_bytes,
    }))
}

fn pack_bins(
    items: Vec<KelivoItem>,
    json_limit: Option<u64>,
    media_limit: Option<u64>,
) -> Vec<Vec<KelivoItem>> {
    if json_limit.is_none() && media_limit.is_none() {
        return vec![items];
    }

    let mut indexed: Vec<(usize, f64)> = items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let jn = json_limit
                .map(|lim| item.json_bytes as f64 / lim as f64)
                .unwrap_or(0.0);
            let mn = media_limit
                .map(|lim| item.media_bytes as f64 / lim as f64)
                .unwrap_or(0.0);
            (i, jn.max(mn))
        })
        .collect();
    indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    struct Bin {
        json_used: u64,
        media_used: u64,
        indices: Vec<usize>,
    }

    let mut bins: Vec<Bin> = Vec::new();

    for (idx, _) in indexed {
        let jb = items[idx].json_bytes;
        let mb = items[idx].media_bytes;
        let exceeds = json_limit.map(|lim| jb > lim).unwrap_or(false)
            || media_limit.map(|lim| mb > lim).unwrap_or(false);

        if exceeds {
            bins.push(Bin {
                json_used: jb,
                media_used: mb,
                indices: vec![idx],
            });
            continue;
        }

        let mut placed = false;
        for bin in &mut bins {
            let json_ok = json_limit
                .map(|lim| bin.json_used + jb <= lim)
                .unwrap_or(true);
            let media_ok = media_limit
                .map(|lim| bin.media_used + mb <= lim)
                .unwrap_or(true);
            if json_ok && media_ok {
                bin.indices.push(idx);
                bin.json_used += jb;
                bin.media_used += mb;
                placed = true;
                break;
            }
        }

        if !placed {
            bins.push(Bin {
                json_used: jb,
                media_used: mb,
                indices: vec![idx],
            });
        }
    }

    let mut items_opt: Vec<Option<KelivoItem>> = items.into_iter().map(Some).collect();
    bins.into_iter()
        .map(|bin| {
            bin.indices
                .into_iter()
                .map(|i| items_opt[i].take().unwrap())
                .collect()
        })
        .collect()
}

fn write_kelivo_zip(
    zip_path: &Path,
    bin_items: &[KelivoItem],
    media_dir: &Path,
) -> Result<(usize, usize, usize, usize), String> {
    let all_convs: Vec<&serde_json::Value> = bin_items.iter().map(|it| &it.kelivo_conv).collect();
    let all_msgs: Vec<&serde_json::Value> = bin_items
        .iter()
        .flat_map(|it| it.kelivo_msgs.iter())
        .collect();
    let mut all_mids: Vec<&str> = bin_items
        .iter()
        .flat_map(|it| it.media_ids.iter().map(|s| s.as_str()))
        .collect();
    all_mids.sort_unstable();
    all_mids.dedup();

    let chats_obj = serde_json::json!({
        "version": 1,
        "conversations": all_convs,
        "messages": all_msgs,
        "toolEvents": {},
        "geminiThoughtSigs": {},
    });
    let chats_json = serde_json::to_string(&chats_obj).str_err()?;

    if let Some(parent) = zip_path.parent() {
        std::fs::create_dir_all(parent).str_err()?;
    }

    let file = std::fs::File::create(zip_path).str_err()?;
    let mut zw = zip::ZipWriter::new(file);
    let opts_deflate = zip_opts_deflate();
    let opts_stored = zip_opts_stored();

    zw.start_file("chats.json", opts_deflate).str_err()?;
    zw.write_all(chats_json.as_bytes()).str_err()?;

    let mut media_found = 0usize;
    let mut media_missing = 0usize;
    for mid in &all_mids {
        let src = media_dir.join(mid);
        if src.exists() {
            let opts = if should_store(&src) {
                opts_stored
            } else {
                opts_deflate
            };
            zw.start_file(format!("upload/{}", mid), opts).str_err()?;
            let mut f = std::fs::File::open(&src).str_err()?;
            std::io::copy(&mut f, &mut zw).str_err()?;
            media_found += 1;
        } else {
            media_missing += 1;
        }
    }

    zw.finish().str_err()?;

    Ok((all_convs.len(), all_msgs.len(), media_found, media_missing))
}

fn kelivo_export_impl(
    data_dir: &Path,
    account_id: &str,
    output_path: &Path,
    json_limit: Option<u64>,
    media_limit: Option<u64>,
    after_date: Option<&str>,
) -> Result<String, String> {
    let account_dir = data_dir.join("accounts").join(account_id);
    let conv_dir = account_dir.join("conversations");
    let media_dir = account_dir.join("media");

    if !conv_dir.exists() {
        return Err(format!("对话目录不存在: {}", conv_dir.display()));
    }

    let mut jsonl_files: Vec<PathBuf> = std::fs::read_dir(&conv_dir)
        .str_err()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| storage::is_jsonl_file(p))
        .collect();
    jsonl_files.sort();

    let mut items: Vec<KelivoItem> = Vec::new();
    let mut skipped = 0usize;

    for jsonl_path in &jsonl_files {
        match parse_kelivo_jsonl(jsonl_path, &media_dir, after_date) {
            Ok(Some(item)) => items.push(item),
            Ok(None) => skipped += 1,
            Err(_) => skipped += 1,
        }
    }

    let total_convs = items.len();
    let total_msgs: usize = items.iter().map(|it| it.kelivo_msgs.len()).sum();

    let bins = pack_bins(items, json_limit, media_limit);
    let multi = bins.len() > 1;

    let stem = output_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("kelivo_backup")
        .to_string();
    let suffix = output_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("zip")
        .to_string();
    let output_dir = output_path.parent().unwrap_or(output_path);

    let mut result_lines: Vec<String> = Vec::new();

    for (idx, bin_items) in bins.iter().enumerate() {
        let zip_path = if multi {
            let label = idx_to_label(idx);
            output_dir.join(format!(
                "{}_{}{}",
                label,
                stem,
                if suffix.is_empty() {
                    String::new()
                } else {
                    format!(".{}", suffix)
                }
            ))
        } else {
            output_path.to_path_buf()
        };

        let (conv_count, msg_count, media_found, media_missing) =
            write_kelivo_zip(&zip_path, bin_items, &media_dir)?;

        let size_mb = std::fs::metadata(&zip_path)
            .map(|m| m.len() as f64 / 1024.0 / 1024.0)
            .unwrap_or(0.0);

        let label_prefix = if multi {
            format!("[{}] ", idx_to_label(idx))
        } else {
            String::new()
        };
        result_lines.push(format!(
            "  {}{}  {} 对话  {} 消息  媒体 {}✓/{}✗  {:.1}MB",
            label_prefix,
            zip_path.file_name().and_then(|s| s.to_str()).unwrap_or(""),
            conv_count,
            msg_count,
            media_found,
            media_missing,
            size_mb,
        ));
    }

    let summary = if multi {
        format!(
            "[信息] 成功转换: {} 对话，{} 条消息，跳过 {}\n{}\n[完成] 共 {} 个包，输出到 {}",
            total_convs,
            total_msgs,
            skipped,
            result_lines.join("\n"),
            bins.len(),
            output_dir.display(),
        )
    } else {
        let zip_path = output_path;
        let size_mb = std::fs::metadata(zip_path)
            .map(|m| m.len() as f64 / 1024.0 / 1024.0)
            .unwrap_or(0.0);
        format!(
            "[信息] 成功转换: {} 对话，{} 条消息，跳过 {}\n{}\n[完成] 输出: {}  ({:.1} MB)",
            total_convs,
            total_msgs,
            skipped,
            result_lines.join("\n"),
            zip_path.display(),
            size_mb,
        )
    };

    Ok(summary)
}

#[tauri::command]
pub async fn export_account_kelivo(
    app: tauri::AppHandle,
    account_id: String,
    output_path: String,
    after_date: Option<String>,
) -> Result<String, String> {
    let data_dir = app.path().app_data_dir().str_err()?;
    let output = PathBuf::from(&output_path);
    let after = after_date.clone();

    tauri::async_runtime::spawn_blocking(move || {
        kelivo_export_impl(
            &data_dir,
            &account_id,
            &output,
            None,
            None,
            after.as_deref(),
        )
    })
    .await
    .str_err()?
}

#[tauri::command]
pub async fn export_account_kelivo_split(
    app: tauri::AppHandle,
    account_id: String,
    output_path: String,
    max_json: Option<String>,
    max_upload: Option<String>,
    after_date: Option<String>,
) -> Result<String, String> {
    let json_limit = match &max_json {
        Some(s) if !s.trim().is_empty() => Some(parse_size(s)?),
        _ => None,
    };
    let media_limit = match &max_upload {
        Some(s) if !s.trim().is_empty() => Some(parse_size(s)?),
        _ => None,
    };

    let data_dir = app.path().app_data_dir().str_err()?;
    let output = PathBuf::from(&output_path);
    let after = after_date.clone();

    tauri::async_runtime::spawn_blocking(move || {
        kelivo_export_impl(
            &data_dir,
            &account_id,
            &output,
            json_limit,
            media_limit,
            after.as_deref(),
        )
    })
    .await
    .str_err()?
}
