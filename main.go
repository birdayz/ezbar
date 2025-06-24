package main

import (
	"cmp"
	"context"
	"fmt"
	"html"
	"io/ioutil"
	"log/slog"
	"os"
	"os/exec"
	"os/signal"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/dustin/go-humanize"
	layershell "github.com/diamondburned/gotk4-layer-shell/pkg/gtk4layershell"
	"github.com/diamondburned/gotk4/pkg/cairo"
	"github.com/diamondburned/gotk4/pkg/gdk/v4"
	"github.com/diamondburned/gotk4/pkg/gio/v2"
	"github.com/diamondburned/gotk4/pkg/glib/v2"
	"github.com/diamondburned/gotk4/pkg/gtk/v4"
	"github.com/joshuarubin/go-sway"
)

var workspaceLabel *gtk.Label
var batteryLabel *gtk.Label
var batterySeparator *gtk.Label

// New widget-based components
var cpuLabelWidget *LabelWidget
var cpuGraphWidget *GraphWidget
var tempLabelWidget *LabelWidget
var tempGraphWidget *GraphWidget
var memoryLabelWidget *LabelWidget
var memoryGraphWidget *GraphWidget
var timeLabelWidget *LabelWidget

// Data sources
var cpuDataSource *CPUDataSource
var memoryDataSource *MemoryDataSource
var temperatureDataSource *TemperatureDataSource
var timeDataSource *TimeDataSource

// History tracking structures
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

// Data source and widget interfaces
type DataSource interface {
	Start(ctx context.Context)
	Subscribe(callback func(value interface{}))
	GetCurrentValue() interface{}
}

type Widget interface {
	Update(value interface{})
	GetGTKWidget() *gtk.Widget
}

// Data value types
type CPUData struct {
	Usage       float64
	UsageString string
}

type MemoryData struct {
	Usage       float64
	UsageString string
}

type TemperatureData struct {
	Temperature float64
	TempString  string
}

type TimeData struct {
	Time      time.Time
	TimeString string
}

// Old tempHistory methods removed - now handled by GraphWidget

func extractTemperatureValue(tempStr string) float64 {
	// Extract temperature value from strings like "🌡️ 45°C"
	if strings.Contains(tempStr, "°C") {
		parts := strings.Split(tempStr, " ")
		for _, part := range parts {
			if strings.Contains(part, "°C") {
				tempPart := strings.Replace(part, "°C", "", -1)
				if temp, err := strconv.ParseFloat(tempPart, 64); err == nil {
					return temp
				}
			}
		}
	}
	return 0
}

func extractCPUUsageValue(cpuStr string) float64 {
	// Extract CPU usage value from strings like "🖥️ 45%"
	if strings.Contains(cpuStr, "%") {
		parts := strings.Split(cpuStr, " ")
		for _, part := range parts {
			if strings.Contains(part, "%") {
				cpuPart := strings.Replace(part, "%", "", -1)
				if cpu, err := strconv.ParseFloat(cpuPart, 64); err == nil {
					return cpu
				}
			}
		}
	}
	return 0
}

func extractMemoryUsageValue(memStr string) float64 {
	// Extract memory usage percentage from strings like "💾 8.2GB/16GB"
	if strings.Contains(memStr, "/") {
		parts := strings.Split(memStr, " ")
		for _, part := range parts {
			if strings.Contains(part, "/") {
				fractionParts := strings.Split(part, "/")
				if len(fractionParts) == 2 {
					used := parseMemorySize(fractionParts[0])
					total := parseMemorySize(fractionParts[1])
					if used > 0 && total > 0 {
						return (used / total) * 100
					}
				}
			}
		}
	}
	return 0
}

func parseMemorySize(sizeStr string) float64 {
	// Parse sizes like "8.2GB", "512MB", etc.
	sizeStr = strings.TrimSpace(sizeStr)
	if strings.HasSuffix(sizeStr, "GB") {
		if val, err := strconv.ParseFloat(strings.TrimSuffix(sizeStr, "GB"), 64); err == nil {
			return val * 1024 * 1024 * 1024
		}
	} else if strings.HasSuffix(sizeStr, "MB") {
		if val, err := strconv.ParseFloat(strings.TrimSuffix(sizeStr, "MB"), 64); err == nil {
			return val * 1024 * 1024
		}
	} else if strings.HasSuffix(sizeStr, "KB") {
		if val, err := strconv.ParseFloat(strings.TrimSuffix(sizeStr, "KB"), 64); err == nil {
			return val * 1024
		}
	}
	return 0
}

// Data source implementations
type CPUDataSource struct {
	mu          sync.RWMutex
	currentData CPUData
	callbacks   []func(value interface{})
}

func NewCPUDataSource() *CPUDataSource {
	return &CPUDataSource{
		currentData: CPUData{Usage: 0, UsageString: "🖥️ --"},
		callbacks:   make([]func(value interface{}), 0),
	}
}

func (c *CPUDataSource) Start(ctx context.Context) {
	go func() {
		// Immediate fetch at startup
		usageStr := getCPUUsage()
		usage := extractCPUUsageValue(usageStr)
		
		data := CPUData{
			Usage:       usage,
			UsageString: usageStr,
		}
		
		c.mu.Lock()
		c.currentData = data
		callbacks := make([]func(value interface{}), len(c.callbacks))
		copy(callbacks, c.callbacks)
		c.mu.Unlock()
		
		for _, callback := range callbacks {
			callback(data)
		}
		
		// Continue with regular timer updates
		ticker := time.NewTicker(2 * time.Second)
		defer ticker.Stop()
		
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				usageStr := getCPUUsage()
				usage := extractCPUUsageValue(usageStr)
				
				data := CPUData{
					Usage:       usage,
					UsageString: usageStr,
				}
				
				c.mu.Lock()
				c.currentData = data
				callbacks := make([]func(value interface{}), len(c.callbacks))
				copy(callbacks, c.callbacks)
				c.mu.Unlock()
				
				for _, callback := range callbacks {
					callback(data)
				}
			}
		}
	}()
}

func (c *CPUDataSource) Subscribe(callback func(value interface{})) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.callbacks = append(c.callbacks, callback)
}

func (c *CPUDataSource) GetCurrentValue() interface{} {
	c.mu.RLock()
	defer c.mu.RUnlock()
	return c.currentData
}

type MemoryDataSource struct {
	mu          sync.RWMutex
	currentData MemoryData
	callbacks   []func(value interface{})
}

func NewMemoryDataSource() *MemoryDataSource {
	return &MemoryDataSource{
		currentData: MemoryData{Usage: 0, UsageString: "💾 --"},
		callbacks:   make([]func(value interface{}), 0),
	}
}

func (m *MemoryDataSource) Start(ctx context.Context) {
	go func() {
		// Immediate fetch at startup
		usageStr := getMemoryUsage()
		usage := extractMemoryUsageValue(usageStr)
		
		data := MemoryData{
			Usage:       usage,
			UsageString: usageStr,
		}
		
		m.mu.Lock()
		m.currentData = data
		callbacks := make([]func(value interface{}), len(m.callbacks))
		copy(callbacks, m.callbacks)
		m.mu.Unlock()
		
		for _, callback := range callbacks {
			callback(data)
		}
		
		// Continue with regular timer updates
		ticker := time.NewTicker(3 * time.Second)
		defer ticker.Stop()
		
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				usageStr := getMemoryUsage()
				usage := extractMemoryUsageValue(usageStr)
				
				data := MemoryData{
					Usage:       usage,
					UsageString: usageStr,
				}
				
				m.mu.Lock()
				m.currentData = data
				callbacks := make([]func(value interface{}), len(m.callbacks))
				copy(callbacks, m.callbacks)
				m.mu.Unlock()
				
				for _, callback := range callbacks {
					callback(data)
				}
			}
		}
	}()
}

func (m *MemoryDataSource) Subscribe(callback func(value interface{})) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.callbacks = append(m.callbacks, callback)
}

func (m *MemoryDataSource) GetCurrentValue() interface{} {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.currentData
}

type TemperatureDataSource struct {
	mu          sync.RWMutex
	currentData TemperatureData
	callbacks   []func(value interface{})
}

func NewTemperatureDataSource() *TemperatureDataSource {
	return &TemperatureDataSource{
		currentData: TemperatureData{Temperature: 0, TempString: "🌡️ --"},
		callbacks:   make([]func(value interface{}), 0),
	}
}

func (t *TemperatureDataSource) Start(ctx context.Context) {
	go func() {
		// Immediate fetch at startup
		tempStr := getCPUTemperature()
		temp := extractTemperatureValue(tempStr)
		
		data := TemperatureData{
			Temperature: temp,
			TempString:  tempStr,
		}
		
		t.mu.Lock()
		t.currentData = data
		callbacks := make([]func(value interface{}), len(t.callbacks))
		copy(callbacks, t.callbacks)
		t.mu.Unlock()
		
		for _, callback := range callbacks {
			callback(data)
		}
		
		// Continue with regular timer updates
		ticker := time.NewTicker(2 * time.Second)
		defer ticker.Stop()
		
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				tempStr := getCPUTemperature()
				temp := extractTemperatureValue(tempStr)
				
				data := TemperatureData{
					Temperature: temp,
					TempString:  tempStr,
				}
				
				t.mu.Lock()
				t.currentData = data
				callbacks := make([]func(value interface{}), len(t.callbacks))
				copy(callbacks, t.callbacks)
				t.mu.Unlock()
				
				for _, callback := range callbacks {
					callback(data)
				}
			}
		}
	}()
}

func (t *TemperatureDataSource) Subscribe(callback func(value interface{})) {
	t.mu.Lock()
	defer t.mu.Unlock()
	t.callbacks = append(t.callbacks, callback)
}

func (t *TemperatureDataSource) GetCurrentValue() interface{} {
	t.mu.RLock()
	defer t.mu.RUnlock()
	return t.currentData
}

type TimeDataSource struct {
	mu          sync.RWMutex
	currentData TimeData
	callbacks   []func(value interface{})
}

func NewTimeDataSource() *TimeDataSource {
	return &TimeDataSource{
		currentData: TimeData{Time: time.Now(), TimeString: "Loading…"},
		callbacks:   make([]func(value interface{}), 0),
	}
}

func (td *TimeDataSource) Start(ctx context.Context) {
	go func() {
		// Immediate fetch at startup
		now := time.Now()
		timeStr := now.Format("2006-01-02 15:04:05")
		
		data := TimeData{
			Time:       now,
			TimeString: timeStr,
		}
		
		td.mu.Lock()
		td.currentData = data
		callbacks := make([]func(value interface{}), len(td.callbacks))
		copy(callbacks, td.callbacks)
		td.mu.Unlock()
		
		for _, callback := range callbacks {
			callback(data)
		}
		
		// Continue with regular timer updates
		ticker := time.NewTicker(200 * time.Millisecond)
		defer ticker.Stop()
		
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				now := time.Now()
				timeStr := now.Format("2006-01-02 15:04:05")
				
				data := TimeData{
					Time:       now,
					TimeString: timeStr,
				}
				
				td.mu.Lock()
				td.currentData = data
				callbacks := make([]func(value interface{}), len(td.callbacks))
				copy(callbacks, td.callbacks)
				td.mu.Unlock()
				
				for _, callback := range callbacks {
					callback(data)
				}
			}
		}
	}()
}

func (td *TimeDataSource) Subscribe(callback func(value interface{})) {
	td.mu.Lock()
	defer td.mu.Unlock()
	td.callbacks = append(td.callbacks, callback)
}

func (td *TimeDataSource) GetCurrentValue() interface{} {
	td.mu.RLock()
	defer td.mu.RUnlock()
	return td.currentData
}

// Widget implementations
type LabelWidget struct {
	label           *gtk.Label
	associatedGraph *GraphWidget
}

func NewLabelWidget(initialText string) *LabelWidget {
	widget := &LabelWidget{
		label:           gtk.NewLabel(initialText),
		associatedGraph: nil,
	}
	
	// Add click gesture for toggling associated graph
	gesture := gtk.NewGestureClick()
	gesture.SetButton(1) // Left mouse button
	gesture.ConnectPressed(func(n int, x, y float64) {
		if widget.associatedGraph != nil {
			widget.associatedGraph.toggleVisibility()
		}
	})
	widget.label.AddController(gesture)
	
	return widget
}

func (w *LabelWidget) Update(value interface{}) {
	switch data := value.(type) {
	case CPUData:
		glib.IdleAdd(func() {
			w.label.SetText(data.UsageString)
		})
	case MemoryData:
		glib.IdleAdd(func() {
			w.label.SetText(data.UsageString)
		})
	case TemperatureData:
		glib.IdleAdd(func() {
			w.label.SetText(data.TempString)
		})
	case TimeData:
		glib.IdleAdd(func() {
			w.label.SetText(data.TimeString)
		})
	}
}

func (w *LabelWidget) GetGTKWidget() *gtk.Widget {
	return &w.label.Widget
}

func (w *LabelWidget) SetAssociatedGraph(graph *GraphWidget) {
	w.associatedGraph = graph
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
	
	widget.drawingArea.SetSizeRequest(80, 20)
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
	}
	
	return widget
}

func (w *GraphWidget) Update(value interface{}) {
	var val float64
	
	switch data := value.(type) {
	case CPUData:
		val = data.Usage
	case MemoryData:
		val = data.Usage
	case TemperatureData:
		val = data.Temperature
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

// drawPlaceholder method removed - now using GTK visibility instead

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

// Old drawing functions removed - now handled by GraphWidget methods

func main() {
	log := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{}))

	if os.Getenv("EZBAR_CHILD") == "1" {
		run(log.With("component", "ezbar"))
		return
	}

	log = log.With("component", "launcher")
	restartLoop(log)
}

func restartLoop(log *slog.Logger) {
	for {
		cmd := exec.Command(os.Args[0])
		cmd.Env = append(os.Environ(), "EZBAR_CHILD=1")
		cmd.Stdout = os.Stdout
		cmd.Stderr = os.Stderr

		log.Info("Spawning new bar subprocess...")
		if err := cmd.Run(); err != nil {
			log.Error("Child process crashed:", "err", err)
		} else {
			log.Info("Child exited cleanly")
		}
	}

}

func run(log *slog.Logger) {
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt)
	defer cancel()

	app := gtk.NewApplication("de.nerden.ezbar", gio.ApplicationNonUnique)
	app.ConnectActivate(func() {
		w := activate(log, app)
		w.Show()
	})

	go func() {
		<-ctx.Done()
		glib.IdleAdd(app.Quit)
	}()

	if code := app.Run(os.Args); code > 0 {
		cancel()
		os.Exit(code)
	}
}

func activate(log *slog.Logger, app *gtk.Application) *gtk.Window {
	window := gtk.NewApplicationWindow(app)
	window.SetTitle("ezbar")
	window.SetDecorated(false)
	window.SetName("bar-window")

	mainBox := gtk.NewBox(gtk.OrientationHorizontal, 0)
	window.SetChild(mainBox)

	batteryLabel = gtk.NewLabel("🔋 --")
	
	// Create new architecture widgets and data sources
	cpuLabelWidget = NewLabelWidget("🖥️ --")
	cpuGraphWidget = NewGraphWidget("cpu", 30)
	tempLabelWidget = NewLabelWidget("🌡️ --")
	tempGraphWidget = NewGraphWidget("temperature", 60)
	memoryLabelWidget = NewLabelWidget("💾 --")
	memoryGraphWidget = NewGraphWidget("memory", 20)
	timeLabelWidget = NewLabelWidget("Loading…")
	
	// Create data sources
	cpuDataSource = NewCPUDataSource()
	memoryDataSource = NewMemoryDataSource()
	temperatureDataSource = NewTemperatureDataSource()
	timeDataSource = NewTimeDataSource()
	
	// Associate labels with their graphs for click-to-toggle
	cpuLabelWidget.SetAssociatedGraph(cpuGraphWidget)
	tempLabelWidget.SetAssociatedGraph(tempGraphWidget)
	memoryLabelWidget.SetAssociatedGraph(memoryGraphWidget)
	
	// Connect data sources to widgets
	cpuDataSource.Subscribe(cpuLabelWidget.Update)
	cpuDataSource.Subscribe(cpuGraphWidget.Update)
	
	memoryDataSource.Subscribe(memoryLabelWidget.Update)
	memoryDataSource.Subscribe(memoryGraphWidget.Update)
	
	temperatureDataSource.Subscribe(tempLabelWidget.Update)
	temperatureDataSource.Subscribe(tempGraphWidget.Update)
	
	timeDataSource.Subscribe(timeLabelWidget.Update)

	workspaceLabel = gtk.NewLabel("Starting...")
	workspaceLabel.AddCSSClass("bar-label")

	workspaceLabel.SetHAlign(gtk.AlignCenter)
	workspaceLabel.SetVAlign(gtk.AlignCenter)
	workspaceLabel.SetMarginStart(0)
	workspaceLabel.SetMarginEnd(0)
	workspaceLabel.SetMarginTop(0)
	workspaceLabel.SetMarginBottom(0)

	workspaceLabel.SetSingleLineMode(true)

	centerLabel := gtk.NewLabel("Centered Label")

	leftBox := gtk.NewBox(gtk.OrientationHorizontal, 0)
	leftBox.SetHAlign(gtk.AlignStart)
	leftBox.Append(workspaceLabel)

	// Center box
	centerBox := gtk.NewBox(gtk.OrientationHorizontal, 0)
	centerBox.SetHAlign(gtk.AlignCenter)
	centerBox.Append(centerLabel)

	// Right box
	rightBox := gtk.NewBox(gtk.OrientationHorizontal, 0)
	rightBox.SetHAlign(gtk.AlignEnd)
	
	batterySeparator = gtk.NewLabel("|")
	
	// Add new architecture widgets to UI
	rightBox.Append(cpuLabelWidget.GetGTKWidget())
	rightBox.Append(cpuGraphWidget.GetGTKWidget())
	rightBox.Append(gtk.NewLabel("|"))
	rightBox.Append(tempLabelWidget.GetGTKWidget())
	rightBox.Append(tempGraphWidget.GetGTKWidget())
	rightBox.Append(gtk.NewLabel("|"))
	rightBox.Append(memoryLabelWidget.GetGTKWidget())
	rightBox.Append(memoryGraphWidget.GetGTKWidget())
	rightBox.Append(gtk.NewLabel("|"))
	
	// Only add battery components if battery exists
	if hasBattery() {
		rightBox.Append(batteryLabel)
		rightBox.Append(batterySeparator)
	}
	
	rightBox.Append(timeLabelWidget.GetGTKWidget())

	mainBox.Append(leftBox)
	mainBox.Append(centerBox)
	mainBox.Append(rightBox)

	leftBox.SetHExpand(true)
	leftBox.SetHAlign(gtk.AlignStart)

	centerBox.SetHExpand(true)
	centerBox.SetHAlign(gtk.AlignCenter)

	rightBox.SetHExpand(true)
	rightBox.SetHAlign(gtk.AlignEnd)
	workspaceLabel.AddCSSClass("workspace-label")

	css := gtk.NewCSSProvider()
	css.LoadFromData(`
window {
	background-color: rgba(0, 0, 0, 0.8); /* Fully opaque window background */
}
.emoji {
  font-family: "Noto Color Emoji", "Twemoji", sans-serif; /* Use emoji font for emojis */
}

label {
  font: 14px "Monospace";
  color: #ffffff;
  padding: 5px;
  margin: 0;
}


`)
	styleContext := window.StyleContext()
	gtk.StyleContextAddProviderForDisplay(styleContext.Display(), css, gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)

	layershell.InitForWindow(&window.Window)
	layershell.SetNamespace(&window.Window, "gtk-layer-shell")
	layershell.SetAnchor(&window.Window, layershell.LayerShellEdgeLeft, true)
	layershell.SetAnchor(&window.Window, layershell.LayerShellEdgeRight, true)
	layershell.SetAnchor(&window.Window, layershell.LayerShellEdgeBottom, true)
	layershell.SetLayer(&window.Window, layershell.LayerShellLayerTop)
	layershell.AutoExclusiveZoneEnable(&window.Window)

	go func() {
		ctx := context.Background()
		client, err := sway.New(ctx)
		if err != nil {
			log.Warn("Sway IPC error: %v", "err", err)
			return
		}

		wss := &workspaceState{}
		// Fetch initial workspace state
		workspaces, err := client.GetWorkspaces(ctx)
		if err != nil {
			log.Error("failed to get initial list of workspaces", "error", err)
			app.Quit()
		}

		for _, ws := range workspaces {
			wss.workspaces = append(wss.workspaces, &workspace{
				name:    ws.Name,
				focused: ws.Focused,
			})
		}

		// Do initial draw
		drawWorkspace(wss)

		go func() {
			log.Info("Listening to sway events")
			if err := sway.Subscribe(context.Background(), &eh{wss: wss}, sway.EventTypeWorkspace); err != nil {
				log.Error("failed to subscribe to sway", "error", err)
				app.Quit()
			}
		}()

		for {
			tree, err := client.GetTree(ctx)
			if err != nil {
				log.Warn("Failed to get tree: %v", "err", err)
				time.Sleep(1 * time.Second)
				continue
			}
			if tree.FocusedNode() != nil {

				glib.IdleAdd(func() {
					nodeName := tree.FocusedNode().Name

					formattedText := fmt.Sprintf(
						"%s",
						html.EscapeString(nodeName),
					)

					centerLabel.SetMarkup(formattedText)
				})
			}

			time.Sleep(time.Millisecond * 200)
		}
	}()

	// Start data sources with context
	ctx := context.Background()
	cpuDataSource.Start(ctx)
	memoryDataSource.Start(ctx)
	temperatureDataSource.Start(ctx)
	timeDataSource.Start(ctx)

	if hasBattery() {
		go func() {
			for {
				batteryStatus := getBatteryStatus()
				glib.IdleAdd(func() {
					batteryLabel.SetText(batteryStatus)
				})
				time.Sleep(5 * time.Second)
			}
		}()
	}

	glib.IdleAdd(func() {
		window.Show()
	})
	window.Show()

	display := gdk.DisplayGetDefault()
	if display != nil {
		monitors := display.Monitors()

		if monitors != nil {
			monitors.ConnectItemsChanged(func(position, removed, added uint) {
				log.Info("Monitor change detected. Exiting", "position", position, "removed", removed, "added", added)
				os.Stdout.Sync()
				app.Quit()
			})
			log.Info("Listening for Monitor change events")
		}
	}

	return &window.Window

}

func getBatteryStatus() string {
	capacity, err := ioutil.ReadFile("/sys/class/power_supply/BAT0/capacity")
	if err != nil {
		return "🔋 --"
	}

	status, err := ioutil.ReadFile("/sys/class/power_supply/BAT0/status")
	if err != nil {
		return "🔋 --"
	}

	capacityStr := strings.TrimSpace(string(capacity))
	statusStr := strings.TrimSpace(string(status))

	var icon string
	switch statusStr {
	case "Charging":
		icon = "🔌"
	case "Discharging":
		icon = "🔋"
	case "Full":
		icon = "🔋"
	default:
		icon = "🔋"
	}

	return fmt.Sprintf("%s %s%%", icon, capacityStr)
}

func hasBattery() bool {
	_, err := os.Stat("/sys/class/power_supply/BAT0")
	return err == nil
}

func getCPUUsage() string {
	stat1, err := ioutil.ReadFile("/proc/stat")
	if err != nil {
		return "🖥️ --"
	}

	time.Sleep(100 * time.Millisecond)

	stat2, err := ioutil.ReadFile("/proc/stat")
	if err != nil {
		return "🖥️ --"
	}

	cpu1 := parseCPUStat(string(stat1))
	cpu2 := parseCPUStat(string(stat2))

	if cpu1 == nil || cpu2 == nil {
		return "🖥️ --"
	}

	idle := cpu2[3] - cpu1[3]
	total := (cpu2[0] + cpu2[1] + cpu2[2] + cpu2[3]) - (cpu1[0] + cpu1[1] + cpu1[2] + cpu1[3])

	if total == 0 {
		return "🖥️ 0%"
	}

	usage := 100 - (idle*100)/total
	return fmt.Sprintf("🖥️ %d%%", usage)
}

func parseCPUStat(stat string) []int64 {
	lines := strings.Split(stat, "\n")
	if len(lines) == 0 {
		return nil
	}

	fields := strings.Fields(lines[0])
	if len(fields) < 5 || fields[0] != "cpu" {
		return nil
	}

	values := make([]int64, 4)
	for i := 0; i < 4; i++ {
		val, err := strconv.ParseInt(fields[i+1], 10, 64)
		if err != nil {
			return nil
		}
		values[i] = val
	}

	return values
}

func getMemoryUsage() string {
	meminfo, err := ioutil.ReadFile("/proc/meminfo")
	if err != nil {
		return "💾 --"
	}

	lines := strings.Split(string(meminfo), "\n")
	var memTotal, memAvailable int64

	for _, line := range lines {
		fields := strings.Fields(line)
		if len(fields) < 2 {
			continue
		}

		switch fields[0] {
		case "MemTotal:":
			if val, err := strconv.ParseInt(fields[1], 10, 64); err == nil {
				memTotal = val
			}
		case "MemAvailable:":
			if val, err := strconv.ParseInt(fields[1], 10, 64); err == nil {
				memAvailable = val
			}
		}
	}

	if memTotal == 0 {
		return "💾 --"
	}

	// Calculate used memory like free -h does: total - available
	memUsed := memTotal - memAvailable
	
	// Convert from KB to bytes for go-humanize
	usedBytes := uint64(memUsed * 1024)
	totalBytes := uint64(memTotal * 1024)
	
	return fmt.Sprintf("💾 %s/%s", humanize.Bytes(usedBytes), humanize.Bytes(totalBytes))
}

func getCPUTemperature() string {
	// Try to find CPU temperature from various sources
	tempPaths := []string{
		"/sys/class/thermal/thermal_zone0/temp",
		"/sys/class/thermal/thermal_zone1/temp",
		"/sys/devices/platform/thinkpad_hwmon/hwmon/hwmon7/temp1_input",
		"/sys/devices/platform/coretemp.0/hwmon/hwmon*/temp1_input",
	}
	
	for _, path := range tempPaths {
		if temp, err := ioutil.ReadFile(path); err == nil {
			tempStr := strings.TrimSpace(string(temp))
			if tempVal, err := strconv.ParseFloat(tempStr, 64); err == nil {
				// Temperature is usually in millidegrees Celsius
				tempCelsius := tempVal / 1000.0
				return fmt.Sprintf("🌡️ %.0f°C", tempCelsius)
			}
		}
	}
	
	// Try to find temperature from hwmon sensors
	hwmonFiles, err := ioutil.ReadDir("/sys/class/hwmon")
	if err == nil {
		for _, hwmon := range hwmonFiles {
			tempPath := fmt.Sprintf("/sys/class/hwmon/%s/temp1_input", hwmon.Name())
			if temp, err := ioutil.ReadFile(tempPath); err == nil {
				tempStr := strings.TrimSpace(string(temp))
				if tempVal, err := strconv.ParseFloat(tempStr, 64); err == nil {
					tempCelsius := tempVal / 1000.0
					return fmt.Sprintf("🌡️ %.0f°C", tempCelsius)
				}
			}
		}
	}
	
	return "🌡️ --"
}

type eh struct {
	wss *workspaceState
}

func (eh *eh) Workspace(ctx context.Context, e sway.WorkspaceEvent) {
	if e.Change == sway.WorkspaceInit {
		eh.wss.workspaces = append(eh.wss.workspaces, &workspace{
			name:    e.Current.Name,
			focused: e.Current.Focused,
		})
	}
	if e.Change == sway.WorkspaceEmpty {
		eh.wss.workspaces = slices.DeleteFunc(eh.wss.workspaces, func(ws *workspace) bool {
			return ws.name == e.Current.Name
		})
	}

	if e.Change == sway.WorkspaceFocus {
		newFocus := e.Current.Name
		oldFocus := e.Old.Name

		for _, ws := range eh.wss.workspaces {
			if ws.name == newFocus {
				ws.focused = true
			}

			if ws.name == oldFocus {
				ws.focused = false
			}

		}
	}

	slices.SortFunc(eh.wss.workspaces, func(a, b *workspace) int {
		aInt, errA := strconv.Atoi(a.name)
		bInt, errB := strconv.Atoi(b.name)
		if errA == nil && errB == nil {
			return cmp.Compare(aInt, bInt)
		}
		return cmp.Compare(a.name, b.name)
	})

	drawWorkspace(eh.wss)
}
func (eh *eh) Mode(context.Context, sway.ModeEvent)                       {}
func (eh *eh) Window(context.Context, sway.WindowEvent)                   {}
func (eh *eh) BarConfigUpdate(context.Context, sway.BarConfigUpdateEvent) {}
func (eh *eh) Binding(context.Context, sway.BindingEvent)                 {}
func (eh *eh) Shutdown(context.Context, sway.ShutdownEvent)               {}
func (eh *eh) Tick(context.Context, sway.TickEvent)                       {}
func (eh *eh) BarStateUpdate(context.Context, sway.BarStateUpdateEvent)   {}
func (eh *eh) BarStatusUpdate(context.Context, sway.BarStatusUpdateEvent) {}
func (eh *eh) Input(context.Context, sway.InputEvent)                     {}

type workspaceState struct {
	workspaces []*workspace
}

type workspace struct {
	name    string
	focused bool
}

func drawWorkspace(wss *workspaceState) error {
	text := "<span>"
	for _, ws := range wss.workspaces {
		if ws.focused {
			text += fmt.Sprintf("[%s]", ws.name)
		} else {
			text += fmt.Sprintf(" %s ", ws.name)
		}
	}
	text += "</span>"
	glib.IdleAdd(func() {
		workspaceLabel.SetMarkup(text)
	})
	return nil
}
