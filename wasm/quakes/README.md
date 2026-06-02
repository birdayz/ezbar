# quakes — an ezbar WASM plugin

Recent-earthquake monitor. Polls the USGS GeoJSON summary feed (public, no auth),
shows the quake count + largest magnitude with a sparkline of recent magnitudes,
and lists the strongest recent quakes on hover.

```sh
cargo build --target wasm32-wasip2 --release
cp target/wasm32-wasip2/release/quakes.wasm ~/.config/ezbar/plugins/
```

Preview it in a window before it touches the bar:

```sh
cargo run -p ezbar-wasm --example preview -- \
    wasm/quakes/target/wasm32-wasip2/release/quakes.wasm \
    --net earthquake.usgs.gov --set feed=2.5
```

This plugin needs the network. The user grants the host in
`~/.config/ezbar/config.toml`:

```toml
[modules.quakes]
network = "earthquake.usgs.gov"   # required — else ctx.http_get is denied
feed = "2.5"                       # significant | 4.5 | 2.5 | 1.0 | all (default 2.5)
```

Without the `network` line, `ctx.http_get` returns `capability denied` and the
chip shows a dim placeholder.
