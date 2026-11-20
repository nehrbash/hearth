# Renderer protocol

`--listen SOCKET` and `--ui-socket SOCKET` select the persistent, bidirectional
v2 protocol. `--channel NAME` selects the displayed channel; the default is
`widget`. Other channels remain independently available for inspection and
subscription. `--legacy-listen` selects the separate v1 receiver.

Both sockets require a parent directory owned by the user with mode 0700. The
socket is created with mode 0600; existing paths are never overwritten. Messages
are UTF-8 JSON objects terminated by a newline. A worker handles socket I/O and
validates complete candidate states before publication. Rust handles input and
animation; the renderer never evaluates Scheme or executes received source.

## Handshake and ordering

A connection begins with:

```json
{"type":"hello","major":2,"capabilities":["channels","patch","inspect","input"],"required_capabilities":["patch"]}
```

The response contains `type:"hello"`, `major:2`, a renderer-lifetime `session_id`,
and capabilities `channels`, `patch`, `inspect`, `input`, `presented`, `subscribe`.
An unsupported major or an unsupported entry in optional `required_capabilities`
receives a rejected hello and the connection closes. The `capabilities` list is
an optional advertisement; unknown entries there do not reject the handshake.
Unknown optional fields are ignored within major 2, including node fields.
Unknown message types or node kinds are rejected.

Each channel has unsigned 64-bit `epoch` and `seq` watermarks. Within an epoch,
sequence numbers must strictly increase. A higher epoch requires a baseline and
may reset the sequence. It explicitly transfers ownership to the sending
connection. A different connection cannot update an actively owned channel in
the same epoch. After disconnect, a new baseline may resume that epoch with a
higher sequence. Patches require the current owning connection.

Watermarks survive producer disconnects for the renderer's lifetime, without a
retired-generation list or 64-restart limit. Renderer restart changes
`session_id`; producers must reconnect and send full baselines. The Guile client
uses a higher epoch on every reconnect and retains only acknowledged state for
its reconnect baseline.

## Baselines and keyed patches

```json
{"type":"baseline","channel":"widget","epoch":1,"seq":1,"nodes":[{"key":"button","kind":"rect","bounds":[20,20,160,48],"input":true,"color":[0.1,0.2,0.3,1]}],"data":{"enabled":false}}
{"type":"patch","channel":"widget","epoch":1,"seq":2,"upsert":[{"key":"button","kind":"rect","bounds":[20,20,160,48],"input":true,"color":[0.1,0.6,0.3,1]}],"remove":[],"data":{"enabled":true}}
```

`nodes` and `upsert` are arrays of complete node objects. Upserts replace existing
keys in place or append new keys. `remove` is an array of keys. Duplicate upserts,
duplicate removals, and keys present in both arrays are rejected. Omitted patch
`data` preserves existing data; a supplied value, including null, replaces it.
Baseline data is optional. Data does not execute code.

Validation covers the resulting whole channel, including hierarchy and resource
limits. A rejected transaction changes neither state nor watermarks. Other
channels are unaffected. Every transaction receives:

```json
{"type":"ack","status":"applied","channel":"widget","epoch":1,"seq":2,"reason":null}
```

Rejection uses `status:"rejected"` and a diagnostic `reason`. **Applied means
validated and committed to the channel store; it does not mean displayed.**
Renderer updates coalesce to the latest accepted state per channel.

## Interactive nodes

Nodes form a flat keyed hierarchy. Parents must exist and be groups; cycles are
invalid. Siblings paint in ascending `order`, preserving array order for ties.

| Field | Meaning and default |
| --- | --- |
| `key`, `kind` | Required identity and `rect`, `text`, or `group` |
| `parent` | Parent key, default null |
| `order` | Signed 32-bit ordering value, default 0 |
| `bounds` | Local `[x,y,width,height]`, default `[0,0,0,0]` |
| `offset` | Translation `[x,y]`, default `[0,0]` |
| `color` | Linear float RGBA, default `[1,1,1,1]` |
| `text`, `font_size` | Text content and pixel size, defaults `""` and 16 |
| `input` | Enable hit testing, default false |
| `state` | Producer-selected state name, default `""` |
| `states` | Name-to-style map; styles may override `color` and `offset` |
| `transition_ms` | Default transition duration, 150 ms |
| `transitions` | Optional per-property durations: `color` and `offset` |

Rust applies base properties, then the selected state, then local `hover` and
`pressed` overrides. Transitions use Rust's frame clock and continue while the
Scheme process is blocked. Zero duration applies immediately. Input follows the
animated hierarchy and painter order. Arbitrary WGSL, Image, Layer and Clip nodes
are native APIs, not v2 interactive node kinds.

## Input and presentation

Only the owning producer receives node-key events:

```json
{"type":"input","channel":"widget","epoch":1,"seq":2,"key":"button","event":"activate","x":30,"y":35}
```

Events are `hover_enter`, `hover_leave`, `press`, `release`, `activate`, and
`cancel`. Coordinates are surface coordinates. Input must match the channel's
current epoch and sequence; stale or disconnected input is rejected. Disconnect
revokes routing and publishes `connected:false` while retaining the last state.
Reconnection does not replay queued input.

After rendering a selected channel, Hearth can send:

```json
{"type":"presented","channel":"widget","epoch":1,"seq":2,"frame_ms":0.4,"validation_us":80,"change_to_presented_ms":1.2}
```

This records **GPU submission, not physical scanout**. `validation_us` measures
worker decode/validation work. `change_to_presented_ms` runs from worker receipt
to the submission report; `frame_ms` is the renderer's frame-work duration.
A receipt can identify an older sequence that was actually rendered while a
newer state was accepted. It remains scoped to the same epoch and producer
connection. Retained but undisplayed channels receive no presentation receipt.

## Inspection and subscriptions

```json
{"type":"inspect","channel":"widget","request_id":"read-1"}
{"type":"subscribe","channel":"widget"}
```

Inspection returns `type:"inspect"`, the echoed `request_id`, `channel`, `epoch`,
`seq`, `nodes`, `data`, and `connected`. Request IDs may be null, numbers, or
strings up to 128 bytes. Unknown channels produce a rejected acknowledgment.

A subscription immediately returns the latest full state as
`type:"channel_update"` with those channel-state fields, excluding `request_id`.
For an unknown channel it returns `type:"subscribed",channel`; future creation
then produces an update. Pending subscription updates coalesce per channel;
partially transmitted messages finish intact. Subscribers do not gain ownership.

## Bounds and backpressure

- At most 16 clients and 32 retained channels; channel watermarks are not evicted.
- Channel names are nonempty, at most 128 bytes, and contain no control characters.
- Each JSON message is at most 1 MiB, excluding its newline. Encoded channel
  `nodes` plus `data` must fit within 1 MiB minus 1024 bytes; all channels together
  are limited to 4 MiB.
- At most 4096 nodes, 64 parent levels, 128-byte keys, 4096-byte text, and 32 states
  per node. State names are at most 128 bytes. Geometry is finite with magnitude
  at most 1,000,000; sizes are nonnegative. Font size is positive and at most 512.
  Colors lie in 0..1; transition durations lie in 0..10000 ms.
- Each client has at most 32 subscriptions and 256 queued outbound messages,
  bounded to 2 MiB. The renderer-to-worker queue holds at most 256 messages.
  Queue failures are reported; input is not silently treated as delivered.

## Weft and Scheme processes

`scheme/widget.scm` owns widget behavior in a dedicated Guile process. Its optional
private control socket evaluates one expression in that process for inspection,
redefinition and advice. Scheme source never travels over the renderer socket.

`scheme/live.scm SOCKET [--activity] [--once]` publishes the `weft` channel with
empty `nodes` and a typed workspace snapshot in `data`. Use `--channel weft` and
`--view zones`, `activity`, or read-only `owner`. Snapshot data includes typed
zones and optional sessions, watches, notices, terminal leases and owner proposals.
Missing information remains explicit.

The adapter waits on inotify changes to persisted workspace state and renderer
socket identity, including broker socket replacement on restart. It consults
trusted Weft watches/notices and refreshes baselines
after notice gaps or generation changes. The isolated broker lacks native
notification hooks, so its persisted-state wakeup is translated into a trusted
notice. This is an adapter over persistence, not a native Weft socket stream.
The waiting loop has a 30-second timeout; persisted-state fingerprints are
compared when filesystem notifications arrive.

`scheme/mirror.scm SOCKET CHANNEL` subscribes and stores the latest channel update
as `hearth-mirror-CHANNEL` in isolated Weft, with `observed_at_ms`, `status`
(`available` or `unavailable`) and `error` (null or a reason). This mirror is
best-effort; broker availability cannot block rendering or widget input. It
reconnects and resubscribes after failure. Startup waiting is unavailable with
no observation timestamp. Renderer EOF or mirror shutdown marks the saved state
unavailable while preserving its last observation timestamp.

Publications exceeding the 120 KiB command budget become a small unavailable
summary preserving channel, epoch, sequence, timestamp and reason, without
`nodes` or `data`. The subscription stays active; the next bounded update can
restore available state.

Channels marked `data.sensitive:true` are excluded from Weft mirroring. The
mirror publishes an unavailable summary containing identity and reason, without
node text or semantic data. Pairing controllers use this for ephemeral prompts.

## Legacy v1

`--legacy-listen SOCKET` accepts one newline-terminated snapshot per connection:

```json
{"version":1,"kind":"snapshot","workspace":"isolated","generation":"producer","revision":1,"zones":[]}
```

It has no handshake, input routing, acknowledgments, or subscriptions. Revision
must increase within a generation. It retains 64 retired workspace/generation
identities and requires receiver restart when that history fills. Unknown fields
are rejected. `scheme/hearth.scm` and `scheme/scene-demo.scm` target this route;
`scheme/live.scm` targets v2 instead.

Legacy snapshots support typed zones, optional activity, and nested scene data
with `type` tags `rect`, `text`, `group`, `clip`. The message limit is 1 MiB.
Zone limits are 64 names, 16 KiB strings, 1024 rows and 64 columns; wiring rows
have two cells. Scene limits are 4096 nodes and 64 levels. Rejection preserves the
last valid snapshot. `--owner-controls` is supported only through this legacy
route and still requires explicit pointer decisions through the owner broker.
