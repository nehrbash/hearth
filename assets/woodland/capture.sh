#!/usr/bin/env bash
set -euo pipefail
base=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
burl=${BURL_ROOT:-$HOME/src/burl}
out=${HEARTH_CAPTURE_DIR:-$base/../../artifacts/m5-reference}
: "${HYPRLAND_INSTANCE_SIGNATURE:?Set the current compositor instance}"
: "${QT_SCALE_FACTOR:?Set QT_SCALE_FACTOR=1 and capture on a native 1x output}"
[[ "$QT_SCALE_FACTOR" == 1 ]] || { echo "Native 1x capture required" >&2; exit 1; }
size=$1; target=$2; name=$3; shift 3
w=${size%x*}; h=${size#*x}
mkdir -p "$out/probe-root"
for entry in "$burl"/*; do
    target_path="$out/probe-root/$(basename "$entry")"
    [[ -e "$target_path" ]] || ln -s "$entry" "$target_path"
done
cp "$base"/probes/*.qml "$out/probe-root/"
decl="$out/${name}.decl.qml"
cat > "$decl" <<QML
Component.onCompleted: { host.anchors.fill = undefined; host.width=$w; host.height=$h; }
QML
"$burl/scripts/qs-shot.sh" --root "$out/probe-root" --size "$size" --settle 2500 --decl "$decl" --log "$out/$name.log" "$@" "$target" "$out/$name.png"
identify "$out/$name.png"
