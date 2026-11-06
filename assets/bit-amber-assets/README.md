# bit — Amber assets

Selected direction: 03 / Signal, palette 04 / Amber (#F2B544).
These are clean vector reconstructions of the concept, with a consistent pixel grid,
flat colors, and a redrawn geometric pixel wordmark. They are not crops of the study sheet.

## Contents

- icon-light / icon-dark: standalone flag, SVG and transparent 512px/1024px PNG.
- logo-horizontal-light / dark: flag + bit, SVG and transparent 448px/896px PNG.
- logo-stacked-light / dark: flag above bit, SVG and transparent 352px/704px PNG.
- wordmark-light / dark: text only, SVG and transparent 576px PNG.
- favicons/: optically adjusted SVG, transparent PNG at 16/32/48/64/180/192/512px,
  and multi-resolution ICO, for each theme.
- preview.png: light/dark overview, with opaque backgrounds.
- colors.json: exact palette values.

"light" means for use ON LIGHT backgrounds; "dark" means for use ON DARK backgrounds.
All logo files have transparent backgrounds. SVGs use filled rectangles and require no fonts.
For new sizes, render SVGs; use whole pixel scaling for the crispest pixel edges.
Keep the flag square amber, and keep the pole and lettering neutral gray.

## GitHub README

Copy the horizontal SVGs into assets/, then use:

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/logo-horizontal-dark.svg">
  <img alt="bit" src="assets/logo-horizontal-light.svg" width="224">
</picture>
