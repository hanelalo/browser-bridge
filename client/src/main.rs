//! Browser Bridge CLI client（控制端）。

use clap::{Args, Parser, Subcommand};
use serde_json::{json, Value};

use bridge_core::recipes::googlesearch::googlesearch;
use bridge_core::recipes::googletrends::googletrends;
use bridge_core::recipes::googletrends::googletrends_compare;
use bridge_core::recipes::querydomains::querydomains;
use bridge_core::recipes::redditsearch::redditsearch;
use bridge_core::recipes::youtubeinfo::youtubeinfo;
use bridge_core::recipes::youtubesearch::youtubesearch;
use bridge_core::target;
use bridge_core::transport::Bridge;

#[derive(Parser)]
#[command(
    name = "bridge-client",
    version,
    about = "Browser Bridge CLI（控制端）"
)]
struct Cli {
    /// WebSocket 服务地址
    #[arg(long, default_value = "ws://127.0.0.1:9225", env = "BRIDGE_SERVER")]
    server: String,

    #[command(subcommand)]
    cmd: Cmd,
}

/// 元素定位参数：css / text / xpath 三种方式，可指定第几个匹配。
#[derive(Args)]
struct Target {
    /// 定位值：CSS selector / 可见文本 / XPath
    #[arg(value_name = "TARGET", id = "target")]
    value: String,
    /// 定位方式：css | text | xpath
    #[arg(long, default_value = "css")]
    by: String,
    /// 第几个匹配（从 0 开始，默认 0）
    #[arg(long, id = "target_index", value_name = "INDEX")]
    index: Option<usize>,
}

impl Target {
    fn spec(&self) -> Value {
        target::spec(&self.by, &self.value, self.index)
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// 列出所有标签页
    ListTabs,
    /// 关闭标签页（默认当前激活标签页）
    CloseTab {
        /// 指定标签页 id（默认当前激活标签页）
        #[arg(long)]
        tab: Option<i32>,
    },
    /// 关闭 bridge 自动打开的全部标签页（不碰手动开的）
    CloseAutoTabs,
    /// 新建标签页（可选打开 URL）
    NewTab {
        /// 新标签页打开的 URL（省略则为空白页）
        url: Option<String>,
    },
    /// 切换到指定标签页并聚焦所在窗口
    ActivateTab {
        /// 指定标签页 id（默认当前激活标签页）
        #[arg(long)]
        tab: Option<i32>,
    },
    /// 搜索 Google 并返回结构化结果（JSON 数组：title / description / url）
    Googlesearch {
        /// 搜索关键词
        query: String,
        /// 指定标签页 id（默认当前激活标签页）
        #[arg(long)]
        tab: Option<i32>,
    },
    /// 搜索 Reddit 并返回结构化结果（JSON 数组：title / description / published / published_at / votes / comments / url；votes/comments 为整数数量）
    Redditsearch {
        /// 搜索关键词
        query: String,
        /// 指定标签页 id（默认当前激活标签页）
        #[arg(long)]
        tab: Option<i32>,
    },
    /// 搜索 YouTube 并返回结构化结果（title / channel / views / published / duration / url）
    Youtubesearch {
        /// 搜索关键词
        query: String,
        /// 上传日期筛选（默认 any；today / week / month / year）
        #[arg(long, default_value = "any")]
        time: String,
        /// 优先顺序（默认 relevance；popularity = 热门程度）
        #[arg(long, default_value = "relevance")]
        sort: String,
        /// 最多返回的结果数（默认 5；直接解析数据并翻页续取，无需页面渲染）
        #[arg(long, default_value_t = 5)]
        max: usize,
        /// 指定标签页 id（默认当前激活标签页）
        #[arg(long)]
        tab: Option<i32>,
    },
    /// 获取指定 YouTube 视频的详情（字幕全文、URL、作者、时长、点赞/评论/订阅数）
    Youtubeinfo {
        /// 视频 URL 或 11 位视频 ID（watch?v= / youtu.be / shorts / embed / live 均可）
        url: String,
        /// 指定标签页 id（默认当前激活标签页）
        #[arg(long)]
        tab: Option<i32>,
    },
    /// 查询 Google Trends，返回趋势序列 + 热门/上升关键词
    Googletrends {
        /// 搜索关键词
        query: String,
        /// 时间范围（默认 today 1-m；如 today 3-m / today 12-m / today 5-y / all）
        #[arg(long, default_value = "today 1-m")]
        date: String,
        /// 地区（默认 Worldwide）
        #[arg(long, default_value = "Worldwide")]
        geo: String,
    },
    /// 对比多个关键词在 Google Trends 的走势（共享 0-100 刻度）
    GoogletrendsCompare {
        /// 关键词（可多个；逗号分隔的写法也会被拆分）
        #[arg(required = true)]
        terms: Vec<String>,
        /// 时间范围（默认 today 1-m；如 today 3-m / today 12-m / today 5-y / all）
        #[arg(long, default_value = "today 1-m")]
        date: String,
        /// 地区（默认 Worldwide）
        #[arg(long, default_value = "Worldwide")]
        geo: String,
    },
    /// 用 Query.Domains 按关键词批量查询域名注册情况与价格（WHOIS，SSE 流式返回）
    Querydomains {
        /// 域名关键词（如 browserbridge）
        query: String,
        /// 要检查的 TLD 列表，逗号分隔（默认 com,ai,org,net,cn,info,app,io,xyz,co,run,me,pro,top；最多 20 个）
        #[arg(long, value_delimiter = ',')]
        tlds: Option<Vec<String>>,
        /// 指定标签页 id（默认当前激活标签页）
        #[arg(long)]
        tab: Option<i32>,
    },
    /// 导航到指定 URL
    Navigate {
        url: String,
        /// 指定标签页 id（默认当前激活标签页）
        #[arg(long)]
        tab: Option<i32>,
    },
    /// 点击匹配定位的元素
    Click {
        #[command(flatten)]
        target: Target,
        /// 锚点链接在新标签页打开（默认当前标签页打开，避免流程开 tab 堆积）
        #[arg(long)]
        new_tab: bool,
        /// 等待元素出现的最长时间（毫秒，默认 5000）
        #[arg(long)]
        timeout: Option<u64>,
        /// 指定标签页 id（默认当前激活标签页）
        #[arg(long)]
        tab: Option<i32>,
    },
    /// 按坐标点击
    ClickAt {
        /// 页面坐标 x
        x: f64,
        /// 页面坐标 y
        y: f64,
        /// 指定标签页 id（默认当前激活标签页）
        #[arg(long)]
        tab: Option<i32>,
    },
    /// 模拟按键（如 Enter、Escape、a、F5）
    PressKey {
        /// KeyboardEvent.key 值
        key: String,
        /// 修饰键，可重复：--modifier ctrl --modifier shift
        #[arg(long = "modifier", action = clap::ArgAction::Append)]
        modifiers: Vec<String>,
        /// 目标元素（默认派发到当前聚焦元素）
        #[arg(long)]
        target: Option<String>,
        /// 目标元素定位方式：css | text | xpath
        #[arg(long, default_value = "css")]
        by: String,
        /// 第几个匹配（从 0 开始）
        #[arg(long)]
        index: Option<usize>,
        /// 按键后等待页面加载完成（如回车触发导航）
        #[arg(long)]
        wait_load: bool,
        /// 指定标签页 id（默认当前激活标签页）
        #[arg(long)]
        tab: Option<i32>,
    },
    /// 在页面里执行一段 JS 表达式/函数体，返回 JSON 序列化结果
    RunScript {
        /// JS 代码（表达式，可返回 Promise）
        code: String,
        /// 指定标签页 id（默认当前激活标签页）
        #[arg(long)]
        tab: Option<i32>,
    },
    /// 滚动窗口或指定滚动容器
    Scroll {
        /// 水平滚动量
        #[arg(long, default_value_t = 0.0)]
        dx: f64,
        /// 垂直滚动量
        #[arg(long, default_value_t = 0.0)]
        dy: f64,
        /// 滚动容器元素（省略则滚动整个窗口）
        #[arg(long)]
        target: Option<String>,
        /// 滚动容器定位方式：css | text | xpath
        #[arg(long, default_value = "css")]
        by: String,
        /// 第几个匹配（从 0 开始）
        #[arg(long)]
        index: Option<usize>,
        /// 平滑滚动
        #[arg(long)]
        smooth: bool,
        /// 指定标签页 id（默认当前激活标签页）
        #[arg(long)]
        tab: Option<i32>,
    },
    /// 设置 input / textarea / contenteditable 的值
    SetValue {
        #[command(flatten)]
        target: Target,
        /// 要设置的值
        value: String,
        /// 指定标签页 id（默认当前激活标签页）
        #[arg(long)]
        tab: Option<i32>,
    },
    /// 勾选/取消勾选 checkbox 或 radio
    Check {
        #[command(flatten)]
        target: Target,
        /// 取消勾选
        #[arg(long)]
        uncheck: bool,
        /// 指定标签页 id（默认当前激活标签页）
        #[arg(long)]
        tab: Option<i32>,
    },
    /// 选中 <select> 的某个选项（value / text / index 三选一）
    SelectOption {
        #[command(flatten)]
        target: Target,
        /// 按 option 的 value 匹配
        #[arg(long)]
        value: Option<String>,
        /// 按 option 的显示文本匹配
        #[arg(long)]
        text: Option<String>,
        /// 按 option 下标匹配
        #[arg(long = "option-index")]
        option_index: Option<usize>,
        /// 指定标签页 id（默认当前激活标签页）
        #[arg(long)]
        tab: Option<i32>,
    },
    /// 清空 input / textarea / contenteditable
    Clear {
        #[command(flatten)]
        target: Target,
        /// 指定标签页 id（默认当前激活标签页）
        #[arg(long)]
        tab: Option<i32>,
    },
    /// 读取元素当前值（用于验证）
    GetValue {
        #[command(flatten)]
        target: Target,
        /// 指定标签页 id（默认当前激活标签页）
        #[arg(long)]
        tab: Option<i32>,
    },
    /// 按 CSS 选择器提取结构化数据（不依赖 eval，CSP 安全）
    Scrape {
        /// 每条结果的容器选择器
        item: String,
        /// 自定义字段映射：逗号分隔的 字段名:选择器[@属性]（与 --title/--link/--desc 二选一，优先）
        #[arg(long)]
        fields: Option<String>,
        /// 标题字段选择器（相对 item）
        #[arg(long)]
        title: Option<String>,
        /// 链接字段选择器（相对 item）
        #[arg(long)]
        link: Option<String>,
        /// 描述字段选择器（相对 item）
        #[arg(long)]
        desc: Option<String>,
        /// 等待结果出现的最长时间（毫秒，默认 5000）
        #[arg(long)]
        timeout: Option<u64>,
        /// 指定标签页 id（默认当前激活标签页）
        #[arg(long)]
        tab: Option<i32>,
    },
    /// 读取页面文本内容
    GetPageContent {
        /// 指定标签页 id（默认当前激活标签页）
        #[arg(long)]
        tab: Option<i32>,
    },
    /// 把页面内容转换成标准 Markdown（标题/段落/列表/表格/代码块/链接/图片）
    GetPageMarkdown {
        /// 可选：先导航到该 URL 再转换（省略则用当前/指定标签页）
        #[arg(long)]
        url: Option<String>,
        /// 可选：只转换匹配选择器的容器（如 article / #content）
        #[arg(long)]
        selector: Option<String>,
        /// 指定标签页 id（默认当前激活标签页）
        #[arg(long)]
        tab: Option<i32>,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let (method, params) = match cli.cmd {
        Cmd::ListTabs => ("list_tabs", json!({})),
        Cmd::CloseTab { tab } => ("close_tab", json!({ "tab_id": tab })),
        Cmd::CloseAutoTabs => ("close_auto_tabs", json!({})),
        Cmd::NewTab { url } => ("new_tab", json!({ "url": url })),
        Cmd::ActivateTab { tab } => ("activate_tab", json!({ "tab_id": tab })),
        Cmd::Googlesearch { query, tab } => {
            let mut bridge = match Bridge::connect(&cli.server).await {
                Ok(b) => b,
                Err(err) => {
                    eprintln!("error: {err}");
                    std::process::exit(1);
                }
            };
            match googlesearch(&mut bridge, &query, tab).await {
                Ok(out) => println!("{}", serde_json::to_string_pretty(&out).unwrap()),
                Err(err) => {
                    eprintln!("error: {err}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Cmd::Redditsearch { query, tab } => {
            let mut bridge = match Bridge::connect(&cli.server).await {
                Ok(b) => b,
                Err(err) => {
                    eprintln!("error: {err}");
                    std::process::exit(1);
                }
            };
            match redditsearch(&mut bridge, &query, tab).await {
                Ok(out) => println!("{}", serde_json::to_string_pretty(&out).unwrap()),
                Err(err) => {
                    eprintln!("error: {err}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Cmd::Youtubesearch {
            query,
            time,
            sort,
            max,
            tab,
        } => {
            let mut bridge = match Bridge::connect(&cli.server).await {
                Ok(b) => b,
                Err(err) => {
                    eprintln!("error: {err}");
                    std::process::exit(1);
                }
            };
            match youtubesearch(&mut bridge, &query, &time, &sort, max, tab).await {
                Ok(out) => println!("{}", serde_json::to_string_pretty(&out).unwrap()),
                Err(err) => {
                    eprintln!("error: {err}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Cmd::Youtubeinfo { url, tab } => {
            let mut bridge = match Bridge::connect(&cli.server).await {
                Ok(b) => b,
                Err(err) => {
                    eprintln!("error: {err}");
                    std::process::exit(1);
                }
            };
            match youtubeinfo(&mut bridge, &url, tab).await {
                Ok(out) => println!("{}", serde_json::to_string_pretty(&out).unwrap()),
                Err(err) => {
                    eprintln!("error: {err}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Cmd::Googletrends { query, date, geo } => {
            let mut bridge = match Bridge::connect(&cli.server).await {
                Ok(b) => b,
                Err(err) => {
                    eprintln!("error: {err}");
                    std::process::exit(1);
                }
            };
            match googletrends(&mut bridge, &query, &date, &geo).await {
                Ok(out) => println!("{}", serde_json::to_string_pretty(&out).unwrap()),
                Err(err) => {
                    eprintln!("error: {err}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Cmd::GoogletrendsCompare { terms, date, geo } => {
            let terms: Vec<String> = terms
                .iter()
                .flat_map(|t| t.split(',').map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect();
            let mut bridge = match Bridge::connect(&cli.server).await {
                Ok(b) => b,
                Err(err) => {
                    eprintln!("error: {err}");
                    std::process::exit(1);
                }
            };
            match googletrends_compare(&mut bridge, &terms, &date, &geo).await {
                Ok(out) => println!("{}", serde_json::to_string_pretty(&out).unwrap()),
                Err(err) => {
                    eprintln!("error: {err}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Cmd::Querydomains { query, tlds, tab } => {
            let tlds = tlds.unwrap_or_default();
            let mut bridge = match Bridge::connect(&cli.server).await {
                Ok(b) => b,
                Err(err) => {
                    eprintln!("error: {err}");
                    std::process::exit(1);
                }
            };
            match querydomains(&mut bridge, &query, &tlds, tab).await {
                Ok(out) => println!("{}", serde_json::to_string_pretty(&out).unwrap()),
                Err(err) => {
                    eprintln!("error: {err}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Cmd::Navigate { url, tab } => ("navigate", json!({ "url": url, "tab_id": tab })),
        Cmd::Click {
            target,
            new_tab,
            timeout,
            tab,
        } => {
            let mut params = json!({ "target": target.spec(), "tab_id": tab });
            if new_tab {
                params["new_tab"] = json!(true);
            }
            if let Some(t) = timeout {
                params["timeout"] = json!(t);
            }
            ("click", params)
        }
        Cmd::ClickAt { x, y, tab } => ("click_at", json!({ "x": x, "y": y, "tab_id": tab })),
        Cmd::PressKey {
            key,
            modifiers,
            target,
            by,
            index,
            wait_load,
            tab,
        } => {
            let mut params = json!({ "key": key, "tab_id": tab });
            if !modifiers.is_empty() {
                params["modifiers"] = json!(modifiers);
            }
            if wait_load {
                params["wait_load"] = json!(true);
            }
            if let Some(t) = target::optional_target(target.as_deref(), &by, index) {
                params["target"] = t;
            }
            ("press_key", params)
        }
        Cmd::RunScript { code, tab } => (
            "run_script",
            json!({ "code": code, "tab_id": tab }),
        ),
        Cmd::Scroll {
            dx,
            dy,
            target,
            by,
            index,
            smooth,
            tab,
        } => {
            let mut params = json!({
                "dx": dx,
                "dy": dy,
                "smooth": smooth,
                "tab_id": tab,
            });
            if let Some(t) = target::optional_target(target.as_deref(), &by, index) {
                params["target"] = t;
            }
            ("scroll", params)
        }
        Cmd::SetValue {
            target,
            value,
            tab,
        } => (
            "set_value",
            json!({ "target": target.spec(), "value": value, "tab_id": tab }),
        ),
        Cmd::Check {
            target,
            uncheck,
            tab,
        } => (
            "check",
            json!({
                "target": target.spec(),
                "checked": !uncheck,
                "tab_id": tab,
            }),
        ),
        Cmd::SelectOption {
            target,
            value,
            text,
            option_index,
            tab,
        } => {
            let mut params = json!({ "target": target.spec(), "tab_id": tab });
            if let Some(v) = value {
                params["value"] = json!(v);
            }
            if let Some(t) = text {
                params["text"] = json!(t);
            }
            if let Some(i) = option_index {
                params["index"] = json!(i);
            }
            ("select_option", params)
        }
        Cmd::Clear { target, tab } => (
            "clear",
            json!({ "target": target.spec(), "tab_id": tab }),
        ),
        Cmd::GetValue { target, tab } => (
            "get_value",
            json!({ "target": target.spec(), "tab_id": tab }),
        ),
        Cmd::Scrape {
            item,
            fields,
            title,
            link,
            desc,
            timeout,
            tab,
        } => {
            let mut params = json!({ "item": item, "tab_id": tab });
            if let Some(f) = fields {
                match parse_fields(&f) {
                    Ok(v) => {
                        params["fields"] = v;
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        std::process::exit(1);
                    }
                }
            }
            if let Some(t) = title {
                params["title"] = json!(t);
            }
            if let Some(l) = link {
                params["link"] = json!(l);
            }
            if let Some(d) = desc {
                params["desc"] = json!(d);
            }
            if let Some(t) = timeout {
                params["timeout"] = json!(t);
            }
            ("scrape", params)
        }
        Cmd::GetPageContent { tab } => ("get_page_content", json!({ "tab_id": tab })),
        Cmd::GetPageMarkdown { url, selector, tab } => {
            let mut params = json!({ "tab_id": tab });
            if let Some(u) = url {
                params["url"] = json!(u);
            }
            if let Some(s) = selector {
                params["selector"] = json!(s);
            }
            ("get_page_markdown", params)
        }
    };

    if let Err(err) = run(&cli.server, method, params).await {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

/// 发送单个通用指令并打印结果。
async fn run(server: &str, method: &str, params: Value) -> Result<(), String> {
    let mut bridge = Bridge::connect(server).await?;
    let result = bridge.request("c1", method, params).await?;
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
    Ok(())
}

/// 解析 --fields 'name:.name,price:.price,img:img@src' 为 { 字段名: 选择器[@属性] }。
fn parse_fields(input: &str) -> Result<Value, String> {
    let mut obj = serde_json::Map::new();
    for part in input.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (key, spec) = part
            .split_once(':')
            .ok_or_else(|| format!("字段格式应为 字段名:选择器，实际为：{part}"))?;
        let key = key.trim();
        let spec = spec.trim();
        if key.is_empty() || spec.is_empty() {
            return Err(format!("字段格式应为 字段名:选择器，实际为：{part}"));
        }
        obj.insert(key.to_string(), Value::String(spec.to_string()));
    }
    if obj.is_empty() {
        return Err("--fields 不能为空".to_string());
    }
    Ok(Value::Object(obj))
}
