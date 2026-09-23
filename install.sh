#!/bin/sh
# Builds a release binary and installs it for the current user.
set -e
cd "$(dirname "$0")"
pkg-config --exists vte-2.91-gtk4 || { echo "vte4 is missing: sudo pacman -S vte4"; exit 1; }
cargo build --release
install -Dm755 target/release/codebench "$HOME/.local/bin/codebench"
install -Dm644 codebench.desktop "$HOME/.local/share/applications/codebench.desktop"
echo "installed: codebench"

# The Omarchy bar widget: link it in once; add it to the bar yourself with
#   omarchy bar put io.github.fluxcapctr.codebench
plugins="$HOME/.config/omarchy/plugins"
if [ -d "$plugins" ] && [ ! -e "$plugins/io.github.fluxcapctr.codebench" ]; then
  ln -s "$PWD/plugin" "$plugins/io.github.fluxcapctr.codebench"
  echo "linked the bar widget. add it with: omarchy bar put io.github.fluxcapctr.codebench"
fi

# Starter workflows, only if you have none yet.
wf="${XDG_CONFIG_HOME:-$HOME/.config}/codebench/workflows"
if [ ! -d "$wf" ]; then
  mkdir -p "$wf" && cp workflows/*.md "$wf"/
  echo "added starter workflows to $wf"
fi
