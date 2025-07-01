package widget

import (
	"time"

	"github.com/diamondburned/gotk4/pkg/cairo"
	"github.com/diamondburned/gotk4/pkg/glib/v2"
	"github.com/diamondburned/gotk4/pkg/gtk/v4"
	"github.com/diamondburned/gotk4/pkg/pangocairo"

	"github.com/birdayz/ezbar/pkg/datasource"
)

type SpotifyFixedWidget struct {
	drawingArea      *gtk.DrawingArea
	scrollOffset     int
	scrollTimer      *time.Timer
	scrolling        bool
	maxDisplayLength int
	originalText     string
	currentText      string
}

func NewSpotifyFixedWidget() *SpotifyFixedWidget {
	widget := &SpotifyFixedWidget{
		maxDisplayLength: 40,
		currentText:      "🎵 --",
	}

	// Create drawing area with fixed size
	widget.drawingArea = gtk.NewDrawingArea()
	widget.drawingArea.SetSizeRequest(400, 25) // Fixed 400x25 pixels
	widget.drawingArea.SetHExpand(false)
	widget.drawingArea.SetVExpand(false)
	
	// Set up drawing function
	widget.drawingArea.SetDrawFunc(func(area *gtk.DrawingArea, cr *cairo.Context, width, height int) {
		widget.draw(cr, width, height)
	})

	return widget
}

func (w *SpotifyFixedWidget) Update(value interface{}) {
	if data, ok := value.(datasource.SpotifyData); ok {
		w.updateScrollingText(data.TrackString)
	}
}

func (w *SpotifyFixedWidget) GetGTKWidget() *gtk.Widget {
	return &w.drawingArea.Widget
}

func (w *SpotifyFixedWidget) SetClickHandler(handler func()) {
	gesture := gtk.NewGestureClick()
	gesture.SetButton(1)
	gesture.ConnectPressed(func(n int, x, y float64) {
		handler()
	})
	w.drawingArea.AddController(gesture)
}

func (w *SpotifyFixedWidget) draw(cr *cairo.Context, width, height int) {
	// Set background (optional)
	cr.SetSourceRGBA(0, 0, 0, 0) // Transparent
	cr.Paint()

	// Set text color
	cr.SetSourceRGBA(1, 1, 1, 1) // White text

	// Create Pango layout for proper Unicode text rendering
	layout := pangocairo.CreateLayout(cr)
	layout.SetText(w.currentText)
	
	// Set font smaller (10px monospace)
	fontDesc := layout.Context().FontDescription()
	fontDesc.SetFamily("monospace")
	fontDesc.SetSize(10 * 1024) // Pango uses 1/1024 units - smaller
	layout.SetFontDescription(fontDesc)

	// Position with adjusted vertical alignment
	cr.MoveTo(5, 7) // 5px from left, 7px from top (2px lower)
	pangocairo.ShowLayout(cr, layout)
}

func (w *SpotifyFixedWidget) updateScrollingText(text string) {
	// Only reset scroll if the text actually changed
	if w.originalText == text {
		return // Same text, keep current scroll position
	}
	
	w.originalText = text

	// Stop any existing scrolling
	w.stopScrolling()

	// If text is short enough, don't scroll
	if len([]rune(text)) <= w.maxDisplayLength {
		w.currentText = text
		glib.IdleAdd(func() {
			w.drawingArea.QueueDraw()
		})
		return
	}

	// Start scrolling from beginning only for new text
	w.scrollOffset = 0
	w.scrolling = true
	w.startScrolling()
}

func (w *SpotifyFixedWidget) startScrolling() {
	if !w.scrolling || w.originalText == "" {
		return
	}

	// Use the original proven character scrolling logic
	w.currentText = w.getScrolledText(w.originalText)

	glib.IdleAdd(func() {
		w.drawingArea.QueueDraw()
	})

	// Schedule next scroll update - character-based scrolling (much slower)
	w.scrollTimer = time.AfterFunc(800*time.Millisecond, func() {
		w.scrollOffset++
		// Never reset offset - let it grow infinitely and use modulo in getScrolledText
		w.startScrolling()
	})
}

func (w *SpotifyFixedWidget) getScrolledText(fullText string) string {
	runes := []rune(fullText)
	totalLength := len(runes)
	
	// If text is shorter than display length, still scroll but pad with spaces
	if totalLength <= w.maxDisplayLength {
		// Create a padded version for scrolling
		paddedText := fullText + "    " // Add some spacing
		runes = []rune(paddedText)
		totalLength = len(runes)
	}
	
	// Build the display text by taking characters from the repeating sequence
	var result []rune
	for i := 0; i < w.maxDisplayLength; i++ {
		charIndex := (w.scrollOffset + i) % totalLength
		result = append(result, runes[charIndex])
	}
	
	return string(result)
}


func (w *SpotifyFixedWidget) stopScrolling() {
	w.scrolling = false
	if w.scrollTimer != nil {
		w.scrollTimer.Stop()
		w.scrollTimer = nil
	}
}