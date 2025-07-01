package widget

import (
	"fmt"
	"time"

	"github.com/diamondburned/gotk4/pkg/glib/v2"
	"github.com/diamondburned/gotk4/pkg/gtk/v4"

	"github.com/birdayz/ezbar/pkg/datasource"
)

type SpotifyNativeWidget struct {
	container        *gtk.Box
	iconLabel        *gtk.Label
	textLabel        *gtk.Label
	scrollOffset     int
	scrollTimer      *time.Timer
	scrolling        bool
	maxDisplayLength int
	maxDisplayWidth  int  // Maximum pixel width for display
	originalText     string
	spotifyDataSource interface{
		NextTrack()
		PreviousTrack()
		VolumeUp()
		VolumeDown()
		AdjustVolumeBy(delta int)
	}
	scrollDebounce      time.Duration
	volumeDebounceTimer *time.Timer
	volumeScrollDelta   float64
}

func NewSpotifyNativeWidget() *SpotifyNativeWidget {
	widget := &SpotifyNativeWidget{
		maxDisplayLength: 25, // Fallback character limit
		maxDisplayWidth:  200, // Approximate pixel width (220px - padding)
		scrollDebounce:   150 * time.Millisecond, // Shorter debounce for smoother feel
	}

	// Create icon label (fixed, doesn't scroll)
	widget.iconLabel = gtk.NewLabel("🎵")
	widget.iconLabel.SetHAlign(gtk.AlignStart)
	widget.iconLabel.SetVAlign(gtk.AlignCenter)
	widget.iconLabel.SetMarginEnd(5) // Small spacing after icon
	
	// Create scrolled window for text only
	scrolled := gtk.NewScrolledWindow()
	scrolled.SetSizeRequest(220, 25) // Reduced width to account for icon
	scrolled.SetPolicy(gtk.PolicyAutomatic, gtk.PolicyNever) // Only horizontal scrolling
	scrolled.SetHExpand(false)
	scrolled.SetVExpand(false)
	
	// Add CSS class to hide scrollbar visually
	scrolled.AddCSSClass("spotify-scroll")

	// Create text label with native GTK styling
	widget.textLabel = gtk.NewLabel("--")
	widget.textLabel.SetSingleLineMode(true)
	widget.textLabel.SetEllipsize(0) // No ellipsis
	widget.textLabel.SetHAlign(gtk.AlignStart)
	widget.textLabel.SetVAlign(gtk.AlignCenter)

	// Add scroll controller to the text label too
	scrollController3 := gtk.NewEventControllerScroll(gtk.EventControllerScrollVertical)
	scrollController3.ConnectScroll(func(dx, dy float64) bool {
		fmt.Printf("Spotify text label scroll: dx=%.2f, dy=%.2f\n", dx, dy)
		widget.handleVolumeScroll(dy)
		return true // Event handled
	})
	widget.textLabel.AddController(scrollController3)

	// Put text label in scrolled window
	scrolled.SetChild(widget.textLabel)
	
	// Add scroll controller to the scrolled window as well
	scrollController2 := gtk.NewEventControllerScroll(gtk.EventControllerScrollVertical)
	scrollController2.ConnectScroll(func(dx, dy float64) bool {
		fmt.Printf("Spotify scrolled window scroll: dx=%.2f, dy=%.2f\n", dx, dy)
		widget.handleVolumeScroll(dy)
		return true // Event handled
	})
	scrolled.AddController(scrollController2)
	
	// Wrap in container for easier handling
	widget.container = gtk.NewBox(gtk.OrientationHorizontal, 0)
	widget.container.SetSizeRequest(250, 25)
	widget.container.SetHExpand(false)
	widget.container.SetVExpand(false)
	widget.container.Append(widget.iconLabel)
	widget.container.Append(scrolled)

	return widget
}

// estimateTextWidth approximates the pixel width of text in monospace font
// Assumes roughly 8 pixels per character for 14px monospace font
func (w *SpotifyNativeWidget) estimateTextWidth(text string) int {
	runes := []rune(text)
	charWidth := 8 // Approximate character width in pixels for monospace
	return len(runes) * charWidth
}

// textFitsInDisplay checks if text will fit without scrolling
func (w *SpotifyNativeWidget) textFitsInDisplay(text string) bool {
	estimatedWidth := w.estimateTextWidth(text)
	return estimatedWidth <= w.maxDisplayWidth
}

func (w *SpotifyNativeWidget) Update(value interface{}) {
	if data, ok := value.(datasource.SpotifyData); ok {
		// Update icon separately (doesn't scroll)
		glib.IdleAdd(func() {
			w.iconLabel.SetText(data.Icon)
		})
		// Update scrolling text
		w.updateScrollingText(data.ScrollText)
	}
}

func (w *SpotifyNativeWidget) GetGTKWidget() *gtk.Widget {
	return &w.container.Widget
}

func (w *SpotifyNativeWidget) SetClickHandler(handler func()) {
	// Left click for OAuth/play-pause
	leftClick := gtk.NewGestureClick()
	leftClick.SetButton(1) // Left mouse button
	leftClick.ConnectPressed(func(n int, x, y float64) {
		handler()
	})
	w.container.AddController(leftClick)
	
	// Right click for next track
	rightClick := gtk.NewGestureClick()
	rightClick.SetButton(3) // Right mouse button
	rightClick.ConnectPressed(func(n int, x, y float64) {
		if w.spotifyDataSource != nil {
			w.spotifyDataSource.NextTrack()
			fmt.Println("Spotify: Right-click next track")
		}
	})
	w.container.AddController(rightClick)
	
	// Add scroll wheel support for volume control
	// Scroll up = volume up, Scroll down = volume down
	scrollController := gtk.NewEventControllerScroll(gtk.EventControllerScrollVertical)
	scrollController.ConnectScroll(func(dx, dy float64) bool {
		fmt.Printf("Spotify scroll detected: dx=%.2f, dy=%.2f\n", dx, dy)
		w.handleVolumeScroll(dy)
		return true // Event handled
	})
	w.container.AddController(scrollController)
}

func (w *SpotifyNativeWidget) SetSpotifyDataSource(dataSource interface{
	NextTrack()
	PreviousTrack()
	VolumeUp()
	VolumeDown()
	AdjustVolumeBy(delta int)
}) {
	w.spotifyDataSource = dataSource
}

func (w *SpotifyNativeWidget) handleVolumeScroll(dy float64) {
	if w.spotifyDataSource == nil {
		return
	}
	
	// Always accumulate scroll events
	w.volumeScrollDelta += dy
	fmt.Printf("Spotify: Scroll event (dy=%.2f, total=%.2f)\n", dy, w.volumeScrollDelta)
	
	// If no timer running, start one
	if w.volumeDebounceTimer == nil {
		w.volumeDebounceTimer = time.AfterFunc(w.scrollDebounce, func() {
			w.flushVolumeChanges()
		})
	} else {
		// Reset the timer to extend the debounce window
		w.volumeDebounceTimer.Stop()
		w.volumeDebounceTimer = time.AfterFunc(w.scrollDebounce, func() {
			w.flushVolumeChanges()
		})
	}
}

func (w *SpotifyNativeWidget) flushVolumeChanges() {
	if w.volumeScrollDelta != 0 {
		// Convert accumulated scroll to volume steps
		// Each scroll "notch" is roughly 1.0, so we can use smaller steps
		volumeSteps := int(w.volumeScrollDelta * 3) // 3% per scroll notch instead of 5%
		
		fmt.Printf("Spotify: Flushing scroll (total=%.2f → %d%% volume change)\n", w.volumeScrollDelta, volumeSteps)
		
		if volumeSteps != 0 {
			if volumeSteps > 0 {
				// Positive = scroll down = volume down
				w.spotifyDataSource.AdjustVolumeBy(-abs(volumeSteps))
				fmt.Printf("Spotify: Volume down by %d%%\n", abs(volumeSteps))
			} else {
				// Negative = scroll up = volume up  
				w.spotifyDataSource.AdjustVolumeBy(abs(volumeSteps))
				fmt.Printf("Spotify: Volume up by %d%%\n", abs(volumeSteps))
			}
		}
		
		w.volumeScrollDelta = 0
	}
	
	// Clear the timer
	w.volumeDebounceTimer = nil
	fmt.Println("Spotify: Debounce window closed")
}

func abs(x int) int {
	if x < 0 {
		return -x
	}
	return x
}

func (w *SpotifyNativeWidget) updateScrollingText(text string) {
	// Only reset scroll if the text actually changed
	if w.originalText == text {
		return // Same text, keep current scroll position
	}
	
	w.originalText = text

	// Stop any existing scrolling
	w.stopScrolling()

	// Check if text fits in display
	estimatedWidth := w.estimateTextWidth(text)
	fits := w.textFitsInDisplay(text)
	
	// Debug: troubleshoot scrolling issues
	fmt.Printf("Spotify Text: %q, Length: %d chars, EstWidth: %dpx, MaxWidth: %dpx, Fits: %v\n", 
	          text, len([]rune(text)), estimatedWidth, w.maxDisplayWidth, fits)
	
	if fits {
		glib.IdleAdd(func() {
			w.textLabel.SetText(text)
		})
		return
	}

	// Start scrolling from beginning only for new text
	w.scrollOffset = 0
	w.scrolling = true
	w.startScrolling()
}

func (w *SpotifyNativeWidget) startScrolling() {
	if !w.scrolling || w.originalText == "" {
		return
	}

	// Use the proven character scrolling logic
	displayText := w.getScrolledText(w.originalText)

	glib.IdleAdd(func() {
		w.textLabel.SetText(displayText)
	})

	// Schedule next scroll update - character-based scrolling
	w.scrollTimer = time.AfterFunc(800*time.Millisecond, func() {
		w.scrollOffset++
		// Never reset offset - let it grow infinitely and use modulo in getScrolledText
		w.startScrolling()
	})
}

func (w *SpotifyNativeWidget) getScrolledText(fullText string) string {
	// Create a repeating pattern: text + spaces + text + spaces...
	// This ensures padding is always visible at wrap-around
	padding := "      " // 6 spaces
	originalRunes := []rune(fullText)
	paddingRunes := []rune(padding)
	
	// Calculate how many characters we can actually display based on pixel width
	displayLength := w.maxDisplayWidth / 8 // 8px per character estimate
	if displayLength < w.maxDisplayLength {
		displayLength = w.maxDisplayLength // Use the fallback minimum
	}
	
	// Build the display text by creating an infinite repeating pattern
	var result []rune
	originalLength := len(originalRunes)
	paddingLength := len(paddingRunes)
	totalPatternLength := originalLength + paddingLength
	
	for i := 0; i < displayLength; i++ {
		pos := (w.scrollOffset + i) % totalPatternLength
		if pos < originalLength {
			// We're in the original text portion
			result = append(result, originalRunes[pos])
		} else {
			// We're in the padding portion
			paddingPos := pos - originalLength
			result = append(result, paddingRunes[paddingPos])
		}
	}
	
	resultText := string(result)
	
	// Debug: log the scrolling details
	fmt.Printf("Scroll Debug - Original: %q, Padding: %q, Offset: %d, DisplayLen: %d, Result: %q\n", 
		fullText, padding, w.scrollOffset, displayLength, resultText)
	
	return resultText
}

func (w *SpotifyNativeWidget) stopScrolling() {
	w.scrolling = false
	if w.scrollTimer != nil {
		w.scrollTimer.Stop()
		w.scrollTimer = nil
	}
}