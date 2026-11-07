#!/usr/bin/env python3
"""Build the LocalSend-inspired vector artwork for Apple's Icon Composer.

No upstream raster is traced or resized. Keep the compact runtime mark in
src/ui/icons.rs aligned with the four orbit segments and central node here.
Run this script, then scripts/render-icon.sh, to regenerate the app artwork.
"""
import json
import math
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ICON = ROOT / 'design/AppIcon.icon'
ART = ICON / 'Assets'
ART.mkdir(parents=True, exist_ok=True)


def point(radius, angle):
    angle = math.radians(angle)
    return f'{512 + radius * math.cos(angle):.4f},{512 + radius * math.sin(angle):.4f}'


def arc(start, end):
    # Filled outline with round end caps, rather than an SVG stroke. Scale
    # matches the runtime's 20 pt geometry: radius 7, stroke width 1.6.
    radius, half_width = 358.4, 40.96
    outer, inner = radius + half_width, radius - half_width
    return (f'M {point(outer, start)} A {outer},{outer} 0 0 1 {point(outer, end)} '
            f'A {half_width},{half_width} 0 0 1 {point(inner, end)} '
            f'A {inner},{inner} 0 0 0 {point(inner, start)} '
            f'A {half_width},{half_width} 0 0 1 {point(outer, start)} Z')


def svg(body):
    return ('<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" '
            f'viewBox="0 0 1024 1024">{body}</svg>\n')


orbits = ''.join(f'<path d="{arc(14 + offset, 76 + offset)}"/>'
                 for offset in (0, 90, 180, 270))
node = '<circle cx="512" cy="512" r="163.84"/>'
(ART / '01-orbit.svg').write_text(svg(f'<g fill="#D7FFF6">{orbits}</g>'))
(ART / '02-node.svg').write_text(svg(f'<g fill="#F7FFFC">{node}</g>'))
(ROOT / 'design/mark.svg').write_text(svg(f'<g fill="currentColor">{orbits}{node}</g>'))


def gradient(top, bottom):
    return {'linear-gradient': [top, bottom],
            'orientation': {'start': {'x': 0.5, 'y': 0}, 'stop': {'x': 0.5, 'y': 1}}}


document = {
    'groups': [
        {'layers': [{'image-name': '01-orbit.svg', 'name': '01 · Nearby connections', 'glass': True}],
         'shadow': {'kind': 'neutral', 'opacity': 0.3},
         'translucency': {'enabled': False, 'value': 0.5}},
        {'layers': [{'image-name': '02-node.svg', 'name': '02 · Local node', 'glass': True}],
         'shadow': {'kind': 'neutral', 'opacity': 0.35},
         'translucency': {'enabled': False, 'value': 0.5}},
    ],
    'supported-platforms': {'squares': 'shared'},
    'fill-specializations': [
        {'value': gradient('srgb:0.07843,0.75686,0.68627,1.00000',
                           'srgb:0.00000,0.43922,0.47059,1.00000')},
        {'appearance': 'dark',
         'value': gradient('srgb:0.03137,0.33725,0.32549,1.00000',
                           'srgb:0.01569,0.14118,0.15686,1.00000')},
    ],
}
(ICON / 'icon.json').write_text(json.dumps(document, ensure_ascii=False, indent=2) + '\n')
print('Generated design/AppIcon.icon and design/mark.svg')
