#!/bin/sh
# Double-click to run Better PhotoCraft in the browser: builds the web version if it isn't built
# yet, serves it on http://localhost:8780 and opens it. Close this window to stop the server.
# (Opening dist/web/index.html directly doesn't work: WebAssembly and the service worker need http.)
cd "$(dirname "$0")" || exit 1
PORT=8780
URL="http://localhost:$PORT/"

if [ ! -f dist/web/index.html ]; then
  # rustup's toolchain has the wasm32 target; Homebrew's cargo may come first on PATH without it.
  PATH="$HOME/.cargo/bin:$PATH"
  echo "Building the web version (first run, a few minutes)…"
  (cd apps/photocraft-web && trunk build --release) || { echo "Build failed. Press Return to close."; read -r _; exit 1; }
fi

open_browser() {
  if [ -d "/Applications/Brave Browser.app" ]; then open -a "Brave Browser" "$URL"; else open "$URL"; fi
}

if lsof -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1; then
  echo "Already serving on $URL"
  open_browser
  exit 0
fi

(sleep 1 && open_browser) &
echo "Better PhotoCraft: $URL (close this window to stop)"
exec python3 -m http.server "$PORT" --bind 127.0.0.1 -d dist/web
