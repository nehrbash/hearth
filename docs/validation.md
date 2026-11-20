# Validation evidence

Latency targets and the final-state soak passed using `target/release/hearth`
on the local NVIDIA GeForce RTX 3070 Ti/Vulkan renderer, on output DP-3 at 60 Hz.
The widget and Weft push proofs used the debug binary. Measurements time receipt
of GPU submission reports, not physical display scanout or input-to-photon
latency. They describe this run, not a cross-machine performance guarantee.

## Persistent channel latency

[Raw results](../artifacts/channel-latency.jsonl) contain 200 samples per payload
size. The producer changes one rectangle and carries padding in `data`; this
measures transport, parsing and submission with large messages, **not complex
node-tree layout or text shaping**.

| Actual JSON bytes | Scheme change → submit receipt p50 | p99 | Decode/validate p99 |
| ---: | ---: | ---: | ---: |
| 4,096 | 0.387 ms | 10.522 ms | 0.018 ms |
| 65,536 | 3.612 ms | 4.627 ms | 0.103 ms |
| 1,046,528 | 50.110 ms | 55.529 ms | 1.067 ms |

The 64 KiB p99 of 4.627 ms is below the 21.67 ms acceptance threshold
(one 60 Hz frame plus 5 ms). The near-1 MiB decode/validate p99 of 1.067 ms is
below its 10 ms threshold. The near-1 MiB end-to-end submit-receipt p99 is 55.529 ms; it does
not meet the smaller-message latency target. This payload reserves space below
the 1 MiB protocol ceiling for message and channel-state overhead.

The timer starts in the dedicated Scheme producer before constructing and
serializing each patch. It ends when that producer receives the matching
`presented` receipt, including the return IPC trip. This conservatively measures
change-to-submit-receipt, not the exact GPU submission instant. Renderer-side timing starts on the I/O worker and ends at
submission reporting. An `applied` acknowledgment alone is not presentation.

Reproduce with the native renderer and producer in separate terminals:

```sh
mkdir -p -m 700 "$XDG_RUNTIME_DIR/hearth"
cargo run --release -- --ui-socket "$XDG_RUNTIME_DIR/hearth/bench.sock" --channel bench
```

```sh
guile --no-auto-compile -L scheme tests/channel_latency.scm \
  "$XDG_RUNTIME_DIR/hearth/bench.sock" bench 200 60
```

The script reports results without asserting a latency threshold, so a slow run
still produces evidence for investigation. It does assert final-state delivery.

## Three-channel soak and memory

Three independent producer connections sent 60 updates per second per channel
for 60 seconds: 3,600 updates each. There were zero missed sender deadlines.
Final inspection matched every channel's sequence and data, and the displayed
channel's final submission receipt matched. Intermediate renderer states may
coalesce; this test does not require every accepted state to become a frame.

[RSS samples](../artifacts/channel-benchmark-memory.json) show 256,232 KiB from
14 seconds through the final sample at 71.02 seconds. The observed warmup peak
was 262,212 KiB. This is a bounded-duration stability observation, not proof of
unlimited-runtime memory behavior.

## Dedicated Scheme behavior and Rust animation

[Native interaction evidence](../artifacts/widget-live-proof.json) records the
counter changing from 0 to 1 after a click, then to 11 after redefining its
activation handler, followed by a toggle from false to true. The same producer
epoch remained connected throughout. The native before/after PNGs were inspected
at 1×: [before](../artifacts/widget-native-before.png),
[after](../artifacts/widget-native-after.png).

With the Scheme process deliberately sleeping for two seconds, the renderer's
frame count advanced from 145 to 193: 48 additional frames. The counter stayed
at 11 while Scheme was blocked and became 21 after it resumed. This verifies
that local Rust transitions do not wait for Scheme; application behavior still
waits for its Scheme handler. See [blocked-process evidence](../artifacts/widget-blocked-scheme.json).

## Weft persistence adapter

[Push/restart evidence](../artifacts/weft-push-proof.json) records a real workspace
mutation reaching the renderer in 346.230 ms total, including the 72.707 ms
mutation command; the remaining interval after that command returned was
273.523 ms. Idle observation produced no updates before or after the mutation.
A renderer restart changed session identity and received a new baseline with the
marker restored. Marker removal also propagated.

The adapter exited cleanly and removed its four watches, one advice, two saved
entries and two bindings; all final registration counts were zero. Both renderer
runs exited successfully and their socket was removed. The separate
[activity-view proof](../artifacts/weft-activity-push-proof.json) also passed
automatic push, idle silence, restart baseline recovery and registration cleanup.

This path uses inotify on persisted state plus trusted watches/notices. The
isolated broker has no native notification hook; these numbers do not establish
performance for a native Weft event stream, nor do the direct 64 KiB channel
numbers include Weft inspection subprocesses.

## Best-effort Weft mirror

[Mirror evidence](../artifacts/mirror-proof.json) verifies observation timestamps,
startup unavailability and recovery. An oversized sequence-2 update produced a
bounded unavailable summary preserving its identity and reason; the subscriber
stayed connected, and the smaller sequence-3 update restored available state.
Renderer EOF and mirror shutdown marked the saved state unavailable while
preserving the last observation timestamp. The publication budget is 120 KiB.

## Automated checks

The recorded final check run passed strict Clippy, 220 Rust tests including GPU
snapshot comparisons, and ten Guile suites: activity export, service export,
UI client, Weft watches, mirror, Bluetooth UI, Notifications UI, Audio UI, Tray UI and PAM UI. Focused checks cover atomic invalid updates,
producer ownership, disconnect/reconnect, more than 64 epochs, optional fields,
partial input, bounded clients/queues, input routing and presentation identity.
The UI client suite includes live handler redefinition/advice and 70 renderer
reconnects. The [retained ADR critique](adr-002-review.md) remains the historical
review, not a claim that its original deficiencies still describe the current API.


## Notifications

Twelve private-bus backend tests cover ownership refusal, replacement, actions,
expiry, bounded persistence, slow clients and process cleanup. Scheme tests
exercise generation-bound controls, retired button keys, paging and privacy.
Four new PNGs cover active/history/DND layouts and retained RGBA images; all
were inspected at 1× and compared exactly by the GPU tests.

The [native smoke evidence](../artifacts/notifications-native-proof.json) records
an actual private-bus notification reaching the native 720×760 Scheme panel and
Weft publisher. Renderer, controller and server exited successfully and removed
their sockets. The test made no notification-name request on the host bus.

## PipeWire

The private daemon fixture supplies synthetic sink/source nodes and default
metadata without hardware or session-manager modules. Tests independently read
back volume, mute and configured defaults; reject stale session/serials; reconnect
after server loss; and stop with partial input or blocked output. State tests
cover perceptual scaling, bounds, capabilities and identity checks.

Four Scheme-generated PNGs cover outputs, inputs, applications and unavailable
state. All were inspected at 1×. Effective and requested defaults are distinct,
and inventories show paging counts. The [native smoke evidence](../artifacts/audio-native-proof.json)
records two private audio nodes reaching the 720×760 panel and Weft publisher,
with clean process and socket teardown and no host audio mutations. Physical
hardware route controls are implemented but were not exercised on the owner's
audio devices during this validation.

## SystemTray

Private-bus tests cover watcher coexistence, registration/removal, owner-pinned
controls, both KDE and freedesktop interfaces, ARGB conversion, menu revision
and content-epoch guards, reconnects and blocked-output shutdown. Icon tests
exercise PNG normalization, SVG alpha, disabled resource references and bounds.

The item, menu and unavailable PNGs are generated by the Scheme controller and
compared exactly. They were inspected at 1×, including overflow counts and
check/radio/separator/disabled menu entries. The [native smoke evidence](../artifacts/tray-native-proof.json)
records an empty private tray reaching the 720×760 panel, a second observer
preserving watcher ownership, Weft publication and clean socket teardown.
Item actions and menus are exercised by separate private-bus mocks; no watcher
ownership was requested on the host bus.


## PAM

Eight private-policy tests compile a synthetic PAM module without touching host
policy. They exercise all conversation styles, authentication/account denial,
PAM_USER changes, stale/duplicate replies, deadlines, crash/cancel, parent death,
backpressure and helper-process cleanup. A permissive private `other` policy
cannot authorize a missing explicitly selected service. Three model tests cover
bounds and identity, and Scheme tests cover masked input, privacy, prompt replay,
publication filtering and SIGTERM/SIGINT cleanup.

Three Scheme-generated PNGs show idle, masked response and account denial.
They were inspected at 1× and pass exact comparisons. The
[native smoke evidence](../artifacts/pam-native-proof.json) records the idle
720×760 panel, sensitive-channel metadata, Weft publication and unavailability
on clean shutdown. No authentication attempt or compositor lock change occurred.
The [security review](pam-security.md) records the checked boundary and fixes;
[PAM usage](pam.md) documents the ASCII-only keyboard and separate session-lock
integration. ADR-011 remains Proposed for owner review.


## Notification desktop integration

Three fake-Hyprland tests cover visible normal workspaces (including negative
named IDs), maximized/hidden/special exclusions, fragmented events, disconnect,
reconnect and suppression that manual controls cannot override. Source loss
suppresses popups and reports unavailability; a valid observation restores the
automatic state. Existing worker shutdown fixtures disable compositor discovery.

Four image-resolution tests cover inline precedence, alpha, local file URIs,
private theme paths, remote-resource refusal and malformed/oversized/FIFO
fallback. A private-bus delivery test verifies modern image hints override local
references, which override deprecated icon_data. Four Scheme-generated notification
PNGs were compared exactly and inspected at 1×, including automatic suppression.

The [native integration evidence](../artifacts/notifications-integration-proof.json)
records a local PNG reaching the 720×760 panel, live read-only Hyprland discovery,
sanitized Weft publication and clean controller/renderer/server teardown. The
notification bus was private; no host notification ownership or compositor
mutation occurred. The implementation review found no blocking issues.
ADR-012 remains Proposed for owner review.
