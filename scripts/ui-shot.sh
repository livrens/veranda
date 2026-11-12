#!/usr/bin/env bash
# Headless UI check (Linux): run the debug desktop app on a virtual X display with a throw-away
# Veranda root, capture the window, quit. Needs: Xvfb, ffmpeg, dbus-run-session.
#
#   scripts/ui-shot.sh <tab> [seconds] [autotest]      e.g. scripts/ui-shot.sh hosts 12
#   SIZE=600x480 THEME=light scripts/ui-shot.sh php 14   # window size / theme
#   SCREEN=1920x1080 SIZE=1900x1000 scripts/ui-shot.sh node:versions 16   # wide layouts
#   scripts/ui-shot.sh terminal 26 remote:pwdlg         # scripted check (see remote.js autotest)
#
# Output: target/ui-shots/<tab>[-<autotest>]-<theme>[-<size>].png. The scratch root is target/ui-root,
# never the user's real Veranda root. GDK_BACKEND=x11 and an unset WAYLAND_DISPLAY keep the
# window on the virtual display instead of the user's desktop.
set -u
cd "$(dirname "$0")/.." || exit 1
tab="${1:-dashboard}"; wait="${2:-12}"; auto="${3:-}"
root="$PWD/target/ui-root"; out="$PWD/target/ui-shots"; mkdir -p "$root" "$out"
log="$out/app-run.log"
screen="${SCREEN:-1400x900}"
pgrep -x Xvfb >/dev/null || (Xvfb :99 -screen 0 "${screen}x24" >/dev/null 2>&1 &)
sleep 0.5
env -u WAYLAND_DISPLAY DISPLAY=:99 GDK_BACKEND=x11 timeout 90 dbus-run-session -- \
  env VERANDA_HOME="$root" VERANDA_UI_TAB="$tab" VERANDA_UI_THEME="${THEME:-dark}" \
  ${SIZE:+VERANDA_UI_SIZE=$SIZE} ${auto:+VERANDA_UI_AUTOTEST=$auto} \
  WEBKIT_DISABLE_DMABUF_RENDERER=1 WEBKIT_DISABLE_COMPOSITING_MODE=1 LIBGL_ALWAYS_SOFTWARE=1 \
  ./target/debug/veranda-desktop > "$log" 2>&1 &
sleep "$wait"
shot="$out/${tab//:/_}${auto:+-${auto//:/_}}-${THEME:-dark}${SIZE:+-$SIZE}.png"
DISPLAY=:99 ffmpeg -loglevel error -y -f x11grab -draw_mouse 0 -video_size "$screen" -i :99 -frames:v 1 "$shot"
pkill -f "target/debug/veranda-desktop" || true; sleep 1
grep -E "ui-perf|panick|UI error" "$log" | sed 's/^.*ui-perf/ui-perf/' | head -5
echo "shot: $shot"
exit 0
