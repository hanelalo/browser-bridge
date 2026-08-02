//! YouTube 视频详情配方：导航到指定视频的 watch 页，解析 HTML 内嵌的
//! ytInitialPlayerResponse / ytInitialData 拿到标题、作者、时长、点赞/评论/
//! 订阅数，并抓取字幕轨道全文。字幕优先用页面内嵌的 captionTracks（timedtext
//! json3）；若返回空（YouTube 对 `exp=xpe` 的轨道要求 PO token，页面内无法
//! 生成），则按 yt-dlp 的做法改用 android_vr 客户端调 player API 取无 pot
//! 要求的轨道。评论数用 InnerTube `next` continuation 接口（yt-dlp 同款数据
//! 源），不依赖滚动评论区。

use serde_json::{json, Value};

use crate::transport::Bridge;

/// 把用户输入（11 位视频 ID 或各种 watch / shorts / youtu.be 链接）归一化成 watch URL。
fn watch_url(input: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("视频 URL 或 ID 不能为空".to_string());
    }
    // 裸 ID：YouTube 视频 ID 固定 11 位，字符集为字母数字 + `-_`
    if input.len() == 11
        && input
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Ok(format!("https://www.youtube.com/watch?v={input}"));
    }
    // watch?v=<id>&...（支持 www / m 等任意子域）
    if let Some(pos) = input.find("watch?v=") {
        let id = input[pos + "watch?v=".len()..]
            .split('&')
            .next()
            .unwrap_or("")
            .trim();
        if !id.is_empty() {
            return Ok(format!("https://www.youtube.com/watch?v={id}"));
        }
    }
    // youtu.be/<id> / /shorts/<id> / /embed/<id> / /live/<id>
    for marker in ["youtu.be/", "/shorts/", "/embed/", "/live/"] {
        if let Some(pos) = input.find(marker) {
            let id = input[pos + marker.len()..]
                .split(['/', '?', '#'])
                .next()
                .unwrap_or("")
                .trim();
            if !id.is_empty() {
                return Ok(format!("https://www.youtube.com/watch?v={id}"));
            }
        }
    }
    Err(format!(
        "无法从 '{input}' 识别视频 ID，请提供 watch URL（https://www.youtube.com/watch?v=...）或 11 位视频 ID"
    ))
}

/// 页面内执行的提取脚本。数据全部取自导航返回的 HTML 内嵌 JSON + InnerTube
/// 接口，不需要页面渲染、滚动或点击。
const EXTRACT_SCRIPT: &str = r#"(async () => {
  const html = document.documentElement.outerHTML;
  const grab = (name) => {
    const start = html.indexOf('var ' + name + ' = {');
    if (start < 0) return null;
    const objStart = html.indexOf('{', start);
    let depth = 0;
    let inStr = false;
    let esc = false;
    let end = -1;
    for (let j = objStart; j < html.length; j++) {
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
    if (end < 0) return null;
    try { return JSON.parse(html.slice(objStart, end + 1)); } catch (e) { return null; }
  };
  const walk = (o, cb) => {
    if (!o || typeof o !== 'object') return;
    if (Array.isArray(o)) { for (const v of o) walk(v, cb); return; }
    cb(o);
    for (const k of Object.keys(o)) walk(o[k], cb);
  };
  const initial = grab('ytInitialData');
  const player = grab('ytInitialPlayerResponse');
  if (!player || !player.videoDetails) {
    return { ok: false, reason: 'no ytInitialPlayerResponse' };
  }
  const status = player.playabilityStatus && player.playabilityStatus.status;
  if (status && status !== 'OK' && status !== 'LOGIN_REQUIRED') {
    return { ok: false, reason: 'unplayable: ' + status };
  }
  const vd = player.videoDetails || {};
  const videoId = vd.videoId || null;
  const title = vd.title || null;
  const parsedLength = parseInt(vd.lengthSeconds, 10);
  const durationSeconds = Number.isFinite(parsedLength) ? parsedLength : null;
  const fmtDuration = (s) => {
    if (s === null) return null;
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    const sec = s % 60;
    const mm = String(m).padStart(2, '0');
    const ss = String(sec).padStart(2, '0');
    return h > 0 ? h + ':' + mm + ':' + ss : Number(m) + ':' + ss;
  };

  // ---- ytInitialData：作者 / 订阅数 / 点赞数 ----
  let author = null;
  let authorUrl = null;
  let subscriberText = null;
  let likeRaw = null;
  if (initial) {
    walk(initial, (o) => {
      if (author) return;
      const v = o.videoOwnerRenderer;
      if (!v) return;
      if (v.title && v.title.runs) {
        author = v.title.runs.map((r) => r.text || '').join('');
      }
      const nav = v.navigationEndpoint && v.navigationEndpoint.commandMetadata
        && v.navigationEndpoint.commandMetadata.webCommandMetadata;
      if (nav && nav.url) {
        authorUrl = nav.url.indexOf('http') === 0 ? nav.url : 'https://www.youtube.com' + nav.url;
      }
    });
    walk(initial, (o) => {
      if (subscriberText) return;
      const v = o.videoOwnerRenderer;
      if (!v || !v.subscriberCountText) return;
      const s = v.subscriberCountText;
      const t = s.simpleText || (s.runs ? s.runs.map((r) => r.text || '').join('') : null);
      if (typeof t === 'string') subscriberText = t;
    });
    // 新版点赞按钮：iconName=LIKE 的 buttonViewModel.title 直接就是计数（如 "1.2万"）
    walk(initial, (o) => {
      if (likeRaw) return;
      if (o.buttonViewModel && o.buttonViewModel.iconName === 'LIKE' && o.buttonViewModel.title) {
        likeRaw = o.buttonViewModel.title;
      }
    });
    if (!likeRaw) {
      walk(initial, (o) => {
        if (likeRaw) return;
        const d = o.likeButtonStateData;
        if (d && typeof d.likeCount === 'string') {
          const digits = d.likeCount.replace(/[^0-9]/g, '');
          if (digits.length > 0) likeRaw = d.likeCount;
        }
      });
    }
    if (!likeRaw) {
      walk(initial, (o) => {
        if (likeRaw) return;
        const t = o.accessibilityText
          || (o.toggleButtonViewModel && o.toggleButtonViewModel.defaultButtonViewModel
              && o.toggleButtonViewModel.defaultButtonViewModel.accessibility
              && o.toggleButtonViewModel.defaultButtonViewModel.accessibility.text);
        if (typeof t === 'string' && /like|赞|喜欢/i.test(t) && /[0-9]/.test(t)) likeRaw = t;
      });
    }
  }

  // ---- InnerTube key / context / visitorData（页面内调接口用）----
  const keyM = html.match(/"INNERTUBE_API_KEY":"([^"]+)"/);
  const visitorM = html.match(/"VISITOR_DATA":"([^"]+)"/);
  let context = null;
  const ctxStart = html.indexOf('"INNERTUBE_CONTEXT":');
  if (ctxStart >= 0) {
    const start = html.indexOf('{', ctxStart);
    let depth = 0;
    let inStr = false;
    let esc = false;
    let end = -1;
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
  const api = async (endpoint, body) => {
    if (!keyM || !context) return { j: null };
    try {
      const r = await fetch('/youtubei/v1/' + endpoint + '?key=' + encodeURIComponent(keyM[1]), {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(body),
      });
      let j = null;
      try { j = await r.json(); } catch (e) { j = null; }
      return { j: j };
    } catch (e) { return { j: null }; }
  };

  // ---- 评论数：用 InnerTube next + comments-section continuation ----
  let commentText = null;
  if (initial && keyM && context) {
    const toks = [];
    let lastTid = null;
    walk(initial, (o) => {
      if (o.targetId) lastTid = o.targetId;
      const c = o.continuationItemRenderer;
      if (c && c.continuationEndpoint && c.continuationEndpoint.continuationCommand) {
        const tok = c.continuationEndpoint.continuationCommand.token;
        if (tok) toks.push({ tok: tok, tid: lastTid });
      }
    });
    toks.sort((a, b) => (a.tid === 'comments-section' ? -1 : 1) - (b.tid === 'comments-section' ? -1 : 1));
    for (const t of toks.slice(0, 4)) {
      const r = await api('next', { context: context, continuation: t.tok });
      if (!r.j) continue;
      let ct = null;
      walk(r.j, (o) => {
        if (ct) return;
        if (o.commentsHeaderRenderer && o.commentsHeaderRenderer.countText) {
          const c = o.commentsHeaderRenderer.countText;
          const v = c.simpleText || (c.runs ? c.runs.map((x) => x.text || '').join('') : null);
          if (typeof v === 'string' && /[0-9]/.test(v)) ct = v;
        }
      });
      if (ct) { commentText = ct; break; }
    }
  }
  if (!commentText && initial) {
    walk(initial, (o) => {
      if (commentText) return;
      const h = o.commentsHeaderRenderer;
      if (h && h.countText) {
        const c = h.countText;
        const t = c.simpleText || (c.runs ? c.runs.map((r) => r.text || '').join('') : null);
        if (typeof t === 'string' && /[0-9]/.test(t)) commentText = t;
      }
    });
  }

  // ---- 字幕：页面内嵌轨道优先，失败则 android_vr 客户端兜底（yt-dlp 同款）----
  const nameOf = (t) => t.name
    ? (t.name.simpleText || (t.name.runs ? t.name.runs.map((r) => r.text || '').join('') : null))
    : null;
  const fetchTrack = async (t) => {
    let base = t.baseUrl || '';
    base = base.replace(/[?&]fmt=[^&]*/g, '');
    base += (base.indexOf('?') >= 0 ? '&' : '?') + 'fmt=json3';
    let text = null;
    let error = null;
    try {
      const r = await fetch(base, { credentials: 'include' });
      if (!r.ok) throw new Error('HTTP ' + r.status);
      const j = await r.json();
      const parts = [];
      for (const ev of (j.events || [])) {
        if (Array.isArray(ev.segs)) {
          for (const s of ev.segs) if (s.utf8) parts.push(s.utf8);
        }
      }
      text = parts.join('').replace(/\s+/g, ' ').trim();
      if (!text) error = 'empty transcript';
    } catch (e) {
      error = e && e.message ? e.message : String(e);
    }
    return {
      language_code: t.languageCode || null,
      name: nameOf(t),
      kind: t.kind || null,
      text: text,
      error: error,
    };
  };
  const embeddedTracks = (player.captions && player.captions.playerCaptionsTracklistRenderer
    && player.captions.playerCaptionsTracklistRenderer.captionTracks) || [];
  let captions = [];
  for (const t of embeddedTracks) captions.push(await fetchTrack(t));
  if (!captions.some((c) => c.text) && keyM && context && visitorM) {
    const vr = await api('player', {
      context: {
        client: {
          clientName: 'ANDROID_VR',
          clientVersion: '1.65.10',
          deviceMake: 'Oculus',
          deviceModel: 'Quest 3',
          androidSdkVersion: 32,
          userAgent: 'com.google.android.apps.youtube.vr.oculus/1.65.10 (Linux; U; Android 12L; eureka-user Build/SQ3A.220605.009.A1) gzip',
          osName: 'Android',
          osVersion: '12L',
          visitorData: visitorM[1],
          hl: 'en',
          gl: 'US',
        },
      },
      videoId: videoId,
      contentCheckOk: true,
      racyCheckOk: true,
    });
    if (vr.j && !vr.j.error) {
      const vrTracks = (vr.j.captions && vr.j.captions.playerCaptionsTracklistRenderer
        && vr.j.captions.playerCaptionsTracklistRenderer.captionTracks) || [];
      const vrCaps = [];
      for (const t of vrTracks) vrCaps.push(await fetchTrack(t));
      if (vrCaps.some((c) => c.text)) captions = vrCaps;
    }
  }

  const parseCount = (s) => {
    if (!s) return null;
    const m = String(s).match(/([0-9][0-9,]*\.?[0-9]*)\s*([万亿KMB])?/i);
    if (!m) return null;
    const n = parseFloat(m[1].replace(/,/g, ''));
    if (!Number.isFinite(n)) return null;
    const mult = {
      k: 1000, m: 1000000, b: 1000000000,
      '万': 10000, '亿': 100000000,
    }[m[2] ? m[2].toLowerCase() : ''];
    return mult ? Math.round(n * mult) : Math.round(n);
  };

  return {
    ok: true,
    video: {
      url: videoId ? 'https://www.youtube.com/watch?v=' + videoId : null,
      title: title,
      author: author,
      author_url: authorUrl,
      duration: fmtDuration(durationSeconds),
      duration_seconds: durationSeconds,
      like_count: parseCount(likeRaw),
      like_count_text: likeRaw,
      comment_count: parseCount(commentText),
      comment_count_text: commentText,
      subscriber_count: parseCount(subscriberText),
      subscriber_count_text: subscriberText,
      captions: captions,
    },
  };
})()"#;

/// YouTube 视频详情：导航到视频 watch 页，返回 `{ tab_id, video }`。
/// video 含 url / title / author / author_url / duration / duration_seconds /
/// like_count / like_count_text / comment_count / comment_count_text /
/// subscriber_count / subscriber_count_text / captions[]（每个字幕轨道含
/// language_code / name / kind / text 全文 / error）。
pub async fn youtubeinfo(
    bridge: &mut Bridge,
    video: &str,
    tab: Option<i32>,
) -> Result<Value, String> {
    let url = watch_url(video)?;
    let nav = bridge
        .request("yiv1", "navigate", json!({ "url": url, "tab_id": tab }))
        .await?;
    let tab_id = nav.get("tab_id").cloned().unwrap_or(Value::Null);

    let fetched = bridge
        .request(
            "yiv2",
            "run_script",
            json!({ "code": EXTRACT_SCRIPT, "tab_id": tab_id }),
        )
        .await?;
    let res = fetched.get("result").cloned().unwrap_or(Value::Null);
    if !res.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        let reason = res
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(format!(
            "YouTube 视频数据缺失（{reason}），可能是视频不可用/地区限制/验证墙；确认浏览器能正常打开该视频后重试"
        ));
    }
    let video = res.get("video").cloned().unwrap_or(Value::Null);
    Ok(json!({ "tab_id": tab_id, "video": video }))
}
