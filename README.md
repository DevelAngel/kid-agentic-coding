# kid-agentic-coding

**KID** — Keep It Done. A toolkit for running coding agents in focused, task-scoped sessions instead of one sprawling context.

## The Trap

It's 9 a.m. You just fixed a linting error manually across four repositories, one after another, without a single hiccup. Confident, you hand your agent a smaller job: correct one sentence in a code review request description, in those same four repos.

It fails. Not once — repeatedly. It runs tests nobody asked for. It loses the thread mid-task and needs three, four rounds of correction before the sentence is finally right.

Why does the easy job break what the hard job didn't?

Because the agent never put the hard job down. Across the session it had accumulated git logs, git status, and code review descriptions from all four repositories at once — most of it irrelevant to the one sentence it was supposed to fix. When the context filled up, it got compacted. And the same clutter came right back.

You can blame the compaction algorithm for doing a bad job. But no algorithm compacts well when what goes in is already noise. The context wasn't full of the wrong things because compaction failed — compaction failed because the context was full of the wrong things. The problem sits upstream, not in the summarizer.

## Vision

**Agentic coding that behaves the same way twice.**

Not "trustworthy" in the vague sense of a chatbot you've grown to like. Reliable in the engineering sense: predictable. Give the same task twice, get the same kind of run twice — not a coin flip between a clean fix and a context-poisoned spiral.

This matters more than it used to. A frontier model with a 300K or 1M token window can afford to be a little wasteful with context — there's room to spare. A self-hosted Qwen3-32B running on your own hardware has no such luxury. For agents built on smaller, cheaper, locally-hosted models, focus isn't a nice-to-have. It's the difference between a task that finishes and one that drowns.

## Mission

Provide workflows that run in separate, temporary agent sessions — one focused context per unit of work, not one sprawling context for the whole afternoon.

Subagents don't solve this on their own. A subagent is still triggered by the main agent, and still returns to a single turn in that same accumulating context. What's missing isn't another layer of delegation — it's a hard boundary. A session that starts clean, does one thing, and ends. No leftover git logs from repo two bleeding into the fix for repo four.

There's a second reason, independent of context size. Even a frontier model, with room to spare, forgets to apply a skill it was told to follow. Not once — repeatedly, across sessions, no matter how often you remind it. A workflow doesn't have this problem: the skill isn't a hint floating somewhere in a long context, it's a step the workflow walks through on every run.

That's what this project builds: task-scoped agent sessions, wired together as workflows, so context stays exactly as full as the task requires — no more, no less.
