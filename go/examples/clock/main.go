// A minimal ezbar Go plugin: a clock chip. Build with:
//
//	tinygo build -target=wasip2 -o clock.wasm \
//	    --wit-package ../../wit --wit-world plugin-guest .
//
// The whole plugin is a Plugin impl + the Register call below.
package main

import (
	"time"

	"github.com/birdayz/ezbar/go/ezbar"
)

type Clock struct {
	ezbar.Base // no-op defaults for Load/Popup/SaveState/Restore
	now        string
}

func (c *Clock) Update(ctx ezbar.Ctx, ev ezbar.Event) bool {
	if ev.Kind != ezbar.EvTimer {
		return false
	}
	c.now = time.Now().Format("15:04")
	ctx.SetTimeout(10_000) // tick again in 10s
	return true
}

func (c *Clock) View() ezbar.Render {
	return ezbar.Row(
		ezbar.IconClock.View(14, ezbar.FgDim),
		ezbar.Text(c.now).Color(ezbar.Fg),
	).Spacing(5)
}

func init() { ezbar.Register(&Clock{}) }
func main() {}
