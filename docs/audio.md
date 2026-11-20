# PipeWire audio

The native audio worker owns a PipeWire connection independently of the renderer.
Scheme builds the controls; Rust handles native registry, node, device and metadata
callbacks. The frame loop does not wait for audio or Weft.

```sh
target/debug/hearth-audio --observe | guile -L scheme scheme/services.scm
```

For the native 720×760 controls, run the renderer with `--channel audio` and
`--size 720x760`, then:

```sh
guile -L scheme scheme/audio-ui.scm RENDERER_SOCKET target/debug/hearth-audio
```

The Outputs, Inputs and Applications tabs have explicit overflow paging.

Use `--once` for one observation and `--remote SOCKET` for a specific PipeWire
remote. Tests always select a private daemon with synthetic nodes. Starting an
observer does not change volume, mute, defaults or stream routing.

Observations identify the connection session and each node's ID and object
serial. Missing properties remain unknown. Devices, audio outputs, inputs and
application streams are bounded inventories. Effective defaults and configured
defaults are distinct: a policy request does not prove that existing streams
moved to a different device.

Controls require the current session, node ID and serial. Volume requests use
perceptual values from 0 to 1, while `channel_volumes_linear` reports raw SPA
amplitudes. The UI displays the mean of the channels' cube roots and steps by
0.1. Adjustments preserve channel balance and request unmute; mute also has an
independent control. Hardware route properties are handled through the owning
device when required. Unsupported or unready controls remain disabled.

A command result distinguishes rejection, observed application and an unknown
outcome. Timeout or disconnect can leave the outcome unknown; commands are
never automatically replayed. Default selection writes the existing configured
default metadata and leaves stream routing to WirePlumber.

Bluetooth connection followed by selecting the newly appearing audio sink is a
separate cross-service workflow. This worker observes PipeWire state and does
not initiate BlueZ pairing or connection.

Build and test inside `guix shell -m manifest.scm`. The manifest includes
PipeWire development files and Clang for libspa's bindgen build; GCC remains the
C compiler and Rust linker. A plain Cargo command outside that shell can fail
to find libclang.

The private test configuration is `fixtures/pipewire-test.conf`. Launch it with
private `XDG_RUNTIME_DIR` and `PIPEWIRE_RUNTIME_DIR` directories, and use
`--remote hearth-test` in the worker. It loads no udev, PulseAudio, JACK or
D-Bus modules. Default metadata changes are checked independently of effective
routing; the fixture runs without a session manager.

Normal worker mode accepts one JSON object per line on stdin:

```json
{"type":"action","request_id":"1","request":{"session":"from-observation","id":42,"serial":123,"action":{"action":"volume","value":0.5,"unmute":true}}}
```

The other actions are `{"action":"mute","value":true}` and
`{"action":"default"}`. Copy identity from a fresh observation. Results echo
`request_id` with `applied`, `rejected`, or `unknown`; state observations share
the output stream. Only one mutation executes at a time. Pending work has a
bounded deadline; receipts cannot be silently discarded while continuing the
connection. Inventory changes push new observations; unchanged state has a
five-second heartbeat.

Limits are 64 devices, 256 nodes, 64 channels per node, 4096-byte strings,
120 KiB state, and 16 KiB commands. Queue/output limits disconnect slow consumers
rather than allowing unbounded memory. A random connection nonce and object
serials prevent stale commands from targeting a reused node ID. Device route
identity is checked again before dispatch and confirmation.
