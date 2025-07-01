package widget

import (
	"time"

	"github.com/diamondburned/gotk4/pkg/glib/v2"
	"github.com/diamondburned/gotk4/pkg/gtk/v4"

	"github.com/birdayz/ezbar/pkg/datasource"
)

type SpotifyWidget struct {
	container        *gtk.Box
	label            *gtk.Label
	scrollOffset     int
	scrollTimer      *time.Timer
	scrolling        bool
	maxDisplayLength int
	originalText     string
}

func NewSpotifyWidget() *SpotifyWidget {
	widget := &SpotifyWidget{
		maxDisplayLength: 40,
	}

	// Create fixed-size container
	widget.container = gtk.NewBox(gtk.OrientationHorizontal, 0)
	widget.container.SetSizeRequest(400, -1) // Fixed 400px width
	widget.container.SetHExpand(false)
	widget.container.SetVExpand(false)
	widget.container.SetOverflow(gtk.OverflowHidden)

	// Create label
	widget.label = gtk.NewLabel("|> --")
	widget.label.SetSingleLineMode(true)
	widget.label.SetEllipsize(0)
	widget.label.SetHAlign(gtk.AlignStart)
	widget.label.SetHExpand(false)
	widget.label.SetVExpand(false)

	// Add label to container
	widget.container.Append(widget.label)

	return widget
}

func (w *SpotifyWidget) Update(value interface{}) {
	if data, ok := value.(datasource.SpotifyData); ok {
		w.updateScrollingText(data.TrackString)
	}
}

func (w *SpotifyWidget) GetGTKWidget() *gtk.Widget {
	return &w.container.Widget
}

func (w *SpotifyWidget) SetClickHandler(handler func()) {
	gesture := gtk.NewGestureClick()
	gesture.SetButton(1)
	gesture.ConnectPressed(func(n int, x, y float64) {
		handler()
	})
	w.container.AddController(gesture)
}

func (w *SpotifyWidget) updateScrollingText(text string) {
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

func (w *SpotifyWidget) startScrolling() {
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
		runes := []rune(scrollText)
		if w.scrollOffset >= len(runes) {
			w.scrollOffset = 0
		}
		w.startScrolling()
	})
}

func (w *SpotifyWidget) getScrolledText(fullText string) string {
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

func (w *SpotifyWidget) stopScrolling() {
	w.scrolling = false
	if w.scrollTimer != nil {
		w.scrollTimer.Stop()
		w.scrollTimer = nil
	}
}