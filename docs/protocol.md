# Weft bridge

Hearth accepts one newline-terminated JSON snapshot per Unix connection. The
socket's parent must be owned by the user with no group/other permissions;
Hearth creates the socket with mode 0600 and refuses an existing path. Rendering
does not evaluate Scheme or execute actions.

```json
{"version":1,"kind":"snapshot","workspace":"isolated","generation":"opaque","revision":1,"zones":[{"name":"status","type":"text","display":"Ready","rows":[]}]}
```

`version` is 1 and `kind` is `snapshot`. `workspace` and `generation` are nonempty
strings up to 256 bytes. `revision` is an unsigned 64-bit integer, increasing
within a generation. A new generation supplies a complete baseline. Retired
generations cannot replace a newer baseline.

There are at most 64 uniquely named zones. Text and metric zones use `display`
and empty `rows`; table and wiring zones use `rows` and empty `display`. Wiring
rows have exactly two cells. Strings are bounded to 16 KiB, rows to 1024 and
columns to 64. The complete message is bounded to 1 MiB. Unknown fields, unknown
types and invalid data are rejected without replacing the visible snapshot.

A background thread validates data and keeps only the latest accepted update.
Its notification descriptor wakes the Wayland loop; there is no Scheme call or
socket read in a frame callback. Invalid messages are reported on stderr.
Hearth retains the last valid view; a displayed revision does not assert that
the source is still connected. Restarting Hearth requires a fresh full snapshot.

## Scheme

Load `scheme/hearth.scm` into a trusted Guile process with access to the socket:

```scheme
(use-modules (hearth))
(hearth-send! socket-path
  (hearth-snapshot
    (list (hearth-text 'status "Ready")
          (hearth-metric 'checks 8))
    "example-process" 1))
```

Inside Weft, `(zone-list)` can supply the first argument to `hearth-snapshot`.
The isolated evaluator cannot open the host UI socket. The standalone producer
reads the owner broker's data and checks the workspace generation around each
export:

```sh
guile -L scheme scheme/live.scm "$XDG_RUNTIME_DIR/hearth/ui.sock"
```

It polls outside evaluation once per second and sends only changed data or a
baseline after socket replacement. `--once` sends a single snapshot and exits;
errors produce a nonzero exit. Revision values are seeded from wall-clock
microseconds; `--revision BASE` permits an explicit higher base after a clock
rollback. Receiving bytes is not an acknowledgement that a snapshot was applied.

Owner effects are excluded from this first view. Approval controls require a
separate broker route binding the displayed exact proposal to the owner action.

The receiver remembers 64 retired workspace/generation identities. It rejects
replay of any retired identity. Once that history is full it refuses a new
generation with an explicit restart error; it does not evict identities and
silently permit old data to return. New revisions of the current generation
continue to work.

The optional `activity` field carries a separate trusted generation, sessions,
watches, subscriber-scoped notices, terminal leases, effects, warnings, and the
owner broker's `effects_generation`. Hosts and expiries may be null; null means
unavailable, not an empty or expired resource. `scheme/activity.scm` normalizes
these APIs. Use `scheme/live.scm --activity` to include them in live snapshots.

## Scene data

A snapshot may include `scene`, an array of nodes selected with `--view scene`.
The `(scene)` Guile module builds Rect, Text, Group and Clip data. Rust decodes
these values without evaluating Scheme or source code. Scene updates share the
snapshot generation/revision rules and are validated before publication.

Nodes use a `type` tag (`rect`, `text`, `group`, `clip`). Bounds are `[x,y,w,h]`;
groups carry `offset` and `children`. Rect colors are linear float RGBA; text
colors are byte RGBA. Text supports `font_size`, `family`, `weight`, `overflow`
(`clip`, `wrap`, `ellipsis`) and optional clipping bounds. Limits are 4096 nodes,
64 nesting levels, 4096 bytes per text label, 256 bytes per family name, and
512-pixel font size, within the existing 1 MiB message limit. Unknown fields and
invalid geometry reject the complete update. An empty scene clears its contents.

Rust signals hold the accepted nodes. A coalesced reactive flush updates scene
damage before rendering; equal scene content stays idle even with a newer
snapshot revision. Scheme runs only in the producer process. Rust-only Shader
and Layer APIs remain available to native widgets; arbitrary WGSL is not part of
the socket protocol.
