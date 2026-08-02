//! 站点配方：每个文件封装一个站点的搜索/提取逻辑，
//! 只依赖通用协议指令（navigate / scrape / click 等），扩展与协议保持通用。

pub mod googlesearch;
pub mod googletrends;
pub mod redditsearch;
