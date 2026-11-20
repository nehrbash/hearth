DejaVu 2.37 fixtures from the Guix `font-dejavu` package, unmodified.
Upstream: https://dejavu-fonts.github.io/

`LICENSE-DejaVu` covers all three font files. The fixture font database uses a
fixed locale and these files only, so snapshots do not depend on host fonts.
Sans supplies Arabic fallback for Serif; Sans Bold gives headings a real bold
face. Live clients can opt into system fonts through `TextSystem::with_system_fonts`.
