//! Gemini 对话轮次解析：turn/media 描述项提取、占位 URL 清理。

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::OnceLock;
use url::Url;

use crate::protocol::to_iso_utc;

// ============================================================================
// 输出类型
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resolution {
    pub width: i64,
    pub height: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaFile {
    pub role: String,
    #[serde(rename = "type")]
    pub media_type: String,
    pub filename: Option<String>,
    pub mime: Option<String>,
    pub url: Option<String>,
    pub thumbnail_url: Option<String>,
    pub duration: Option<f64>,
    pub resolution: Option<Resolution>,
    pub media_id: Option<String>,
    pub preview_media_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MusicMeta {
    pub title: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    #[serde(default)]
    pub moods: Vec<String>,
    pub caption: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenMeta {
    pub model: Option<String>,
    pub prompt: Option<String>,
}

/// 深度研究文章条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepResearchArticle {
    pub composite_id: String,
    pub uuid: String,
    pub title: String,
    pub doc_uuid: String,
    pub article_markdown: String,
}

/// 深度研究方案
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepResearchPlan {
    pub title: String,
    pub steps: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleContent {
    pub text: String,
    pub files: Vec<MediaFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantContent {
    pub text: String,
    pub thinking: String,
    pub model: String,
    pub files: Vec<MediaFile>,
    pub music_meta: Option<MusicMeta>,
    pub gen_meta: Option<GenMeta>,
    pub deep_research_plan: Option<DeepResearchPlan>,
    pub deep_research_articles: Vec<DeepResearchArticle>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedTurn {
    pub turn_id: Option<String>,
    pub timestamp: Option<i64>,
    pub timestamp_iso: Option<String>,
    pub user: RoleContent,
    pub assistant: AssistantContent,
}

// ============================================================================
// Value 索引辅助
// ============================================================================

fn vget(v: &Value, idx: usize) -> Option<&Value> {
    v.as_array()?.get(idx)
}

fn vstr(v: &Value, idx: usize) -> Option<&str> {
    v.as_array()?.get(idx)?.as_str()
}

fn vi64(v: &Value, idx: usize) -> Option<i64> {
    v.as_array()?.get(idx)?.as_i64()
}

fn varr(v: &Value, idx: usize) -> Option<&Vec<Value>> {
    v.as_array()?.get(idx)?.as_array()
}

fn vlen(v: &Value) -> usize {
    v.as_array().map(|a| a.len()).unwrap_or(0)
}

// ============================================================================
// 占位 URL 清理
// ============================================================================

fn placeholder_path_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:^|/)[a-z0-9_]+_content(?:/|$)").unwrap())
}

fn url_extract_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"https?://\S+").unwrap())
}

fn is_internal_placeholder_content_url(url_text: &str) -> bool {
    let candidate = url_text.trim().trim_end_matches(
        &[
            '。', '.', ',', ';', '，', '；', '）', ')', ']', '}', '"', '\'',
        ][..],
    );
    if !candidate.starts_with("https://") && !candidate.starts_with("http://") {
        return false;
    }
    let parsed = match Url::parse(candidate) {
        Ok(u) => u,
        Err(_) => return false,
    };
    let host = parsed.host_str().unwrap_or("").to_lowercase();
    if host != "googleusercontent.com" && !host.ends_with(".googleusercontent.com") {
        return false;
    }
    let path = parsed.path().to_lowercase();
    placeholder_path_re().is_match(&path)
}

fn contains_internal_placeholder_content_url(text_line: &str) -> bool {
    if text_line.is_empty() {
        return false;
    }
    for m in url_extract_re().find_iter(text_line) {
        if is_internal_placeholder_content_url(m.as_str()) {
            return true;
        }
    }
    false
}

/// 在已提取到附件时移除旧占位 URL 文本，避免污染 assistant 正文。
pub fn sanitize_generation_placeholder_text(text: &str, has_attachments: bool) -> String {
    if !has_attachments {
        return text.to_string();
    }
    if !text.contains("_content/") || !text.contains("googleusercontent.com") {
        return text.to_string();
    }
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| {
            let stripped = line.trim();
            if stripped.is_empty() {
                return false;
            }
            !contains_internal_placeholder_content_url(stripped)
        })
        .collect();
    kept.join("\n").trim().to_string()
}

// ============================================================================
// 媒体描述项工具
// ============================================================================

fn looks_like_http_url(v: &Value) -> bool {
    v.as_str()
        .map(|s| s.starts_with("https://") || s.starts_with("http://"))
        .unwrap_or(false)
}

/// 判断一个 Value (list) 是否像 Gemini 媒体描述项
fn is_media_descriptor(item: &Value) -> bool {
    let arr = match item.as_array() {
        Some(a) if a.len() >= 2 => a,
        _ => return false,
    };
    let type_val = match arr[1].as_i64() {
        Some(v) => v,
        None => return false,
    };
    if type_val != 1 && type_val != 2 && type_val != 4 && type_val != 16 {
        return false;
    }
    let mut has_url = false;
    if arr.len() > 3 && looks_like_http_url(&arr[3]) {
        has_url = true;
    }
    if !has_url {
        if let Some(url_list) = arr.get(7).and_then(|v| v.as_array()) {
            has_url = url_list.iter().any(looks_like_http_url);
        }
    }
    if !has_url {
        return false;
    }
    let has_name = arr
        .get(2)
        .and_then(|v| v.as_str())
        .map(|s| s.contains('.'))
        .unwrap_or(false);
    let has_mime = arr
        .get(11)
        .and_then(|v| v.as_str())
        .map(|s| s.contains('/'))
        .unwrap_or(false);
    has_name || has_mime
}

fn media_descriptor_size_hint(item: &Value) -> i64 {
    vget(item, 15).and_then(|v| vi64(v, 2)).unwrap_or(0)
}

fn pick_preferred_media_descriptor(items: &[&Value]) -> Option<usize> {
    let valid: Vec<(usize, &Value)> = items
        .iter()
        .enumerate()
        .filter(|(_, it)| is_media_descriptor(it))
        .map(|(i, it)| (i, *it))
        .collect();
    if valid.is_empty() {
        return None;
    }
    let best = valid.iter().max_by_key(|(_, item)| {
        let size_hint = media_descriptor_size_hint(item);
        let mime = vstr(item, 11).unwrap_or("");
        let is_png: i64 = if mime == "image/png" { 1 } else { 0 };
        (size_hint, is_png)
    });
    best.map(|(i, _)| *i)
}

/// 递归收集所有媒体描述项
fn collect_media_descriptors<'a>(node: &'a Value, out: &mut Vec<&'a Value>) {
    match node {
        Value::Array(arr) => {
            if is_media_descriptor(node) {
                out.push(node);
                return;
            }
            for child in arr {
                collect_media_descriptors(child, out);
            }
        }
        // 新格式：block[12] 子节点可能是 object，媒体数据在其 value 中
        Value::Object(map) => {
            for v in map.values() {
                collect_media_descriptors(v, out);
            }
        }
        _ => {}
    }
}

/// 处理 image_generation 双格式结构，同层 3/6 槽位只保留一份主资源
fn collect_primary_media_descriptors<'a>(node: &'a Value, out: &mut Vec<&'a Value>) {
    match node {
        Value::Array(arr) => {
            if is_media_descriptor(node) {
                out.push(node);
                return;
            }
            // 检查 3/6 槽位
            let mut slot_candidates: Vec<&Value> = Vec::new();
            for &idx in &[3usize, 6] {
                if let Some(item) = arr.get(idx) {
                    if item.is_array() && is_media_descriptor(item) {
                        slot_candidates.push(item);
                    }
                }
            }
            if !slot_candidates.is_empty() {
                if let Some(best_idx) = pick_preferred_media_descriptor(&slot_candidates) {
                    out.push(slot_candidates[best_idx]);
                }
                return;
            }
            for child in arr {
                collect_primary_media_descriptors(child, out);
            }
        }
        // 新格式：block[12] 子节点可能是 object，媒体数据在其 value 中
        Value::Object(map) => {
            for v in map.values() {
                collect_primary_media_descriptors(v, out);
            }
        }
        _ => {}
    }
}

/// 从 AI 候选结构中提取可下载的媒体描述项
fn extract_ai_media_items(ai_data: &Value) -> Vec<&Value> {
    if !ai_data.is_array() {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    if vlen(ai_data) > 12 && !ai_data[12].is_null() {
        collect_primary_media_descriptors(&ai_data[12], &mut candidates);
    }
    if candidates.is_empty() {
        collect_media_descriptors(ai_data, &mut candidates);
    }

    // 去重
    let mut deduped = Vec::new();
    let mut seen: HashSet<(String, String, String)> = HashSet::new();
    for item in candidates {
        let parsed = parse_media_item(item, "assistant");
        let url = parsed.url.as_deref().unwrap_or("");
        if url.is_empty() || is_internal_placeholder_content_url(url) {
            continue;
        }
        let key = (
            url.to_string(),
            parsed.filename.unwrap_or_default(),
            parsed.mime.unwrap_or_default(),
        );
        if seen.contains(&key) {
            continue;
        }
        seen.insert(key);
        deduped.push(item);
    }
    deduped
}

/// 从 ai_data[12] 提取 AI 生成的音乐/视频媒体
fn extract_generated_media(
    ai_data: &Value,
) -> (Vec<MediaFile>, Option<MusicMeta>, Option<GenMeta>) {
    let mut files = Vec::new();
    let mut music_meta: Option<MusicMeta> = None;
    let mut gen_meta: Option<GenMeta> = None;

    if !ai_data.is_array() {
        return (files, music_meta, gen_meta);
    }
    let block12 = match vget(ai_data, 12) {
        Some(v) if v.is_array() && vlen(v) > 0 => v,
        _ => return (files, music_meta, gen_meta),
    };
    let block12_arr = block12.as_array().unwrap();

    // 找最后一个非 null 元素
    let last_idx = block12_arr.iter().rposition(|v| !v.is_null());
    let last_idx = match last_idx {
        Some(i) => i,
        None => return (files, music_meta, gen_meta),
    };
    let block = &block12_arr[last_idx];
    if !block.is_array() || vlen(block) == 0 {
        return (files, music_meta, gen_meta);
    }

    // 检测音乐块: block[6] 存在且 block[6..] 的 JSON 含 "music_gen"
    let is_music = vlen(block) > 6 && block[6].is_array() && {
        let tail_json = block.as_array().unwrap()[6..]
            .iter()
            .filter_map(|v| serde_json::to_string(v).ok())
            .collect::<String>();
        tail_json.contains("music_gen")
    };

    if is_music {
        for slot_idx in [0, 1] {
            if let Some(slot) = vget(block, slot_idx) {
                if slot.is_array() {
                    if let Some(media_item) = vget(slot, 1) {
                        if media_item.is_array() {
                            files.push(parse_media_item(media_item, "assistant"));
                        }
                    }
                }
            }
        }
        if let Some(meta) = vget(block, 2) {
            if meta.is_array() {
                let title = vstr(meta, 0).map(|s| s.to_string());
                let album = vstr(meta, 2).map(|s| s.to_string());
                let genre = vstr(meta, 4).map(|s| s.to_string());
                let moods = varr(meta, 5)
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                music_meta = Some(MusicMeta {
                    title,
                    album,
                    genre,
                    moods,
                    caption: None,
                });
            }
        }
        if let Some(b3) = vget(block, 3) {
            if b3.is_array() && vlen(b3) > 3 {
                if let Some(caption) = vstr(b3, 3) {
                    if let Some(ref mut mm) = music_meta {
                        mm.caption = Some(caption.to_string());
                    } else {
                        music_meta = Some(MusicMeta {
                            title: None,
                            album: None,
                            genre: None,
                            moods: Vec::new(),
                            caption: Some(caption.to_string()),
                        });
                    }
                }
            }
        }
        return (files, music_meta, gen_meta);
    }

    // 检测视频生成块
    if let Some(inner) = vget(block, 0) {
        if inner.is_array() && vlen(inner) > 0 {
            if let Some(group) = vget(inner, 0) {
                if group.is_array() && vlen(group) >= 2 {
                    if let Some(media_items) = vget(group, 0) {
                        if let Some(arr) = media_items.as_array() {
                            for m in arr {
                                if m.is_array() && vlen(m) > 1 {
                                    files.push(parse_media_item(m, "assistant"));
                                }
                            }
                        }
                    }
                    if let Some(gen_info) = vget(group, 1) {
                        if gen_info.is_array() && vlen(gen_info) > 0 {
                            let prompt = vstr(gen_info, 0).map(|s| s.to_string());
                            let model = vget(gen_info, 2)
                                .and_then(|v| vstr(v, 2))
                                .map(|s| s.to_string());
                            gen_meta = Some(GenMeta { model, prompt });
                        }
                    }
                }
            }
        }
    }

    (files, music_meta, gen_meta)
}

/// 从 ai_data 中提取深度研究文章（来自 ai_data[30]）
fn extract_deep_research_articles(ai_data: &Value) -> Vec<DeepResearchArticle> {
    let mut articles = Vec::new();

    if !ai_data.is_array() || vlen(ai_data) <= 30 {
        return articles;
    }

    // ai_data[30] 是深度研究文章列表
    let entries = match vget(ai_data, 30) {
        Some(v) if v.is_array() => v.as_array().unwrap(),
        _ => return articles,
    };

    for entry in entries.iter() {
        let entry_arr = match entry.as_array() {
            Some(a) if a.len() >= 5 => a,
            _ => continue,
        };

        // entry[0]: composite_id
        // entry[1]: uuid
        // entry[2]: title
        // entry[3]: doc_uuid
        // entry[4]: article_markdown
        let composite_id = match entry_arr[0].as_str() {
            Some(s) => s.to_string(),
            None => continue,
        };
        let uuid = match entry_arr[1].as_str() {
            Some(s) => s.to_string(),
            None => continue,
        };
        let title = match entry_arr[2].as_str() {
            Some(s) => s.to_string(),
            None => continue,
        };
        let doc_uuid = match entry_arr[3].as_str() {
            Some(s) => s.to_string(),
            None => continue,
        };
        let article_markdown = match entry_arr[4].as_str() {
            Some(s) => s.to_string(),
            None => continue,
        };

        articles.push(DeepResearchArticle {
            composite_id,
            uuid,
            title,
            doc_uuid,
            article_markdown: remove_placeholder_links(&article_markdown),
        });
    }

    articles
}

/// 清理文本中的 Google 占位符链接
fn remove_placeholder_links(text: &str) -> String {
    let result = text.to_string();

    // 移除包含占位符链接的整行
    let lines: Vec<&str> = result.lines().collect();
    let filtered_lines: Vec<&str> = lines
        .into_iter()
        .filter(|line| {
            !line.contains("googleusercontent.com/immersive_entry_chip")
                && !line.contains("googleusercontent.com/deep_research_confirmation_content")
        })
        .collect();

    filtered_lines.join("\n")
}

/// 从 ai_data[12][8]['56'] 提取研究方案确认消息（"这是我拟定的方案" 或 "我已经更新了方案"）
fn extract_deep_research_plan_confirmation(ai_data: &Value) -> Option<DeepResearchPlan> {
    if !ai_data.is_array() || vlen(ai_data) <= 12 {
        return None;
    }

    let field_12 = vget(ai_data, 12)?;
    if !field_12.is_array() || vlen(field_12) <= 8 {
        return None;
    }

    let field_8 = vget(field_12, 8)?;
    if !field_8.is_object() {
        return None;
    }

    // 提取 "56" 字段（研究方案确认消息）
    let field_56 = field_8.get("56")?.as_array()?;
    if field_56.len() < 2 {
        return None;
    }

    // field_56[0] 是标题
    let title = field_56[0].as_str()?.to_string();

    // field_56[1] 是步骤数组
    // 格式: [[1, "研究网站", "(1) xxx\n(2) xxx\n..."], [2, "分析结果"], [3, "生成报告"]]
    let steps_wrapper = field_56[1].as_array()?;
    if steps_wrapper.is_empty() {
        return None;
    }

    // 第一个元素: [1, "研究网站", "步骤内容"]
    let first_step = steps_wrapper[0].as_array()?;
    if first_step.len() < 3 {
        return None;
    }

    // first_step[2] 是步骤内容文本（编号格式）
    let steps_text = first_step[2].as_str()?.to_string();
    if steps_text.is_empty() {
        return None;
    }

    // 将单换行符替换为双换行符，以便 Markdown 渲染器正确识别段落
    // 格式: "(1) xxx\n(2) xxx\n..." -> "(1) xxx\n\n(2) xxx\n\n..."
    let formatted_steps = steps_text
        .split('\n')
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

    Some(DeepResearchPlan {
        title,
        steps: remove_placeholder_links(&formatted_steps),
    })
}

/// 从 ai_data[12][8]['58'] 提取深度研究方案（最终完成后的方案）
fn extract_deep_research_plan(ai_data: &Value) -> Option<DeepResearchPlan> {
    // ai_data[12][8]['58'][1][4] 是研究方案
    if !ai_data.is_array() || vlen(ai_data) <= 12 {
        return None;
    }

    let field_12 = vget(ai_data, 12)?;
    if !field_12.is_array() || vlen(field_12) <= 8 {
        return None;
    }

    let field_8 = vget(field_12, 8)?;
    if !field_8.is_object() {
        return None;
    }

    let field_58 = field_8.get("58")?.as_array()?;
    if field_58.len() < 2 {
        return None;
    }

    // field_58[1][4] 是包含方案数据的数组
    let field_58_1 = vget(&field_58[1], 4)?.as_array()?;
    if field_58_1.len() < 3 {
        return None;
    }

    // field_58_1[0] 是标题
    let title = field_58_1[0].as_str()?.to_string();

    // field_58_1[2] 是步骤数组（field_58_1[1] 是 null）
    let steps_arr = field_58_1[2].as_array()?;

    let mut steps_text = String::new();

    for step in steps_arr.iter() {
        if let Some(step_arr) = step.as_array() {
            // 步骤结构: [null, null, null, null, null, [标题, 内容, 数字]]
            if step_arr.len() > 5 {
                if let Some(step_data) = step_arr[5].as_array() {
                    if step_data.len() >= 2 {
                        // step_data[0] 是步骤标题，step_data[1] 是详细内容
                        if let (Some(step_title), Some(step_detail)) =
                            (step_data[0].as_str(), step_data[1].as_str())
                        {
                            if !steps_text.is_empty() {
                                steps_text.push_str("\n\n");
                            }
                            steps_text.push_str(&format!("## {}\n\n{}", step_title, step_detail));
                        }
                    }
                }
            }
        }
    }

    if steps_text.is_empty() {
        return None;
    }

    Some(DeepResearchPlan {
        title,
        steps: remove_placeholder_links(&steps_text),
    })
}

// ============================================================================
// Turn / media 解析
// ============================================================================

/// 解析单个媒体项目
pub fn parse_media_item(item: &Value, role: &str) -> MediaFile {
    let mut media = MediaFile {
        role: role.to_string(),
        media_type: "unknown".to_string(),
        filename: None,
        mime: None,
        url: None,
        thumbnail_url: None,
        duration: None,
        resolution: None,
        media_id: None,
        preview_media_id: None,
    };

    let arr = match item.as_array() {
        Some(a) => a,
        None => return media,
    };

    let type_val = arr.get(1).and_then(|v| v.as_i64());
    media.filename = arr.get(2).and_then(|v| v.as_str()).map(|s| s.to_string());
    media.mime = arr.get(11).and_then(|v| v.as_str()).map(|s| s.to_string());

    match type_val {
        Some(1) => {
            media.media_type = "image".to_string();
            if let Some(u) = arr.get(3).and_then(|v| v.as_str()) {
                media.url = Some(u.to_string());
            }
        }
        Some(2) => {
            media.media_type = "video".to_string();
            if let Some(urls) = arr.get(7).and_then(|v| v.as_array()) {
                if urls.len() > 1 {
                    if let Some(u) = urls[1].as_str() {
                        media.url = Some(u.to_string());
                    }
                }
                if media.url.is_none() {
                    if let Some(u) = urls.first().and_then(|v| v.as_str()) {
                        media.url = Some(u.to_string());
                    }
                }
                if let Some(u) = urls.first().and_then(|v| v.as_str()) {
                    media.thumbnail_url = Some(u.to_string());
                }
            }
            if media.url.is_none() {
                if let Some(u) = arr.get(3).and_then(|v| v.as_str()) {
                    media.url = Some(u.to_string());
                }
            }
        }
        Some(4) => {
            media.media_type = "audio".to_string();
            if let Some(urls) = arr.get(7).and_then(|v| v.as_array()) {
                if urls.len() > 1 {
                    if let Some(u) = urls[1].as_str() {
                        media.url = Some(u.to_string());
                    }
                }
                if media.url.is_none() {
                    if let Some(u) = urls.first().and_then(|v| v.as_str()) {
                        media.url = Some(u.to_string());
                    }
                }
                if let Some(u) = urls.first().and_then(|v| v.as_str()) {
                    media.thumbnail_url = Some(u.to_string());
                }
            }
            if media.url.is_none() {
                if let Some(u) = arr.get(3).and_then(|v| v.as_str()) {
                    media.url = Some(u.to_string());
                }
            }
        }
        Some(16) => {
            media.media_type = "attachment".to_string();
            // 下载 URL 在 [7][1]，不需要缩略图
            if let Some(urls) = arr.get(7).and_then(|v| v.as_array()) {
                if urls.len() > 1 {
                    if let Some(u) = urls[1].as_str() {
                        media.url = Some(u.to_string());
                    }
                }
                if media.url.is_none() {
                    if let Some(u) = urls.first().and_then(|v| v.as_str()) {
                        media.url = Some(u.to_string());
                    }
                }
            }
        }
        _ => {
            if let Some(u) = arr.get(3).and_then(|v| v.as_str()) {
                media.url = Some(u.to_string());
            }
        }
    }

    // 时长: item[14] 如 [[30, 772244000]] → 30.77 秒
    if let Some(dur_field) = arr.get(14).filter(|v| v.is_array()) {
        let dur = if let Some(inner) = vget(dur_field, 0).filter(|v| v.is_array()) {
            inner
        } else {
            dur_field
        };
        if let Some(secs) = vi64(dur, 0) {
            let nanos = vi64(dur, 1).unwrap_or(0);
            let raw = secs as f64 + nanos as f64 / 1e9;
            media.duration = Some((raw * 100.0).round() / 100.0);
        }
    }

    // 分辨率: item[17] 如 [[8], 1280, 720]
    if let Some(res) = arr.get(17).and_then(|v| v.as_array()) {
        if res.len() >= 3 {
            if let (Some(w), Some(h)) = (res[1].as_i64(), res[2].as_i64()) {
                media.resolution = Some(Resolution {
                    width: w,
                    height: h,
                });
            }
        }
    }

    media
}

/// 解析单个对话轮次
pub fn parse_turn(turn: &Value) -> ParsedTurn {
    // 提取 turn_id 用于日志
    let turn_id_for_log = turn
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_array())
        .and_then(|ids| {
            if ids.len() > 1 {
                ids[1].as_str()
            } else {
                ids.first().and_then(|v| v.as_str())
            }
        })
        .unwrap_or("unknown");

    let mut result = ParsedTurn {
        turn_id: None,
        timestamp: None,
        timestamp_iso: None,
        user: RoleContent {
            text: String::new(),
            files: Vec::new(),
        },
        assistant: AssistantContent {
            text: String::new(),
            thinking: String::new(),
            model: String::new(),
            files: Vec::new(),
            music_meta: None,
            gen_meta: None,
            deep_research_plan: None,

            deep_research_articles: Vec::new(),
        },
    };

    let arr = match turn.as_array() {
        Some(a) => a,
        None => return result,
    };

    // turn_id from turn[0]
    if let Some(ids) = arr.first().and_then(|v| v.as_array()) {
        result.turn_id = if ids.len() > 1 {
            ids[1].as_str().map(|s| s.to_string())
        } else {
            ids.first().and_then(|v| v.as_str()).map(|s| s.to_string())
        };
    }

    // timestamp from turn[4]
    if arr.len() > 4 {
        if let Some(t4) = arr[4].as_array() {
            if !t4.is_empty() {
                if let Some(ts) = t4[0].as_i64() {
                    result.timestamp = Some(ts);
                    result.timestamp_iso = to_iso_utc(Some(ts));
                }
            }
        }
    }

    // user text from turn[2][0][0]
    if arr.len() > 2 {
        let content = &arr[2];
        if let Some(msg) = vget(content, 0) {
            if let Some(text) = vstr(msg, 0) {
                result.user.text = text.to_string();
            }
            // user files from msg[4][0][3]
            if let Some(m4) = vget(msg, 4) {
                if let Some(m40) = vget(m4, 0) {
                    if let Some(user_files) = varr(m40, 3) {
                        for f in user_files {
                            if f.is_array() {
                                result.user.files.push(parse_media_item(f, "user"));
                            }
                        }
                    }
                }
            }
        }
    }

    // assistant data from turn[3]
    if arr.len() <= 3 {
        return result;
    }
    let detail = &arr[3];
    let detail_arr = match detail.as_array() {
        Some(a) => a,
        None => return result,
    };

    // model from detail[21]
    if let Some(model) = detail_arr.get(21).and_then(|v| v.as_str()) {
        result.assistant.model = model.to_string();
    }

    // select AI candidate
    let mut ai_data: Option<&Value> = None;
    let selected_candidate_id = detail_arr.get(3).and_then(|v| v.as_str());
    log::info!(
        "🔍 parse_turn [{}]: selected_candidate_id={:?}",
        turn_id_for_log,
        selected_candidate_id
    );

    if let Some(candidates_arr) = detail_arr.first().and_then(|v| v.as_array()) {
        let candidates: Vec<&Value> = candidates_arr.iter().filter(|c| c.is_array()).collect();
        log::info!(
            "🔍 parse_turn [{}]: 找到 {} 个候选响应",
            turn_id_for_log,
            candidates.len()
        );

        if let Some(sel_id) = selected_candidate_id {
            for c in &candidates {
                if vstr(c, 0) == Some(sel_id) {
                    ai_data = Some(c);
                    break;
                }
            }
        }
        if ai_data.is_none() && !candidates.is_empty() {
            ai_data = Some(candidates[0]);
        }

        if let Some(ai) = ai_data {
            log::info!(
                "🔍 parse_turn [{}]: 选中的 ai_data 是数组={}, 长度={}",
                turn_id_for_log,
                ai.is_array(),
                vlen(ai)
            );
        } else {
            log::warn!("🔍 parse_turn [{}]: 没有找到 ai_data", turn_id_for_log);
        }
    }

    // Build user media keys for dedup
    let user_media_keys: HashSet<(String, String, String, String)> = result
        .user
        .files
        .iter()
        .map(|f| {
            (
                f.url.clone().unwrap_or_default(),
                f.filename.clone().unwrap_or_default(),
                f.mime.clone().unwrap_or_default(),
                f.media_type.clone(),
            )
        })
        .collect();

    if let Some(ai) = ai_data {
        // assistant text from ai_data[1][0]
        if let Some(text_arr) = vget(ai, 1) {
            if let Some(text) = vstr(text_arr, 0) {
                result.assistant.text = remove_placeholder_links(text);
            }
        }

        // thinking from ai_data[37]
        if vlen(ai) > 37 && !ai[37].is_null() {
            if let Some(thinking_arr) = ai[37].as_array() {
                if !thinking_arr.is_empty() {
                    if let Some(inner) = thinking_arr[0].as_array() {
                        if !inner.is_empty() {
                            if let Some(s) = inner[0].as_str() {
                                result.assistant.thinking = s.to_string();
                            }
                        }
                    } else if let Some(s) = thinking_arr[0].as_str() {
                        result.assistant.thinking = s.to_string();
                    }
                }
            }
        }

        // AI media items
        let ai_media_items = extract_ai_media_items(ai);
        let mut seen_ai: HashSet<(String, String, String, String)> = HashSet::new();
        for item in &ai_media_items {
            let parsed = parse_media_item(item, "assistant");
            let key = (
                parsed.url.clone().unwrap_or_default(),
                parsed.filename.clone().unwrap_or_default(),
                parsed.mime.clone().unwrap_or_default(),
                parsed.media_type.clone(),
            );
            if user_media_keys.contains(&key) || seen_ai.contains(&key) {
                continue;
            }
            seen_ai.insert(key);
            result.assistant.files.push(parsed);
        }

        // Sanitize placeholder text
        result.assistant.text = sanitize_generation_placeholder_text(
            &result.assistant.text,
            !result.assistant.files.is_empty(),
        );

        // Generated media (music/video from ai_data[12])
        let (gen_files, music_meta, gen_meta) = extract_generated_media(ai);
        if !gen_files.is_empty() {
            let existing_urls: HashSet<String> = result
                .assistant
                .files
                .iter()
                .filter_map(|f| f.url.clone())
                .collect();
            for gf in gen_files {
                if let Some(ref u) = gf.url {
                    if !existing_urls.contains(u) {
                        result.assistant.files.push(gf);
                    }
                } else {
                    result.assistant.files.push(gf);
                }
            }
        }
        if music_meta.is_some() {
            result.assistant.music_meta = music_meta;
        }
        if gen_meta.is_some() {
            result.assistant.gen_meta = gen_meta;
        }

        // Deep research articles from ai_data[30]
        let articles = extract_deep_research_articles(ai);
        if !articles.is_empty() {
            result.assistant.deep_research_articles = articles;
        }

        // Deep research plan from ai_data[12][8]['56'] or ai_data[12][8]['58']
        // 优先尝试提取研究方案确认消息（"56" 字段）
        if let Some(plan) = extract_deep_research_plan_confirmation(ai) {
            result.assistant.deep_research_plan = Some(plan);
        } else if let Some(plan) = extract_deep_research_plan(ai) {
            // 如果没有找到 "56" 字段，尝试提取最终研究方案（"58" 字段）
            result.assistant.deep_research_plan = Some(plan);
        }
    }

    log::info!(
        "🔍 parse_turn: 返回结果，deep_research_articles 数量={}",
        result.assistant.deep_research_articles.len()
    );

    result
}

// ============================================================================
// 媒体身份键与堆叠去重
// ============================================================================

type MediaIdentityKey = (String, String, String, String, String);

fn media_identity_key(file_item: &MediaFile) -> MediaIdentityKey {
    if let Some(ref mid) = file_item.media_id {
        if !mid.is_empty() {
            return (
                "media_id".into(),
                mid.clone(),
                String::new(),
                String::new(),
                String::new(),
            );
        }
    }
    if let Some(ref u) = file_item.url {
        if !u.is_empty() {
            return (
                "url".into(),
                u.clone(),
                String::new(),
                String::new(),
                String::new(),
            );
        }
    }
    (
        "fallback".into(),
        file_item.media_type.clone(),
        file_item.filename.clone().unwrap_or_default(),
        file_item.mime.clone().unwrap_or_default(),
        file_item.thumbnail_url.clone().unwrap_or_default(),
    )
}

/// 处理 Gemini 媒体"堆叠回放"结构：
/// - 按时间正序识别媒体首次出现位置
/// - 仅在首次出现 turn 保留该媒体
/// - 后续 turn 的重复媒体移除
pub fn normalize_turn_media_first_seen(parsed_turns: &mut [ParsedTurn]) {
    if parsed_turns.is_empty() {
        return;
    }
    let mut seen_user: HashSet<MediaIdentityKey> = HashSet::new();
    let mut seen_assistant: HashSet<MediaIdentityKey> = HashSet::new();

    // 从末尾（最新）往前遍历
    for turn in parsed_turns.iter_mut().rev() {
        // user files
        {
            let mut deduped = Vec::new();
            let mut turn_seen: HashSet<MediaIdentityKey> = HashSet::new();
            for f in &turn.user.files {
                let key = media_identity_key(f);
                if turn_seen.contains(&key) || seen_user.contains(&key) {
                    continue;
                }
                turn_seen.insert(key.clone());
                seen_user.insert(key);
                deduped.push(f.clone());
            }
            turn.user.files = deduped;
        }
        // assistant files: 同时排除已在 user 侧出现过的媒体
        // （Gemini 会在 AI 的 ai_data[12] 中 stacking 用户上传的附件）
        {
            let mut deduped = Vec::new();
            let mut turn_seen: HashSet<MediaIdentityKey> = HashSet::new();
            for f in &turn.assistant.files {
                let key = media_identity_key(f);
                if turn_seen.contains(&key)
                    || seen_assistant.contains(&key)
                    || seen_user.contains(&key)
                {
                    continue;
                }
                turn_seen.insert(key.clone());
                seen_assistant.insert(key);
                deduped.push(f.clone());
            }
            turn.assistant.files = deduped;
        }
    }
}

// ============================================================================
// Value 转换辅助（供 export_cli 使用）
// ============================================================================

/// 解析 raw turn 并返回 serde_json::Value（用于与 storage 层对接）
pub fn parse_turn_to_value(raw_turn: &Value) -> Value {
    let parsed = parse_turn(raw_turn);
    serde_json::to_value(&parsed).unwrap_or(Value::Null)
}

/// 对 Value 形式的 parsed_turns 做 normalize_turn_media_first_seen
pub fn normalize_turn_media_first_seen_values(parsed_turns: &mut [Value]) {
    // 反序列化为 ParsedTurn，处理后再写回
    let mut turns: Vec<ParsedTurn> = parsed_turns
        .iter()
        .filter_map(|v| serde_json::from_value(v.clone()).ok())
        .collect();
    normalize_turn_media_first_seen(&mut turns);
    for (i, turn) in turns.into_iter().enumerate() {
        if i < parsed_turns.len() {
            if let Ok(v) = serde_json::to_value(&turn) {
                parsed_turns[i] = v;
            }
        }
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_is_internal_placeholder_content_url() {
        assert!(is_internal_placeholder_content_url(
            "https://lh3.googleusercontent.com/abc_content/def"
        ));
        assert!(is_internal_placeholder_content_url(
            "https://lh3.googleusercontent.com/some_content/"
        ));
        assert!(!is_internal_placeholder_content_url(
            "https://example.com/abc_content/def"
        ));
        assert!(!is_internal_placeholder_content_url(
            "https://lh3.googleusercontent.com/normal/path"
        ));
    }

    #[test]
    fn test_sanitize_generation_placeholder_text() {
        let text =
            "Here is your image\nhttps://lh3.googleusercontent.com/abc_content/img.png\nEnjoy!";
        let result = sanitize_generation_placeholder_text(text, true);
        assert_eq!(result, "Here is your image\nEnjoy!");

        // No attachments → no change
        let result2 = sanitize_generation_placeholder_text(text, false);
        assert_eq!(result2, text);
    }

    #[test]
    fn test_is_media_descriptor() {
        let image_desc = json!([
            null,
            1,
            "photo.jpg",
            "https://example.com/img.jpg",
            null,
            null,
            null,
            null,
            null,
            null,
            null,
            "image/jpeg"
        ]);
        assert!(is_media_descriptor(&image_desc));

        let video_desc = json!([
            null,
            2,
            "vid.mp4",
            null,
            null,
            null,
            null,
            ["https://thumb.com/t.jpg", "https://example.com/v.mp4"],
            null,
            null,
            null,
            "video/mp4"
        ]);
        assert!(is_media_descriptor(&video_desc));

        let not_media = json!([null, 3, "file.txt"]);
        assert!(!is_media_descriptor(&not_media));
    }

    #[test]
    fn test_parse_media_item_image() {
        let item = json!([
            null,
            1,
            "photo.jpg",
            "https://example.com/img.jpg",
            null,
            null,
            null,
            null,
            null,
            null,
            null,
            "image/jpeg"
        ]);
        let parsed = parse_media_item(&item, "assistant");
        assert_eq!(parsed.media_type, "image");
        assert_eq!(parsed.url.as_deref(), Some("https://example.com/img.jpg"));
        assert_eq!(parsed.filename.as_deref(), Some("photo.jpg"));
        assert_eq!(parsed.mime.as_deref(), Some("image/jpeg"));
    }

    #[test]
    fn test_parse_media_item_video() {
        let item = json!([
            null,
            2,
            "vid.mp4",
            null,
            null,
            null,
            null,
            ["https://thumb.com/t.jpg", "https://example.com/v.mp4"],
            null,
            null,
            null,
            "video/mp4"
        ]);
        let parsed = parse_media_item(&item, "user");
        assert_eq!(parsed.media_type, "video");
        assert_eq!(parsed.url.as_deref(), Some("https://example.com/v.mp4"));
        assert_eq!(
            parsed.thumbnail_url.as_deref(),
            Some("https://thumb.com/t.jpg")
        );
    }

    #[test]
    fn test_parse_media_item_duration() {
        let item = json!([
            null,
            2,
            "vid.mp4",
            null,
            null,
            null,
            null,
            ["https://example.com/v.mp4"],
            null,
            null,
            null,
            "video/mp4",
            null,
            null,
            [[30, 772244000]]
        ]);
        let parsed = parse_media_item(&item, "assistant");
        assert!(parsed.duration.is_some());
        let dur = parsed.duration.unwrap();
        assert!((dur - 30.77).abs() < 0.01);
    }

    #[test]
    fn test_parse_turn_basic() {
        let turn = json!([
            ["conv_id", "turn_abc"], // [0] ids
            null,                    // [1]
            [[
                // [2] content
                "Hello world", // [2][0][0] user text
                null,
                null,
                null,
                null
            ]],
            [
                // [3] detail
                [[
                    // [3][0] candidates
                    "cand_1",             // [3][0][0][0] candidate id
                    ["AI response text"], // [3][0][0][1] text
                ]],
                null,
                null,
                "cand_1", // [3][3] selected candidate id
            ],
            [1700000000] // [4] timestamp
        ]);
        let parsed = parse_turn(&turn);
        assert_eq!(parsed.turn_id.as_deref(), Some("turn_abc"));
        assert_eq!(parsed.timestamp, Some(1700000000));
        assert_eq!(parsed.user.text, "Hello world");
        assert_eq!(parsed.assistant.text, "AI response text");
    }

    #[test]
    fn test_normalize_turn_media_first_seen() {
        let mut turns = vec![
            ParsedTurn {
                turn_id: Some("t1".into()),
                timestamp: Some(100),
                timestamp_iso: None,
                user: RoleContent {
                    text: String::new(),
                    files: Vec::new(),
                },
                assistant: AssistantContent {
                    text: String::new(),
                    thinking: String::new(),
                    model: String::new(),
                    files: vec![MediaFile {
                        role: "assistant".into(),
                        media_type: "image".into(),
                        filename: None,
                        mime: None,
                        url: Some("https://example.com/a.jpg".into()),
                        thumbnail_url: None,
                        duration: None,
                        resolution: None,
                        media_id: None,
                        preview_media_id: None,
                    }],
                    music_meta: None,
                    gen_meta: None,
                    deep_research_plan: None,
                    deep_research_articles: Vec::new(),
                },
            },
            ParsedTurn {
                turn_id: Some("t2".into()),
                timestamp: Some(200),
                timestamp_iso: None,
                user: RoleContent {
                    text: String::new(),
                    files: Vec::new(),
                },
                assistant: AssistantContent {
                    text: String::new(),
                    thinking: String::new(),
                    model: String::new(),
                    files: vec![
                        MediaFile {
                            role: "assistant".into(),
                            media_type: "image".into(),
                            filename: None,
                            mime: None,
                            url: Some("https://example.com/a.jpg".into()),
                            thumbnail_url: None,
                            duration: None,
                            resolution: None,
                            media_id: None,
                            preview_media_id: None,
                        },
                        MediaFile {
                            role: "assistant".into(),
                            media_type: "image".into(),
                            filename: None,
                            mime: None,
                            url: Some("https://example.com/b.jpg".into()),
                            thumbnail_url: None,
                            duration: None,
                            resolution: None,
                            media_id: None,
                            preview_media_id: None,
                        },
                    ],
                    music_meta: None,
                    gen_meta: None,
                    deep_research_plan: None,
                    deep_research_articles: Vec::new(),
                },
            },
        ];

        normalize_turn_media_first_seen(&mut turns);
        // t2 (latest) keeps both; t1 loses "a.jpg" because t2 claimed it first
        assert_eq!(turns[1].assistant.files.len(), 2);
        assert_eq!(turns[0].assistant.files.len(), 0);
    }

    #[test]
    fn test_extract_deep_research_articles() {
        // 模拟包含深度研究文章的 ai_data
        let ai_data = json!([
            null,
            null,
            null,
            null,
            null,
            null,
            null,
            null,
            null,
            null, // 0-9
            null,
            null,
            null,
            null,
            null,
            null,
            null,
            null,
            null,
            null, // 10-19
            null,
            null,
            null,
            null,
            null,
            null,
            null,
            null,
            null,
            null, // 20-29
            [
                // [30] 深度研究文章列表
                [
                    "c_test_composite_id",
                    "test-uuid-1234",
                    "测试文章标题",
                    "doc-uuid-5678",
                    "# 测试文章\n\n这是一篇测试深度研究文章的内容。\n\n## 章节1\n内容..."
                ]
            ]
        ]);

        let articles = extract_deep_research_articles(&ai_data);
        assert_eq!(articles.len(), 1);

        let article = &articles[0];
        assert_eq!(article.composite_id, "c_test_composite_id");
        assert_eq!(article.uuid, "test-uuid-1234");
        assert_eq!(article.title, "测试文章标题");
        assert_eq!(article.doc_uuid, "doc-uuid-5678");
        assert!(article.article_markdown.starts_with("# 测试文章"));
    }

    #[test]
    fn test_extract_deep_research_articles_empty() {
        // ai_data 长度不足
        let ai_data = json!([null, null, null]);
        let articles = extract_deep_research_articles(&ai_data);
        assert_eq!(articles.len(), 0);

        // ai_data[30] 为 null
        let ai_data = json!([
            null, null, null, null, null, null, null, null, null, null, null, null, null, null,
            null, null, null, null, null, null, null, null, null, null, null, null, null, null,
            null, null, null
        ]);
        let articles = extract_deep_research_articles(&ai_data);
        assert_eq!(articles.len(), 0);
    }
}
