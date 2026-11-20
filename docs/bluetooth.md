# Bluetooth service and controller

`hearth-bluetooth` keeps BlueZ connections alive for discovery and pairing.
Startup only inspects state. It never powers an adapter, scans or pairs without
an explicit command. The controller runs in a separate Guile process:

```sh
cargo build --bin hearth --bin hearth-bluetooth
mkdir -p "$XDG_RUNTIME_DIR/hearth"
chmod 700 "$XDG_RUNTIME_DIR/hearth"
target/debug/hearth --listen "$XDG_RUNTIME_DIR/hearth/bluetooth.sock" --channel bluetooth --size 720x760
guile -L scheme scheme/bluetooth-ui.scm "$XDG_RUNTIME_DIR/hearth/bluetooth.sock" target/debug/hearth-bluetooth
```

The native panel supports adapter selection, power, discovery, device paging,
connect/disconnect, trust, remove and pair. Pairing has explicit accept/reject
prompts and on-screen PIN/passkey entry. No default agent is requested and no
confirmation is accepted automatically. Renderer disconnect cancels pending
interaction; reconnect does not replay an action or response.

For read-only Weft publication:

```sh
target/debug/hearth-bluetooth --once | guile -L scheme scheme/services.scm --once
target/debug/hearth-bluetooth --observe | guile -L scheme scheme/services.scm
```

The saved `hearth-bluetooth` envelope contains the unique BlueZ owner, adapters,
devices, optional RSSI/battery, connection/pair/trust/block state and observation
time. Missing optional readings remain unknown. A failed bus is unavailable,
not an empty inventory. Schema validation rejects challenge and credential fields.

## Worker protocol

Normal mode uses newline-delimited JSON on stdin/stdout. State output is the
standard service envelope. Actions and replies are separate from observations:

```json
{"type":"action","request_id":"1","request":{"owner":":1.42","path":"/org/bluez/hci0/dev_00_11_22_33_44_55","action":{"action":"pair"}}}
{"type":"reply","challenge_id":1,"reply":{"kind":"accept"}}
{"type":"cancel"}
```

Action variants are `power`/`discovery`/`trust` with boolean `value`, and `pair`,
`connect`, `disconnect`, `remove`. `result` reports the request ID and outcome.
A `challenge` identifies the action request, unique owner, device, challenge ID,
kind and deadline; optional fields carry display values. Reply variants are
`accept`, `reject`, `pin` with a printable ASCII string of 1–16 characters, and
`passkey` with an integer from 0 through 999999.

Challenges expire after 30 seconds; a pairing attempt is bounded to 120 seconds.
The worker verifies the D-Bus sender and device, rejects duplicate/late replies,
and cancels on EOF, signal or owner replacement. Failure can leave an action's
outcome unknown; never retry it automatically. Discovery lives on the persistent
connection and is released when that connection closes.

Credentials are ephemeral controller/renderer content, absent from semantic
service data. The controller marks its channel `data.sensitive:true`; the Weft
mirror publishes only an unavailable summary for such channels, without nodes
or data. Local renderer inspection still belongs to the same trusted user.

## Verification

Private-bus tests cover inventory, controls, persistent discovery, all seven
Agent1 challenge forms, sender/device binding, stale replies and cancellation.
Guile tests exercise input identity, pagination, code entry, expiry and secret
exclusion. `bluetooth_snapshot` generates scenes from the Scheme controller and
compares device, confirmation and keypad PNGs at 720×760. The native read-only
proof is [recorded here](../artifacts/bluetooth-native-proof.json); it sends no
host action.

Contracts: [BlueZ adapter](https://bluez.readthedocs.io/en/latest/adapter-api/),
[device](https://bluez.readthedocs.io/en/latest/device-api/),
[agent](https://bluez.readthedocs.io/en/latest/agent-api/).
