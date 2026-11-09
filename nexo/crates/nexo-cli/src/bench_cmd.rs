//! `nexo bench`: the command line of the benchmark (built with the `bench` feature).

use std::path::PathBuf;

use clap::Subcommand;

/// `nexo bench`: every task done by the agent with every arm, graded, priced and summed up.
#[derive(clap::Args, Debug)]
pub struct BenchArgs {
    #[command(subcommand)]
    pub action: Option<BenchCmd>,
    /// The provider's address, e.g. https://api.deepseek.com/anthropic (needed to run).
    #[arg(long, value_name = "URL")]
    pub upstream: Option<String>,
    /// The model, e.g. deepseek-chat (needed to run).
    #[arg(long)]
    pub model: Option<String>,
    /// The API key (none for a local server; `-` reads it from stdin).
    #[arg(long, value_name = "KEY")]
    pub key: Option<String>,
    /// anthropic | openai-chat | openai-responses | gemini (default: guessed from the address).
    #[arg(long)]
    pub protocol: Option<String>,
    /// Task ids or groups (all, core, long, heavy, more, hard, padded), the chat task padded-revert-chat (named only), the external benchmark's repoqa and canonstep (with --llmb), or polyglot (Aider's exercises, see --polyglot),
    /// comma separated or repeated.
    #[arg(long, value_name = "TASKS", value_delimiter = ',', default_value = "all")]
    pub tasks: Vec<String>,
    /// Arms `[NAME@]POLICY[:k=v;...]` (POLICY: plain, compact, curator; the pairs are the curator spec),
    /// comma separated or repeated.
    #[arg(long, value_name = "ARMS", value_delimiter = ',', default_value = nexo_bench::arm::DEFAULT_ARMS)]
    pub arms: Vec<String>,
    /// Times each task is run with each arm.
    #[arg(long, default_value_t = 1)]
    pub repeats: u32,
    /// Most tool steps (model requests) a task may take.
    #[arg(long, default_value_t = 100)]
    pub max_steps: u32,
    /// Stop the whole benchmark once this many dollars are spent (needs prices: nexo's own or --price).
    #[arg(long, value_name = "USD")]
    pub budget: Option<f64>,
    /// The model's prices in dollars per million tokens: input on a cache miss, on a cache hit, output
    /// (default: nexo's own for the model, if it has any).
    #[arg(long, value_name = "MISS,HIT,OUT", value_parser = price_arg)]
    pub price: Option<nexo_core::Pricing>,
    /// Largest reply, in tokens.
    #[arg(long)]
    pub max_tokens: Option<u32>,
    /// Sampling temperature of every request (default: 0 with the task polyglot, as Aider; the provider's own otherwise).
    #[arg(long)]
    pub temperature: Option<f32>,
    /// End a turn on the reply that holds the answer and a passing check after a change (agent.close_on_verified).
    #[arg(long)]
    pub close_on_verified: bool,
    /// Seconds one run may take before it is interrupted, its commands killed and its record says `timeout` (0: no limit).
    #[arg(long, value_name = "SECS", default_value_t = 1200)]
    pub run_timeout: u64,
    /// The model's context window in tokens, the same for every arm (default: what nexo knows of it): `32k`, `32000`, `1m`.
    /// With a window set, `plain` summarises the history when it fills (as `compact` does), the curator keeps its view under it.
    #[arg(long, value_name = "TOKENS", value_parser = window_arg)]
    pub window: Option<u32>,
    /// Where results.jsonl, the transcripts and summary.md go.
    #[arg(long, value_name = "DIR", default_value = "./bench-out")]
    pub out: PathBuf,
    /// Start without the one small request that checks the key and the model's tool calls.
    #[arg(long)]
    pub skip_preflight: bool,
    /// The external benchmark's distribution folder (for --tasks repoqa,canonstep).
    #[arg(long, value_name = "DIR")]
    pub llmb: Option<PathBuf>,
    /// The Polyglot benchmark checkout (for --tasks polyglot; default: the pinned commit is cloned into the cache).
    #[arg(long, value_name = "DIR")]
    pub polyglot: Option<PathBuf>,
    /// Languages of polyglot, comma separated (default: python,rust,go,cpp,javascript,java).
    #[arg(long, value_name = "LANGS", value_delimiter = ',')]
    pub langs: Vec<String>,
    /// Instance ids of repoqa, canonstep and polyglot (lang/name), comma separated (default: all of them).
    #[arg(long, value_name = "IDS", value_delimiter = ',')]
    pub instances: Vec<String>,
    /// At most this many instances per task (per language for polyglot, in a fixed shuffled order), in id order otherwise.
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,
    /// Where repoqa's repositories, polyglot's checkout and its package caches are kept once (default: <out>/.cache/repos).
    #[arg(long, value_name = "DIR")]
    pub cache: Option<PathBuf>,
}

/// `nexo bench` subcommands.
#[derive(Subcommand, Debug)]
pub enum BenchCmd {
    /// Write report.html in --out from one or more results.jsonl (files or folders; default: --out).
    Report {
        #[arg(long, value_name = "DIR", default_value = "./bench-out")]
        out: PathBuf,
        /// results.jsonl files, or folders holding one.
        results: Vec<PathBuf>,
        /// Also write the Markdown summary here, with the counters read again from the transcripts.
        #[arg(long, value_name = "FILE")]
        summary: Option<PathBuf>,
    },
}

impl BenchArgs {
    fn options(&self) -> anyhow::Result<nexo_bench::Options> {
        let (Some(upstream), Some(model)) = (self.upstream.clone(), self.model.clone()) else {
            anyhow::bail!("nexo bench needs --upstream and --model");
        };
        Ok(nexo_bench::Options {
            upstream,
            model,
            key: key_arg(self.key.as_deref(), std::io::stdin().lock())?,
            protocol: self.protocol.clone(),
            tasks: self.tasks.clone(),
            arms: self.arms.clone(),
            repeats: self.repeats,
            max_steps: self.max_steps,
            budget: self.budget,
            price: self.price,
            max_tokens: self.max_tokens,
            window: self.window,
            temperature: self.temperature,
            close_on_verified: self.close_on_verified,
            run_timeout: self.run_timeout,
            out: self.out.clone(),
            skip_preflight: self.skip_preflight,
            llmb: self.llmb.clone(),
            polyglot: self.polyglot.clone(),
            langs: self.langs.clone(),
            instances: self.instances.clone(),
            limit: self.limit,
            cache: self.cache.clone(),
        })
    }
}

/// Runs `nexo bench`.
pub async fn run(b: &BenchArgs) -> anyhow::Result<i32> {
    match &b.action {
        Some(BenchCmd::Report { out, results, summary }) => nexo_bench::report(out, results, summary.as_deref()),
        None => nexo_bench::run(b.options()?).await,
    }
}

/// `--price MISS,HIT,OUT`: dollars per million tokens of input on a cache miss, input on a cache hit, output.
fn price_arg(s: &str) -> Result<nexo_core::Pricing, String> {
    let n: Vec<f64> =
        s.split(',').map(|p| p.trim().parse::<f64>()).collect::<Result<_, _>>().map_err(|e| e.to_string())?;
    match n[..] {
        [input, cache_read, output] if n.iter().all(|x| x.is_finite() && *x >= 0.0) && input + output > 0.0 => {
            Ok(nexo_core::Pricing { input, output, cache_read, cache_write: 0.0 })
        }
        _ => Err("three prices, not all zero: MISS,HIT,OUT in dollars per million tokens, e.g. 0.28,0.028,0.42".into()),
    }
}

/// `--window 32k`: tokens of context, with a `k` (thousand) or `m` (million) suffix allowed.
fn window_arg(s: &str) -> Result<u32, String> {
    let t = s.trim().to_ascii_lowercase();
    let (digits, unit) = match t.strip_suffix('k') {
        Some(d) => (d, 1_000u64),
        None => match t.strip_suffix('m') {
            Some(d) => (d, 1_000_000),
            None => (t.as_str(), 1),
        },
    };
    digits
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(unit))
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n >= 1_000)
        .ok_or_else(|| "a window of at least 1000 tokens: 32k, 32000, 1m".to_string())
}

/// The key as given, or read from `input` when it is `-`: then it stays out of the process list and the shell
/// history.
fn key_arg(key: Option<&str>, mut input: impl std::io::Read) -> anyhow::Result<Option<String>> {
    match key {
        Some("-") => {
            let mut all = String::new();
            input.read_to_string(&mut all)?;
            let k = all.trim();
            anyhow::ensure!(!k.is_empty(), "--key -: nothing on stdin");
            Ok(Some(k.to_string()))
        }
        k => Ok(k.map(str::to_string)),
    }
}

#[cfg(test)]
mod tests {
    use super::{key_arg, price_arg, window_arg};

    #[test]
    fn bench_key_dash_reads_stdin() {
        let key = |arg, input: &str| key_arg(arg, input.as_bytes()).map_err(|e| e.to_string());
        assert_eq!(key(Some("-"), "k-123\n"), Ok(Some("k-123".to_string())));
        assert_eq!(key(Some("k-456"), "ignored"), Ok(Some("k-456".to_string())));
        assert_eq!(key(None, "ignored"), Ok(None));
        assert_eq!(key(Some("-"), " \n"), Err("--key -: nothing on stdin".to_string()));
    }

    #[test]
    fn bench_window_reads_k_and_m_suffixes() {
        for (text, want) in [("32k", 32_000), ("32K", 32_000), ("32000", 32_000), (" 1m ", 1_000_000), ("8k", 8_000)] {
            assert_eq!(window_arg(text), Ok(want), "{text:?}");
        }
        for bad in ["", "k", "0", "999", "-32k", "3.5k", "32kb", "5000000m"] {
            assert!(window_arg(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn bench_price_reads_miss_hit_out() {
        let p = price_arg("0.28, 0.028,0.42").unwrap();
        assert_eq!((p.input, p.cache_read, p.output, p.cache_write), (0.28, 0.028, 0.42, 0.0));
        for bad in ["0.28,0.028", "0.28,0.028,0.42,1", "a,b,c", "0,0,0", "-1,0,1", "NaN,0,1", ""] {
            assert!(price_arg(bad).is_err(), "{bad:?}");
        }
    }
}
