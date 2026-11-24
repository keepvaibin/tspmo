# UI design system

Derived from the user's references (`~/Downloads/ui1-10`): G-Helper's main window (ui1-6, ui7,
ui10) and Lenovo Legion Toolkit (ui8, ui9). The popup follows G-Helper: one flat, dense column.

## Layout

- One page, 468 x 640 logical px, 14 px padding, sections 12 px apart, 6 px inside a section.
- Order: Mode, GPU Mode, Laptop Screen, Flicker-free Dimming / Visual Mode, Slash Lighting,
  Laptop Keyboard, Battery Charge Limit, System, footer (`Fans + Power`, `Quit`).
- A section exists only if the machine has the hardware. `Fans + Power` is a second page
  (back arrow, mode, fan tabs, curve editor, Auto/Full/Reset, power limits).
- Section header: 16 px icon, bold 14 px title that carries the state ("Mode: Balanced"), dim
  12 px live status on the right (fan RPM, dGPU state, battery %, W).

## Controls

- Option row: equal flat buttons, 56 px (icon above label) for Mode and GPU, 34 px (label only)
  for screen, colour and charge-limit rows. The chosen one has a 2 px coloured outline: the mode
  accent for Mode, blue for the rest, neutral for the refresh row. No fill change.
- Slider: 5 px track, 16 px round knob, accent fill when custom.
- Checkbox: 16 px square, blue when on. Toggles only where a firmware switch is a switch.
- Steppers (`<` value `>`) replace dropdowns for effect and mode choices.

## Tokens

| | dark | light |
|---|---|---|
| page | `#1c1c1c` | `#f3f3f3` |
| button | `#2d2d2d` | `#e6e6e6` |
| hover | `#3d3d3d` | `#cfcfcf` |
| line | `#3a3a3a` | `#c4c4c4` |
| text / dim | `#f2f2f2` / `#a0a0a0` | `#1a1a1a` / `#606060` |

Mode accents: Eco+ `#8bd450`, Silent `#2fc77a`, Balanced `#3da5f5`, Turbo `#f0503c`. Blue
`#3da5f5`, danger `#ff6b6b`.

- Radius: 4 px everywhere (3 px checkbox, pill only for toggles). Square window: no transparent
  corners, hence no corner artifacts.
- Type: system sans; 14 px bold headers, 12.5-13 px labels, 12 px status, 10.5 px captions.
- Motion: 120-200 ms ease for outline, background and accent colour; 90 ms scroll; nothing
  animates while idle.
- Wording: labels only, no sentences; errors are one short line at the bottom.
