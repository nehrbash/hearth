# Burl workspace-bar reference

Source checkout: `/home/nehrbash/src/burl`, commit `f5df3303e25b9445ab5f038f1d33b80d54607f6e`. These PNGs were rendered by the checkout's `scripts/qs-shot.sh`, without changing Burl source or the running shell. The probe maps a temporary window. No compositor workspace changes were dispatched.

`bar-live-1x.png` is the actual composed `BarWrapper` at 360×1440. It is a reference, not an asset to paint as the UI. The focused monitor was DP-3, workspace 2; workspace 4 was active on DP-4. The temporary probe itself is a window in workspace 2. `bar-live-1x.log` records live dimensions and verifies successful Hyprland connection. Earlier `bar-composed.png` is a stale-Hypr, 1.5× diagnostic; do not use it as the reference.

## Native geometry

Runtime tokens: strip width 60, padding 10, card width 40, card height 60, row gap 12; seven normal workspaces. `showWindows=true`, `occupiedBg=false`. The current 1440-high composition places workspace origins at x=12 and y=167,239,324,396,481,553,625. Heights are 60,73,60,73,60,60,60. Position includes the actual logo, special-workspace and lower-control layout; it is not a fixed universal top margin.

Painter order within each workspace:

1. BranchCradle at (-8,-3), 56×72; selected only for the active workspace.
2. TarotCard 40×60, horizontal offset -2. Active scale settles to 1; inactive scale 0.94. The card includes BranchMount extending 5px on all sides, badge, and other-monitor marker.
3. Focused-monitor eye at centered x, y=-7, 30×19; shown only when both active and focused.
4. Occupied app socket 22×22 centered horizontally, y=60-22*.45=50.1. Occupancy adds ceil(60+22*.55)=73 total row height. App icon has a 16% socket inset. The exported socket has no baked app icon.

Badge: bottom/right inset 4, height 12, width max(11,text width+4). Qt inherited font, 10px bold; this host resolves sans to DejaVu Sans. Active fill #dd211728 and text #fff0be; idle fill #dd15121c and text #e8c87d. Global configured font default is GoogleSansFlex, but the TarotCard badge does not bind Tokens.font.

Other-monitor marker: right inset 1, width 3, height 24, vertically centered, dark rim; inner accent width 1 and height 22. Accent is the monitor's primary/secondary/tertiary palette slot. Active status, focus status, occupancy and other-monitor status are independent.

## Assets and material

- Original deck: `assets/images/tarot/deck.png`, 1619×971, SHA256 `1c7cfa6139575c5075a3f5bd69124a7d929ed1a71bd5239f9e92b8d9ac33f062`. Ten cards in five columns/two rows. QML displays the whole atlas at cardWidth*5 by cardHeight*2, offsets by column*cardWidth and row*cardHeight, and clips to the card. Preserve the atlas unchanged.
- Original bark: `assets/images/tree/rendered/sidebar-grown.png`, 724×2172, SHA256 `4f1a53aa5a8b302ce9a5bc671b8e8337ad2daee11af1feb86c3c2dbe8de05643`. Preserve it unchanged. SidebarTrunk stretches source (0,0,724,1800) into the top 91% and (0,1800,724,372) into the bottom 9%; brightness -0.16, saturation -0.35.
- `trunk-60x1440.png`: reusable texture-only foreground strip, including BarWrapper's horizontal darkening gradient. Probe geometry follows the requested trunkWidth=358.4 and trunkX=30-358.4*.265; four-pixel bark overhang. The composed wrapper's own content offset can add 2px to the trunk origin.
- `BranchCradle-{idle,selected}.png`: 56×72, directly rendered Canvas ornaments.
- `BranchMount-{idle,selected}.png`: 50×70, directly rendered Canvas ornaments.
- `focused-eye.png`: 30×19, verbatim Workspace Canvas drawing isolated into `probe-root/Eye.qml`.
- `socket.png` / `BarkSocket.png`: identical 22×22 bark bezel, no app icon.
- `special-seal-{idle,active}.png`: 40×40, the BarkSocket, Shape arcs and seven radial marks copied verbatim from SpecialWsDelegate into `probe-root/SpecialSeal.qml`; no letter or app icon.

Woodland bark colours are #6b5040/#4a3425/#2f2117; parchment is #f3e7c9/#ead9b5/#d9c391. BranchCradle uses bark and parchmentEdge. BranchMount mixes parchmentEdge toward current m3primary by 0.35; selected alpha .9 versus .48. Therefore ornaments containing palette colours should be regenerated if the palette changes.

## Reproduction

`capture.sh` creates an artifact probe root linked to the source checkout and copies the small standalone probes from `probes/`. `capture.sh SIZE TARGET NAME [qs-shot flags...]` writes NAME.png, NAME.log and NAME.decl.qml here. For example:

```sh
bash capture.sh 56x72 components/widgets/BranchCradle.qml BranchCradle-idle --set 'selected: false'
bash capture.sh 56x72 components/widgets/BranchCradle.qml BranchCradle-selected --set 'selected: true'
bash capture.sh 50x70 components/widgets/BranchMount.qml BranchMount-idle --set 'selected: false'
bash capture.sh 50x70 components/widgets/BranchMount.qml BranchMount-selected --set 'selected: true'
bash capture.sh 22x22 components/widgets/BarkSocket.qml BarkSocket
cp BarkSocket.png socket.png
bash capture.sh 40x40 SpecialSeal.qml special-seal-idle
bash capture.sh 40x40 SpecialSeal.qml special-seal-active --set 'isActive: true'
bash capture.sh 30x19 Eye.qml focused-eye
bash capture.sh 60x1440 Trunk.qml trunk-60x1440
bash capture.sh 360x1440 Reference.qml bar-live-1x
```

Set child-only HYPRLAND_INSTANCE_SIGNATURE from `hyprctl instances -j` and
QT_SCALE_FACTOR=1. Capture on a native 1× output. The helper requires both values.
A reciprocal scale factor can produce correctly sized PNGs with incorrectly
rasterized Canvas details; it is not a substitute for a native 1× capture.
The focused eye was verified against the actual Workspace monitorFocusSigil
child; both native exports have SHA256
`9eb2c36b5636e7a398402d807f1b58f6200fa113ffa7ceafaa59f760a22c5ded`.
Explicit host dimensions avoid floating-window size rounding. Inspect fine
ornaments at 1× as well as checking PNG dimensions.

`workspace-active.png`, `workspace-empty.png` and `workspace-other-monitor.png` are 80×110 isolated component references with 20px margins. All three set anchors.margins=20, groupOffset=0, activeWsId=2 and focusedMonitor=true. Active sets index=1, occupied=({2:true}), otherMonitorWs=({}); empty sets index=0, occupied=({}), otherMonitorWs=({}); other-monitor sets index=3, occupied=({4:true}), otherMonitorWs=({4:"#e8c87d"}). Pass each as a qs-shot --set argument. Treat these as comparison images, not reusable state sprites. Live application icons remain host-dependent.
