# Do the providers allow what the curator does?

A first check of the terms of the providers this work runs on or may run on next. It is a reading of
public terms by the author of a final-year project, not legal advice. Two pages could not be opened from
the machine this was written on (DeepSeek's terms page and Google's Gemini terms page); for those the
clauses are taken from search results quoting them and are marked **to verify**. Before the next round of
runs, each row should be checked against the provider's current page and the date of that page written
here.

## What the curator does, in the terms' own vocabulary

| What it does | Why it matters for the terms |
|---|---|
| Sits between the agent and the API, with **the user's own key**, and sends a shorter conversation | It is the user's own input, edited before it is sent. No key is shared, pooled or resold. |
| Keeps what left the view in local files (spill) and in a local database | Data stays on the user's machine. Nothing is sent to a third party. |
| Uses a small local model (`nexo/crates/nexo-curator/data/model.json`: a logistic ranking of which parts of the context will be used again) trained on the user's own recorded sessions | It is a classifier of the user's own context, not a language model. It generates no text and does not imitate the provider's model. This is the one point that touches the "no competing models" clauses (below). |
| Runs benchmarks: the same tasks with the whole history and with the curated one, and publishes the scores and costs | Ordinary use of the API. None of the terms read forbids publishing measurements of one's own application. |
| Does not try to extract weights, system prompts or training data, and does not bypass safety filters or rate limits | Outside every "reverse engineering" and "circumvention" clause. |

## Provider by provider

| Provider | Relevant clauses | Reading for this work |
|---|---|---|
| **DeepSeek** (Open Platform Terms of Service, January 2025; **to verify**) | The platform terms refer to the general Terms of Use for conduct. Search results quote bans on network intrusion, **reverse engineering** the models and disrupting the service. The "no developing other models with the output" clause found is in the **open-source model licence**, not in the API terms. | Allowed: an intermediary that edits the user's own requests is not mentioned and not excluded. The curator's ranker is not trained on DeepSeek's output to imitate it. |
| **OpenAI** (Business Terms and Services Agreement) | Customers may not "use Output … to develop any artificial intelligence models that compete with our products and services". The November 2023 version explicitly allowed models "primarily intended to categorize, classify, or organize data (e.g., embeddings or classifiers)" if not distributed or made commercially available; whether that carve-out is still in force was **not confirmed**. | Allowed for the curator and the benchmarks. The ranker is a classifier that does not compete with a language model; to be safe, when OpenAI is the model, either train the ranker only on the shape of tool results and calls (which come from the user's machine, not from the model), or ship no ranker trained on its outputs. |
| **Google Gemini API** (Additional Terms, 2025–2026 versions; **to verify**) | "You may not use the Services to develop models that compete with the Services"; no reverse engineering, extracting or replicating models or data; do not bypass safety features. On the free tier, prompts and outputs may be used to improve Google's products. An older version barred using the Services to power another API; whether that clause remains was not confirmed. | Allowed for the curator and the benchmarks, with the same care about the ranker as for OpenAI. Run the benchmarks on a **paid** key, so that the task files and the recorded sessions are not used for training. The curator runs inside one user's harness, not as a public API. |
| **Mistral** (Terms of Service, Commercial Terms, Usage Policy of November 2025) | Usage Policy incorporated in the terms; no sharing of keys or credentials; no multiple accounts to evade rate limits. A third-party record reports that API access was moved to business customers in 2026 (dates conflict between sources). | Allowed. Check which plan gives API access today before buying a key. |

## Conclusion, and what to do before the next runs

1. **The curator and the benchmarks fit every set of terms read.** The curator only shortens what the user
   sends with their own key.
2. **The one clause to handle is "no models that compete".** The ranker is not a competing model, but it is
   trained on transcripts that include model output. Decision for the next runs: keep training it only on
   the user's own sessions, never distribute a ranker trained on another provider's outputs, and say so in
   the report.
3. **Use paid keys** for Gemini (and any provider with a free tier that trains on prompts).
4. **Write the date of each page checked** in this file when the rows marked *to verify* are confirmed.

Sources read (through search results): DeepSeek Open Platform Terms of Service
(platform.deepseek.com, cdn.deepseek.com); OpenAI Business Terms and the November 2023 Business Terms
(openai.com/policies); OpenAI Services Agreement (cdn.openai.com/osa); Gemini API Additional Terms of
Service (ai.google.dev/gemini-api/terms); Mistral Terms and Usage Policy (legal.mistral.ai/terms); the
ConductAtlas summaries of these documents, used only as pointers.
