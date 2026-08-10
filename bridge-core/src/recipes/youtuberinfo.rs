//! YouTube 频道（youtuber）视频列表配方：导航到频道 /videos 页，直接解析 HTML
//! 内嵌的 ytInitialData 拿到频道名、订阅数与首屏视频（名称/URL/观看数/时长/
//! 发布时间），不足 max 条时用 InnerTube `browse` continuation 续取——与
//! yt-dlp 同源，不需要页面渲染、滚动或窗口操作。频道页视频条目同时兼容
//! 旧版 `videoRenderer`/`gridVideoRenderer` 与新版 `lockupViewModel`（2026 年
//! 实测 YouTube 新版频道页已全面切到 richItemRenderer + lockupViewModel）。
//! 频道页数据在部分会话/变体下会把超长标题截断（实测截到 100 字符甚至更短），
//! 因此最后会用 YouTube 官方 oEmbed 接口兜底校验，把疑似截断的标题替换成
//! 完整标题。

use serde_json::{json, Value};

use crate::transport::Bridge;

/// 路径段编码：字母数字及 `-_.~@` 原样，其余按 UTF-8 字节转 %XX
/// （handle 的 `@` 前缀保持可读）。
fn enc_segment(input: &str) -> String {
    let mut out = String::new();
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'@' => {
                out.push(byte as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{byte:02X}"));
            }
        }
    }
    out
}

/// 把用户输入（频道 URL 或 handle）归一化成频道 /videos 页 URL。
/// 支持 `https://www.youtube.com/@handle/videos`、`youtube.com/@handle`、
/// `@handle`、裸 handle，以及 `/c/` `/user/` `/channel/` 等路径形式。
fn videos_url(input: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("频道 URL 或 handle 不能为空".to_string());
    }
    let rest = input
        .strip_prefix("http://")
        .or_else(|| input.strip_prefix("https://"))
        .unwrap_or(input);
    if rest == "youtube.com" || rest == "www.youtube.com" {
        return Err(format!(
            "无法从 '{input}' 识别频道 handle，请提供如 https://www.youtube.com/@handle/videos 或 @handle"
        ));
    }
    // 取路径部分：去掉域名（如有）后保留第一个 '/' 之后的路径；
    // 没有 '/' 说明整个 rest 就是 handle（如 @xiaojunpodcast）
    let path = match rest.find('/') {
        Some(pos) => &rest[pos..],
        None => {
            return Ok(format!(
                "https://www.youtube.com/{}/videos",
                enc_segment(rest)
            ))
        }
    };
    // 去掉查询参数 / 锚点，取路径段
    let path = path.split(['?', '#']).next().unwrap_or("").trim();
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return Err(format!(
            "无法从 '{input}' 识别频道 handle，请提供如 https://www.youtube.com/@handle/videos 或 @handle"
        ));
    }
    // 保留前 1-2 段：@handle 只有 1 段（后面的 /videos /shorts 等标签统一
    // 归一化成 /videos）；c/名称、user/名称、channel/UC... 保留 2 段
    let take = if segments[0].starts_with('@') {
        1
    } else if matches!(segments[0], "c" | "user" | "channel") {
        segments.len().min(2)
    } else {
        1
    };
    let base = segments[..take]
        .iter()
        .map(|s| enc_segment(s))
        .collect::<Vec<_>>()
        .join("/");
    Ok(format!("https://www.youtube.com/{base}/videos"))
}

/// 页面内执行的提取脚本（`__MAX__` 在 Rust 侧替换成目标条数）。数据全部取自
/// 导航返回的 HTML 内嵌 ytInitialData + InnerTube `browse` continuation，
/// 不需要页面渲染、滚动或窗口操作。
const EXTRACT_SCRIPT: &str = r#"(async () => {
  const MAX = __MAX__;
  const html = document.documentElement.outerHTML;
  const m = html.match(/var ytInitialData = (\{.*?\});<\/script>/s);
  if (!m) return { ok: false, reason: 'no ytInitialData' };
  let data = null;
  try { data = JSON.parse(m[1]); } catch (e) { return { ok: false, reason: 'ytInitialData parse failed' }; }

  const walk = (o, cb) => {
    if (!o || typeof o !== 'object') return;
    if (Array.isArray(o)) { for (const v of o) walk(v, cb); return; }
    cb(o);
    for (const k of Object.keys(o)) walk(o[k], cb);
  };
  const textOf = (t) => {
    if (!t) return null;
    if (typeof t === 'string') return t;
    if (t.simpleText) return t.simpleText;
    if (t.runs) return t.runs.map((r) => r.text || '').join('');
    if (typeof t.content === 'string') return t.content;
    return null;
  };
  const parseCount = (s) => {
    if (!s) return null;
    const mm = String(s).match(/([0-9][0-9,]*\.?[0-9]*)\s*([万亿KMB])?/i);
    if (!mm) return null;
    const n = parseFloat(mm[1].replace(/,/g, ''));
    if (!Number.isFinite(n)) return null;
    const mult = { k: 1000, m: 1000000, b: 1000000000, '万': 10000, '亿': 100000000 }[mm[2] ? mm[2].toLowerCase() : ''];
    return mult ? Math.round(n * mult) : Math.round(n);
  };

  // ---- 频道名称 / 频道 URL ----
  let channelName = null;
  let channelUrl = null;
  walk(data, (o) => {
    if (o.channelMetadataRenderer) {
      const r = o.channelMetadataRenderer;
      if (!channelName && r.title) channelName = r.title;
      if (r.vanityChannelUrl) {
        channelUrl = r.vanityChannelUrl.startsWith('http') ? r.vanityChannelUrl : 'https://www.youtube.com' + r.vanityChannelUrl;
      } else if (!channelUrl && r.channelUrl) {
        channelUrl = r.channelUrl;
      }
    }
  });
  if (!channelName) {
    walk(data, (o) => {
      if (channelName) return;
      const t = o.c4TabbedHeaderRenderer && o.c4TabbedHeaderRenderer.title;
      if (t) channelName = textOf(t);
    });
  }
  if (!channelName) {
    walk(data, (o) => {
      if (channelName) return;
      const h = o.pageHeaderViewModel;
      if (h && h.title) channelName = textOf(h.title);
    });
  }

  // ---- 订阅数（旧 c4TabbedHeaderRenderer 或新版 contentMetadataViewModel）----
  let subscriberRaw = null;
  walk(data, (o) => {
    if (subscriberRaw) return;
    const s = o.c4TabbedHeaderRenderer && o.c4TabbedHeaderRenderer.subscriberCountText;
    if (s) {
      const t = textOf(s);
      if (t && t.trim()) subscriberRaw = t.trim();
    }
  });
  if (!subscriberRaw) {
    walk(data, (o) => {
      if (subscriberRaw) return;
      const rows = o.contentMetadataViewModel && o.contentMetadataViewModel.metadataRows;
      if (!rows) return;
      for (const row of rows) {
        for (const part of (row.metadataParts || [])) {
          const t = textOf(part.text);
          if (t && /\d/.test(t) && /订阅|subscriber/i.test(t)) { subscriberRaw = t.trim(); return; }
        }
      }
    });
  }

  // ---- 视频列表：videoRenderer / gridVideoRenderer + continuation 续取 ----
  const items = [];
  const seenUrls = new Set();
  const pushVideo = (v) => {
    if (!v || !v.videoId) return;
    const title = textOf(v.title);
    if (!title) return;
    const url = 'https://www.youtube.com/watch?v=' + v.videoId;
    if (seenUrls.has(url)) return;
    seenUrls.add(url);
    items.push({
      title,
      url,
      views: textOf(v.viewCountText),
      views_count: parseCount(textOf(v.viewCountText)),
      duration: textOf(v.lengthText),
      published: textOf(v.publishedTimeText),
    });
  };
  const pushLockup = (l) => {
    if (!l || !l.contentId || !l.metadata || !l.metadata.lockupMetadataViewModel) return;
    const m = l.metadata.lockupMetadataViewModel;
    const title = m.title && textOf(m.title);
    if (!title) return;
    const url = 'https://www.youtube.com/watch?v=' + l.contentId;
    if (seenUrls.has(url)) return;
    let views = null;
    let published = null;
    const rows = m.metadata && m.metadata.contentMetadataViewModel && m.metadata.contentMetadataViewModel.metadataRows;
    if (rows) {
      for (const row of rows) {
        for (const part of (row.metadataParts || [])) {
          const t = textOf(part.text);
          if (!t) continue;
          if (!views && /\d/.test(t) && /次观看|观看|views/i.test(t)) views = t;
          else if (!published && /前|ago|天|小时|分钟|周|月|年|昨天|today|yesterday/i.test(t)) published = t;
        }
      }
    }
    let duration = null;
    walk(l, (o) => {
      if (duration) return;
      const b = o.thumbnailBadgeViewModel;
      if (b && typeof b.text === 'string' && /^\d{1,2}:\d{2}(:\d{2})?$/.test(b.text)) {
        duration = b.text;
      }
    });
    seenUrls.add(url);
    items.push({
      title,
      url,
      views,
      views_count: parseCount(views),
      duration,
      published,
    });
  };
  const extract = (root) => {
    const out = { tokens: [] };
    walk(root, (o) => {
      if (o.videoRenderer) pushVideo(o.videoRenderer);
      else if (o.gridVideoRenderer) pushVideo(o.gridVideoRenderer);
      else if (o.lockupViewModel) pushLockup(o.lockupViewModel);
      const c = o.continuationItemRenderer;
      if (c && c.continuationEndpoint && c.continuationEndpoint.continuationCommand) {
        const tok = c.continuationEndpoint.continuationCommand.token;
        if (tok) out.tokens.push(tok);
      }
    });
    return out;
  };

  const first = extract(data);
  const keyM = html.match(/"INNERTUBE_API_KEY":"([^"]+)"/);
  let context = null;
  const ctxStart = html.indexOf('"INNERTUBE_CONTEXT":');
  if (ctxStart >= 0) {
    const start = html.indexOf('{', ctxStart);
    let depth = 0, inStr = false, esc = false, end = -1;
    for (let j = start; j < html.length; j++) {
      const ch = html[j];
      if (inStr) {
        if (esc) esc = false;
        else if (ch === '\\') esc = true;
        else if (ch === '"') inStr = false;
        continue;
      }
      if (ch === '"') inStr = true;
      else if (ch === '{') depth++;
      else if (ch === '}') {
        depth--;
        if (depth === 0) { end = j; break; }
      }
    }
    if (end > 0) {
      try { context = JSON.parse(html.slice(start, end + 1)); } catch (e) { context = null; }
    }
  }
  const api = async (tok) => {
    if (!keyM || !context) return null;
    try {
      const r = await fetch('/youtubei/v1/browse?key=' + encodeURIComponent(keyM[1]), {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ context, continuation: tok }),
      });
      const j = await r.json();
      if (!j || j.error) return null;
      return j;
    } catch (e) { return null; }
  };

  const queue = [];
  const seenTokens = new Set();
  const enqueue = (tok) => {
    if (tok && !seenTokens.has(tok)) { seenTokens.add(tok); queue.push(tok); }
  };
  for (const t of first.tokens) enqueue(t);
  let rounds = 0;
  while (items.length < MAX && queue.length > 0 && rounds < 20) {
    const tok = queue.shift();
    const j = await api(tok);
    if (!j) break;
    const next = extract(j);
    for (const t of next.tokens) enqueue(t);
    rounds++;
  }

  // ---- 标题兜底校验 ----
  // YouTube 频道页数据在部分会话/变体下会把超长标题截断（实测英文标题被截到
  // 100 字符，甚至只剩几个单词）。oEmbed 是 YouTube 官方接口，返回完整规范
  // 标题，且与页面同源可直接 fetch。仅当页面标题疑似截断（长度 >= 100 或含
  // 省略号）或 oEmbed 标题明显更长时才替换，正常标题不受影响。
  const looksTruncated = (t) => !t || t.length >= 100 || /…|\.\.\./.test(t);
  const oembedTitle = async (url) => {
    try {
      const r = await fetch('https://www.youtube.com/oembed?url=' + encodeURIComponent(url) + '&format=json', { signal: AbortSignal.timeout(5000) });
      if (!r.ok) return null;
      const j = await r.json();
      return j && typeof j.title === 'string' && j.title.trim() ? j.title.trim() : null;
    } catch (e) { return null; }
  };
  const targets = items.slice(0, MAX);
  let cursor = 0;
  const worker = async () => {
    while (cursor < targets.length) {
      const i = cursor++;
      const it = targets[i];
      const full = await oembedTitle(it.url);
      if (full && (looksTruncated(it.title) || full.length > (it.title || '').length)) {
        it.title = full;
      }
    }
  };
  await Promise.all(Array.from({ length: Math.min(6, targets.length) }, worker));

  return {
    ok: true,
    channel: {
      name: channelName,
      url: channelUrl,
      subscriber_count: parseCount(subscriberRaw),
      subscriber_count_text: subscriberRaw,
    },
    items: items.slice(0, MAX),
    total: items.length,
    rounds,
  };
})()"#;

/// YouTube 频道视频列表：导航到频道 /videos 页，返回
/// `{ tab_id, channel, videos }`。channel 含 name / url / subscriber_count /
/// subscriber_count_text；videos 每项含 title / url / views / views_count /
/// duration / published / target（target 可直接喂给 click 打开视频）。
/// `max` 控制最多返回多少条（至少 1，默认由调用方传 10）。
pub async fn youtuberinfo(
    bridge: &mut Bridge,
    channel: &str,
    max: usize,
    tab: Option<i32>,
) -> Result<Value, String> {
    let max = max.max(1);
    let url = videos_url(channel)?;
    let nav = bridge
        .request("ytbi1", "navigate", json!({ "url": url, "tab_id": tab }))
        .await?;
    let tab_id = nav.get("tab_id").cloned().unwrap_or(Value::Null);

    let code = EXTRACT_SCRIPT.replace("__MAX__", &max.to_string());
    let fetched = bridge
        .request(
            "ytbi2",
            "run_script",
            json!({ "code": code, "tab_id": tab_id }),
        )
        .await?;
    let res = fetched.get("result").cloned().unwrap_or(Value::Null);
    if !res.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        let reason = res
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(format!(
            "YouTube 频道数据缺失（{reason}），可能是频道不存在/地区限制/验证墙；确认浏览器能正常打开该频道后重试"
        ));
    }

    let channel_obj = res.get("channel").cloned().unwrap_or(Value::Null);
    let items = res
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut videos = Vec::with_capacity(items.len());
    for (i, it) in items.into_iter().enumerate() {
        videos.push(json!({
            "title": it.get("title").cloned().unwrap_or(Value::Null),
            "url": it.get("url").cloned().unwrap_or(Value::Null),
            "views": it.get("views").cloned().unwrap_or(Value::Null),
            "views_count": it.get("views_count").cloned().unwrap_or(Value::Null),
            "duration": it.get("duration").cloned().unwrap_or(Value::Null),
            "published": it.get("published").cloned().unwrap_or(Value::Null),
            "target": json!({
                "by": "css",
                "value": "a#video-title-link, a#video-title",
                "index": i,
            }),
        }));
    }

    Ok(json!({
        "tab_id": tab_id,
        "channel": channel_obj,
        "videos": videos,
    }))
}

#[cfg(test)]
mod tests {
    use super::videos_url;

    #[test]
    fn normalize_handle_url() {
        assert_eq!(
            videos_url("https://www.youtube.com/@xiaojunpodcast/videos").unwrap(),
            "https://www.youtube.com/@xiaojunpodcast/videos"
        );
        assert_eq!(
            videos_url("https://www.youtube.com/@xiaojunpodcast").unwrap(),
            "https://www.youtube.com/@xiaojunpodcast/videos"
        );
    }

    #[test]
    fn normalize_bare_handle_and_domainless() {
        assert_eq!(
            videos_url("@xiaojunpodcast").unwrap(),
            "https://www.youtube.com/@xiaojunpodcast/videos"
        );
        assert_eq!(
            videos_url("youtube.com/@xiaojunpodcast/shorts").unwrap(),
            "https://www.youtube.com/@xiaojunpodcast/videos"
        );
    }

    #[test]
    fn normalize_legacy_paths() {
        assert_eq!(
            videos_url("https://www.youtube.com/channel/UC123abc/videos").unwrap(),
            "https://www.youtube.com/channel/UC123abc/videos"
        );
        assert_eq!(
            videos_url("https://www.youtube.com/c/SomeName").unwrap(),
            "https://www.youtube.com/c/SomeName/videos"
        );
    }

    #[test]
    fn reject_empty() {
        assert!(videos_url("").is_err());
        assert!(videos_url("https://www.youtube.com").is_err());
    }
}
