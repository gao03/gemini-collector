//! Gemini 媒体工具：视频预览生成、媒体类型推断、URL 辅助函数。

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use url::Url;

use crate::turn_parser::ParsedTurn;

// ============================================================================
// 常量
// ============================================================================

pub const PROTECTED_MEDIA_HOSTS: &[&str] = &[
    "lh3.google.com",
    "lh3.googleusercontent.com",
    "contribution.usercontent.google.com",
];

// ============================================================================
// 视频预览
// ============================================================================

/// 从 media_id 生成预览文件名：`{stem}_preview.jpg`
pub fn video_preview_name(media_id: &str) -> String {
    let stem = Path::new(media_id)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(media_id);
    format!("{}_preview.jpg", stem)
}

/// 从视频首帧生成固定尺寸预览图（匹配前端预览卡片 160x110）。
/// 当前使用 ffmpeg CLI，后续可替换为平台原生 API。
pub fn generate_video_preview(video_path: &Path, preview_path: &Path) -> bool {
    let ffmpeg_bin = match which_ffmpeg() {
        Some(bin) => bin,
        None => return false,
    };
    if let Some(parent) = preview_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let result = Command::new(ffmpeg_bin)
        .args([
            "-y",
            "-i",
        ])
        .arg(video_path)
        .args([
            "-frames:v", "1",
            "-vf", "scale=160:110:force_original_aspect_ratio=increase,crop=160:110",
            "-q:v", "4",
        ])
        .arg(preview_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    match result {
        Ok(status) => {
            status.success()
                && preview_path.exists()
                && preview_path.metadata().map(|m| m.len() > 0).unwrap_or(false)
        }
        Err(_) => false,
    }
}

fn which_ffmpeg() -> Option<String> {
    // Check common locations
    for candidate in &["/usr/local/bin/ffmpeg", "/opt/homebrew/bin/ffmpeg", "/usr/bin/ffmpeg"] {
        if Path::new(candidate).exists() {
            return Some(candidate.to_string());
        }
    }
    // Try PATH via `which`
    Command::new("which")
        .arg("ffmpeg")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() { None } else { Some(s) }
        })
}

/// 遍历 turn 中的视频附件，确保存在对应首帧预览图。
/// 返回 (preview_generated, preview_failed) 计数。
pub fn ensure_video_previews_from_turns(
    parsed_turns: &mut [ParsedTurn],
    media_dir: &Path,
) -> (usize, usize) {
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut generated = 0usize;
    let mut failed = 0usize;

    for turn in parsed_turns.iter_mut() {
        for role in ["user", "assistant"] {
            let files = match role {
                "user" => &mut turn.user.files,
                _ => &mut turn.assistant.files,
            };
            for f in files.iter_mut() {
                if f.media_type != "video" {
                    continue;
                }
                let media_id = match f.media_id.as_deref() {
                    Some(id) if !id.is_empty() => id.to_string(),
                    _ => continue,
                };
                let preview_id = f
                    .preview_media_id
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| video_preview_name(&media_id));
                f.preview_media_id = Some(preview_id.clone());

                let key = (media_id.clone(), preview_id.clone());
                if seen.contains(&key) {
                    continue;
                }
                seen.insert(key);

                let video_path = media_dir.join(&media_id);
                let preview_path = media_dir.join(&preview_id);

                if preview_path.exists()
                    && preview_path.metadata().map(|m| m.len() > 0).unwrap_or(false)
                {
                    continue;
                }
                if !video_path.exists() {
                    failed += 1;
                    continue;
                }

                if generate_video_preview(&video_path, &preview_path) {
                    generated += 1;
                } else {
                    failed += 1;
                }
            }
        }
    }
    (generated, failed)
}

/// Value 版本：遍历 Value 形式的 parsed_turns，生成视频预览。
pub struct PreviewStats {
    pub preview_generated: usize,
    pub preview_failed: usize,
}

pub fn ensure_video_previews_from_turns_values(
    parsed_turns: &[serde_json::Value],
    media_dir: &Path,
) -> PreviewStats {
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut generated = 0usize;
    let mut failed = 0usize;

    for turn in parsed_turns {
        for role_key in &["user", "assistant"] {
            let files = match turn.get(role_key).and_then(|v| v.get("files")).and_then(|v| v.as_array()) {
                Some(f) => f,
                None => continue,
            };
            for f in files {
                let media_type = f.get("media_type").or_else(|| f.get("type")).and_then(|v| v.as_str()).unwrap_or("");
                if media_type != "video" {
                    continue;
                }
                let media_id = match f.get("media_id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                    Some(id) => id.to_string(),
                    None => continue,
                };
                let preview_id = f
                    .get("preview_media_id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| video_preview_name(&media_id));

                let key = (media_id.clone(), preview_id.clone());
                if seen.contains(&key) {
                    continue;
                }
                seen.insert(key);

                let video_path = media_dir.join(&media_id);
                let preview_path = media_dir.join(&preview_id);

                if preview_path.exists()
                    && preview_path.metadata().map(|m| m.len() > 0).unwrap_or(false)
                {
                    continue;
                }
                if !video_path.exists() {
                    failed += 1;
                    continue;
                }

                if generate_video_preview(&video_path, &preview_path) {
                    generated += 1;
                } else {
                    failed += 1;
                }
            }
        }
    }

    PreviewStats {
        preview_generated: generated,
        preview_failed: failed,
    }
}

// ============================================================================
// 媒体类型推断
// ============================================================================

/// 根据文件名/扩展名推断媒体类型
pub fn infer_media_type(media_hint: &str) -> &'static str {
    if media_hint.is_empty() {
        return "file";
    }
    let ext = Path::new(media_hint)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp" | "avif" | "heic" | "heif" | "svg" => {
            "image"
        }
        "mp4" | "mov" | "webm" | "mkv" | "m4v" | "avi" | "3gp" => "video",
        "mp3" | "m4a" | "wav" | "aac" | "flac" | "ogg" | "opus" | "wma" | "aiff" => "audio",
        _ => "file",
    }
}

// ============================================================================
// URL 工具
// ============================================================================

/// 媒体日志字段
pub struct MediaLogFields {
    pub media: String,
    pub domain: String,
}

/// 从 URL 和类型信息构建日志字段
pub fn media_log_fields(
    url_text: Option<&str>,
    media_type: Option<&str>,
    media_hint: Option<&str>,
) -> MediaLogFields {
    let domain = url_text
        .and_then(|u| Url::parse(u).ok())
        .and_then(|u| u.host_str().map(|h| h.to_lowercase()))
        .unwrap_or_else(|| "-".to_string());

    let kind = match media_type {
        Some(t) if matches!(t, "image" | "video" | "file") => t.to_string(),
        _ => infer_media_type(media_hint.unwrap_or("")).to_string(),
    };

    MediaLogFields {
        media: kind,
        domain,
    }
}

/// 为 URL 附加 authuser 查询参数
pub fn append_authuser(url_str: &str, authuser: &str) -> String {
    let mut parsed = match Url::parse(url_str) {
        Ok(u) => u,
        Err(_) => return url_str.to_string(),
    };
    // Remove existing authuser if any, then append
    let pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(k, _)| k != "authuser")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    parsed.query_pairs_mut().clear();
    for (k, v) in &pairs {
        parsed.query_pairs_mut().append_pair(k, v);
    }
    parsed
        .query_pairs_mut()
        .append_pair("authuser", authuser);
    parsed.to_string()
}

/// 检查 URL 是否属于受保护的媒体域名
pub fn is_protected_media_url(url_text: &str) -> bool {
    let host = match Url::parse(url_text) {
        Ok(u) => u.host_str().unwrap_or("").to_lowercase(),
        Err(_) => return false,
    };
    PROTECTED_MEDIA_HOSTS.iter().any(|&h| host == h)
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_video_preview_name() {
        assert_eq!(video_preview_name("clip.mp4"), "clip_preview.jpg");
        assert_eq!(video_preview_name("no_ext"), "no_ext_preview.jpg");
        assert_eq!(video_preview_name("a.b.mp4"), "a.b_preview.jpg");
    }

    #[test]
    fn test_infer_media_type() {
        assert_eq!(infer_media_type("photo.jpg"), "image");
        assert_eq!(infer_media_type("video.mp4"), "video");
        assert_eq!(infer_media_type("song.mp3"), "audio");
        assert_eq!(infer_media_type("doc.pdf"), "file");
        assert_eq!(infer_media_type(""), "file");
    }

    #[test]
    fn test_append_authuser() {
        let url = "https://lh3.google.com/path?key=val";
        let result = append_authuser(url, "2");
        assert!(result.contains("authuser=2"));
        assert!(result.contains("key=val"));
    }

    #[test]
    fn test_append_authuser_replaces_existing() {
        let url = "https://lh3.google.com/path?authuser=0&key=val";
        let result = append_authuser(url, "3");
        assert!(result.contains("authuser=3"));
        assert!(!result.contains("authuser=0"));
    }

    #[test]
    fn test_is_protected_media_url() {
        assert!(is_protected_media_url("https://lh3.googleusercontent.com/img.jpg"));
        assert!(is_protected_media_url("https://lh3.google.com/media/abc"));
        assert!(!is_protected_media_url("https://example.com/img.jpg"));
    }

    #[test]
    fn test_media_log_fields() {
        let fields = media_log_fields(
            Some("https://lh3.google.com/img.jpg"),
            Some("image"),
            None,
        );
        assert_eq!(fields.media, "image");
        assert_eq!(fields.domain, "lh3.google.com");
    }

    #[test]
    fn test_media_log_fields_infer() {
        let fields = media_log_fields(None, None, Some("clip.mp4"));
        assert_eq!(fields.media, "video");
        assert_eq!(fields.domain, "-");
    }
}
