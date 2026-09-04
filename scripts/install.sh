#!/usr/bin/env bash
set -euo pipefail

cargo install --path "$(dirname "$0")/.."

config_dir="${XDG_CONFIG_HOME:-$HOME/.config}/delegate"
if [ ! -f "$config_dir/config.yml" ]; then
    delegate config init
fi

delegate --version
delegate config check
