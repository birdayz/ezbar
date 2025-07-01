package widget

import (
	"time"
	
	"github.com/diamondburned/gotk4/pkg/glib/v2"
	"github.com/diamondburned/gotk4/pkg/gtk/v4"
	
	"github.com/birdayz/ezbar/pkg/datasource"
)

type LabelWidget struct {
	label            *gtk.Label
	container        *gtk.Box // Container for fixed-size Spotify widget
	associatedGraph  *GraphWidget
	scrollOffset     int
	scrollTimer      *time.Timer
	scrolling        bool
	maxDisplayLength int
	originalText     string
	isSpotifyWidget  bool
	customClickHandler func()
	rightClickHandler func()
}

func NewLabelWidget(initialText string) *LabelWidget {
	widget := &LabelWidget{
		label:            gtk.NewLabel(initialText),
		associatedGraph:  nil,
		maxDisplayLength: 40, // Default max length before scrolling
		isSpotifyWidget:  false,
	}
	
	// Add left click gesture for toggling associated graph
	leftGesture := gtk.NewGestureClick()
	leftGesture.SetButton(1) // Left mouse button
	leftGesture.ConnectPressed(func(n int, x, y float64) {
		if widget.customClickHandler != nil {
			widget.customClickHandler()
		} else if widget.associatedGraph != nil {
			widget.associatedGraph.toggleVisibility()
		}
	})
	widget.label.AddController(leftGesture)
	
	// Add right click gesture
	rightGesture := gtk.NewGestureClick()
	rightGesture.SetButton(3) // Right mouse button
	rightGesture.ConnectPressed(func(n int, x, y float64) {
		if widget.rightClickHandler != nil {
			widget.rightClickHandler()
		}
	})
	widget.label.AddController(rightGesture)
	
	return widget
}

func (w *LabelWidget) Update(value interface{}) {
	switch data := value.(type) {
	case datasource.CPUData:
		glib.IdleAdd(func() {
			w.label.SetText(data.UsageString)
		})
	case datasource.MemoryData:
		glib.IdleAdd(func() {
			w.label.SetText(data.UsageString)
		})
	case datasource.TemperatureData:
		glib.IdleAdd(func() {
			w.label.SetText(data.TempString)
		})
	case datasource.PingData:
		glib.IdleAdd(func() {
			w.label.SetText(data.PingString)
		})
	case datasource.TimeData:
		glib.IdleAdd(func() {
			w.label.SetText(data.TimeString)
		})
	case datasource.SpotifyData:
		if w.isSpotifyWidget {
			w.updateScrollingText(data.TrackString)
		} else {
			glib.IdleAdd(func() {
				w.label.SetText(data.TrackString)
			})
		}
	case datasource.StockData:
		glib.IdleAdd(func() {
			w.label.SetText(data.DisplayText)
		})
	case datasource.KubectlData:
		glib.IdleAdd(func() {
			w.label.SetText(data.ContextString)
			if data.IsProduction {
				w.label.AddCSSClass("production-context")
			} else {
				w.label.RemoveCSSClass("production-context")
			}
		})
	}
}

func (w *LabelWidget) GetGTKWidget() *gtk.Widget {
	if w.container != nil {
		return &w.container.Widget
	}
	return &w.label.Widget
}

func (w *LabelWidget) SetAssociatedGraph(graph *GraphWidget) {
	w.associatedGraph = graph
}

func (w *LabelWidget) SetClickHandler(handler func()) {
	w.customClickHandler = handler
}

func (w *LabelWidget) SetRightClickHandler(handler func()) {
	w.rightClickHandler = handler
}

func (w *LabelWidget) SetSpotifyClickHandler(handler func()) {
	// Mark this as a Spotify widget to enable scrolling
	w.isSpotifyWidget = true
	
	// Create a fixed-size container with overflow control
	w.container = gtk.NewBox(gtk.OrientationHorizontal, 0)
	w.container.SetSizeRequest(400, -1) // Fixed 400px width
	w.container.SetHExpand(false)
	w.container.SetVExpand(false)
	w.container.SetOverflow(gtk.OverflowHidden) // Hide overflow
	
	// Set alignment to prevent expansion
	w.container.SetHAlign(gtk.AlignStart)
	w.container.SetVAlign(gtk.AlignCenter)
	
	// Configure the label to not request any specific size
	w.label.SetSingleLineMode(true)
	w.label.SetEllipsize(0)
	w.label.SetHAlign(gtk.AlignStart)
	w.label.SetHExpand(false)
	w.label.SetVExpand(false)
	w.label.SetSizeRequest(-1, -1) // Let it be natural size
	
	// Add label to container
	w.container.Append(w.label)
	
	// Add a new click gesture specifically for Spotify
	spotifyGesture := gtk.NewGestureClick()
	spotifyGesture.SetButton(1) // Left mouse button
	spotifyGesture.ConnectPressed(func(n int, x, y float64) {
		handler()
	})
	w.container.AddController(spotifyGesture)
}

func (w *LabelWidget) updateScrollingText(text string) {
	w.originalText = text
	
	// Stop any existing scrolling
	w.stopScrolling()
	
	// If text is short enough, don't scroll
	if len([]rune(text)) <= w.maxDisplayLength {
		glib.IdleAdd(func() {
			w.label.SetText(text)
		})
		return
	}
	
	// Start scrolling
	w.scrollOffset = 0
	w.scrolling = true
	w.startScrolling()
}

func (w *LabelWidget) startScrolling() {
	if !w.scrolling || w.originalText == "" {
		return
	}
	
	// Create scrolling text with padding
	scrollText := w.originalText + "    " // Add some padding between loops
	displayText := w.getScrolledText(scrollText)
	
	glib.IdleAdd(func() {
		w.label.SetText(displayText)
	})
	
	// Schedule next scroll update
	w.scrollTimer = time.AfterFunc(200*time.Millisecond, func() {
		w.scrollOffset++
		if w.scrollOffset >= len(scrollText) {
			w.scrollOffset = 0
		}
		w.startScrolling()
	})
}

func (w *LabelWidget) getScrolledText(fullText string) string {
	// Just return the text as-is for scrolling, no character manipulation
	if len([]rune(fullText)) <= w.maxDisplayLength {
		return fullText
	}
	
	// Scroll long text
	paddedText := fullText + "    " // Add separator
	runes := []rune(paddedText)
	start := w.scrollOffset % len(runes)
	end := start + w.maxDisplayLength
	
	if end <= len(runes) {
		return string(runes[start:end])
	}
	
	// Wrap around
	return string(runes[start:]) + string(runes[:end-len(runes)])
}


func (w *LabelWidget) stopScrolling() {
	w.scrolling = false
	if w.scrollTimer != nil {
		w.scrollTimer.Stop()
		w.scrollTimer = nil
	}
}