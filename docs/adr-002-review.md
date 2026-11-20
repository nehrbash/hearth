# design-critic review of Hearth ADR-002 (2026-11-19)

VERDICT: Revise. Accept the process boundary; rework the protocol before accepting.

Kill-assumptions, ranked:
1. A one-way snapshot bridge is not "Scheme bindings": no events, acks, queries; no stable node IDs; pointer input goes Rust -> `weft owner` CLI; "animation targets" claimed but absent from the protocol (deny_unknown_fields); the M5 bar is pure Rust. Test: build one interactive widget (button toggling state with a press animation) with behaviour in Scheme. Today impossible.
2. Weft is the wrong home for UI Scheme: its evaluator is serialized ("long evaluations delay other requests") and aborted today. scheme/live.scm is already a separate Guile process shelling out to `weft` three times per tick. Test: `(sleep 10)` via workspace-eval while live.scm --activity runs.
3. One global full-snapshot stream cannot carry services: multiple producers clobber each other (single `current` store), 60 Hz updates (PipeWire, MPRIS) resend everything over a new connection per message; Hearth->Weft export undefined; a renderer crash would take org.freedesktop.Notifications / StatusNotifierWatcher with it.
4. Versioning: deny_unknown_fields + no handshake makes any additive field a lockstep break, undetectable by the producer.
5. MAX_RETIRED_GENERATIONS = 64: after 64 Weft restarts the shell refuses all baselines until restarted. Use a monotonic per-channel epoch instead.
6. Latency unmeasured: 1 s polling + 3 subprocess spawns per tick. Measure Scheme-change-to-presented-frame p50/p99 at 4 KiB, 64 KiB, 1 MiB.
7. Alternatives not compared on evidence. Embedding Guile is in fact worse than argued: libgc "Too many root sets" under repeated compile (guile-devel 2022-08/msg00043) would abort the renderer; libgc would scan the NVIDIA driver's segments.

Blank-slate alternative (recommended direction):
- Hearth renderer (Rust): keyed retained tree (stable node IDs), declarative per-property transitions and state sets (QML Behavior/States equivalent) so hover/press need no Scheme round trip.
- A dedicated Guile UI process (not Weft) owning widget definitions and event handlers, with a REPL for live redefinition and advice.
- Service modules in Rust (in Hearth or a sibling daemon) publishing to named channels; decide crash behaviour for D-Bus names.
- Bidirectional, channel-scoped protocol: hello with major version + capabilities (ignore unknown optional fields within a major); (channel, epoch, seq) ordering; baseline + keyed patches; Hearth -> producer: input events by node key, applied/rejected acks with reasons, inspect query results. Weft is one channel producer/subscriber, never on the input path.
- Switching cost: rework ~700 lines protocol.rs + ~400 lines Scheme + an event path in wayland.rs (~1.5-3k lines); renderer/animation/text untouched.

Rewrite mandates for ADR-002:
- Context: state the owner's goal (Scheme bindings = live redefinition, advice, inspection; Hearth as a Weft UI; replace Quickshell incl. services) and which parts this ADR delivers.
- Decision: explicit UI-Scheme process separate from Weft; protocol redone as above; monotonic epochs; where services live and their crash behaviour; Hearth->Weft export as best-effort mirror.
- Alternatives: redo embedded-Guile with the libgc evidence (strengthens the process boundary).
- Consequences: acceptance thresholds with measured numbers (p99 change-to-frame under one frame + 5 ms at 64 KiB; 1 MiB decode/validate time; 60 Hz service soak without dropped final states). Replace the 1 s polling producer with Weft watch/notice push.

Sound and worth keeping: Scheme out of frame callbacks with validation on an I/O worker and latest-wins at frame boundaries; full validation before replacing the tree with limits; owner authority separate from render data; socket hygiene; no Scheme text on the wire; process isolation.
