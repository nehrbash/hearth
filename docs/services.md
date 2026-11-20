# Service state

Hearth service workers inspect the host outside the frame loop. `hearth-service`
emits bounded JSON lines; the Scheme adapter publishes data into the isolated
Weft workspace. Rendering never dispatches compositor actions.

```sh
cargo build --bins
target/debug/hearth-service hyprland --once | guile -L scheme scheme/services.scm --once
target/debug/hearth-service hyprland | guile -L scheme scheme/services.scm
```

Run inside `guix shell -m manifest.scm`. The installed `weft` CLI is required.
Continuous Hyprland observations follow workspace events and refresh every five
seconds, rediscovering the compositor for the current Wayland display. Fresh
heartbeats retry publication after a broker outage or restart. This command does
not install or manage a desktop service.

Each version-1 envelope contains `service`, `status`, `observed_at_ms`, `state`
and `error`. Available state contains the compositor instance, monitors and
workspaces. Unavailable state is null and has a diagnostic; it never represents
an empty desktop. Observation times are Unix milliseconds, not a liveness lease.
Consumers must check age if the publisher disappears unexpectedly.

The adapter validates data, quotes it as a Scheme value and writes
`(saved-ref 'hearth-hyprland)`, with symbol keys and lists. A table zone of the
same name presents a compact preview. The saved value retains the full inventory.
Ordinary stream EOF publishes an unavailable observation; `--once` leaves the
single timestamped observation. Killing the adapter prevents that final update.
No service data is evaluated as code. Input is bounded to 1 MiB; publication is
bounded to 120 KiB after Scheme serialization because the Weft CLI accepts its
expression as one OS argument. Oversize publication fails explicitly. Each Weft
command has a ten-second deadline and a one-second forced-stop grace period.

```sh
guile -L scheme tests/services_export.scm
cargo test --test service_stream
```

## UPower

```sh
target/debug/hearth-service upower --once | guile -L scheme scheme/services.scm --once
target/debug/hearth-service upower | guile -L scheme scheme/services.scm
```

UPower is sampled every five seconds through zbus. Each observation runs in a
probe process with a 2.5-second deadline, so a stalled bus connection cannot
trap the publisher. D-Bus method calls also have a 400-millisecond deadline.
Calls target the daemon’s unique bus owner; querying does not activate an absent
daemon. The service publishes `hearth-upower` as saved data and a table zone.

An available daemon with no display battery reports `presence: absent`. A
present display battery carries its kind, charging state, percentage or coarse
charge level, and remaining times. Zero remaining time means unknown; it does
not imply full or empty. Raw enum values preserve unknown future states. A
missing daemon or failed probe reports unavailable with no retained battery data.

The [UPower device contract](https://upower.freedesktop.org/docs/Device.html)
defines percentage as 0–100 and distinguishes coarse levels. The
[root interface](https://upower.freedesktop.org/docs/UPower.html) supplies
`OnBattery` separately from charging state. Power-profile controls and peripheral
battery enumeration are separate extensions.
