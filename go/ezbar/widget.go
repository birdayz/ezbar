package ezbar

import (
	"github.com/birdayz/ezbar/go/internal/ezbar/plugin/types"
	"github.com/birdayz/ezbar/go/internal/ezbar/plugin/ui"
	"go.bytecodealliance.org/cm"
)

// ── colours ─────────────────────────────────────────────────────────────────

// Color is a chip colour: a semantic theme token (preferred — it follows the
// user's theme) or a literal [RGBA]. The exported token values ([Fg], [Accent],
// …) are Colors you use directly.
type Color struct {
	rgba   types.Rgba8
	tok    types.ThemeToken
	isRGBA bool
}

// Theme tokens. Prefer these over RGBA so the chip respects the user's theme.
var (
	Fg     = Color{tok: types.ThemeTokenFg}     // primary text
	FgDim  = Color{tok: types.ThemeTokenFgDim}  // muted / secondary
	Accent = Color{tok: types.ThemeTokenAccent} // brand / interactive
	OK     = Color{tok: types.ThemeTokenOK}     // good / healthy (green)
	Warn   = Color{tok: types.ThemeTokenWarn}   // nearing a limit (yellow)
	Urgent = Color{tok: types.ThemeTokenUrgent} // critical (red)
	Bg     = Color{tok: types.ThemeTokenBg}     // background
)

// RGBA is a literal colour escape hatch; prefer a theme token where you can.
func RGBA(r, g, b, a uint8) Color {
	return Color{isRGBA: true, rgba: types.Rgba8{R: r, G: g, B: b, A: a}}
}

func (c Color) paint() ui.Paint {
	if c.isRGBA {
		return types.PaintRgba(c.rgba)
	}
	return types.PaintToken(c.tok)
}

// ── components ──────────────────────────────────────────────────────────────

// Icon is one of the host's embedded icons. Render it with [Icon.View]. The
// values are kept in lock-step with the WIT icon-id enum (same order).
type Icon uint8

// The host icon set. Use like: ezbar.IconCloud.View(14, ezbar.Fg).
const (
	IconCPU Icon = iota
	IconMemory
	IconTemperature
	IconPing
	IconVolumeHigh
	IconVolumeMedium
	IconVolumeMute
	IconBattery
	IconBatteryCharging
	IconBatteryWarning
	IconBot
	IconGithub
	IconSpotify
	IconKubernetes
	IconClock
	IconCalendar
	IconDisk
	IconNet
	IconIP
	IconUpdates
	IconKeyboard
	IconCloud
	IconSun
	IconMoon
	IconAlert
	IconDot
	// weather conditions (WMO-coded)
	IconCloudSun
	IconCloudMoon
	IconCloudFog
	IconCloudDrizzle
	IconCloudRain
	IconCloudRainWind
	IconCloudSnow
	IconCloudHail
	IconCloudLightning
	IconDroplets
	IconWind
	IconSunrise
	IconSunset
	IconSnowflake
)

// GraphKind hints the host's auto-scaling for a [Graph]. It does NOT set colour
// (use Line for that). Generic = min/max of your data — a fine default.
type GraphKind = types.GraphKind

const (
	GraphCPU         = types.GraphKindCPU
	GraphMemory      = types.GraphKindMemory
	GraphTemperature = types.GraphKindTemperature
	GraphPing        = types.GraphKindPing
	GraphGeneric     = types.GraphKindGeneric
)

// Align is row/column cross-axis alignment.
type Align = types.Align

const (
	AlignStart  = types.AlignStart
	AlignCenter = types.AlignCenter
	AlignEnd    = types.AlignEnd
)

// Graph is the host sparkline over your data — the thing a shell script can't do.
// An empty or single-element Values renders as an empty/flat sparkline (safe — no
// trap), so it's fine to build one before your first data tick lands.
type Graph struct {
	Values []float64
	Kind   GraphKind // auto-scale hint; GraphGeneric is the usual choice
	Line   Color     // line colour (a token is best)
}

// View turns the Graph into a [Render].
func (g Graph) View() Render {
	return Render{kind: kGraph, values: g.Values, gkind: g.Kind, color: g.Line}
}

// Chart is the high-fidelity smoothed gradient area chart (the stock-popup look),
// for a hover popup.
type Chart struct {
	Values        []float64
	Line          Color
	Width, Height float32
}

// View turns the Chart into a [Render].
func (c Chart) View() Render {
	return Render{kind: kChart, values: c.Values, color: c.Line, width: c.Width, height: c.Height}
}

// View renders the icon at the given pixel size and colour.
func (id Icon) View(size float32, color Color) Render {
	return Render{kind: kIcon, icon: id, isize: size, color: color}
}

// ── the render tree ─────────────────────────────────────────────────────────

type renderKind uint8

const (
	kSpacer renderKind = iota // zero value → a 0-width spacer (a safe no-op node)
	kText
	kRow
	kColumn
	kContainer
	kMouseArea
	kIcon
	kGraph
	kChart
)

// Render is a node in the widget tree. Build it with [Text]/[Row]/[Column]/
// [Container]/[MouseArea]/[Spacer] and the component [Icon.View]/[Graph.View]/
// [Chart.View], then tune it with the fluent setters.
type Render struct {
	kind    renderKind
	text    string
	color   Color
	size    float32 // text size (0 = host default)
	hasSize bool
	icon    Icon
	isize   float32 // icon size
	values  []float64
	gkind   GraphKind
	width   float32 // chart width / spacer px
	height  float32 // chart height
	spacing float32 // row/column
	align   Align
	padding float32 // container
	hitID   string  // mouse-area id
	kids    []Render
}

// Text is a text run (default colour [Fg]).
func Text(s string) Render { return Render{kind: kText, text: s, color: Fg} }

// Row lays children out horizontally (cross-axis centered by default).
func Row(children ...Render) Render {
	return Render{kind: kRow, kids: children, align: AlignCenter}
}

// Column lays children out vertically (start-aligned by default).
func Column(children ...Render) Render {
	return Render{kind: kColumn, kids: children, align: AlignStart}
}

// Container wraps a child (use [Render.Padding] to inset it).
func Container(child Render) Render { return Render{kind: kContainer, kids: []Render{child}} }

// MouseArea makes its child interactive: the host sends EvPointer events tagged
// with id to your Update. Not needed for the hover popup — that is automatic.
func MouseArea(id string, child Render) Render {
	return Render{kind: kMouseArea, hitID: id, kids: []Render{child}}
}

// Spacer is fixed empty horizontal space.
func Spacer(px float32) Render { return Render{kind: kSpacer, width: px} }

// ── fluent setters (each applies only where it makes sense; no-op elsewhere) ──

// Color sets text/icon colour, or a graph/chart line colour.
func (r Render) Color(c Color) Render { r.color = c; return r }

// Size sets a text node's pixel size.
func (r Render) Size(px float32) Render { r.size = px; r.hasSize = true; return r }

// Spacing sets the gap between row/column children.
func (r Render) Spacing(px float32) Render { r.spacing = px; return r }

// Align sets row/column cross-axis alignment.
func (r Render) Align(a Align) Render { r.align = a; return r }

// Padding insets a container's child.
func (r Render) Padding(px float32) Render { r.padding = px; return r }

// lower flattens the tree into the WIT arena the host expects: children are
// emitted before their parent, so every node references only lower indices
// (a forward-referencing DAG) and root is the last node.
func lower(r Render) ui.Tree {
	var nodes []ui.Node
	root := push(&nodes, r)
	return ui.Tree{Nodes: cm.ToList(nodes), Root: root}
}

func push(nodes *[]ui.Node, r Render) uint32 {
	switch r.kind {
	case kText:
		n := ui.TextNode{Content: r.text, Color: r.color.paint()}
		if r.hasSize {
			n.Size = cm.Some(r.size)
		}
		return emit(nodes, ui.NodeText(n))
	case kRow, kColumn:
		idx := make([]uint32, len(r.kids))
		for i, k := range r.kids {
			idx[i] = push(nodes, k)
		}
		ln := ui.LayoutNode{Children: cm.ToList(idx), Spacing: r.spacing, Align: r.align}
		if r.kind == kRow {
			return emit(nodes, ui.NodeRow(ln))
		}
		return emit(nodes, ui.NodeColumn(ln))
	case kContainer:
		c := push(nodes, r.kids[0])
		return emit(nodes, ui.NodeContainer(ui.BoxNode{Child: c, Padding: r.padding}))
	case kMouseArea:
		c := push(nodes, r.kids[0])
		return emit(nodes, ui.NodeMouseArea(ui.HitNode{Child: c, ID: r.hitID}))
	case kIcon:
		return emit(nodes, ui.NodeIcon(ui.IconNode{ID: types.IconID(r.icon), Color: r.color.paint(), Size: r.isize}))
	case kGraph:
		return emit(nodes, ui.NodeGraph(ui.GraphNode{Values: cm.ToList(r.values), Kind: r.gkind, Line: r.color.paint()}))
	case kChart:
		return emit(nodes, ui.NodeChart(ui.ChartNode{Values: cm.ToList(r.values), Line: r.color.paint(), Width: r.width, Height: r.height}))
	default: // kSpacer and the zero Render
		return emit(nodes, ui.NodeSpacer(r.width))
	}
}

func emit(nodes *[]ui.Node, n ui.Node) uint32 {
	*nodes = append(*nodes, n)
	return uint32(len(*nodes) - 1)
}
