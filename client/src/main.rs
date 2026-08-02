//! Browser Bridge CLI client（控制端）。

use clap::{Args, Parser, Subcommand};
use serde_json::{json, Value};

use bridge_core::recipes::googlesearch::googlesearch;
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
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let (method, params) = match cli.cmd {
        Cmd::ListTabs => ("list_tabs", json!({})),
        Cmd::CloseTab { tab } => ("close_tab", json!({ "tab_id": tab })),
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
        Cmd::Navigate { url, tab } => ("navigate", json!({ "url": url, "tab_id": tab })),
        Cmd::Click {
            target,
            timeout,
            tab,
        } => {
            let mut params = json!({ "target": target.spec(), "tab_id": tab });
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
