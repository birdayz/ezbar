package widget

import (
	"fmt"
	"testing"
)

func TestSpotifySmoothScrolling(t *testing.T) {
	widget := &SpotifyFixedWidget{
		maxDisplayLength: 10,
		originalText:     "HELLO", // 5 characters, shorter than display
	}

	fmt.Printf("Testing smooth scrolling with text: %q (length: %d)\n", widget.originalText, len([]rune(widget.originalText)))
	fmt.Printf("Display length: %d\n", widget.maxDisplayLength)
	fmt.Printf("Padded text would be: %q (length: %d)\n", widget.originalText+"    ", len([]rune(widget.originalText+"    ")))
	fmt.Println()

	// Calculate how many steps for 5 full cycles
	paddedLength := len([]rune(widget.originalText + "    "))
	totalSteps := paddedLength * 5 // 5 complete cycles
	
	fmt.Printf("Testing %d steps (%d cycles of %d chars each)\n", totalSteps, 5, paddedLength)
	fmt.Println()

	var prevText string
	jumpCount := 0
	
	for offset := 0; offset < totalSteps; offset++ {
		widget.scrollOffset = offset
		displayText := widget.getScrolledText(widget.originalText)
		
		fmt.Printf("Offset %2d: %q", offset, displayText)
		
		// Check for smooth transition from previous
		if offset > 0 {
			prevRunes := []rune(prevText)
			currRunes := []rune(displayText)
			
			// For smooth scrolling, the current text should be the previous text shifted left by 1
			// So prevText[1:] should match the beginning of currText
			if len(prevRunes) > 1 && len(currRunes) > 0 {
				// Get what we expect: previous text shifted left by 1
				expectedPrefix := string(prevRunes[1:])
				actualPrefix := string(currRunes[:len(prevRunes)-1])
				
				if expectedPrefix != actualPrefix {
					fmt.Printf(" <- JUMP! Expected prefix %q, got %q", expectedPrefix, actualPrefix)
					jumpCount++
				} else {
					fmt.Printf(" <- smooth")
				}
			}
		} else {
			fmt.Printf(" <- start")
		}
		
		fmt.Println()
		prevText = displayText
	}
	
	fmt.Printf("\nSummary: %d jumps detected out of %d transitions\n", jumpCount, totalSteps-1)
	
	if jumpCount > 0 {
		t.Errorf("Detected %d jumps in scrolling - should be 0 for smooth scrolling", jumpCount)
	}
}

func TestSpotifyLongTextScrolling(t *testing.T) {
	widget := &SpotifyFixedWidget{
		maxDisplayLength: 8,
		originalText:     "ABCDEFGHIJKLMNOP", // 16 characters, longer than display
	}

	fmt.Printf("Testing smooth scrolling with long text: %q (length: %d)\n", widget.originalText, len([]rune(widget.originalText)))
	fmt.Printf("Display length: %d\n", widget.maxDisplayLength)
	fmt.Println()

	// Test 3 complete cycles
	originalLength := len([]rune(widget.originalText))
	totalSteps := originalLength * 3 // 3 complete cycles
	
	fmt.Printf("Testing %d steps (%d cycles of %d chars each)\n", totalSteps, 3, originalLength)
	fmt.Println()

	var prevText string
	jumpCount := 0
	
	for offset := 0; offset < totalSteps; offset++ {
		widget.scrollOffset = offset
		displayText := widget.getScrolledText(widget.originalText)
		
		fmt.Printf("Offset %2d: %q", offset, displayText)
		
		// Check for smooth transition from previous
		if offset > 0 {
			prevRunes := []rune(prevText)
			currRunes := []rune(displayText)
			
			// For smooth scrolling, currText should be prevText shifted left by 1
			if len(prevRunes) > 1 && len(currRunes) > 0 {
				expectedPrefix := string(prevRunes[1:])
				actualPrefix := string(currRunes[:len(prevRunes)-1])
				
				if expectedPrefix != actualPrefix {
					fmt.Printf(" <- JUMP! Expected prefix %q, got %q", expectedPrefix, actualPrefix)
					jumpCount++
				} else {
					fmt.Printf(" <- smooth")
				}
			}
		} else {
			fmt.Printf(" <- start")
		}
		
		fmt.Println()
		prevText = displayText
	}
	
	fmt.Printf("\nSummary: %d jumps detected out of %d transitions\n", jumpCount, totalSteps-1)
	
	if jumpCount > 0 {
		t.Errorf("Detected %d jumps in scrolling - should be 0 for smooth scrolling", jumpCount)
	}
}