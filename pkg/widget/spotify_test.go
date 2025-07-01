package widget

import (
	"fmt"
	"testing"
)

func TestSpotifyScrolling(t *testing.T) {
	widget := &SpotifyFixedWidget{
		maxDisplayLength: 10, // Short for testing
		originalText:     "Hello World Test",
	}

	fmt.Printf("Original text: %q (length: %d runes)\n", widget.originalText, len([]rune(widget.originalText)))
	fmt.Printf("Display length: %d\n", widget.maxDisplayLength)
	fmt.Println()

	// Test scrolling through the entire text
	for offset := 0; offset < len([]rune(widget.originalText))+5; offset++ {
		widget.scrollOffset = offset
		displayText := widget.getScrolledText(widget.originalText)
		
		fmt.Printf("Offset %2d: %q\n", offset, displayText)
		
		// Check that we always get exactly maxDisplayLength characters
		if len([]rune(displayText)) != widget.maxDisplayLength {
			t.Errorf("At offset %d, got %d characters, expected %d", 
				offset, len([]rune(displayText)), widget.maxDisplayLength)
		}
	}
}

func TestSpotifyScrollingContinuity(t *testing.T) {
	widget := &SpotifyFixedWidget{
		maxDisplayLength: 8,
		originalText:     "ABCDEF", // 6 characters
	}

	fmt.Printf("Testing continuity with text: %q (length: %d)\n", widget.originalText, len([]rune(widget.originalText)))
	fmt.Printf("Display length: %d\n", widget.maxDisplayLength)
	fmt.Println()

	// Test several cycles to see the pattern
	for offset := 0; offset < 20; offset++ {
		widget.scrollOffset = offset
		displayText := widget.getScrolledText(widget.originalText)
		
		fmt.Printf("Offset %2d: %q\n", offset, displayText)
		
		// Check for smooth transition - the next offset should shift by exactly 1 character
		if offset > 0 {
			widget.scrollOffset = offset - 1
			prevText := widget.getScrolledText(widget.originalText)
			widget.scrollOffset = offset
			
			// The current text should be the previous text shifted left by 1
			prevRunes := []rune(prevText)
			currRunes := []rune(displayText)
			
			if len(prevRunes) > 1 && len(currRunes) > 0 {
				// Check if the shift is smooth (prev[1:] should match curr[0:len-1])
				expectedShift := string(prevRunes[1:]) + string(currRunes[len(currRunes)-1:])
				if expectedShift != displayText {
					fmt.Printf("  WARNING: Non-smooth transition from offset %d to %d\n", offset-1, offset)
					fmt.Printf("  Previous: %q\n", prevText)
					fmt.Printf("  Current:  %q\n", displayText)
					fmt.Printf("  Expected: %q\n", expectedShift)
				}
			}
		}
	}
}