use super::*;

// ------------------------------------------------------------------ search

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    DuckDuckGo,
    Brave,
    Tavily,
    Exa,
    Searxng,
}

impl Backend {
    pub fn parse(s: &str) -> Option<Backend> {
        match s.trim().to_ascii_lowercase().as_str() {
            "duckduckgo" | "ddg" => Some(Backend::DuckDuckGo),
            "brave" => Some(Backend::Brave),
            "tavily" => Some(Backend::Tavily),
            "exa" => Some(Backend::Exa),
            "searxng" | "searx" => Some(Backend::Searxng),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Backend::DuckDuckGo => "duckduckgo",
            Backend::Brave => "brave",
            Backend::Tavily => "tavily",
            Backend::Exa => "exa",
            Backend::Searxng => "searxng",
        }
    }

    /// The environment variables that may hold its key, in order.
    fn key_vars(self) -> &'static [&'static str] {
        match self {
            Backend::Brave => &["BRAVE_API_KEY", "BRAVE_SEARCH_API_KEY"],
            Backend::Tavily => &["TAVILY_API_KEY"],
            Backend::Exa => &["EXA_API_KEY"],
            Backend::DuckDuckGo | Backend::Searxng => &[],
        }
    }

    fn base(self) -> &'static str {
        match self {
            Backend::DuckDuckGo => DDG_BASE,
            Backend::Brave => "https://api.search.brave.com",
            Backend::Tavily => "https://api.tavily.com",
            Backend::Exa => "https://api.exa.ai",
            Backend::Searxng => "",
        }
    }
}

/// One hit.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

pub struct WebSearch {
    cfg: WebConfig,
    http: Result<reqwest::Client, String>,
    env: Env,
    pub(super) ddg_base: String,
    pub(super) breaker: Breaker,
}

impl WebSearch {
    pub fn new(cfg: &WebConfig) -> WebSearch {
        WebSearch::with_env(cfg, process_env())
    }

    pub(super) fn with_env(cfg: &WebConfig, env: Env) -> WebSearch {
        WebSearch {
            cfg: cfg.clone(),
            http: client(cfg, 5),
            env,
            ddg_base: DDG_BASE.to_string(),
            breaker: Breaker::new(),
        }
    }

    fn http(&self) -> Result<&reqwest::Client, String> {
        self.http.as_ref().map_err(Clone::clone)
    }

    fn key(&self, b: Backend) -> Option<String> {
        if let Some(var) = &self.cfg.api_key_env {
            return (self.env)(var);
        }
        b.key_vars().iter().find_map(|v| (self.env)(v))
    }

    fn is_auto(&self) -> bool {
        let s = self.cfg.search.trim();
        s.is_empty() || s.eq_ignore_ascii_case("auto")
    }

    /// The backend to ask, and whether DuckDuckGo stands behind it.
    pub(super) fn choose(&self) -> Result<(Backend, bool), String> {
        let wanted = self.cfg.search.trim();
        if self.is_auto() {
            for b in [Backend::Tavily, Backend::Brave, Backend::Exa] {
                // a key named in the config belongs to no backend in particular: auto cannot use it
                if self.cfg.api_key_env.is_none() && self.key(b).is_some() {
                    return Ok((b, true));
                }
            }
            if self.cfg.url.is_some() {
                return Ok((Backend::Searxng, true));
            }
            return Ok((Backend::DuckDuckGo, false));
        }
        let b = Backend::parse(wanted).ok_or_else(|| {
            format!("unknown backend `{wanted}` ([web] search: auto, duckduckgo, brave, tavily, exa, searxng)")
        })?;
        Ok((b, false))
    }

    fn base(&self, b: Backend) -> String {
        let base = match (b, &self.cfg.url) {
            // in auto mode `url` belongs to the backend chosen first, never to the fallback
            (Backend::DuckDuckGo, _) if self.is_auto() => self.ddg_base.clone(),
            (_, Some(u)) => u.clone(),
            (Backend::DuckDuckGo, None) => self.ddg_base.clone(),
            (b, None) => b.base().to_string(),
        };
        base.trim_end_matches('/').to_string()
    }

    pub(super) async fn ask(&self, b: Backend, query: &str, n: u64, domains: &[String]) -> Result<Vec<Hit>, Fail> {
        let base = self.base(b);
        let key = || self.key(b).ok_or_else(|| format!("{} needs a key in {}", b.name(), self.key_hint(b)));
        let site_query = with_sites(query, domains);
        match b {
            Backend::Tavily => {
                let mut body = json!({"query": query, "max_results": n});
                if !domains.is_empty() {
                    body["include_domains"] = json!(domains);
                }
                let v =
                    self.json(self.http()?.post(format!("{base}/search")).bearer_auth(key()?).json(&body), b).await?;
                Ok(hits(&v["results"], "title", "url", &["content"]))
            }
            Backend::Brave => {
                let count = n.to_string();
                let req = self
                    .http()?
                    .get(with_query(&format!("{base}/res/v1/web/search"), &[("q", &site_query), ("count", &count)])?)
                    .header("Accept", "application/json")
                    .header("X-Subscription-Token", key()?);
                let v = self.json(req, b).await?;
                Ok(hits(&v["web"]["results"], "title", "url", &["description"]))
            }
            Backend::Exa => {
                let mut body =
                    json!({"query": query, "numResults": n, "type": "auto", "contents": {"highlights": true}});
                if !domains.is_empty() {
                    body["includeDomains"] = json!(domains);
                }
                let v = self
                    .json(self.http()?.post(format!("{base}/search")).header("x-api-key", key()?).json(&body), b)
                    .await?;
                Ok(hits(&v["results"], "title", "url", &["highlights", "summary", "text"]))
            }
            Backend::Searxng => {
                if base.is_empty() {
                    return Err("searxng needs its address: [web] url = \"https://searx.example.org\""
                        .to_string()
                        .into());
                }
                let req =
                    self.http()?.get(with_query(&format!("{base}/search"), &[("q", &site_query), ("format", "json")])?);
                let v = self.json(req, b).await?;
                Ok(hits(&v["results"], "title", "url", &["content"]))
            }
            Backend::DuckDuckGo => {
                let resp = self
                    .http()?
                    .post(format!("{base}/html/"))
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(form_encode(&[("q", &site_query), ("kl", "wt-wt")]))
                    .send()
                    .await
                    .map_err(|e| Fail::refused(format!("duckduckgo: {}", net_error(&e))))?;
                let status = resp.status();
                let text = resp.text().await.map_err(|e| Fail::refused(format!("duckduckgo: {}", net_error(&e))))?;
                if !status.is_success() || text.contains("anomaly-modal") || text.contains("challenge-form") {
                    return Err(Fail::refused(format!(
                        "duckduckgo refused the search (HTTP {}; usually a rate limit or a bot check); \
                         a key for Tavily or Brave, or a SearXNG instance, avoids it",
                        status.as_u16()
                    )));
                }
                let mut out = parse_ddg(&text);
                out.truncate(n as usize);
                Ok(out)
            }
        }
    }

    fn key_hint(&self, b: Backend) -> String {
        match &self.cfg.api_key_env {
            Some(v) => format!("${v}"),
            None => b.key_vars().iter().map(|v| format!("${v}")).collect::<Vec<_>>().join(" or "),
        }
    }

    async fn json(&self, req: reqwest::RequestBuilder, b: Backend) -> Result<Value, Fail> {
        let resp = req.send().await.map_err(|e| Fail::refused(format!("{}: {}", b.name(), net_error(&e))))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| Fail::refused(format!("{}: {}", b.name(), net_error(&e))))?;
        if !status.is_success() {
            let text = format!("{}: HTTP {} {}", b.name(), status.as_u16(), crate::util::one_line(&text, 200));
            // 400, 404 and 422 blame the request; any other refusal is the backend's
            return Err(Fail { text, refused: !matches!(status.as_u16(), 400 | 404 | 422) });
        }
        Ok(serde_json::from_str(&text).map_err(|e| format!("{}: the answer is not JSON ({e})", b.name()))?)
    }
}
