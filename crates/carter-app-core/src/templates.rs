//! 渠道模板：新建渠道时「这是哪家」的那一排按钮。
//!
//! 每条模板要给出四样东西：写进配置的 `kind`（决定线协议）、默认 `base_url`、
//! 界面上的标签，以及 models.dev 里的服务商 id。有了最后一项，模型列表可以**直接
//! 从目录填好** —— 官方渠道的模型名与目录 id 本来就一一对应，不用先联网问一遍。
//!
//! ## 为什么模板只有这十来个
//!
//! 每个模板都是「一家服务商 + 一个核实过的端点」。多写一条写错的端点，用户会照着
//! 填、然后拿到一个 404，而 404 的真实原因（路径拼错）从日志里几乎看不出来。
//! 所以这张表只收确认过的；其余都走「自定义」，反正 `kind` + `base_url` 两个框
//! 就能接任何一家。
//!
//! ## `kind` 的取舍
//!
//! `kind` 只写三种线协议之一（`anthropic` / `openai_chat` / `openai_responses`），
//! 服务商身份由模板带来的 `base_url` 表达。不写 `deepseek` / `xai` 这类别名有两个
//! 好处：界面上「协议」只有一个下拉、三个选项，用户不用先学会 carter 的别名表；
//! 而 `base_url` 反正要写下来（模板就是干这个的），别名能省的默认地址也就省不掉了。
//!
//! ## 与 `lib/protocols.ts` 的分工
//!
//! 线协议那三种是**协议**；模板是**服务商**。同一家可以支持多种协议（DeepSeek 三种
//! 都认），模板选的是「最不会出错」的那种。

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ProviderTemplate {
    /// 稳定标识，界面拿它做 key 与「当前选中」。不是渠道名。
    pub id: &'static str,
    /// 按钮上的名字。
    pub label: &'static str,
    /// 写进配置的 `kind`。
    pub kind: &'static str,
    /// 这个 `kind` 实际走的线协议。
    pub protocol: &'static str,
    /// 默认端点。`base_url` 留空时 carter 会用协议默认地址；模板一律写死，
    /// 因为「官方地址」本身就是模板要解决的问题。
    pub base_url: &'static str,
    /// 填进「渠道名」的默认值。
    pub name: &'static str,
    /// models.dev 的服务商 id。有值时模型列表可以直接从目录预填。
    pub catalog: Option<&'static str>,
    /// 一句话说明（为什么是这个协议 / 端点）。
    pub note: &'static str,
}

/// 全部模板。官方几家在前 —— 大多数人的第一件事就是配它们。
pub const TEMPLATES: &[ProviderTemplate] = &[
    ProviderTemplate {
        id: "anthropic",
        label: "Anthropic",
        kind: "anthropic",
        protocol: "anthropic",
        base_url: "https://api.anthropic.com",
        name: "anthropic",
        catalog: Some("anthropic"),
        note: "Messages 协议，原样使用",
    },
    ProviderTemplate {
        id: "openai",
        label: "OpenAI",
        kind: "openai_responses",
        protocol: "openai_responses",
        base_url: "https://api.openai.com",
        name: "openai",
        catalog: Some("openai"),
        note: "Responses 协议；要 Chat Completions 另选「OpenAI Chat」",
    },
    ProviderTemplate {
        id: "deepseek",
        label: "DeepSeek",
        kind: "openai_chat",
        protocol: "openai_chat",
        base_url: "https://api.deepseek.com",
        name: "deepseek",
        catalog: Some("deepseek"),
        note: "OpenAI 兼容；官方另有 /anthropic 入口，这里走兼容端点",
    },
    ProviderTemplate {
        id: "moonshot",
        label: "Kimi / Moonshot",
        kind: "openai_chat",
        protocol: "openai_chat",
        base_url: "https://api.moonshot.cn/v1",
        name: "moonshot",
        catalog: Some("moonshotai-cn"),
        note: "国内站 api.moonshot.cn；国际站换成 api.moonshot.ai",
    },
    ProviderTemplate {
        id: "zhipu",
        label: "智谱 GLM",
        kind: "openai_chat",
        protocol: "openai_chat",
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        name: "zhipu",
        catalog: Some("zhipuai"),
        note: "路径是 /api/paas/v4，不是 /v1",
    },
    ProviderTemplate {
        id: "qwen",
        label: "通义千问",
        kind: "openai_chat",
        protocol: "openai_chat",
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        name: "qwen",
        catalog: Some("alibaba-cn"),
        note: "百炼的 OpenAI 兼容入口",
    },
    ProviderTemplate {
        id: "siliconflow",
        label: "硅基流动",
        kind: "openai_chat",
        protocol: "openai_chat",
        base_url: "https://api.siliconflow.cn/v1",
        name: "siliconflow",
        catalog: Some("siliconflow-cn"),
        note: "聚合站：模型名与目录 id 不一定一致，建好后点一次「从上游拉取」",
    },
    ProviderTemplate {
        id: "openrouter",
        label: "OpenRouter",
        kind: "openai_chat",
        protocol: "openai_chat",
        base_url: "https://openrouter.ai/api/v1",
        name: "openrouter",
        catalog: Some("openrouter"),
        note: "聚合站，模型名带厂商前缀（如 anthropic/claude-sonnet-4-5）",
    },
    ProviderTemplate {
        id: "xai",
        label: "xAI / Grok",
        kind: "openai_chat",
        protocol: "openai_chat",
        base_url: "https://api.x.ai/v1",
        name: "xai",
        catalog: Some("xai"),
        note: "OpenAI 兼容",
    },
    ProviderTemplate {
        id: "groq",
        label: "Groq",
        kind: "openai_chat",
        protocol: "openai_chat",
        base_url: "https://api.groq.com/openai/v1",
        name: "groq",
        catalog: Some("groq"),
        note: "路径是 /openai/v1",
    },
    ProviderTemplate {
        id: "gemini",
        label: "Gemini",
        kind: "openai_chat",
        protocol: "openai_chat",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        name: "gemini",
        catalog: Some("google"),
        note: "走 Google 的 OpenAI 兼容端点",
    },
    ProviderTemplate {
        id: "ollama",
        label: "Ollama 本地",
        kind: "openai_chat",
        protocol: "openai_chat",
        base_url: "http://localhost:11434/v1",
        name: "ollama",
        catalog: None,
        note: "本地不用密钥；模型列表用「从上游拉取」问它自己要",
    },
];

pub fn find(id: &str) -> Option<&'static ProviderTemplate> {
    TEMPLATES.iter().find(|t| t.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 模板是给用户照着填的，字段自相矛盾比没有模板更糟。
    #[test]
    fn every_template_is_self_consistent() {
        for t in TEMPLATES {
            assert!(!t.id.is_empty() && !t.label.is_empty() && !t.base_url.is_empty());
            assert!(
                ["anthropic", "openai_chat", "openai_responses"].contains(&t.protocol),
                "{} 的 protocol 不是三种线协议之一：{}",
                t.id,
                t.protocol
            );
            // kind 必须是 carter 认得的三种之一，且映射到模板声称的那个协议；
            // 地址则完全靠模板自己给（`openai_chat` 没有默认地址）。
            let p = carter::config::ProviderConfig {
                kind: t.kind.to_string(),
                api_key: None,
                base_url: Some(t.base_url.to_string()),
                models: Default::default(),
                server_tools: Vec::new(),
                server_tool_specs: Vec::new(),
                server_tool_max_uses: None,
            };
            assert_eq!(
                p.protocol().to_string(),
                t.protocol,
                "{} 的协议对不上",
                t.id
            );
            let cc = p.to_client_config().expect("模板必须有可用的端点");
            assert_eq!(cc.base_url, t.base_url, "{} 的地址对不上", t.id);
        }
        let mut ids: Vec<&str> = TEMPLATES.iter().map(|t| t.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), TEMPLATES.len(), "模板 id 有重复");
    }

    /// 模板没写 base_url 是「用户照着填都不会错」的前提，所以它必须是完整的地址，
    /// 不是 `/v1` 这种需要再拼一次的片段。
    #[test]
    fn every_base_url_is_absolute() {
        for t in TEMPLATES {
            assert!(
                t.base_url.starts_with("http://") || t.base_url.starts_with("https://"),
                "{} 的 base_url 不是绝对地址：{}",
                t.id,
                t.base_url
            );
            assert!(!t.base_url.ends_with('/'), "{} 的 base_url 带尾斜杠", t.id);
        }
    }

    #[test]
    fn catalogs_only_name_providers_that_can_be_filled_from() {
        assert_eq!(find("deepseek").unwrap().catalog, Some("deepseek"));
        // 本地的 ollama 在 models.dev 里没有对应服务商 —— 模型只能问它自己。
        assert_eq!(find("ollama").unwrap().catalog, None);
        assert!(find("nope").is_none());
    }
}
