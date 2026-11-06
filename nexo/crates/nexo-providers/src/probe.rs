//! Finding out, by asking, what answers at an address and what one of its
//! models can do: the setup behind `/model`. Every server kind has a route that
//! describes it (Ollama `/api/version`, `/api/tags`, `/api/show`; llama.cpp
//! `/props`; LM Studio `/api/v0/models`; vLLM and OpenRouter in `/v1/models`;
//! Anthropic and Gemini in their model lists); this module tries them, short
//! timeouts each, and says what it asked and what answered.

use std::time::Duration;

use nexo_core::{ModelInfo, Pricing, ProviderConfig, ServerProbe};
use reqwest::header::HeaderMap;
use serde_json::{json, Value};

use crate::http;
use crate::{anthropic, gemini};

/// How long one question waits; a server on this machine answers in milliseconds.
const ASK: Duration = Duration::from_secs(8);
/// How long the scan of this machine waits per port.
const SCAN: Duration = Duration::from_millis(700);

pub use nexo_core::provider::{kind_label, normalize_url, LOCAL};

/// An answer: the status and the body as JSON (`Null` when it is not JSON).
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

async fn ask(
    method: reqwest::Method,
    url: &str,
    headers: HeaderMap,
    body: Option<&Value>,
    wait: Duration,
) -> Result<Answer, String> {
    let mut rb = http::client().request(method, url).headers(headers).timeout(wait);
    if let Some(b) = body {
        rb = rb.json(b);
    }
    let resp = rb.send().await.map_err(|e| reason(&e))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    Ok(Answer { status, body: serde_json::from_str(&text).unwrap_or(Value::Null) })
}

async fn get(url: &str, headers: HeaderMap, wait: Duration) -> Result<Answer, String> {
    ask(reqwest::Method::GET, url, headers, None, wait).await
}

/// A connection failure in a few words.
fn reason(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "no answer in time".into()
    } else if e.is_connect() {
        "connection refused or host not found".into()
    } else {
        let s = e.to_string();
        s.split(": ").last().unwrap_or(&s).to_string()
    }
}

/// `scheme://host[:port]` of a URL.
pub fn root_of(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(u) => {
            let mut r = format!("{}://{}", u.scheme(), u.host_str().unwrap_or(""));
            if let Some(p) = u.port() {
                r.push_str(&format!(":{p}"));
            }
            r
        }
        Err(_) => url.trim_end_matches('/').to_string(),
    }
}

fn bearer(cfg: &ProviderConfig, key: Option<&str>) -> HeaderMap {
    let own: Vec<(&str, String)> = key.map(|k| ("authorization", format!("Bearer {k}"))).into_iter().collect();
    http::headers(cfg, &own)
}

/// Looks for the usual local servers at their usual ports, all at once:
/// `(preset, what answered)` for each one that is running.
pub async fn scan_local() -> Vec<(String, String)> {
    let asks = LOCAL.iter().map(|(name, root)| async move {
        // each port ends within 2 s whatever it does, so the scan always ends
        let found = tokio::time::timeout(Duration::from_secs(2), local_kind(root, SCAN)).await.ok()??;
        let mut what = found.label();
        if let Some(v) = &found.version {
            what.push_str(&format!(" {v}"));
        }
        Some((name.to_string(), what))
    });
    futures::future::join_all(asks).await.into_iter().flatten().collect()
}

/// A server that describes itself on its own routes.
struct Local {
    kind: &'static str,
    version: Option<String>,
    /// What the describing route said (the model list or the properties).
    body: Value,
}

impl Local {
    fn label(&self) -> String {
        kind_label(self.kind).to_string()
    }
}

/// Ollama, llama.cpp or LM Studio at `root`, by the route each one has.
async fn local_kind(root: &str, wait: Duration) -> Option<Local> {
    let none = HeaderMap::new();
    if let Ok(a) = get(&format!("{root}/api/version"), none.clone(), wait).await {
        if a.ok() {
            if let Some(v) = a.body["version"].as_str() {
                return Some(Local { kind: "ollama", version: Some(v.to_string()), body: a.body.clone() });
            }
        }
    }
    if let Ok(a) = get(&format!("{root}/props"), none.clone(), wait).await {
        if a.ok() && a.body.get("default_generation_settings").is_some() {
            let version = a.body["build_info"].as_str().map(String::from);
            return Some(Local { kind: "llama.cpp", version, body: a.body });
        }
    }
    if let Ok(a) = get(&format!("{root}/api/v0/models"), none.clone(), wait).await {
        if a.ok() && a.body["data"].is_array() {
            return Some(Local { kind: "lmstudio", version: None, body: a.body });
        }
    }
    // vLLM: `/version` and an OpenAI model list whose entries carry `max_model_len`
    if let Ok(a) = get(&format!("{root}/v1/models"), none, wait).await {
        let vllm = a.body["data"]
            .as_array()
            .is_some_and(|d| d.iter().any(|m| m["owned_by"] == "vllm" || m.get("max_model_len").is_some()));
        if a.ok() && vllm {
            let version = match get(&format!("{root}/version"), HeaderMap::new(), wait).await {
                Ok(v) => v.body["version"].as_str().map(String::from),
                Err(_) => None,
            };
            return Some(Local { kind: "vllm", version, body: a.body });
        }
    }
    None
}

/// What answers at the provider's address, the protocol to talk to it and its
/// models. `Err` only when nothing answers at all; a server that wants a key
/// comes back with `needs_key` and no models.
pub async fn server(cfg: &ProviderConfig, key: Option<&str>) -> Result<ServerProbe, String> {
    let base = normalize_url(&cfg.base_url);
    let mut cfg = cfg.clone();
    cfg.base_url = base.clone();
    match cfg.protocol.as_str() {
        "anthropic" => return cloud_list(&cfg, key, "anthropic").await,
        "gemini" => return cloud_list(&cfg, key, "gemini").await,
        "mock" => {
            return Ok(ServerProbe {
                kind: "mock".into(),
                protocol: cfg.protocol.clone(),
                base_url: base,
                models: cfg
                    .models
                    .iter()
                    .map(|m| ModelInfo { id: m.clone(), tools: true, ..Default::default() })
                    .collect(),
                trail: vec!["mock: the built-in test provider".into()],
                ..Default::default()
            })
        }
        _ => {}
    }
    // a subscription backend lists no models to an account: the ones nexo knows are offered, and the
    // test request is the proof that the login works
    if cfg.login.is_some() && !cfg.models.is_empty() {
        return Ok(ServerProbe {
            kind: "subscription".into(),
            protocol: cfg.protocol.clone(),
            base_url: base,
            models: cfg.models.iter().map(|m| ModelInfo { id: m.clone(), tools: true, ..Default::default() }).collect(),
            trail: vec!["subscription: the models nexo knows for this login".into()],
            ..Default::default()
        });
    }
    let root = root_of(&base);
    let mut trail = Vec::new();
    let mut probe = ServerProbe {
        kind: "openai-compatible".into(),
        protocol: if cfg.protocol.is_empty() { "openai-chat".into() } else { cfg.protocol.clone() },
        base_url: base.clone(),
        ..Default::default()
    };
    if let Some(local) = local_kind(&root, ASK).await {
        probe.kind = local.kind.into();
        probe.version = local.version.clone();
        // every one of them speaks the OpenAI protocol under /v1
        probe.protocol = "openai-chat".into();
        probe.base_url = format!("{root}/v1");
        trail.push(format!(
            "{} {}answers at {root}",
            local.label(),
            local.version.as_deref().map(|v| format!("{v} ")).unwrap_or_default()
        ));
        match local.kind {
            "ollama" => {
                if let Ok(a) = get(&format!("{root}/api/tags"), HeaderMap::new(), ASK).await {
                    probe.models = a.body["models"]
                        .as_array()
                        .map(|ms| {
                            ms.iter()
                                .filter_map(|m| m["name"].as_str())
                                .map(|id| ModelInfo { id: id.into(), ..Default::default() })
                                .collect()
                        })
                        .unwrap_or_default();
                    trail.push(format!("/api/tags: {} models pulled", probe.models.len()));
                }
            }
            "lmstudio" => {
                probe.models = local.body["data"]
                    .as_array()
                    .map(|ms| ms.iter().filter(|m| m["type"] != "embeddings").map(lmstudio_info).collect())
                    .unwrap_or_default();
                trail.push(format!("/api/v0/models: {} models", probe.models.len()));
            }
            "vllm" => {
                probe.models = openai_models(&local.body);
                trail.push(format!("/v1/models: {} served", probe.models.len()));
            }
            _ => {}
        }
        if !probe.models.is_empty() {
            probe.models.sort_by(|a, b| a.id.cmp(&b.id));
            probe.trail = trail;
            return Ok(probe);
        }
    }
    let url = http::join_openai(&base, "models");
    let a = get(&url, bearer(&cfg, key), ASK).await.map_err(|e| format!("nothing answers at {base}: {e}"))?;
    match a.status {
        200..=299 => {
            probe.models = openai_models(&a.body);
            if base.contains("openrouter.ai") {
                probe.kind = "openrouter".into();
            } else if a.body["data"].as_array().is_some_and(|d| d.iter().any(|m| m["owned_by"] == "vllm")) {
                probe.kind = "vllm".into();
            }
            trail.push(format!("{url}: {} models", probe.models.len()));
        }
        401 | 403 => {
            probe.needs_key = true;
            trail.push(format!(
                "{url}: {} — {}",
                a.status,
                if key.is_some() { "the key was refused" } else { "a key is needed" }
            ));
        }
        404 => trail.push(format!("{url}: 404, this server has no model list; type the model name")),
        s => trail.push(format!("{url}: {s} {}", http::pretty_error(&a.body.to_string()))),
    }
    probe.trail = trail;
    Ok(probe)
}

/// Anthropic and Gemini: their own model lists, through the adapters' headers.
async fn cloud_list(cfg: &ProviderConfig, key: Option<&str>, kind: &str) -> Result<ServerProbe, String> {
    let (url, headers) = match kind {
        "anthropic" => {
            (http::join_versioned(&cfg.base_url, "v1", "models?limit=100"), anthropic::Anthropic::headers(cfg, key))
        }
        _ => (http::join_versioned(&cfg.base_url, "v1beta", "models?pageSize=200"), gemini::Gemini::headers(cfg, key)),
    };
    let a = get(&url, headers, ASK).await.map_err(|e| format!("nothing answers at {}: {e}", cfg.base_url))?;
    let mut probe = ServerProbe {
        kind: kind.into(),
        protocol: cfg.protocol.clone(),
        base_url: cfg.base_url.clone(),
        ..Default::default()
    };
    match a.status {
        200..=299 => {
            probe.models = if kind == "anthropic" {
                a.body["data"]
                    .as_array()
                    .map(|ms| {
                        ms.iter()
                            .filter_map(|m| {
                                Some(ModelInfo {
                                    id: m["id"].as_str()?.to_string(),
                                    display: m["display_name"].as_str().map(String::from),
                                    tools: true,
                                    vision: true,
                                    ..Default::default()
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                gemini_models(&a.body)
            };
            probe.trail.push(format!("{}: {} models", kind_label(kind), probe.models.len()));
        }
        400 | 401 | 403 => {
            probe.needs_key = true;
            probe.trail.push(format!(
                "{}: {} — {}",
                kind_label(kind),
                a.status,
                if key.is_some() { "the key was refused" } else { "a key is needed" }
            ));
        }
        s => probe.trail.push(format!("{}: {s} {}", kind_label(kind), http::pretty_error(&a.body.to_string()))),
    }
    Ok(probe)
}

fn gemini_models(body: &Value) -> Vec<ModelInfo> {
    body["models"]
        .as_array()
        .map(|ms| {
            ms.iter()
                .filter(|m| {
                    m["supportedGenerationMethods"].as_array().is_none_or(|g| g.iter().any(|x| x == "generateContent"))
                })
                .filter_map(|m| {
                    Some(ModelInfo {
                        id: m["name"].as_str()?.trim_start_matches("models/").to_string(),
                        display: m["displayName"].as_str().map(String::from),
                        context_window: u32_of(&m["inputTokenLimit"]),
                        max_output: u32_of(&m["outputTokenLimit"]),
                        reasoning: m["thinking"].as_bool().unwrap_or(false),
                        tools: true,
                        vision: true,
                        ..Default::default()
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn u32_of(v: &Value) -> Option<u32> {
    v.as_u64().filter(|n| *n > 0 && *n <= u32::MAX as u64).map(|n| n as u32)
}

/// The entries of an OpenAI-style `/models` list, with what each vendor adds to
/// them: `context_length` (OpenRouter, Together), `context_window` (Groq),
/// `max_context_length` (Mistral), `max_model_len` (vLLM), prices per token
/// (OpenRouter), `supported_parameters` and `capabilities`.
pub fn openai_models(body: &Value) -> Vec<ModelInfo> {
    let list = body["data"].as_array().or_else(|| body.as_array());
    let mut out: Vec<ModelInfo> = list.map(|ms| ms.iter().filter_map(openai_info).collect()).unwrap_or_default();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn openai_info(m: &Value) -> Option<ModelInfo> {
    let id = m["id"].as_str()?.to_string();
    let first = |keys: &[&str]| keys.iter().find_map(|k| m.pointer(k).and_then(u32_of_ref));
    let context = first(&[
        "/context_length",
        "/context_window",
        "/max_context_length",
        "/max_model_len",
        "/top_provider/context_length",
    ]);
    let max_output = first(&["/top_provider/max_completion_tokens", "/max_output_tokens", "/max_completion_tokens"]);
    let params: Vec<&str> =
        m["supported_parameters"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let caps = &m["capabilities"];
    let per_token = |k: &str| m["pricing"][k].as_str().and_then(|s| s.parse::<f64>().ok()).map(|x| x * 1e6);
    let pricing = match (per_token("prompt"), per_token("completion")) {
        (Some(input), Some(output)) => Some(Pricing {
            input,
            output,
            cache_read: per_token("input_cache_read").unwrap_or(0.0),
            cache_write: per_token("input_cache_write").unwrap_or(0.0),
        }),
        _ => None,
    };
    let modalities = m.pointer("/architecture/input_modalities").and_then(Value::as_array);
    Some(ModelInfo {
        display: m["name"].as_str().filter(|n| *n != id).map(String::from),
        context_window: context,
        max_output,
        pricing,
        reasoning: params.iter().any(|p| *p == "reasoning" || *p == "include_reasoning") || caps["reasoning"] == true,
        tools: params.contains(&"tools") || caps["function_calling"] == true || caps["tool_calling"] == true,
        vision: modalities.is_some_and(|a| a.iter().any(|x| x == "image")) || caps["vision"] == true,
        id,
        ..Default::default()
    })
}

fn u32_of_ref(v: &Value) -> Option<u32> {
    u32_of(v).or_else(|| v.as_str().and_then(|s| s.parse::<u32>().ok()))
}

fn lmstudio_info(m: &Value) -> ModelInfo {
    let caps: Vec<&str> =
        m["capabilities"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    ModelInfo {
        id: m["id"].as_str().unwrap_or_default().to_string(),
        context_window: u32_of(&m["loaded_context_length"]).or_else(|| u32_of(&m["max_context_length"])),
        tools: caps.contains(&"tool_use"),
        vision: m["type"] == "vlm",
        ..Default::default()
    }
}

/// What the server itself says of one model, beyond its list: Ollama's
/// `/api/show` (the trained context, `thinking`, `tools`, `vision`) and the
/// window it serves with now (`/api/ps`); llama.cpp's `/props` (the `n_ctx` it
/// was started with). `(facts, where each came from)`.
pub async fn model_details(probe: &ServerProbe, model: &str) -> (ModelInfo, Vec<String>) {
    let mut info = probe
        .models
        .iter()
        .find(|m| m.id == model)
        .cloned()
        .unwrap_or_else(|| ModelInfo { id: model.into(), ..Default::default() });
    let mut said = Vec::new();
    let root = root_of(&probe.base_url);
    match probe.kind.as_str() {
        "ollama" => {
            let body = json!({"model": model});
            if let Ok(a) =
                ask(reqwest::Method::POST, &format!("{root}/api/show"), HeaderMap::new(), Some(&body), ASK).await
            {
                if a.ok() {
                    let caps: Vec<&str> = a.body["capabilities"]
                        .as_array()
                        .map(|c| c.iter().filter_map(Value::as_str).collect())
                        .unwrap_or_default();
                    info.tools = caps.contains(&"tools");
                    info.reasoning = caps.contains(&"thinking");
                    info.vision = caps.contains(&"vision");
                    said.push(format!(
                        "capabilities from Ollama /api/show: {}",
                        if caps.is_empty() { "none listed".into() } else { caps.join(", ") }
                    ));
                    let trained = a.body["model_info"].as_object().and_then(|o| {
                        o.iter().find(|(k, _)| k.ends_with(".context_length")).and_then(|(_, v)| u32_of(v))
                    });
                    if let Some(n) = trained {
                        info.context_window = Some(n);
                        said.push(format!("context {n} from Ollama /api/show (what the model was trained for)"));
                    }
                }
            }
            // the window Ollama actually serves with is its own setting, often far below the model's
            if let Ok(a) = get(&format!("{root}/api/ps"), HeaderMap::new(), ASK).await {
                let loaded = a.body["models"]
                    .as_array()
                    .and_then(|ms| ms.iter().find(|m| m["name"] == model || m["model"] == model))
                    .and_then(|m| u32_of(&m["context_length"]));
                if let Some(n) = loaded {
                    info.context_window = Some(n);
                    said.push(format!("context {n} from Ollama /api/ps (the window it is loaded with now)"));
                } else {
                    said.push("Ollama serves with its own window (OLLAMA_CONTEXT_LENGTH, 4096 by default on older versions), which may be smaller than the model's".into());
                }
            }
        }
        "llama.cpp" => {
            if let Ok(a) = get(&format!("{root}/props"), HeaderMap::new(), ASK).await {
                if let Some(n) = u32_of(&a.body["default_generation_settings"]["n_ctx"]) {
                    info.context_window = Some(n);
                    said.push(format!("context {n} from llama.cpp /props (n_ctx it was started with)"));
                }
            }
        }
        "vllm" if info.context_window.is_some() => {
            said.push(format!(
                "context {} from vLLM /v1/models (max_model_len)",
                info.context_window.unwrap_or_default()
            ));
        }
        "lmstudio" if info.context_window.is_some() => {
            said.push(format!("context {} from LM Studio /api/v0/models", info.context_window.unwrap_or_default()));
        }
        "openrouter" | "gemini" | "openai-compatible" => {
            if let Some(n) = info.context_window {
                said.push(format!("context {n} from the {} model list", kind_label(&probe.kind)));
            }
            if info.pricing.is_some() {
                said.push(format!("prices from the {} model list", kind_label(&probe.kind)));
            }
        }
        _ => {}
    }
    (info, said)
}

/// The reasoning levels a model takes, by protocol and server: OpenAI's
/// `reasoning_effort` (`minimal` on GPT-5), Anthropic's and Gemini's thinking
/// budgets (nexo maps the levels to budgets; Gemini Pro cannot turn it off),
/// and none for models whose reasoning is fixed (DeepSeek's reasoner).
pub fn effort_levels(protocol: &str, kind: &str, base_url: &str, model: &str, reasoning: bool) -> Vec<String> {
    if !reasoning {
        return Vec::new();
    }
    let m = model.to_ascii_lowercase();
    let v = |l: &[&str]| l.iter().map(|s| s.to_string()).collect();
    if base_url.contains("deepseek.com") {
        return Vec::new();
    }
    match protocol {
        "anthropic" => v(&["off", "low", "medium", "high"]),
        "gemini" if m.contains("pro") => v(&["low", "medium", "high"]),
        "gemini" => v(&["off", "low", "medium", "high"]),
        _ if kind == "ollama" && !m.contains("gpt-oss") => v(&["off", "on"]),
        _ if m.contains("gpt-5") => v(&["minimal", "low", "medium", "high"]),
        _ => v(&["low", "medium", "high"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;

    fn cfg(protocol: &str, base: &str) -> ProviderConfig {
        ProviderConfig::new(protocol, base, None)
    }

    #[test]
    fn typed_addresses_become_urls() {
        assert_eq!(normalize_url("localhost:11434"), "http://localhost:11434");
        assert_eq!(normalize_url("192.168.1.20:8000/v1/"), "http://192.168.1.20:8000/v1");
        assert_eq!(normalize_url("api.deepseek.com"), "https://api.deepseek.com");
        assert_eq!(normalize_url(" https://x.ai/v1 "), "https://x.ai/v1");
        assert_eq!(root_of("http://localhost:11434/v1"), "http://localhost:11434");
    }

    #[tokio::test]
    async fn ollama_is_found_with_its_models_and_its_window() {
        let s = MockServer::start();
        s.mock(|w, t| {
            w.method(GET).path("/api/version");
            t.status(200).json_body(json!({"version": "0.12.6"}));
        });
        s.mock(|w, t| {
            w.method(GET).path("/api/tags");
            t.status(200).json_body(json!({"models": [{"name": "qwen3:8b"}, {"name": "gpt-oss:20b"}]}));
        });
        s.mock(|w, t| {
            w.method(POST).path("/api/show");
            t.status(200).json_body(json!({"capabilities": ["completion", "tools", "thinking"], "model_info": {"qwen3.context_length": 40960}}));
        });
        s.mock(|w, t| {
            w.method(GET).path("/api/ps");
            t.status(200).json_body(json!({"models": [{"name": "qwen3:8b", "context_length": 8192}]}));
        });
        let p = server(&cfg("openai-chat", &s.base_url()), None).await.unwrap();
        assert_eq!(p.kind, "ollama");
        assert_eq!(p.version.as_deref(), Some("0.12.6"));
        assert!(p.trail.iter().any(|l| l.contains("/api/tags: 2 models")), "{:?}", p.trail);
        assert_eq!(p.base_url, format!("{}/v1", s.base_url()));
        assert_eq!(p.models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["gpt-oss:20b", "qwen3:8b"]);
        let (info, said) = model_details(&p, "qwen3:8b").await;
        assert!(info.tools && info.reasoning && !info.vision);
        assert_eq!(info.context_window, Some(8192), "the loaded window wins over the trained one");
        assert!(said.iter().any(|l| l.contains("/api/ps")), "{said:?}");
        assert_eq!(effort_levels(&p.protocol, &p.kind, &p.base_url, "qwen3:8b", true), ["off", "on"]);
        assert_eq!(effort_levels(&p.protocol, &p.kind, &p.base_url, "gpt-oss:20b", true), ["low", "medium", "high"]);
    }

    #[tokio::test]
    async fn vllm_and_llamacpp_say_their_windows() {
        let s = MockServer::start();
        s.mock(|w, t| {
            w.method(GET).path("/v1/models");
            t.status(200)
                .json_body(json!({"data": [{"id": "Qwen/Qwen3-32B", "owned_by": "vllm", "max_model_len": 32768}]}));
        });
        let p = server(&cfg("openai-chat", &format!("{}/v1", s.base_url())), None).await.unwrap();
        assert_eq!(p.kind, "vllm");
        assert_eq!(p.models[0].context_window, Some(32768));

        let l = MockServer::start();
        l.mock(|w, t| {
            w.method(GET).path("/props");
            t.status(200).json_body(json!({"default_generation_settings": {"n_ctx": 16384}, "build_info": "b6500"}));
        });
        l.mock(|w, t| {
            w.method(GET).path("/v1/models");
            t.status(200).json_body(json!({"data": [{"id": "local.gguf"}]}));
        });
        let p = server(&cfg("openai-chat", &l.base_url()), None).await.unwrap();
        assert_eq!(p.kind, "llama.cpp");
        assert_eq!(p.models.len(), 1, "the OpenAI list fills the models: {p:?}");
        let (info, _) = model_details(&p, "local.gguf").await;
        assert_eq!(info.context_window, Some(16384));
    }

    #[tokio::test]
    async fn a_cloud_list_gives_windows_prices_and_capabilities() {
        let s = MockServer::start();
        s.mock(|w, t| {
            w.method(GET).path("/api/v1/models").header("authorization", "Bearer k");
            t.status(200).json_body(json!({"data": [{
                "id": "deepseek/deepseek-r1", "name": "DeepSeek R1", "context_length": 163840,
                "pricing": {"prompt": "0.0000004", "completion": "0.000002"},
                "top_provider": {"max_completion_tokens": 32768},
                "supported_parameters": ["tools", "reasoning"],
                "architecture": {"input_modalities": ["text"]}
            }]}));
        });
        let p = server(&cfg("openai-chat", &format!("{}/api/v1", s.base_url())), Some("k")).await.unwrap();
        let m = &p.models[0];
        assert_eq!((m.context_window, m.max_output), (Some(163840), Some(32768)));
        assert!(m.tools && m.reasoning && !m.vision);
        let price = m.pricing.unwrap();
        assert!((price.input - 0.4).abs() < 1e-9 && (price.output - 2.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn a_refused_key_and_a_dead_address_are_told_apart() {
        let s = MockServer::start();
        s.mock(|w, t| {
            w.method(GET).path("/v1/models");
            t.status(401).json_body(json!({"error": {"message": "invalid api key"}}));
        });
        let p = server(&cfg("openai-chat", &format!("{}/v1", s.base_url())), Some("bad")).await.unwrap();
        assert!(p.needs_key && p.models.is_empty());
        assert!(p.trail.iter().any(|l| l.contains("refused")), "{:?}", p.trail);
        let dead = server(&cfg("openai-chat", "http://127.0.0.1:9"), None).await.unwrap_err();
        assert!(dead.contains("nothing answers"), "{dead}");
    }

    #[test]
    fn levels_follow_the_protocol_and_the_vendor() {
        assert!(
            effort_levels("openai-chat", "openai-compatible", "https://api.openai.com/v1", "gpt-4.1", false).is_empty()
        );
        assert_eq!(effort_levels("openai-chat", "x", "https://api.openai.com/v1", "gpt-5-mini", true)[0], "minimal");
        assert!(effort_levels("openai-chat", "x", "https://api.deepseek.com", "deepseek-reasoner", true).is_empty());
        assert_eq!(effort_levels("gemini", "gemini", "", "gemini-2.5-pro", true), ["low", "medium", "high"]);
        assert_eq!(effort_levels("anthropic", "anthropic", "", "claude-x", true)[0], "off");
    }
}
