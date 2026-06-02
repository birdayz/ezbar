// ezbar Go plugin: loadgauge — a self-contained CPU-style load gauge.
//
// No network needed: it synthesises a structured load series (a slow sine
// "duty cycle" plus jitter) and renders it as a sparkline chip whose colour
// shifts as load climbs through warn/urgent thresholds. Click the chip to flip
// between the sparkline and a numeric readout; hover for a popup with the
// high-fidelity area chart and min/avg/max stats.
//
// Build:
//
//	tinygo build -target=wasip2 -o loadgauge.wasm \
//	    --wit-package ../../wit --wit-world plugin-guest .
package main

import (
	"math"
	"strconv"

	"github.com/birdayz/ezbar/go/ezbar"
)

const (
	window    = 48   // samples kept in the ring
	tickMS    = 1000 // sample cadence
	warnPct   = 60.0
	urgentPct = 85.0
)

type LoadGauge struct {
	ezbar.Base
	samples []float64 // load %, oldest→newest, len ≤ window
	t       int       // tick counter, drives the synthetic series
	numeric bool      // chip mode: false = sparkline, true = numeric
}

// synth produces a structured-but-fake load% in [2,98]: a slow duty cycle so the
// sparkline has shape, plus a deterministic jitter so it looks alive.
func synth(t int) float64 {
	base := 50 + 38*math.Sin(float64(t)/9.0) // slow swell
	jit := 9 * math.Sin(float64(t)*1.7)      // fast wobble
	v := base + jit
	if v < 2 {
		v = 2
	}
	if v > 98 {
		v = 98
	}
	return v
}

func (g *LoadGauge) Update(ctx ezbar.Ctx, ev ezbar.Event) bool {
	switch ev.Kind {
	case ezbar.EvTimer:
		g.samples = append(g.samples, synth(g.t))
		if len(g.samples) > window {
			g.samples = g.samples[len(g.samples)-window:]
		}
		g.t++
		ctx.SetTimeout(tickMS)
		return true
	case ezbar.EvPointer:
		if ev.PointerID == "chip" && ev.PointerKind == ezbar.Press {
			g.numeric = !g.numeric
			return true
		}
	}
	return false
}

func (g *LoadGauge) cur() float64 {
	if len(g.samples) == 0 {
		return 0
	}
	return g.samples[len(g.samples)-1]
}

// loadColor maps the latest reading to a theme token.
func loadColor(v float64) ezbar.Color {
	switch {
	case v >= urgentPct:
		return ezbar.Urgent
	case v >= warnPct:
		return ezbar.Warn
	default:
		return ezbar.OK
	}
}

func pct(v float64) string { return strconv.Itoa(int(v+0.5)) + "%" }

func (g *LoadGauge) View() ezbar.Render {
	v := g.cur()
	col := loadColor(v)

	var body ezbar.Render
	if g.numeric {
		body = ezbar.Text(pct(v)).Color(col)
	} else {
		body = ezbar.Graph{Values: g.samples, Kind: ezbar.GraphGeneric, Line: col}.View()
	}

	chip := ezbar.Row(
		ezbar.IconCPU.View(14, ezbar.FgDim),
		body,
	).Spacing(6)

	// MouseArea so a click flips the chip mode (the hover popup is automatic).
	return ezbar.MouseArea("chip", chip)
}

func (g *LoadGauge) Popup() (ezbar.Render, bool) {
	if len(g.samples) == 0 {
		return ezbar.Render{}, false
	}
	lo, hi, sum := g.samples[0], g.samples[0], 0.0
	for _, s := range g.samples {
		if s < lo {
			lo = s
		}
		if s > hi {
			hi = s
		}
		sum += s
	}
	avg := sum / float64(len(g.samples))

	stat := func(label string, val float64) ezbar.Render {
		return ezbar.Row(
			ezbar.Text(label).Color(ezbar.FgDim).Size(11),
			ezbar.Spacer(8),
			ezbar.Text(pct(val)).Color(ezbar.Fg).Size(11),
		)
	}

	return ezbar.Column(
		ezbar.Row(
			ezbar.IconCPU.View(13, ezbar.Accent),
			ezbar.Text("load").Color(ezbar.Fg).Size(12),
		).Spacing(6),
		ezbar.Chart{
			Values: g.samples,
			Line:   loadColor(g.cur()),
			Width:  180,
			Height: 56,
		}.View(),
		stat("min", lo),
		stat("avg", avg),
		stat("max", hi),
	).Spacing(6), true
}

func init() { ezbar.Register(&LoadGauge{}) }
func main() {}
