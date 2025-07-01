package widget

import (
	"fmt"
	"testing"
)

func TestSpotifyInfiniteScrolling(t *testing.T) {
	widget := &SpotifyFixedWidget{
		maxDisplayLength: 8,
		originalText:     "ABCDEF", // 6 characters
	}

	fmt.Printf("Testing infinite scrolling with text: %q (length: %d)\n", widget.originalText, len([]rune(widget.originalText)))
	fmt.Printf("Display length: %d\n", widget.maxDisplayLength)
	fmt.Println()

	// Test many cycles to see if there are any jumps
	var prevText string
	for offset := 0; offset < 50; offset++ {
		widget.scrollOffset = offset
		displayText := widget.getScrolledText(widget.originalText)
		
		fmt.Printf("Offset %2d: %q\n", offset, displayText)
		
		// Check for smooth transition from previous
		if offset > 0 {
			// Each step should shift by exactly one character to the left
			if len(prevText) > 0 && len(displayText) > 0 {
				// The last character of current should become the first character of next
				// OR we should see smooth character-by-character progression
				prevRunes := []rune(prevText)
				currRunes := []rune(displayText)
				
				if len(prevRunes) >= 2 && len(currRunes) >= 1 {
					// Check if current text is previous text shifted left by 1
					expectedFirst := prevRunes[1] // Second char of prev should be first of curr
					actualFirst := currRunes[0]
					
					if expectedFirst != actualFirst {
						fmt.Printf("  JUMP DETECTED: Expected first char %q, got %q\n", 
							string(expectedFirst), string(actualFirst))
					}
				}
			}
		}
		
		prevText = displayText
	}
}