# Related work

The references the thesis builds on, grouped by the question each answers. Peer-reviewed venues are marked
[PR]; documentation, preprints, technical reports and books [NPR]. Every entry was checked against the
publisher's page or the web index at the time of writing; the three marked "DOI unconfirmed" are
bibliographically consistent across sources but their DOI page was not opened. Provider documentation
changes often: the version read is the one dated here (October 2026).

## Prompt caching and its economics

1. [NPR] Anthropic. *Prompt caching*. Claude Developer Platform documentation.
   https://platform.claude.com/docs/en/build-with-claude/prompt-caching — the cost model's primary source:
   5-minute TTL refreshed on every hit, 1-hour option, cache write at 1.25× (5 min) or 2× (1 h), read at
   0.1× or lower on newer models, at most 4 breakpoints, exact prefix match, minimum cacheable prefix by model.
2. [NPR] Anthropic (2024-08-14). *Prompt caching with Claude*. https://www.anthropic.com/news/prompt-caching
   — dates the product and the 90% cost / 85% latency claims.
3. [NPR] Anthropic. *How Claude Code uses prompt caching*. https://code.claude.com/docs/en/prompt-caching —
   the prefix layers (system → project → conversation), what invalidates the cache, that compaction invalidates
   the conversation layer on purpose, warm and cold cache after idle gaps, the 5-minute against 1-hour
   trade-off: a production statement of the rent-or-buy decision this thesis formalises.
4. [NPR] Anthropic. *Manage costs effectively*. https://code.claude.com/docs/en/costs — the bill of a session
   split into cache read, cache write, input and output; why usage climbs in a long session.
5. [NPR] OpenAI (2024-10-01). *Prompt Caching in the API*. https://openai.com/index/api-prompt-caching —
   automatic prefix caching from 1,024 tokens, 50% discount at launch, 5–10 minutes of retention (up to 1 h).
6. [NPR] OpenAI. *Prompt caching* (API guide). https://developers.openai.com/docs/guides/prompt-caching —
   1,024-token minimum, 128-token increments, exact prefix, `cached_tokens`, extended retention.
7. [NPR] DeepSeek (2024-08-02). *DeepSeek API introduces Context Caching on Disk*.
   https://api-docs.deepseek.com/news/news0802/ — automatic, prefix from token 0, hits priced at about 1/10.
8. [NPR] DeepSeek. *Context Caching* (API guide). https://api-docs.deepseek.com/guides/kv_cache/ — cache
   units, `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens`, best-effort hits, expiry "hours to days":
   the provider of the live test, whose cache does not expire within a session.
9. [PR] Gu, C., Li, X. L., Kuditipudi, R., Liang, P., Hashimoto, T. (2025). *Auditing Prompt Caching in
   Language Model APIs*. ICML 2025. https://arxiv.org/abs/2502.07776 — timing audits of seven providers'
   caches; the only peer-reviewed empirical study of commercial prompt caches.
10. [PR] Gao, B. et al. (2024). *Cost-Efficient Large Language Model Serving for Multi-turn Conversations
    with CachedAttention*. USENIX ATC 2024. https://arxiv.org/abs/2403.19708 — the serving side of the same
    economics: keeping a session's KV cache across idle gaps.
11. [PR] Gim, I., Chen, G., Lee, S., Sarda, N., Khandelwal, A., Zhong, L. (2024). *Prompt Cache: Modular
    Attention Reuse for Low-Latency Inference*. MLSys 2024. https://arxiv.org/abs/2311.04934.
12. [PR] Zheng, L. et al. (2024). *SGLang: Efficient Execution of Structured Language Model Programs*.
    NeurIPS 2024. https://arxiv.org/abs/2312.07104 — RadixAttention, the mechanism behind automatic prefix caches.
13. [PR] Kwon, W. et al. (2023). *Efficient Memory Management for Large Language Model Serving with
    PagedAttention*. SOSP 2023. https://arxiv.org/abs/2309.06180.

No peer-reviewed paper was found that models the economics of commercial prefix caches (TTL, write premium,
read discount) for agent sessions; the closest are 9 (an external audit), 10 (serving cost) and the Claude
Code documentation (3, 4). This is the gap the cost model of the thesis fills.

## Context management, compaction and memory for agents

14. [NPR] Anthropic (2025-09-29). *Effective context engineering for AI agents*.
    https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents — context as a finite
    attention budget; compaction, structured notes, sub-agents.
15. [NPR] Anthropic. *Compaction* and *Context editing*. https://platform.claude.com/docs/en/build-with-claude/compaction,
    https://platform.claude.com/docs/en/build-with-claude/context-editing — the provider's own curation
    primitives, the baselines a curator competes with.
16. [NPR] Packer, C. et al. (2023). *MemGPT: Towards LLMs as Operating Systems*. https://arxiv.org/abs/2310.08560
    — virtual context management, the "operating system paging" framing.
17. [PR] Liu, N. F., Lin, K., Hewitt, J., Paranjape, A., Bevilacqua, M., Petroni, F., Liang, P. (2024).
    *Lost in the Middle: How Language Models Use Long Contexts*. TACL 12:157–173.
    https://aclanthology.org/2024.tacl-1.9/ — a larger context is not free in quality.
18. [NPR] Hong, K., Troynikov, A., Huber, J. (2025). *Context Rot: How Increasing Input Tokens Impacts LLM
    Performance*. Chroma technical report. https://research.trychroma.com/context-rot.
19. [PR] Jiang, H., Wu, Q., Lin, C.-Y., Yang, Y., Qiu, L. (2023). *LLMLingua: Compressing Prompts for
    Accelerated Inference of Large Language Models*. EMNLP 2023. https://aclanthology.org/2023.emnlp-main.825/
    — the "compress" alternative to "keep or drop".
20. [PR] Li, Y., Dong, B., Guerin, F., Lin, C. (2023). *Compressing Context to Enhance Inference Efficiency of
    Large Language Models* (Selective Context). EMNLP 2023. https://aclanthology.org/2023.emnlp-main.391/ —
    pruning lexical units by self-information; the closest prior to line-level selection.
21. [PR] Xu, F., Shi, W., Choi, E. (2024). *RECOMP: Improving Retrieval-Augmented LMs with Context
    Compression and Selective Augmentation*. ICLR 2024. https://arxiv.org/abs/2310.04408.
22. [NPR] Lindenbauer, T. et al. (2025). *The Complexity Trap: Simple Observation Masking Is as Efficient as
    LLM Summarization for Agent Context Management*. https://arxiv.org/abs/2508.21433 — on SWE-bench Verified,
    masking old observations halves the cost at the same solve rate as summarisation: the most direct
    precedent for cost-aware context policies in coding agents.
23. [PR] Kang, M. et al. (2026). *ACON: Optimizing Context Compression for Long-horizon LLM Agents*. ICLR 2026
    (https://arxiv.org/abs/2510.00615) — compression guidelines learned from the cases where the full context
    succeeds and the compressed one fails: hindsight labels, as here.
24. [PR] Ye, R. et al. (2026). *AgentFold: Long-Horizon Web Agents with Proactive Context Management*. ICLR
    2026 (https://arxiv.org/abs/2510.24699).
25. [PR] Chirkova, N., Formal, T., Nikoulina, V., Clinchant, S. (2025). *Provence: efficient and robust
    context pruning for retrieval-augmented generation*. ICLR 2025. https://arxiv.org/abs/2501.16214 —
    pruning as sequence labelling with derived labels.
26. [PR] Cohen-Wang, B., Shah, H., Georgiev, K., Mądry, A. (2024). *ContextCite: Attributing Model Generation
    to Context*. NeurIPS 2024. https://arxiv.org/abs/2409.00729 — model-based attribution of outputs to
    context spans; the counterpart of the lexical provenance used here.
27. [PR] Paranjape, A., Khattab, O., Potts, C., Zaharia, M., Manning, C. D. (2022). *Hindsight:
    Posterior-guided training of retrievers for improved open-ended generation*. ICLR 2022.
    https://arxiv.org/abs/2110.07752 — labels for a retriever derived from the output.
28. [PR] Belady, L. A. (1966). *A study of replacement algorithms for a virtual-storage computer*. IBM
    Systems Journal 5(2):78–101. DOI 10.1147/sj.52.0078 — the clairvoyant optimum (MIN), the reference point
    of the clairvoyant bound.
29. [PR] Jain, A., Lin, C. (2016). *Back to the Future: Leveraging Belady's Algorithm for Improved Cache
    Replacement*. ISCA 2016. https://www.cs.utexas.edu/~akanksha/isca16.pdf — a replacement policy learned
    from the clairvoyant algorithm run on past accesses: self-supervised labels from hindsight, in hardware.
30. [PR] Andrychowicz, M. et al. (2017). *Hindsight Experience Replay*. NeurIPS 2017.
    https://arxiv.org/abs/1707.01495.
31. [PR] Elsner, M., Charniak, E. (2010). *Disentangling Chat*. Computational Linguistics 36(3):389–409.
    https://aclanthology.org/J10-3004/ — splitting an interleaved stream into threads (the goal model).
32. [PR] Kummerfeld, J. K. et al. (2019). *A Large-Scale Corpus for Conversation Disentanglement*. ACL 2019.
    https://aclanthology.org/P19-1374/.
33. [PR] Xing, L., Carenini, G. (2021). *Improving Unsupervised Dialogue Topic Segmentation with
    Utterance-Pair Coherence Scoring*. SIGDIAL 2021. https://aclanthology.org/2021.sigdial-1.18/.
34. [PR] Hearst, M. A. (1997). *TextTiling: Segmenting Text into Multi-paragraph Subtopic Passages*.
    Computational Linguistics 23(1):33–64. https://aclanthology.org/J97-1003/.

## Online algorithms: rent or buy, competitive analysis, predictions

35. [PR] Sleator, D. D., Tarjan, R. E. (1985). *Amortized efficiency of list update and paging rules*.
    Communications of the ACM 28(2):202–208. https://doi.org/10.1145/2786.2793 — competitive analysis.
36. [PR] Karlin, A. R., Manasse, M. S., Rudolph, L., Sleator, D. D. (1988). *Competitive snoopy caching*.
    Algorithmica 3:79–119. https://doi.org/10.1007/BF01762111 — the rent-or-buy (ski rental) problem and
    its 2-competitive deterministic rule.
37. [PR] Karlin, A. R., Manasse, M. S., McGeoch, L. A., Owicki, S. (1994). *Competitive randomized algorithms
    for nonuniform problems*. Algorithmica 11:542–571 (DOI unconfirmed) — randomised e/(e−1) ski rental and
    the spin-block problem (the keep-alive decision).
38. [PR] Karlin, A. R., Kenyon, C., Randall, D. (2003). *Dynamic TCP Acknowledgment and Other Stories about
    e/(e−1)*. Algorithmica 36:209–224. https://doi.org/10.1007/s00453-003-1013-x — when to pay a batch cost.
39. [PR] Fujiwara, H., Iwama, K. (2005). *Average-case competitive analyses for ski-rental problems*.
    Algorithmica 42(1):95–107. https://doi.org/10.1007/s00453-004-1142-x — ski rental against a known
    distribution of the duration: the keep-alive ping on the pause distribution.
40. [PR] Augustine, J., Irani, S., Swamy, C. (2008). *Optimal Power-Down Strategies*. SIAM Journal on
    Computing 37(5):1499–1516. https://doi.org/10.1137/05063787X — idle-period power-down as continuous ski
    rental with a known idle-length distribution: the closest formal analogue of keeping a cache warm.
41. [PR] Purohit, M., Svitkina, Z., Kumar, R. (2018). *Improving Online Algorithms via ML Predictions*.
    NeurIPS 2018. https://proceedings.neurips.cc/paper/2018/hash/73a427badebe0e32caa2e1fc7530b7f3-Abstract.html
    — ski rental with possibly wrong predictions, consistency against robustness: the `trust` parameter.
42. [PR] Lykouris, T., Vassilvitskii, S. (2021). *Competitive Caching with Machine Learned Advice*. Journal of
    the ACM 68(4). https://doi.org/10.1145/3447579 — caching with learned next-use predictions.
43. [PR] Bamas, É., Maggiori, A., Svensson, O. (2020). *The Primal-Dual method for Learning Augmented
    Algorithms*. NeurIPS 2020.
44. [PR] Wei, A., Zhang, F. (2020). *Optimal Robustness-Consistency Trade-offs for Learning-Augmented Online
    Algorithms*. NeurIPS 2020 — the bounds of 41 are tight.
45. [NPR] Mitzenmacher, M., Vassilvitskii, S. (2022). *Algorithms with Predictions*. Communications of the ACM
    65(7):33–35; survey https://arxiv.org/abs/2006.09123.
46. [NPR] Borodin, A., El-Yaniv, R. (1998). *Online Computation and Competitive Analysis*. Cambridge
    University Press.

## The economic order quantity

47. [NPR] Harris, F. W. (1913). *How Many Parts to Make at Once*. Factory, The Magazine of Management
    10(2):135–136, 152; reprinted in Operations Research 38(6):947–950 (1990).
    https://doi.org/10.1287/opre.38.6.947 — the square-root formula the cap of the view follows.
48. [PR] Erlenkotter, D. (1990). *Ford Whitman Harris and the Economic Order Quantity Model*. Operations
    Research 38(6):937–946. https://doi.org/10.1287/opre.38.6.937.
49. [NPR] Zipkin, P. H. (2000). *Foundations of Inventory Management*. McGraw-Hill — the sensitivity of the
    cost near the optimum (flat), which is why the cap's plateau is expected.

## Submodular maximisation under a budget (the ledger)

50. [PR] Nemhauser, G. L., Wolsey, L. A., Fisher, M. L. (1978). *An analysis of approximations for maximizing
    submodular set functions—I*. Mathematical Programming 14:265–294. https://doi.org/10.1007/BF01588971.
51. [PR] Khuller, S., Moss, A., Naor, J. (1999). *The budgeted maximum coverage problem*. Information
    Processing Letters 70(1):39–45. https://doi.org/10.1016/S0020-0190(99)00031-9 — the ledger's selection
    problem exactly; the (1−1/e) guarantee of the cost-benefit greedy for the budget less the largest item.
52. [PR] Sviridenko, M. (2004). *A note on maximizing a submodular set function subject to a knapsack
    constraint*. Operations Research Letters 32(1):41–43 (DOI unconfirmed).
53. [NPR] Krause, A., Guestrin, C. (2005). *A Note on the Budgeted Maximization of Submodular Functions*.
    CMU-CALD-05-103 — the cheap rule (best of the greedy and the best single item) reaches (1−1/e)/2.
54. [PR] Leskovec, J., Krause, A., Guestrin, C., Faloutsos, C., VanBriesen, J., Glance, N. (2007).
    *Cost-effective Outbreak Detection in Networks*. KDD 2007. https://doi.org/10.1145/1281192.1281239 — the
    lazy greedy (CELF) the ledger uses.
55. [PR] Minoux, M. (1978). *Accelerated greedy algorithms for maximizing submodular set functions*. In
    Optimization Techniques, LNCIS 7, Springer (DOI unconfirmed) — the original lazy evaluation.
56. [PR] Lin, H., Bilmes, J. (2010). *Multi-document Summarization via Budgeted Maximization of Submodular
    Functions*. NAACL-HLT 2010. https://aclanthology.org/N10-1134/ — sentences under a length budget.
57. [NPR] Krause, A., Golovin, D. (2014). *Submodular Function Maximization*. In Tractability, Cambridge
    University Press, pp. 71–104.

## Evaluation of coding agents and their cost

58. [PR] Jimenez, C. E. et al. (2024). *SWE-bench: Can Language Models Resolve Real-World GitHub Issues?*.
    ICLR 2024. https://arxiv.org/abs/2310.06770.
59. [NPR] OpenAI (2024-08-13). *Introducing SWE-bench Verified*. https://openai.com/index/introducing-swe-bench-verified/.
60. [PR] Yang, J., Jimenez, C. E., Wettig, A., Lieret, K., Yao, S., Narasimhan, K., Press, O. (2024).
    *SWE-agent: Agent-Computer Interfaces Enable Automated Software Engineering*. NeurIPS 2024.
    https://arxiv.org/abs/2405.15793.
61. [PR] Wang, X. et al. (2025). *OpenHands: An Open Platform for AI Software Developers as Generalist
    Agents*. ICLR 2025. https://arxiv.org/abs/2407.16741.
62. [PR] Kapoor, S., Stroebl, B., Siegel, Z. S., Nadgir, N., Narayanan, A. (2025). *AI Agents That Matter*.
    TMLR. https://arxiv.org/abs/2407.01502 — agent evaluation must control for cost: accuracy against cost.
63. [PR] Kapoor, S. et al. (2026). *Holistic Agent Leaderboard: The Missing Infrastructure for AI Agent
    Evaluation*. ICLR 2026 (https://arxiv.org/abs/2510.11977).
64. [PR] Erol, M. H., El, B., Suzgun, M., Yuksekgonul, M., Zou, J. (2026). *Cost-of-Pass: An Economic
    Framework for Evaluating Language Models*. ICLR 2026 (https://arxiv.org/abs/2504.13359) — expected cost
    per correct solution.
65. [NPR] Wang, N. et al. (2025). *Efficient Agents: Building Effective Agents While Reducing Cost*.
    https://arxiv.org/abs/2508.02694.
66. [PR] Merrill, M. A., Shaw, A. G. et al. (2026). *Terminal-Bench: Benchmarking Agents on Hard, Realistic
    Tasks in Command Line Interfaces*. ICLR 2026 (https://arxiv.org/abs/2601.11868).

## Prompting and the agent's own efficiency (the directives)

67. [NPR] Anthropic. *Claude prompting best practices*.
    https://platform.claude.com/docs/en/build-with-claude/prompt-engineering/claude-4-best-practices —
    targeted prompting raises parallel tool-call rates; controlling verbosity.
68. [NPR] OpenAI (2025). *GPT-4.1 Prompting Guide*. OpenAI Cookbook.
    https://developers.openai.com/cookbook/examples/gpt4-1_prompting_guide — three short agentic reminders
    changed tool behaviour measurably.
69. [PR] Kim, S., Moon, S., Tabrizi, R., Lee, N., Mahoney, M. W., Keutzer, K., Gholami, A. (2024). *An LLM
    Compiler for Parallel Function Calling*. ICML 2024. https://arxiv.org/abs/2312.04511 — batching
    independent tool calls: up to 3.7× latency and 6.7× cost reduction against sequential ReAct.
70. [NPR] Xu, B. et al. (2023). *ReWOO: Decoupling Reasoning from Observations for Efficient Augmented
    Language Models*. https://arxiv.org/abs/2305.18323 — the token cost of re-sending context per tool round.
71. [PR] Han, T., Wang, Z., Fang, C., Zhao, S., Ma, S., Chen, Z. (2025). *Token-Budget-Aware LLM Reasoning*.
    Findings of ACL 2025. https://aclanthology.org/2025.findings-acl.1274/ — prompted token budgets cut output
    60–70% at near-equal accuracy.
72. [NPR] Nayab, S. et al. (2024). *Concise Thoughts: Impact of Output Length on LLM Reasoning and Cost*.
    https://arxiv.org/abs/2407.19825.
73. [NPR] Xu, S., Xie, W., Zhao, L., He, P. (2025). *Chain of Draft: Thinking Faster by Writing Less*.
    https://arxiv.org/abs/2502.18600.
