#!/usr/bin/env bash
# Scaffold a new ezbar Go/TinyGo plugin (RFC 0006): one `Plugin` impl + one
# `ezbar.Register` call. Compiles to a wasm32-wasip2 component with TinyGo.
#   ./new-plugin.sh my-widget
# Then build (see the printed command, or the generated README).
set -euo pipefail
name="${1:?usage: new-plugin.sh <name>}"
root="$(cd "$(dirname "$0")" && pwd)"
dir="$root/examples/$name"
[ -e "$dir" ] && { echo "error: $dir already exists" >&2; exit 1; }
# CamelCase the type name from a kebab/snake name
ty="$(echo "$name" | sed -E 's/[-_]+/ /g' | awk '{for(i=1;i<=NF;i++)$i=toupper(substr($i,1,1)) substr($i,2)}1' | tr -d ' ')"
mkdir -p "$dir"

cat > "$dir/main.go" <<EOF
// ezbar Go plugin: $name. Write your logic; \`ezbar.Register\` + empty \`main\`
// are the only glue (the SDK owns wit-bindgen and the component bindings).
package main

import "github.com/birdayz/ezbar/go/ezbar"

type $ty struct {
	ezbar.Base // no-op defaults for Load/Popup/SaveState/Restore
	label      string
}

// Update can use gated host services on ctx (e.g. ctx.HTTPGet). Return true when
// the chip changed and should repaint. Drive polling off EvTimer.
func (p *$ty) Update(ctx ezbar.Ctx, ev ezbar.Event) bool {
	if ev.Kind == ezbar.EvTimer {
		p.label = "hello"
		ctx.SetTimeout(10_000) // next tick in 10s
		return true
	}
	return false
}

// View is pure + synchronous: build the chip from the widget DSL.
func (p *$ty) View() ezbar.Render {
	return ezbar.Row(
		ezbar.IconDot.View(14, ezbar.Accent),
		ezbar.Text(p.label),
	).Spacing(5)
}

func init() { ezbar.Register(&$ty{}) }
func main()  {}
EOF

# build.sh — fronts a Go 1.24 SDK if the system Go is too new for TinyGo, runs
# gofmt/vet, and builds the component. Fully self-contained (quoted heredoc):
cat > "$dir/build.sh" <<'EOF'
#!/usr/bin/env bash
# Build this ezbar Go plugin to a wasm32-wasip2 component.
set -euo pipefail
cd "$(dirname "$0")"
name="$(basename "$(pwd)")"
# TinyGo 0.37 caps at Go 1.24; if the system go is newer, front a local 1.24 SDK.
if go version | grep -qE 'go1\.(2[5-9]|[3-9][0-9])'; then
  sdk="$(ls -d "$HOME"/sdk/go1.24* 2>/dev/null | sort -V | tail -1 || true)"
  if [ -z "$sdk" ]; then
    echo "TinyGo needs a Go <=1.24 SDK (system go is too new). Install one:" >&2
    echo "  go install golang.org/dl/go1.24.4@latest && go1.24.4 download" >&2
    exit 1
  fi
  export GOROOT="$sdk"; export PATH="$GOROOT/bin:$HOME/go/bin:$PATH"
fi
gofmt -w . >/dev/null; go vet . || true
tinygo build -target=wasip2 -o "$name.wasm" --wit-package ../../wit --wit-world plugin-guest .
echo "built $(pwd)/$name.wasm"
echo "preview: (cd ../../.. && cargo run -p ezbar-wasm --example preview -- go/examples/$name/$name.wasm --check)"
EOF
chmod +x "$dir/build.sh"

cat > "$dir/README.md" <<EOF
# $name — an ezbar Go/TinyGo plugin

## Build

\`\`\`sh
./build.sh        # fronts a Go 1.24 SDK if needed, runs gofmt/vet, builds <name>.wasm
\`\`\`

## Build (manual / one-time toolchain)

- TinyGo 0.30+ with a wasip2 target (\`tinygo targets | grep wasip2\`).
- TinyGo uses the Go toolchain for typechecking; it currently supports **Go ≤ 1.24**,
  so if your system \`go\` is newer, front a 1.24 SDK on PATH for the build
  (\`go install golang.org/dl/go1.24.4@latest && go1.24.4 download\`, then
  \`GOROOT=\$HOME/sdk/go1.24.4 PATH=\$GOROOT/bin:\$PATH ...\`).

## Build the component

\`\`\`sh
cd $(basename "$root")/examples/$name
tinygo build -target=wasip2 -o $name.wasm --wit-package ../../wit --wit-world plugin-guest .
\`\`\`

The \`../../wit\` guest world (shared infra) unions the WASI imports TinyGo needs
with the ezbar plugin world — you don't touch it.

## Preview & install

\`\`\`sh
# render it in a window (or --check to verify headlessly):
cargo run -p ezbar-wasm --example preview -- examples/$name/$name.wasm
cp examples/$name/$name.wasm ~/.config/ezbar/plugins/
\`\`\`

If this plugin fetches from the network, the user grants the host in
\`~/.config/ezbar/config.toml\` (\`[modules.$name]\` then \`network = "REPLACE-WITH-HOST"\`).
EOF

echo "created $dir"
echo "next: $(basename "$root")/examples/$name/build.sh"
