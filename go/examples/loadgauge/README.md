# loadgauge — an ezbar Go/TinyGo plugin

## Build (one-time toolchain)

- TinyGo 0.30+ with a wasip2 target (`tinygo targets | grep wasip2`).
- TinyGo uses the Go toolchain for typechecking; it currently supports **Go ≤ 1.24**,
  so if your system `go` is newer, front a 1.24 SDK on PATH for the build
  (`go install golang.org/dl/go1.24.4@latest && go1.24.4 download`, then
  `GOROOT=$HOME/sdk/go1.24.4 PATH=$GOROOT/bin:$PATH ...`).

## Build the component

```sh
cd go/examples/loadgauge
tinygo build -target=wasip2 -o loadgauge.wasm --wit-package ../../wit --wit-world plugin-guest .
```

The `../../wit` guest world (shared infra) unions the WASI imports TinyGo needs
with the ezbar plugin world — you don't touch it.

## Preview & install

```sh
# render it in a window (or --check to verify headlessly):
cargo run -p ezbar-wasm --example preview -- examples/loadgauge/loadgauge.wasm
cp examples/loadgauge/loadgauge.wasm ~/.config/ezbar/plugins/
```

If this plugin fetches from the network, the user grants the host in
`~/.config/ezbar/config.toml` (`[modules.loadgauge]` then `network = "REPLACE-WITH-HOST"`).
