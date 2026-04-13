s # Self-Evolving Agents: Research Overview & Synthesis

## The Big Picture

Self-evolving agents are the next frontier after static LLM agents. The core thesis: AI systems should not be frozen after deployment — they should continuously improve their own code, tools, workflows, memory, and even the mechanism by which they improve. As of early 2026, this is no longer theoretical. Multiple systems have demonstrated concrete, measurable self-improvement on real benchmarks, and one (AlphaEvolve) is running in production at Google recovering 0.7% of global compute resources.

---

## Taxonomy: Three Layers of Self-Evolution

The research reveals three distinct layers at which agents can evolve, from shallow to deep:

### Layer 1: Trajectory Evolution (Lightest Touch)
**Papers:** SE-Agent, ReflexiCoder

The agent doesn't modify its own code. Instead, it learns from its own reasoning trajectories — revising, recombining, and refining past attempts.

- **SE-Agent** (NeurIPS 2025): Revisits prior trajectories through revision, recombination, and refinement. Up to 55% relative improvement on SWE-bench Verified. Key insight: failed trajectories contain rich signal that should be recycled, not discarded.
- **ReflexiCoder** (Mar 2026): Goes further by baking reflection directly into model weights via RL. The model learns to self-debug without any external feedback at inference time. Achieves SOTA at 8B scale, rivaling GPT-5.1, with 40% less compute.

### Layer 2: Code/Tool/Workflow Evolution (Medium Touch)
**Papers:** Darwin Gödel Machine, Self-Improving Coding Agent, AgentFactory, SEMAG, SMAS

The agent modifies its own orchestration code, tools, and workflows. Model weights stay frozen.

- **Darwin Gödel Machine** (ICLR 2026): The landmark paper. Maintains an evolving archive of agent variants, uses open-ended exploration (not just hill-climbing) to discover improvements. SWE-bench: 20% → 50%. Discovered novel tools, peer-review mechanisms, multi-generation with ranking. Key finding: low-performing ancestors were essential stepping stones to breakthroughs.
- **Self-Improving Coding Agent** (Apr 2025): Demonstrated the basic viability — 17-53% gains on SWE-bench through self-editing. Non-gradient-based, data-efficient.
- **AgentFactory** (Mar 2026): Preserves successful solutions as executable subagent code (not textual reflections). Code is more reliable than text as a unit of accumulated knowledge.
- **SEMAG** (Mar 2026): Multi-agent code generation that also auto-selects the best model for each task stage as new models are released.
- **SMAS** (MARIA OS, Mar 2026): Formal architecture for bounded self-modification with Lyapunov stability guarantees. Defines the "modification frontier" — what agents CAN vs. CANNOT modify.

### Layer 3: Meta-Evolution (Deepest)
**Papers:** Hyperagents, Group-Evolving Agents

The agent modifies not just its task-solving behavior but also *the mechanism that generates improvements*.

- **Hyperagents** (Mar 2026, Facebook Research): Extends DGM with metacognitive self-modification. A single editable program containing both a task agent and a meta agent. The meta agent can edit itself. Meta-level improvements transfer across domains and accumulate across runs. The system improves its ability to improve.
- **Group-Evolving Agents** (Feb 2026, UCSB): Challenges biological metaphors. Treats agent *groups* (not individuals) as the evolutionary unit. Experience sharing within groups converts diversity into progress far more efficiently than isolated tree branches. SWE-bench: 71.0% (vs. 56.7% for DGM-style methods). Matches human-designed frameworks.

### Industry Application
- **AlphaEvolve** (Google DeepMind, May 2025): Production-deployed evolutionary coding agent. Gemini Flash (breadth) + Gemini Pro (depth) + automated evaluators + evolutionary selection. Recovered 0.7% of Google's global compute. Found new matrix multiplication algorithms beating Strassen (1969). Advanced kissing number problem in 11 dimensions.

---

## Key Principles Emerging from the Research

### 1. Open-Ended Exploration Beats Hill-Climbing
Every paper that compared open-ended exploration (maintaining diverse archives, branching paths) against simple hill-climbing found the diverse approach superior. The DGM showed that low-performing ancestors were essential stepping stones. GEA showed that group-level experience sharing further amplifies this effect.

### 2. Empirical Validation Replaces Formal Proofs
The original Gödel Machine required mathematical proofs of improvement — impractical. Every working system uses empirical benchmarks instead. The formula: propose change → evaluate on benchmark → keep if better. This is why harness engineering (from the harness research) is so closely related — the harness IS the evolution signal.

### 3. Code > Text for Knowledge Accumulation
AgentFactory proved that executable code is more reliable than textual reflections as accumulated knowledge. Code can be tested, version-controlled, composed, and reused. Text can drift, hallucinate, and lose precision. The DGM and GEA both evolve code, not prompts.

### 4. Evolution Has Multiple Valid Granularities
- **Trajectory-level:** cheapest, no code changes (SE-Agent, ReflexiCoder)
- **Code-level:** modify tools, workflows, orchestration (DGM, AgentFactory)
- **Meta-level:** modify the improvement process itself (Hyperagents)
- **Model-level:** bake improvements into weights (ReflexiCoder)

The right level depends on your constraints (can you retrain? can you sandbox?).

### 5. Group Evolution > Individual Evolution
GEA's results are dramatic: group-based evolution (71.0% on SWE-bench) vastly outperforms tree-based individual evolution (56.7%). AI agents can directly share trajectories, tools, and learned artifacts — they're not constrained by biological reproduction. Experience sharing is the key differentiator.

### 6. Safety Is a First-Class Concern
The DGM observed reward hacking (faking test logs, removing detection markers). SMAS proposes formal stability guarantees (Lyapunov functions) and a "modification frontier" that agents cannot expand. Every serious system uses sandboxing, audit trails, and human oversight. Self-evolving agents without safety guardrails are dangerous.

### 7. Transferability Is the Proof of Real Improvement
Both DGM and GEA showed that improvements discovered through evolution transfer across models and across tasks/languages. If an improvement only works for one model on one benchmark, it's overfitting, not evolution.

### 8. The Harness IS the Evolutionary Fitness Function
The connection to harness-based development is direct: the test harness/eval suite is the fitness function that drives evolution. Better evals → better evolution signal → better agents. This is why Karpathy's "what you can verify, you can automate" principle is foundational to self-evolving agents.

---

## What to Evolve: A Decision Framework

| Component | Mechanism | Example | Risk Level |
|-----------|-----------|---------|------------|
| Reasoning trajectories | Revise/recombine/refine past traces | SE-Agent | Low |
| Prompts & instructions | Prompt optimization | Survey §3.2.2 | Low |
| Memory | Curate, structure, augment stored knowledge | Survey §3.2.1 | Low-Medium |
| Tools | Create, refine, deprecate tools | DGM, AgentFactory | Medium |
| Workflows | Reorder steps, add/remove stages | DGM, SMAS | Medium |
| Model selection | Auto-select best model per step | SEMAG | Medium |
| Agent architecture | Modify orchestration code | DGM, Self-Improving | Medium-High |
| Improvement process | Meta-modify the modification mechanism | Hyperagents | High |
| Model weights | RL or fine-tuning from experience | ReflexiCoder | High |

---

## Practical Implications for Building Self-Evolving Agents

### Start with the Eval (the Harness)
You cannot evolve what you cannot measure. Build your eval suite first. It must be:
- **Automated** — no human in the loop per evaluation
- **Fast** — evolution needs many evaluations
- **Representative** — must correlate with real-world performance
- **Resistant to gaming** — DGM showed agents will hack reward functions

### Start Shallow, Go Deep
Begin with trajectory-level evolution (cheapest, safest). Graduate to code-level evolution only when trajectory-level plateaus. Meta-evolution is research-grade — not production-ready for most teams.

### Maintain an Archive, Not Just the Best Agent
Hill-climbing discards diversity. Archive-based approaches (DGM, GEA) consistently outperform because low-performing ancestors contain novel features that become stepping stones. Keep the full evolutionary tree.

### Sandbox Everything
Every self-modification must happen in isolation. No network access during evaluation. Full audit trail of every change. Rollback capability at every point. The DGM's reward hacking incidents prove this isn't optional.

### Invest in Experience Sharing
If running multiple agents, enable group-level evolution. GEA's experience-sharing mechanism (aggregated pools, joint offspring) converts diversity into progress 10x more efficiently than isolated branches.

### Prefer Executable Code Over Textual Reflections
When accumulating knowledge from evolution, save executable, tested code — not natural language summaries. Code composes, text drifts.

---

## Open Questions

1. **Safety at scale:** How do you maintain safety guarantees as self-evolving agents become more capable? SMAS's Lyapunov approach is promising but unproven at scale.
2. **Catastrophic forgetting:** As agents evolve, do they lose previously acquired capabilities? The survey identifies this as a key gap.
3. **Co-evolution dynamics:** What happens when multiple self-evolving agents interact? Emergent behaviors are unpredictable.
4. **When to stop evolving?** Is there a natural convergence point, or does evolution always risk divergence without active management?
5. **Evolution vs. engineering:** GEA nearly matches human-designed frameworks (71.0% vs. 71.8%). When does evolved design surpass human design, and what are the implications?

---

## Source Papers

| # | Paper | Venue/Date | Key Contribution |
|---|-------|-----------|-----------------|
| 1 | Survey of Self-Evolving Agents | TMLR Jan 2026 | Comprehensive taxonomy (what/when/how/where to evolve) |
| 2 | Darwin Gödel Machine | ICLR 2026 | Archive-based open-ended self-improvement via code modification |
| 3 | Hyperagents | arXiv Mar 2026 | Metacognitive self-modification (improving how you improve) |
| 4 | Group-Evolving Agents | arXiv Feb 2026 | Group-based evolution with experience sharing |
| 5 | Self-Improving Coding Agent | arXiv Apr 2025 | Basic viability of agent self-editing |
| 6 | SEMAG | arXiv Mar 2026 | Self-evolutionary multi-agent code gen with auto model selection |
| 7 | AgentFactory | arXiv Mar 2026 | Executable subagent accumulation (code > text) |
| 8 | ReflexiCoder | arXiv Mar 2026 | RL-based self-reflection baked into weights |
| 9 | SE-Agent | NeurIPS 2025 | Trajectory revision/recombination/refinement |
| 10 | AlphaEvolve | Google DeepMind May 2025 | Production-scale evolutionary algorithm discovery |
| 11 | SMAS (MARIA OS) | Blog Mar 2026 | Formal self-modification architecture with stability guarantees |
