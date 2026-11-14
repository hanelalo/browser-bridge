//! `upload_file` 指令的文件预处理：读取本地文件并编码为 base64 payload。
//! 被 CLI（bridge-client）与 MCP server（bridge-mcp）复用。

use base64::Engine;
use serde_json::{json, Value};
use std::path::Path;

/// 单次 `upload_file` 调用的文件总大小上限（30 MB）。
pub const MAX_UPLOAD_TOTAL_BYTES: u64 = 30 * 1024 * 1024;

/// 按扩展名猜 MIME 类型，覆盖表单上传常见类型，未知回落 application/octet-stream。
fn guess_mime(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "xml" => "application/xml",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "tar" => "application/x-tar",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        _ => "application/octet-stream",
    }
}

/// 把本地文件列表转成 `upload_file` 的 `params.files`（base64 编码）。
/// 总大小超过 [`MAX_UPLOAD_TOTAL_BYTES`] 或列表为空时报错。
pub fn prepare_files(paths: &[String]) -> Result<Vec<Value>, String> {
    if paths.is_empty() {
        return Err("upload_file 至少需要一个文件".to_string());
    }
    let mut files = Vec::with_capacity(paths.len());
    let mut total: u64 = 0;
    for p in paths {
        let path = Path::new(p);
        let meta = std::fs::metadata(p).map_err(|e| format!("读取 {p} 失败: {e}"))?;
        if !meta.is_file() {
            return Err(format!("{p} 不是普通文件"));
        }
        total += meta.len();
        if total > MAX_UPLOAD_TOTAL_BYTES {
            return Err(format!(
                "上传文件总大小超过上限（{} MB）",
                MAX_UPLOAD_TOTAL_BYTES / 1024 / 1024
            ));
        }
        let data = std::fs::read(p).map_err(|e| format!("读取 {p} 失败: {e}"))?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("{p} 文件名不是合法 UTF-8"))?
            .to_string();
        files.push(json!({
            "name": name,
            "mime": guess_mime(path),
            "size": data.len(),
            "data_base64": base64::engine::general_purpose::STANDARD.encode(&data),
        }));
    }
    Ok(files)
}
