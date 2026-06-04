//! ezbar WASM plugin: `fstest` — show the first line of a file, read with **normal
//! `std::fs`** over the sandboxed **fs capability**. The honest proof that a file-reading
//! widget can be a plugin: it has no ambient filesystem; the host preopens exactly the
//! granted directory and WASI jails it there (no `..`/symlink escape, read-only here).
//!
//! Grant a directory and point at a file inside its guest mount:
//! ```toml
//! [modules.fstest]
//! file = "/notes/today.txt"
//! fs   = [{ path = "~/notes", at = "/notes", mode = "r" }]
//! ```

use ezbar_plugin_wasm::prelude::*;

#[derive(Default)]
struct FsTest {
    path: String,
    text: String,
}

impl Plugin for FsTest {
    fn load(&mut self, config: Vec<(String, String)>) {
        self.path = config
            .iter()
            .find(|(k, _)| k == "file")
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| "/data/status.txt".to_string());
    }

    fn update(&mut self, ctx: &mut dyn Ctx, ev: Event) -> bool {
        let Event::Timer = ev else { return false };
        let new = match std::fs::read_to_string(&self.path) {
            // first line, clamped — a status/now-playing/note line from a file
            Ok(s) => s.lines().next().unwrap_or("").chars().take(60).collect(),
            // ungranted / missing → degrade to a short error (WASI denies it cleanly)
            Err(e) => format!("\u{26a0} {e}"),
        };
        let changed = new != self.text;
        self.text = new;
        ctx.set_timeout(2000);
        changed
    }

    fn view(&self) -> Render {
        text(self.text.clone()).color(Token::Fg)
    }
}

export_plugin!(FsTest);
