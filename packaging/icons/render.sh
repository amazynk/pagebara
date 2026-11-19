#!/bin/sh
# Export the desktop and mobile icons from app.svg with librsvg or Inkscape and Pillow.
set -eu
cd "$(dirname "$0")"
exec python3 -I render.py
