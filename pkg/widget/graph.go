package widget

import (
	"sync"
	
	"github.com/diamondburned/gotk4/pkg/cairo"
	"github.com/diamondburned/gotk4/pkg/glib/v2"
	"github.com/diamondburned/gotk4/pkg/gtk/v4"
	
	"github.com/birdayz/ezbar/pkg/datasource"
)

// History tracking structure
type usageHistory struct {
	mu     sync.RWMutex
	values []float64
	maxLen int
	pos    int
}

func (uh *usageHistory) addValue(value float64) {
	uh.mu.Lock()
	defer uh.mu.Unlock()
	
	uh.values[uh.pos] = value
	uh.pos = (uh.pos + 1) % uh.maxLen
}

func (uh *usageHistory) getValues() []float64 {
	uh.mu.RLock()
	defer uh.mu.RUnlock()
	
	result := make([]float64, uh.maxLen)
	for i := 0; i < uh.maxLen; i++ {
		idx := (uh.pos + i) % uh.maxLen
		result[i] = uh.values[idx]
	}
	return result
}

type GraphWidget struct {
	drawingArea *gtk.DrawingArea
	history     *usageHistory
	graphType   string
	visible     bool
}

func NewGraphWidget(graphType string, historySize int) *GraphWidget {
	widget := &GraphWidget{
		drawingArea: gtk.NewDrawingArea(),
		history: &usageHistory{
			values: make([]float64, historySize),
			maxLen: historySize,
			pos:    0,
		},
		graphType: graphType,
		visible:   true, // Start visible by default
	}
	
	// Memory and ping graphs start hidden by default
	if graphType == "memory" || graphType == "ping" {
		widget.visible = false
	}
	
	widget.drawingArea.SetSizeRequest(80, 20)
	
	// Set initial visibility
	widget.drawingArea.SetVisible(widget.visible)
	widget.drawingArea.SetMarginStart(5)
	widget.drawingArea.SetMarginEnd(5)
	widget.drawingArea.SetMarginTop(5)
	widget.drawingArea.SetMarginBottom(5)
	
	// Add click gesture for toggle
	gesture := gtk.NewGestureClick()
	gesture.SetButton(1) // Left mouse button
	gesture.ConnectPressed(func(n int, x, y float64) {
		widget.toggleVisibility()
	})
	widget.drawingArea.AddController(gesture)
	
	// Set the appropriate draw function based on graph type
	switch graphType {
	case "cpu":
		widget.drawingArea.SetDrawFunc(func(area *gtk.DrawingArea, cr *cairo.Context, width, height int) {
			widget.drawGraph(cr, width, height, 0, 100, widget.getCPUColor)
		})
	case "memory":
		widget.drawingArea.SetDrawFunc(func(area *gtk.DrawingArea, cr *cairo.Context, width, height int) {
			widget.drawGraph(cr, width, height, 0, 100, widget.getMemoryColor)
		})
	case "temperature":
		widget.drawingArea.SetDrawFunc(func(area *gtk.DrawingArea, cr *cairo.Context, width, height int) {
			widget.drawTemperatureGraph(cr, width, height)
		})
	case "ping":
		widget.drawingArea.SetDrawFunc(func(area *gtk.DrawingArea, cr *cairo.Context, width, height int) {
			widget.drawPingGraph(cr, width, height)
		})
	}
	
	return widget
}

func (w *GraphWidget) Update(value interface{}) {
	var val float64
	
	switch data := value.(type) {
	case datasource.CPUData:
		val = data.Usage
	case datasource.MemoryData:
		val = data.Usage
	case datasource.TemperatureData:
		val = data.Temperature
	case datasource.PingData:
		if data.IsUp {
			val = data.Latency
		} else {
			val = -1 // Mark as down
		}
	default:
		return
	}
	
	if val >= 0 {
		w.history.addValue(val)
		glib.IdleAdd(func() {
			w.drawingArea.QueueDraw()
		})
	}
}

func (w *GraphWidget) GetGTKWidget() *gtk.Widget {
	return &w.drawingArea.Widget
}

func (w *GraphWidget) toggleVisibility() {
	w.visible = !w.visible
	glib.IdleAdd(func() {
		w.drawingArea.SetVisible(w.visible)
	})
}

func (w *GraphWidget) getCPUColor(val float64) (float64, float64, float64) {
	if val <= 25 {
		return 0.2, 0.8, 0.2 // Green
	} else if val <= 50 {
		return 1.0, 1.0, 0.0 // Yellow
	} else if val <= 75 {
		return 1.0, 0.6, 0.0 // Orange
	} else {
		return 1.0, 0.2, 0.2 // Red
	}
}

func (w *GraphWidget) getMemoryColor(val float64) (float64, float64, float64) {
	if val <= 50 {
		return 0.2, 0.8, 0.2 // Green
	} else if val <= 70 {
		return 1.0, 1.0, 0.0 // Yellow
	} else if val <= 85 {
		return 1.0, 0.6, 0.0 // Orange
	} else {
		return 1.0, 0.2, 0.2 // Red
	}
}

func (w *GraphWidget) getTemperatureColor(val float64) (float64, float64, float64) {
	if val <= 50 {
		return 0.2, 0.8, 0.2 // Green
	} else if val <= 60 {
		return 1.0, 1.0, 0.0 // Yellow
	} else if val <= 70 {
		return 1.0, 0.6, 0.0 // Orange
	} else {
		return 1.0, 0.2, 0.2 // Red
	}
}

func (w *GraphWidget) getPingColor(val float64) (float64, float64, float64) {
	if val <= 20 {
		return 0.2, 0.8, 0.2 // Green
	} else if val <= 50 {
		return 1.0, 1.0, 0.0 // Yellow
	} else if val <= 100 {
		return 1.0, 0.6, 0.0 // Orange
	} else {
		return 1.0, 0.2, 0.2 // Red
	}
}

func (w *GraphWidget) drawGraph(cr *cairo.Context, width, height int, minVal, maxVal float64, colorFunc func(float64) (float64, float64, float64)) {
	// Set transparent background
	cr.SetOperator(cairo.OperatorClear)
	cr.Paint()
	cr.SetOperator(cairo.OperatorOver)
	
	values := w.history.getValues()
	if len(values) == 0 {
		return
	}
	
	cr.SetLineWidth(1.5)
	
	var prevX, prevY float64
	var prevVal float64
	firstPoint := true
	
	for i, val := range values {
		if val >= 0 {
			x := float64(i) * float64(width) / float64(len(values)-1)
			y := float64(height) - ((val-minVal)/(maxVal-minVal))*float64(height)
			
			if !firstPoint {
				segmentVal := val
				if prevVal > val {
					segmentVal = prevVal
				}
				
				r, g, b := colorFunc(segmentVal)
				cr.SetSourceRGB(r, g, b)
				
				cr.MoveTo(prevX, prevY)
				cr.LineTo(x, y)
				cr.Stroke()
			} else {
				firstPoint = false
			}
			
			prevX, prevY = x, y
			prevVal = val
		}
	}
}

func (w *GraphWidget) drawTemperatureGraph(cr *cairo.Context, width, height int) {
	// Set transparent background
	cr.SetOperator(cairo.OperatorClear)
	cr.Paint()
	cr.SetOperator(cairo.OperatorOver)
	
	temps := w.history.getValues()
	if len(temps) == 0 {
		return
	}
	
	// Find min/max for scaling
	minTemp, maxTemp := temps[0], temps[0]
	validCount := 0
	for _, temp := range temps {
		if temp > 0 {
			if temp < minTemp || minTemp == 0 {
				minTemp = temp
			}
			if temp > maxTemp {
				maxTemp = temp
			}
			validCount++
		}
	}
	
	if validCount == 0 || maxTemp <= minTemp {
		return
	}
	
	// Add some padding to the range
	tempRange := maxTemp - minTemp
	if tempRange < 10 {
		tempRange = 10
		minTemp = maxTemp - tempRange
	} else {
		padding := tempRange * 0.1
		minTemp -= padding
		maxTemp += padding
	}
	
	cr.SetLineWidth(1.5)
	
	var prevX, prevY float64
	var prevTemp float64
	firstPoint := true
	
	for i, temp := range temps {
		if temp > 0 {
			x := float64(i) * float64(width) / float64(len(temps)-1)
			y := float64(height) - ((temp-minTemp)/(maxTemp-minTemp))*float64(height)
			
			if !firstPoint {
				segmentTemp := temp
				if prevTemp > temp {
					segmentTemp = prevTemp
				}
				
				r, g, b := w.getTemperatureColor(segmentTemp)
				cr.SetSourceRGB(r, g, b)
				
				cr.MoveTo(prevX, prevY)
				cr.LineTo(x, y)
				cr.Stroke()
			} else {
				firstPoint = false
			}
			
			prevX, prevY = x, y
			prevTemp = temp
		}
	}
}

func (w *GraphWidget) drawPingGraph(cr *cairo.Context, width, height int) {
	// Set transparent background
	cr.SetOperator(cairo.OperatorClear)
	cr.Paint()
	cr.SetOperator(cairo.OperatorOver)
	
	pings := w.history.getValues()
	if len(pings) == 0 {
		return
	}
	
	// Find min/max for scaling, ignoring -1 values (down)
	minPing, maxPing := 0.0, 0.0
	validCount := 0
	for _, ping := range pings {
		if ping >= 0 {
			if validCount == 0 || ping < minPing {
				minPing = ping
			}
			if ping > maxPing {
				maxPing = ping
			}
			validCount++
		}
	}
	
	if validCount == 0 {
		return
	}
	
	// Set reasonable bounds for ping times
	if maxPing < 20 {
		maxPing = 20
	} else if maxPing < 50 {
		maxPing = 50
	} else if maxPing < 100 {
		maxPing = 100
	} else {
		maxPing = maxPing * 1.1 // Add 10% padding
	}
	
	cr.SetLineWidth(1.5)
	
	var prevX, prevY float64
	var prevPing float64
	firstPoint := true
	
	for i, ping := range pings {
		if ping >= 0 {
			x := float64(i) * float64(width) / float64(len(pings)-1)
			y := float64(height) - ((ping-minPing)/(maxPing-minPing))*float64(height)
			
			if !firstPoint {
				segmentPing := ping
				if prevPing > ping {
					segmentPing = prevPing
				}
				
				r, g, b := w.getPingColor(segmentPing)
				cr.SetSourceRGB(r, g, b)
				
				cr.MoveTo(prevX, prevY)
				cr.LineTo(x, y)
				cr.Stroke()
			} else {
				firstPoint = false
			}
			
			prevX, prevY = x, y
			prevPing = ping
		} else {
			// Reset for disconnected segments when ping is down
			firstPoint = true
		}
	}
}