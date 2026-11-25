# 11 Open questions

Decisions this specification leaves to the user. Each has a recommendation; none blocks stages 1
to 8.

| # | Question | Options | Recommendation |
| --- | --- | --- | --- |
| Q1 | Should `std:default` eventually replace the native Rust loop as the default harness? | (a) Never: the native loop stays default, Ratio is opt-in. (b) After stage 12's differential suite passes and a release cycle of opt-in use. (c) Replace the native loop entirely and delete it | (b): one loop to maintain in the end, but only after parity is measured; the research session's loop work would then move into Ratio packages |
| Q2 | May a model ever propose a harness? | (a) No. (b) As an extension install: the model writes a package to a staging folder, the user reviews and approves it in the desktop app, like a code-running plugin | (a) for now; (b) once the plugin approval path (item 51) and Windows signing (item 59) are in place |
| Q3 | Confidentiality labels: should Ratio add `Secret<T>` (text that may not reach network tool arguments or hosted models)? | (a) No; rely on frame structure. (b) Add `Secret<T>` from `read_file` of files matching secret patterns, from memory marked private, and from `.env`-like sources; network tools and hosted models refuse it | (b) in a later version; it addresses the exfiltration residual risk (T10) that taint cannot |
| Q4 | Should harness calls appear in the desktop's Agents panel? | (a) Only nested in the transcript (events). (b) As ephemeral agents in the panel while running | (a) at first; (b) if users ask to watch long searches |
| Q5 | Default limits: are 5,000,000 steps, 64 MiB heap, depth 8 and 500 tool calls per turn right? | Measure with the examples on the eval suites | Keep as draft values; stage 13 sets them from measurements |
| Q6 | Should `think.*` built-in prompt texts be user-overridable per installation (a prompt pack), or only per call? | (a) Per call only (`instructions:`). (b) Also a `<config>/ratio/prompts/` override folder | (a): overrides per installation would make eval results depend on local files, which `imperium run` avoids on purpose |
| Q7 | Exposing reasoning text: should `ModelReply` carry the model's reasoning when the backend returns it? | (a) No (today's Imperium keeps none). (b) Yes, as `Untrusted<Str>`, opt-in per call, never journalled | (a); revisit if a research method needs it |
| Q8 | Plugin harnesses: should enabling a plugin offer its harnesses to the model (`harness.offered`) automatically? | (a) No, the user adds them. (b) Yes for plugins the user approved | (a), consistent with T4 and T6 |
| Q9 | Should code mode programs (item 65) be able to call Ratio harnesses as functions? | (a) No. (b) Yes, as host functions, with the same attenuation | (a): code mode is for computation over tool results; mixing the two doubles the review surface |
| Q10 | The file extension and command names: `.ratio`, `imperium harness …`? | Any | `.ratio` and `imperium harness` as written |
| Q11 | Should the Consul escalation threshold stay a constant (as item 64 says: no setting) even for users who copy the package? | (a) Constant in the built-in; copies may change it. (b) A package parameter | (a), as item 64 asks; a copy is the user's own workflow |
| Q12 | Where should Ratio's specification live once the crate exists: this repository, or `docs/` of Imperium? | (a) Here, vendored into the crate's conformance suite at a pinned commit. (b) Moved into Imperium | (a): the language can version independently, as the tool schemas do for Emperor |
