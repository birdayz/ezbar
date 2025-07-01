package widget

import (
	"fmt"
	"time"
	"encoding/json"
	"net/http"
	"io/ioutil"
	"strconv"
	
	"github.com/diamondburned/gotk4/pkg/glib/v2"
	"github.com/diamondburned/gotk4/pkg/gtk/v4"
	"github.com/diamondburned/gotk4/pkg/cairo"
	layershell "github.com/diamondburned/gotk4-layer-shell/pkg/gtk4layershell"
	
	"github.com/birdayz/ezbar/pkg/datasource"
)

type StockWidget struct {
	container        *gtk.Box
	symbolLabel      *gtk.Label
	priceLabel       *gtk.Label
	changeLabel      *gtk.Label
	trendLabel       *gtk.Label
	lastUpdate       time.Time
	animationTimer   *time.Timer
	isAnimating      bool
	pulseCount       int
	currentStockData *datasource.StockData
	chartOverlay     *gtk.Window
	chartDrawingArea *gtk.DrawingArea
	chartData        []float64
	hoverController  *gtk.EventControllerMotion
}

func NewStockWidget() *StockWidget {
	widget := &StockWidget{
		container:   gtk.NewBox(gtk.OrientationHorizontal, 8),
		symbolLabel: gtk.NewLabel(""),
		priceLabel:  gtk.NewLabel(""),
		changeLabel: gtk.NewLabel(""),
		trendLabel:  gtk.NewLabel(""),
		lastUpdate:  time.Now(),
	}
	
	// Configure container
	widget.container.SetHAlign(gtk.AlignStart)
	widget.container.SetVAlign(gtk.AlignCenter)
	widget.container.SetMarginStart(4)
	widget.container.SetMarginEnd(4)
	widget.container.SetMarginTop(2)
	widget.container.SetMarginBottom(2)
	
	// Style labels to match other panels (14px monospace)
	widget.trendLabel.SetText("📈")
	widget.symbolLabel.SetText("NASDAQ")
	widget.priceLabel.SetText("$0.00")
	widget.changeLabel.SetText("+0.00 (0.00%)")
	
	// Add all components to container
	widget.container.Append(widget.trendLabel)
	widget.container.Append(widget.symbolLabel)
	widget.container.Append(widget.priceLabel)
	widget.container.Append(widget.changeLabel)
	
	// Add click gesture for additional info
	gesture := gtk.NewGestureClick()
	gesture.SetButton(1) // Left mouse button
	gesture.ConnectPressed(func(n int, x, y float64) {
		widget.showDetailedInfo()
	})
	widget.container.AddController(gesture)
	
	// Add hover events for chart overlay
	widget.hoverController = gtk.NewEventControllerMotion()
	widget.hoverController.ConnectEnter(func(x, y float64) {
		widget.showChartOverlay()
	})
	widget.hoverController.ConnectLeave(func() {
		widget.hideChartOverlay()
	})
	widget.container.AddController(widget.hoverController)
	
	return widget
}

func (w *StockWidget) Update(value interface{}) {
	if stockData, ok := value.(datasource.StockData); ok {
		w.currentStockData = &stockData
		w.updateDisplay(stockData)
		w.triggerUpdateAnimation()
	}
}

func (w *StockWidget) updateDisplay(data datasource.StockData) {
	glib.IdleAdd(func() {
		// Update trend emoji
		w.trendLabel.SetText(data.TrendEmoji)
		
		// Update symbol
		w.symbolLabel.SetText(data.Symbol)
		
		// Update price with color based on trend
		var priceColor string
		if data.IsPositive {
			priceColor = "#00AA00" // Green
		} else if data.IsNegative {
			priceColor = "#FF4444" // Red
		} else {
			priceColor = "#FFFFFF" // White/default
		}
		
		w.priceLabel.SetMarkup(fmt.Sprintf("<span color='%s'>%s</span>", 
			priceColor, data.PriceString))
		
		// Update change with color and styling
		var changeColor string
		var changePrefix string
		if data.IsPositive {
			changeColor = "#00AA00"
			changePrefix = "▲ +"
		} else if data.IsNegative {
			changeColor = "#FF4444" 
			changePrefix = "▼ "
		} else {
			changeColor = "#AAAAAA"
			changePrefix = "● "
		}
		
		changeText := fmt.Sprintf("%.2f (%.2f%%)", data.Change, data.ChangePercent)
		w.changeLabel.SetMarkup(fmt.Sprintf("<span color='%s'>%s%s</span>", 
			changeColor, changePrefix, changeText))
	})
}

func (w *StockWidget) triggerUpdateAnimation() {
	if w.isAnimating {
		return
	}
	
	w.isAnimating = true
	w.pulseCount = 0
	w.startPulseAnimation()
}

func (w *StockWidget) startPulseAnimation() {
	if !w.isAnimating || w.pulseCount >= 3 {
		w.isAnimating = false
		return
	}
	
	// Create subtle pulse effect by modifying opacity
	opacity := 0.7
	if w.pulseCount%2 == 0 {
		opacity = 1.0
	}
	
	glib.IdleAdd(func() {
		w.container.SetOpacity(opacity)
	})
	
	w.pulseCount++
	w.animationTimer = time.AfterFunc(200*time.Millisecond, func() {
		w.startPulseAnimation()
	})
}

func (w *StockWidget) showDetailedInfo() {
	if w.currentStockData == nil {
		return
	}
	
	// Create a simple dialog with detailed stock information
	glib.IdleAdd(func() {
		// Create dialog
		dialog := gtk.NewDialog()
		dialog.SetTitle(fmt.Sprintf("%s Stock Details", w.currentStockData.Symbol))
		dialog.SetModal(true)
		dialog.SetResizable(false)
		
		// Create content
		content := dialog.ContentArea()
		content.SetSpacing(10)
		content.SetMarginStart(20)
		content.SetMarginEnd(20)
		content.SetMarginTop(10) 
		content.SetMarginBottom(10)
		
		// Add stock details
		details := []string{
			fmt.Sprintf("Symbol: %s", w.currentStockData.Symbol),
			fmt.Sprintf("Current Price: %s", w.currentStockData.PriceString),
			fmt.Sprintf("Change: %s", w.currentStockData.ChangeString),
			fmt.Sprintf("Trend: %s", w.currentStockData.TrendEmoji),
			fmt.Sprintf("Last Updated: %s", w.lastUpdate.Format("15:04:05")),
		}
		
		for _, detail := range details {
			label := gtk.NewLabel(detail)
			label.SetHAlign(gtk.AlignStart)
			content.Append(label)
		}
		
		// Add close button
		dialog.AddButton("Close", int(gtk.ResponseClose))
		dialog.ConnectResponse(func(responseID int) {
			dialog.Close()
		})
		
		dialog.Show()
	})
}

func (w *StockWidget) GetGTKWidget() *gtk.Widget {
	return &w.container.Widget
}

// Additional method to set custom styling
func (w *StockWidget) SetCustomStyle(backgroundColor, textColor string) {
	glib.IdleAdd(func() {
		// Apply custom CSS styling if needed
		cssProvider := gtk.NewCSSProvider()
		css := fmt.Sprintf(`
			box {
				background-color: %s;
				color: %s;
				border-radius: 6px;
				padding: 4px 8px;
				border: 1px solid rgba(255,255,255,0.1);
			}
		`, backgroundColor, textColor)
		
		cssProvider.LoadFromData(css)
		
		styleContext := w.container.StyleContext()
		styleContext.AddProvider(cssProvider, gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
	})
}

// Method to enable/disable animations
func (w *StockWidget) SetAnimationsEnabled(enabled bool) {
	if !enabled && w.isAnimating {
		w.isAnimating = false
		if w.animationTimer != nil {
			w.animationTimer.Stop()
		}
		glib.IdleAdd(func() {
			w.container.SetOpacity(1.0)
		})
	}
}

func (w *StockWidget) showChartOverlay() {
	if w.currentStockData == nil {
		return
	}
	
	glib.IdleAdd(func() {
		if w.chartOverlay != nil {
			return // Already showing
		}
		
		// Create overlay window using layer shell
		w.chartOverlay = gtk.NewWindow()
		w.chartOverlay.SetTitle(fmt.Sprintf("%s 7d Chart", w.currentStockData.Symbol))
		w.chartOverlay.SetDecorated(false)
		w.chartOverlay.SetModal(false)
		w.chartOverlay.SetResizable(false)
		w.chartOverlay.SetDefaultSize(500, 300)
		
		// Use layer shell to position above the bar on the correct monitor
		layershell.InitForWindow(w.chartOverlay)
		layershell.SetNamespace(w.chartOverlay, "stock-chart-overlay")
		layershell.SetAnchor(w.chartOverlay, layershell.LayerShellEdgeBottom, true)
		layershell.SetAnchor(w.chartOverlay, layershell.LayerShellEdgeRight, true)
		layershell.SetLayer(w.chartOverlay, layershell.LayerShellLayerOverlay)
		layershell.SetMargin(w.chartOverlay, layershell.LayerShellEdgeBottom, 40) // Above the bar
		layershell.SetMargin(w.chartOverlay, layershell.LayerShellEdgeRight, 20)  // Some padding from edge
		
		// Set the monitor to the one where this widget is located
		w.setOverlayToWidgetMonitor()
		
		// Create drawing area for chart
		w.chartDrawingArea = gtk.NewDrawingArea()
		w.chartDrawingArea.SetSizeRequest(500, 300)
		w.chartDrawingArea.SetDrawFunc(w.drawChart)
		
		w.chartOverlay.SetChild(w.chartDrawingArea)
		w.chartOverlay.Show()
		
		// Fetch chart data after overlay is shown
		go w.fetchChartData()
	})
}

func (w *StockWidget) hideChartOverlay() {
	glib.IdleAdd(func() {
		if w.chartOverlay != nil {
			w.chartOverlay.Close()
			w.chartOverlay = nil
			w.chartDrawingArea = nil
		}
	})
}

func (w *StockWidget) fetchChartData() {
	if w.currentStockData == nil {
		return
	}
	
	// Use Yahoo Finance for 7-day data with 1-hour intervals
	url := fmt.Sprintf("https://query1.finance.yahoo.com/v8/finance/chart/%s?interval=1h&range=7d", w.currentStockData.Symbol)
	
	client := &http.Client{Timeout: 15 * time.Second}
	req, err := http.NewRequest("GET", url, nil)
	if err != nil {
		return
	}
	
	req.Header.Set("User-Agent", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36")
	req.Header.Set("Accept", "application/json")
	
	resp, err := client.Do(req)
	if err != nil {
		return
	}
	defer resp.Body.Close()
	
	body, err := ioutil.ReadAll(resp.Body)
	if err != nil {
		return
	}
	
	var chartResponse struct {
		Chart struct {
			Result []struct {
				Indicators struct {
					Quote []struct {
						Close []interface{} `json:"close"`
					} `json:"quote"`
				} `json:"indicators"`
			} `json:"result"`
		} `json:"chart"`
	}
	
	if err := json.Unmarshal(body, &chartResponse); err != nil {
		return
	}
	
	if len(chartResponse.Chart.Result) == 0 || len(chartResponse.Chart.Result[0].Indicators.Quote) == 0 {
		return
	}
	
	closes := chartResponse.Chart.Result[0].Indicators.Quote[0].Close
	w.chartData = make([]float64, 0, len(closes))
	
	for _, close := range closes {
		if close != nil {
			if price, ok := close.(float64); ok {
				w.chartData = append(w.chartData, price)
			} else if priceStr, ok := close.(string); ok {
				if price, err := strconv.ParseFloat(priceStr, 64); err == nil {
					w.chartData = append(w.chartData, price)
				}
			}
		}
	}
	
	// Redraw chart if overlay is visible
	glib.IdleAdd(func() {
		if w.chartDrawingArea != nil {
			w.chartDrawingArea.QueueDraw()
		}
	})
}

func (w *StockWidget) drawChart(area *gtk.DrawingArea, cr *cairo.Context, width, height int) {
	if len(w.chartData) < 2 {
		// Draw "Loading..." text
		cr.SetSourceRGB(1, 1, 1)
		cr.SetFontSize(16)
		cr.MoveTo(float64(width/2-50), float64(height/2))
		cr.ShowText("Loading 7d chart...")
		return
	}
	
	// Set background
	cr.SetSourceRGB(0.05, 0.05, 0.05)
	cr.Rectangle(0, 0, float64(width), float64(height))
	cr.Fill()
	
	// Chart margins for axes
	leftMargin := 60.0
	rightMargin := 20.0
	topMargin := 30.0
	bottomMargin := 40.0
	
	chartWidth := float64(width) - leftMargin - rightMargin
	chartHeight := float64(height) - topMargin - bottomMargin
	
	// Calculate price range
	minPrice, maxPrice := w.chartData[0], w.chartData[0]
	for _, price := range w.chartData {
		if price < minPrice {
			minPrice = price
		}
		if price > maxPrice {
			maxPrice = price
		}
	}
	
	if maxPrice == minPrice {
		maxPrice = minPrice + 1 // Avoid division by zero
	}
	
	// Add some padding to price range
	priceRange := maxPrice - minPrice
	minPrice -= priceRange * 0.05
	maxPrice += priceRange * 0.05
	
	// Draw axes
	cr.SetSourceRGB(0.3, 0.3, 0.3)
	cr.SetLineWidth(1)
	
	// Y-axis (left)
	cr.MoveTo(leftMargin, topMargin)
	cr.LineTo(leftMargin, topMargin+chartHeight)
	
	// X-axis (bottom)
	cr.MoveTo(leftMargin, topMargin+chartHeight)
	cr.LineTo(leftMargin+chartWidth, topMargin+chartHeight)
	cr.Stroke()
	
	// Draw Y-axis price labels
	cr.SetSourceRGB(0.7, 0.7, 0.7)
	cr.SetFontSize(10)
	
	numYTicks := 5
	for i := 0; i <= numYTicks; i++ {
		ratio := float64(i) / float64(numYTicks)
		price := minPrice + (maxPrice-minPrice)*ratio
		y := topMargin + chartHeight - ratio*chartHeight
		
		// Tick mark
		cr.SetSourceRGB(0.3, 0.3, 0.3)
		cr.MoveTo(leftMargin-5, y)
		cr.LineTo(leftMargin, y)
		cr.Stroke()
		
		// Price label
		cr.SetSourceRGB(0.7, 0.7, 0.7)
		cr.MoveTo(5, y+3)
		cr.ShowText(fmt.Sprintf("$%.2f", price))
	}
	
	// Draw X-axis time labels (simplified for 7 days)
	numXTicks := 7
	days := []string{"7d", "6d", "5d", "4d", "3d", "2d", "1d"}
	for i := 0; i <= numXTicks; i++ {
		if i >= len(days) {
			continue
		}
		x := leftMargin + (float64(i)/float64(numXTicks))*chartWidth
		
		// Tick mark
		cr.SetSourceRGB(0.3, 0.3, 0.3)
		cr.MoveTo(x, topMargin+chartHeight)
		cr.LineTo(x, topMargin+chartHeight+5)
		cr.Stroke()
		
		// Day label
		cr.SetSourceRGB(0.7, 0.7, 0.7)
		cr.MoveTo(x-8, topMargin+chartHeight+18)
		cr.ShowText(days[i])
	}
	
	// Draw grid lines
	cr.SetSourceRGB(0.15, 0.15, 0.15)
	cr.SetLineWidth(0.5)
	
	// Horizontal grid lines
	for i := 1; i < numYTicks; i++ {
		ratio := float64(i) / float64(numYTicks)
		y := topMargin + chartHeight - ratio*chartHeight
		cr.MoveTo(leftMargin, y)
		cr.LineTo(leftMargin+chartWidth, y)
		cr.Stroke()
	}
	
	// Vertical grid lines
	for i := 1; i < numXTicks; i++ {
		x := leftMargin + (float64(i)/float64(numXTicks))*chartWidth
		cr.MoveTo(x, topMargin)
		cr.LineTo(x, topMargin+chartHeight)
		cr.Stroke()
	}
	
	// Draw chart line
	firstPrice := w.chartData[0]
	lastPrice := w.chartData[len(w.chartData)-1]
	
	// Color based on overall trend
	if lastPrice >= firstPrice {
		cr.SetSourceRGB(0.2, 0.8, 0.2) // Green for up
	} else {
		cr.SetSourceRGB(0.8, 0.2, 0.2) // Red for down
	}
	cr.SetLineWidth(2)
	
	for i, price := range w.chartData {
		x := leftMargin + (float64(i)/float64(len(w.chartData)-1))*chartWidth
		y := topMargin + chartHeight - ((price-minPrice)/(maxPrice-minPrice))*chartHeight
		
		if i == 0 {
			cr.MoveTo(x, y)
		} else {
			cr.LineTo(x, y)
		}
	}
	cr.Stroke()
	
	// Draw title
	cr.SetSourceRGB(1, 1, 1)
	cr.SetFontSize(14)
	title := fmt.Sprintf("%s - 7 Day Chart", w.currentStockData.Symbol)
	cr.MoveTo(leftMargin, 20)
	cr.ShowText(title)
	
	// Current price indicator
	if len(w.chartData) > 0 {
		currentPrice := w.chartData[len(w.chartData)-1]
		changeFromStart := currentPrice - firstPrice
		changePercent := (changeFromStart / firstPrice) * 100
		
		cr.SetFontSize(12)
		if changeFromStart >= 0 {
			cr.SetSourceRGB(0.2, 0.8, 0.2)
			cr.MoveTo(leftMargin+chartWidth-100, 20)
			cr.ShowText(fmt.Sprintf("▲ +%.2f%%", changePercent))
		} else {
			cr.SetSourceRGB(0.8, 0.2, 0.2)
			cr.MoveTo(leftMargin+chartWidth-100, 20)
			cr.ShowText(fmt.Sprintf("▼ %.2f%%", changePercent))
		}
	}
}

func (w *StockWidget) setOverlayToWidgetMonitor() {
	// Get the toplevel window that contains this widget
	toplevel := w.container.Root()
	if toplevel == nil {
		return
	}
	
	// Try to get the native surface of the toplevel window  
	widget := &w.container.Widget
	native := widget.Native()
	if native == nil {
		return
	}
	
	surface := native.Surface()
	if surface == nil {
		return
	}
	
	// Get display from the widget directly
	display := w.container.Display()
	if display == nil {
		return
	}
	
	// Get the monitor that contains this surface
	monitor := display.MonitorAtSurface(surface)
	if monitor != nil {
		layershell.SetMonitor(w.chartOverlay, monitor)
	}
}

