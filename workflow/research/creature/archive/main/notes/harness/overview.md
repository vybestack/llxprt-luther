o# Harness-Based Development: How to Do It Effectively

*Synthesized from research across Anthropic, OpenAI, LangChain, academic papers, and practitioner experience.*

---

## What Is Harness-Based Development?

Harness-based development is the emerging discipline where the engineer's primary job shifts from writing code to **designing the environment in which AI agents write code**. The "harness" is everything surrounding the model: the test suites, the feedback loops, the context assembly, the verification systems, the architectural guardrails, and the observability infrastructure that together enable agents to do reliable, autonomous work.

The term draws from Karpathy's insight: **Software 1.0 automates what you can specify; Software 2.0 automates what you can verify.** The harness is how you make things verifiable.

---

## The Core Principles

### 1. Start With Evals, Not Code

Evaluations are the starting point, not an afterthought. As CodeRabbit's David Loker puts it: "It's not something I can be passive about." Both OpenAI (Greg Brockman) and Instagram's Mike Krieger agree that "evals are surprisingly often all you need."

**What this means in practice:**
- Define what "correct" looks like as a deterministic function before writing any prompts
- Build the grader and harness before the implementation
- Evals must run in CI — if they don't run on every change, they don't exist
- Probabilistic systems require statistical proof: sample sizes, confidence intervals, regression baselines
- Cost (tokens, latency, compute) is an evaluation dimension, not an afterthought

### 2. Design the Workflow First, Then Embed Intelligence

Don't start with a model and hope. Map your domain process, identify which steps require judgment (agentic) and which are mechanical (deterministic), build the workflow skeleton, then embed agents where they add value.

Hybrid architectures that combine structured workflow with embedded agentic loops achieve 88.8% average Goal Completion Rate, outperforming pure ReAct, chain-of-thought, and tool-only approaches. Pure ReAct (the single reasoning-action loop) is fragile because it skips most agent subsystems.

**Key workflow elements:**
- Deterministic pipeline for mechanical steps (fetching data, running static analysis, building code graphs)
- Agentic loops inserted only where reasoning/judgment is actually needed
- Multiple model choices per step (10+ variants at CodeRabbit)
- Cross-model verification (different model checks the first model's output)

### 3. Engineer Context, Not Prompts

Context engineering is assembling the right information, from the right sources, in the right structure, at the right time, for each step. It's the whole game.

**Critical findings from research:**
- **More context can make your agent worse.** Irrelevant context actively degrades performance. Better retrievers produce MORE dangerous distractors (counterintuitive). Hedged wrong answers ("may have been...") are the most dangerous distractor type.
- **Context should be an evolving playbook, not static.** Use incremental delta updates, not monolithic rewrites. Monolithic rewriting caused context to collapse from 18,282 tokens to 122 in ACE study.
- **Filter aggressively.** Use multi-agent consensus to decide what's relevant before passing to the generator (MAIN-RAG approach).
- **2-3 focused skills are optimal.** More than 4 skills collapse gains. Comprehensive documentation actually HURTS by -2.9pp. Self-generated skills provide zero benefit on average.

### 4. Make AGENTS.md a Map, Not a Manual

Both OpenAI and Anthropic independently discovered this: one big instruction file fails catastrophically.

**Why monolithic instructions fail:**
- Context is scarce — giant files crowd out the actual task
- When everything is "important," nothing is
- Rules rot instantly; agents can't tell what's still true
- Can't be mechanically verified

**The solution:**
- ~100 line AGENTS.md as table of contents with pointers
- Structured docs/ directory as the system of record
- Progressive disclosure: agents start with small entry point, learn where to look next
- Mechanical enforcement via linters and CI that validate the knowledge base
- Recurring "doc-gardening" agent to clean stale docs

### 5. Make Everything Legible to the Agent

From the agent's point of view, anything it can't access in-context effectively doesn't exist. Knowledge in Google Docs, Slack, or people's heads is invisible.

**Legibility infrastructure:**
- App bootable per git worktree (one instance per change)
- Chrome DevTools Protocol wired into agent runtime (screenshots, DOM snapshots, navigation)
- Ephemeral observability stack per worktree (logs via LogQL, metrics via PromQL, traces via TraceQL)
- Test output designed for agent consumption: few lines of output, greppable errors, pre-computed summaries
- Time awareness injected (agents are "time blind")

### 6. Build a Self-Verification Loop

The most common failure mode: agent writes solution, re-reads own code, confirms it looks ok, stops. Self-evaluation is broken — agents confidently praise mediocre work.

**The fix: separate generation from verification.**

Three approaches that work:
1. **Cross-model verification:** Use a different model to check the output. Training differences mean different blind spots.
2. **GAN-inspired generator/evaluator:** Separate agent generates, separate agent evaluates against explicit criteria with hard thresholds.
3. **PreCompletionChecklistMiddleware:** Intercept agent before exit, force verification pass against task spec (Ralph Wiggum Loop).

Anthropic found Claude is a poor QA agent out of the box — it identifies issues then talks itself into approving. Required several tuning rounds before grading was reasonable. The evaluator must be calibrated with few-shot examples.

### 7. Enforce Architecture Mechanically, Not Through Instructions

Agents replicate patterns that already exist — including bad ones. Over time, drift is inevitable.

**Mechanical enforcement:**
- Strict layered domain architecture with validated dependency directions
- Custom linters (written by the agent) that inject remediation instructions into agent context
- Structural tests that enforce boundaries
- Enforce invariants (e.g., "parse data at the boundary"), not implementations (don't prescribe which library)
- "Golden principles" encoded in repo with recurring cleanup agents

**Entropy management:**
- OpenAI initially spent 20% of engineering time on "AI slop" cleanup — didn't scale
- Solution: background agent tasks that scan for deviations and open refactoring PRs
- Technical debt as high-interest loan — pay down continuously in small increments

### 8. Use the Right Model for Each Step

Different steps need different capabilities. CodeRabbit uses 10+ model variants depending on workflow position.

**Considerations per step:**
- Reasoning token budget (more for planning and verification, less for implementation — the "reasoning sandwich")
- Latency requirements (high-latency models are terrible for loops)
- Cost (cheaper model sufficient for many steps; expensive model only where quality demands it)
- A smaller model + good skills can match a larger model without skills (Haiku 4.5 at 27.7% outperformed Opus 4.5 at 22.0%)

### 9. Build Tools Deliberately

Research formalizes tool use as four stages, each needing explicit engineering:
1. **Discovery:** How does the agent know what tools exist?
2. **Selection:** Given the task, which tool? (This is the critical bottleneck — most failures occur here)
3. **Invocation:** Calling correctly with proper parameters and error handling
4. **Integration:** Parsing output and injecting back into reasoning

### 10. Build Memory With Curation

Don't just log — augment, structure, and filter what gets stored and retrieved. Autonomous memory augmentation yields +34% recall over naive RAG.

Per-organization customization at scale: store feedback, retrieve by context using RAG, inject into future reviews. All without building new models.

---

## The Anthropic C Compiler: A Case Study in Harness Design

Anthropic's most instructive experiment: 16 parallel Claude Opus 4.6 instances building a 100,000-line C compiler from scratch over 2 weeks, $20,000 in API costs.

**What made it work was the harness, not the model:**
- Simple bash loop running Claude continuously in Docker containers
- Task locking via git file synchronization
- High-quality test suites as the north star (Claude solves whatever the tests define)
- GCC used as oracle compiler to enable parallelism on monolithic tasks (kernel compilation)
- Multiple agent roles: main builders, code quality critic, documentation agent, performance optimizer
- Test harness designed for Claude: greppable errors, pre-computed summaries, deterministic subsampling, no context pollution

**What the experiment revealed about limits:**
- New features frequently broke existing functionality near the capability ceiling
- Rust code quality reasonable but not expert-level
- Generated code less efficient than GCC with all optimizations disabled

---

## The Three-Layer Stack

Drawing from Zak El Fassi's synthesis:

1. **Knowledge Layer (Skills-Driven Development):** Agents forge reusable skills as they build. The repo accumulates callable, discoverable, composable capabilities.
2. **Environment Layer (Harness Engineering):** The repo is designed for agents to navigate. AGENTS.md is a map. Verification is automated. Observability is queryable. The environment makes work tractable.
3. **Orchestration Layer (Symphony/Daemon):** A daemon reads the work queue, dispatches agents per issue, collects proof of work, lands PRs. Humans manage work, not agents.

> "You can't run Symphony on an unharnessed repo — it just produces faster chaos."

---

## Checklist: Building an Effective Harness

1. **Define what you can evaluate.** Build evals first. Every task needs an eval, every eval needs a threshold, every threshold needs a justification.
2. **Map the workflow.** Identify deterministic vs. judgment steps. Don't make the whole thing agentic.
3. **Structure the knowledge base.** AGENTS.md as ~100 line map. Structured docs/. Progressive disclosure. Mechanical enforcement of freshness.
4. **Engineer context per step.** Right information, right granularity, right time. 2-3 focused skills max. Filter aggressively.
5. **Make the app legible to agents.** Per-worktree instances, DevTools access, ephemeral observability, greppable errors, time awareness.
6. **Build self-verification.** Separate generator from evaluator. Cross-model verification. PreCompletion checks. Calibrate the evaluator.
7. **Enforce architecture mechanically.** Custom linters with agent-readable error messages. Structural tests. Golden principles. Recurring cleanup agents.
8. **Choose models per step.** Reasoning sandwich. Latency-aware selection. Cost-quality tradeoffs per step.
9. **Build tools deliberately.** Each of four stages (discovery, selection, invocation, integration) needs its own engineering.
10. **Build curated memory.** Augment and structure stored interactions. Filter with multi-agent consensus. Retrieve by context.
11. **Design feedback loops from day one.** Environmental signals (cheapest, most reliable), user feedback (highest quality, doesn't scale), cross-model critique, tool feedback (static analysis, tests).
12. **Iterate the harness continuously.** Every component encodes an assumption about what the model can't do. Those assumptions go stale as models improve. Stress-test regularly. Simplify when possible.

---

## Key Insight

The bottleneck is never code generation. It's environment legibility. Debug the harness before debugging the model.

> "We shouldn't all be modeling Claude Code or OpenClaw and making a linear agent that manages its own context and does 'whatever.' Instead we should be developing curated workflows with very specific tools." — Andrew C. Oliver

> "Humans steer. Agents execute." — OpenAI Harness Engineering Team

> "Software 1.0 easily automates what you can specify. Software 2.0 easily automates what you can verify." — Andrej Karpathy
