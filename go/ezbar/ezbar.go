// Package ezbar is the Go SDK for writing sandboxed WASM plugins for the ezbar
// status bar (RFC 0006). You write a [Plugin] — an Elm loop that builds its chip
// from a bounded widget vocabulary ([Text]/[Row]/[Column]/[Icon]/[Graph]/…) —
// compile it to a wasm32-wasip2 component with TinyGo, and the bar renders it.
//
// The whole plugin is a Plugin impl + one `ezbar.Register(...)` call:
//
//	type Clock struct{ ezbar.Base; now string }
//	func (c *Clock) Update(ctx ezbar.Ctx, ev ezbar.Event) bool {
//		if ev.Kind == ezbar.EvTimer { c.now = "12:34"; return true }
//		return false
//	}
//	func (c *Clock) View() ezbar.Render {
//		return ezbar.Row(ezbar.IconClock.View(14, ezbar.Fg), ezbar.Text(c.now)).Spacing(5)
//	}
//	func init() { ezbar.Register(&Clock{}) }
//	func main()  {}
//
// This is NOT arbitrary iced: there is no canvas/shader/custom widget. The plugin
// describes intent (text, an icon, a sparkline over your data, a popup); the host
// owns the look and themes it.
package ezbar

import (
	"errors"

	"github.com/birdayz/ezbar/go/internal/ezbar/plugin/events"
	"github.com/birdayz/ezbar/go/internal/ezbar/plugin/host"
	"github.com/birdayz/ezbar/go/internal/ezbar/plugin/plugin"
	"github.com/birdayz/ezbar/go/internal/ezbar/plugin/types"
	"go.bytecodealliance.org/cm"
)

// Ctx is the gated host services a plugin may call from [Plugin.Update]. It runs
// off the GUI thread, so a blocking HTTPGet is fine.
type Ctx interface {
	// HTTPGet does a blocking GET. It only works if the user granted the URL's
	// host via [modules.<id>].network in their config; otherwise it errors.
	HTTPGet(url string) ([]byte, error)
	// Log writes a line to the bar's log (stderr).
	Log(msg string)
	// SetTimeout asks the host to deliver the next EvTimer after ms milliseconds.
	SetTimeout(ms uint32)
}

// Plugin is the thing you implement. Only View is required; embed [Base] for
// no-op defaults of the rest, and override what you need.
type Plugin interface {
	// Load receives the [modules.<id>] config table (also re-delivered on a live
	// config change).
	Load(config map[string]string)
	// Update advances state on an event; return true if the chip must re-render.
	Update(ctx Ctx, ev Event) bool
	// View builds the chip. PURE + synchronous: no host calls, no I/O — do that
	// in Update. Called only when Update returned true.
	View() Render
	// Popup builds the hover-detail surface; return ok=false for none. The host
	// hovers the whole chip for you — no mouse-area needed.
	Popup() (tree Render, ok bool)
	// SaveState / Restore hand state across a CLEAN reload only (lost on a trap).
	SaveState() []byte
	Restore(state []byte)
}

// Base provides no-op defaults for every optional [Plugin] method. Embed it and
// implement only View (plus whatever else you need):
//
//	type My struct{ ezbar.Base }
type Base struct{}

func (Base) Load(map[string]string) {}
func (Base) Update(Ctx, Event) bool { return false }
func (Base) Popup() (Render, bool)  { return Render{}, false }
func (Base) SaveState() []byte      { return nil }
func (Base) Restore([]byte)         {}

// Register wires your plugin to the component exports. Call it once, from init:
//
//	func init() { ezbar.Register(&My{}) }
//	func main()  {}
func Register(p Plugin) {
	plugin.Exports.Init = func(config cm.List[[2]string]) {
		p.Load(pairsToMap(config))
	}
	plugin.Exports.Update = func(ev plugin.Event) bool {
		// a config event re-delivers the [modules.<id>] table to Load.
		if cfg := ev.Config(); cfg != nil {
			p.Load(pairsToMap(*cfg))
			return true
		}
		return p.Update(hostCtx{}, fromWASMEvent(ev))
	}
	plugin.Exports.View = func() plugin.Tree { return lower(p.View()) }
	plugin.Exports.Popup = func() cm.Option[plugin.Tree] {
		if tree, ok := p.Popup(); ok {
			return cm.Some(lower(tree))
		}
		return cm.None[plugin.Tree]()
	}
	plugin.Exports.SaveState = func() cm.List[uint8] { return cm.ToList(p.SaveState()) }
	plugin.Exports.Restore = func(state cm.List[uint8]) { p.Restore(state.Slice()) }
}

// hostCtx bridges Ctx onto the generated host imports.
type hostCtx struct{}

func (hostCtx) Log(msg string)       { host.Log(msg) }
func (hostCtx) SetTimeout(ms uint32) { host.SetTimeout(ms) }
func (hostCtx) HTTPGet(url string) ([]byte, error) {
	res := host.HTTPGet(url)
	if res.IsErr() {
		return nil, errors.New(*res.Err())
	}
	return res.OK().Slice(), nil
}

func pairsToMap(l cm.List[[2]string]) map[string]string {
	m := make(map[string]string, l.Len())
	for _, kv := range l.Slice() {
		m[kv[0]] = kv[1]
	}
	return m
}

// ── events ──────────────────────────────────────────────────────────────────

// EventKind tags an [Event].
type EventKind uint8

const (
	EvTimer   EventKind = iota // a timer tick (drive your polling here)
	EvPointer                  // a pointer event on a mouse-area you declared
	EvFeed                     // a host data-feed sample you subscribed to
)

// PointerKind is which pointer interaction fired (for EvPointer).
type PointerKind = events.PointerKind

const (
	Press      = events.PointerKindPress
	RightPress = events.PointerKindRightPress
	Scroll     = events.PointerKindScroll
	Enter      = events.PointerKindEnter
	Leave      = events.PointerKindLeave
)

// FeedKind names a host data feed (for EvFeed).
type FeedKind = types.FeedKind

// Event is delivered to [Plugin.Update]. Switch on Kind; the relevant fields are
// set for that kind.
type Event struct {
	Kind EventKind

	// EvPointer:
	PointerID   string      // the mouse-area id you set
	PointerKind PointerKind // Press / RightPress / Scroll / Enter / Leave
	Delta       float32     // scroll delta (for Scroll)

	// EvFeed:
	Feed  FeedKind
	Value float64
}

func fromWASMEvent(ev plugin.Event) Event {
	if p := ev.Pointer(); p != nil {
		return Event{Kind: EvPointer, PointerID: p.ID, PointerKind: p.Kind, Delta: p.Delta}
	}
	if f := ev.Feed(); f != nil {
		return Event{Kind: EvFeed, Feed: f.Feed, Value: f.Value}
	}
	return Event{Kind: EvTimer}
}
