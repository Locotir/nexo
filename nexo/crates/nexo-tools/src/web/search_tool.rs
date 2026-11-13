use super::*;

#[async_trait]
impl Tool for WebSearch {
    fn name(&self) -> &str {
        "web_search"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_search".into(),
            description: "Search the web. Use it for anything current or outside the repository: library docs and \
                          versions, error messages, APIs, news. Answers with titles, URLs and snippets; read the \
                          pages that matter with `web_fetch` before relying on them, and cite their URLs."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "max_results": {"type": "integer", "description": "Default 8, at most 20."},
                    "domains": {"type": "array", "items": {"type": "string"}, "description": "Only results from these sites, e.g. [\"docs.rs\"]."}
                },
                "required": ["query"]
            }),
        }
    }

    fn compact(&self) -> Option<Compact<'_>> {
        Some(Compact {
            description: "Search the web for anything current or outside the repository; returns titles, URLs, snippets. Read pages with `web_fetch`.",
            params: &[
                ("max_results", "Default 8"),
                ("domains", "Only these sites"),
            ],
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Search
    }

    fn summary(&self, args: &Value) -> String {
        let q = str_arg(args, "query").unwrap_or("?");
        let d = domains_arg(args);
        if d.is_empty() {
            format!("web_search: {}", crate::util::one_line(q, 100))
        } else {
            format!("web_search: {} (on {})", crate::util::one_line(q, 100), d.join(", "))
        }
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(query) = str_arg(&args, "query").map(str::trim).filter(|q| !q.is_empty()) else {
            return ToolOutput::error("web_search: `query` is required");
        };
        let n = int_arg(&args, "max_results").unwrap_or(DEFAULT_RESULTS).clamp(1, MAX_RESULTS);
        let domains = domains_arg(&args);
        let (backend, fallback) = match self.choose() {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("web_search: {e}")),
        };
        if self.breaker.is_open() {
            return ToolOutput::error(BREAKER_SAID);
        }
        // Err carries the text and whether every backend tried for this call refused
        let work = async {
            match self.ask(backend, query, n, &domains).await {
                Ok(h) => Ok(render_hits(query, backend.name(), &h, None)),
                Err(e) if fallback && backend != Backend::DuckDuckGo => {
                    match self.ask(Backend::DuckDuckGo, query, n, &domains).await {
                        Ok(h) => Ok(render_hits(
                            query,
                            "duckduckgo",
                            &h,
                            Some(&format!("({}; fell back to duckduckgo)", e.text)),
                        )),
                        Err(e2) => {
                            Err((format!("{}; the fallback failed too: {}", e.text, e2.text), e.refused && e2.refused))
                        }
                    }
                }
                Err(e) => Err((e.text, e.refused)),
            }
        };
        tokio::select! {
            r = work => match r {
                Ok(text) => {
                    self.breaker.reset();
                    ToolOutput::ok(text)
                }
                Err((e, true)) => {
                    self.breaker.refused();
                    ToolOutput::error(format!("web_search: {}. {NO_RETRY}", e.trim_end_matches('.')))
                }
                Err((e, false)) => {
                    // the query's own trouble: not a refusal, and it ends the run of them
                    self.breaker.reset();
                    ToolOutput::error(format!("web_search: {e}"))
                }
            },
            _ = ctx.cancel.cancelled() => ToolOutput::error("web_search: cancelled"),
        }
    }
}
