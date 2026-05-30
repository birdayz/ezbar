package widget

import (
	"fmt"
	"time"

	layershell "github.com/diamondburned/gotk4-layer-shell/pkg/gtk4layershell"
	"github.com/diamondburned/gotk4/pkg/glib/v2"
	"github.com/diamondburned/gotk4/pkg/gtk/v4"

	"github.com/birdayz/ezbar/pkg/datasource"
)

type CalendarWidget struct {
	container       *gtk.Box
	iconLabel       *gtk.Label
	textLabel       *gtk.Label
	timeLabel       *gtk.Label
	currentData     *datasource.CalendarData
	hoverPopup      *gtk.Window
	hoverController *gtk.EventControllerMotion
	blinkTimer      *time.Timer
	blinkState      bool
	blinkMuted      bool
	mutedEventTitle string // Track which event was muted so we unmute for new events
}

func NewCalendarWidget() *CalendarWidget {
	widget := &CalendarWidget{}

	// Create container
	widget.container = gtk.NewBox(gtk.OrientationHorizontal, 4)
	widget.container.SetHAlign(gtk.AlignStart)
	widget.container.SetVAlign(gtk.AlignCenter)
	widget.container.SetMarginStart(4)
	widget.container.SetMarginEnd(4)

	// Icon label
	widget.iconLabel = gtk.NewLabel("📅")
	widget.iconLabel.SetHAlign(gtk.AlignStart)

	// Text label for event title
	widget.textLabel = gtk.NewLabel("Loading...")
	widget.textLabel.SetHAlign(gtk.AlignStart)
	widget.textLabel.SetSingleLineMode(true)

	// Time label for countdown
	widget.timeLabel = gtk.NewLabel("")
	widget.timeLabel.SetHAlign(gtk.AlignStart)

	widget.container.Append(widget.iconLabel)
	widget.container.Append(widget.textLabel)
	widget.container.Append(widget.timeLabel)

	// Add hover controller for popup
	widget.hoverController = gtk.NewEventControllerMotion()
	widget.hoverController.ConnectEnter(func(x, y float64) {
		widget.showHoverPopup()
	})
	widget.hoverController.ConnectLeave(func() {
		widget.hideHoverPopup()
	})
	widget.container.AddController(widget.hoverController)

	// Add click handler to mute blinking
	clickGesture := gtk.NewGestureClick()
	clickGesture.SetButton(1)
	clickGesture.ConnectPressed(func(n int, x, y float64) {
		widget.muteBlinking()
	})
	widget.container.AddController(clickGesture)

	return widget
}

func (w *CalendarWidget) Update(value interface{}) {
	if data, ok := value.(datasource.CalendarData); ok {
		w.currentData = &data
		w.updateDisplay(data)
	}
}

func (w *CalendarWidget) updateDisplay(data datasource.CalendarData) {
	glib.IdleAdd(func() {
		// Stop any existing blink timer
		if w.blinkTimer != nil {
			w.blinkTimer.Stop()
			w.blinkTimer = nil
		}

		// Unmute if the event changed (so we blink for new events)
		currentEventTitle := ""
		if data.NextEvent != nil {
			currentEventTitle = data.NextEvent.Title
		}
		if w.mutedEventTitle != "" && w.mutedEventTitle != currentEventTitle {
			w.blinkMuted = false
			w.mutedEventTitle = ""
		}

		// Update text
		w.textLabel.SetText(data.DisplayText)

		// Update time countdown
		if data.TimeUntilNext != "" {
			w.timeLabel.SetText(fmt.Sprintf("[%s]", data.TimeUntilNext))
			w.timeLabel.SetVisible(true)
		} else {
			w.timeLabel.SetVisible(false)
		}

		// Apply styling based on urgency (skip if muted)
		w.textLabel.RemoveCSSClass("calendar-urgent")
		w.textLabel.RemoveCSSClass("calendar-overdue")
		w.timeLabel.RemoveCSSClass("calendar-urgent")
		w.timeLabel.RemoveCSSClass("calendar-overdue")

		if !w.blinkMuted {
			if data.IsOverdue {
				w.textLabel.AddCSSClass("calendar-overdue")
				w.timeLabel.AddCSSClass("calendar-overdue")
				w.startBlinking()
			} else if data.IsUrgent {
				w.textLabel.AddCSSClass("calendar-urgent")
				w.timeLabel.AddCSSClass("calendar-urgent")
				w.startBlinking()
			}
		}
	})
}

func (w *CalendarWidget) startBlinking() {
	if w.blinkMuted {
		return
	}
	w.blinkState = true
	w.blinkTimer = time.AfterFunc(500*time.Millisecond, w.toggleBlink)
}

func (w *CalendarWidget) muteBlinking() {
	if w.blinkTimer != nil {
		w.blinkTimer.Stop()
		w.blinkTimer = nil
	}
	w.blinkMuted = true
	if w.currentData != nil && w.currentData.NextEvent != nil {
		w.mutedEventTitle = w.currentData.NextEvent.Title
	}
	// Reset opacity and remove urgent/overdue styling
	glib.IdleAdd(func() {
		w.container.SetOpacity(1.0)
		w.textLabel.RemoveCSSClass("calendar-urgent")
		w.textLabel.RemoveCSSClass("calendar-overdue")
		w.timeLabel.RemoveCSSClass("calendar-urgent")
		w.timeLabel.RemoveCSSClass("calendar-overdue")
	})
}

func (w *CalendarWidget) toggleBlink() {
	if w.blinkTimer == nil {
		return
	}

	w.blinkState = !w.blinkState
	glib.IdleAdd(func() {
		if w.blinkState {
			w.container.SetOpacity(1.0)
		} else {
			w.container.SetOpacity(0.5)
		}
	})

	// Schedule next toggle
	w.blinkTimer = time.AfterFunc(500*time.Millisecond, w.toggleBlink)
}

func (w *CalendarWidget) GetGTKWidget() *gtk.Widget {
	return &w.container.Widget
}

func (w *CalendarWidget) SetClickHandler(handler func()) {
	gesture := gtk.NewGestureClick()
	gesture.SetButton(1)
	gesture.ConnectPressed(func(n int, x, y float64) {
		handler()
	})
	w.container.AddController(gesture)
}

func (w *CalendarWidget) showHoverPopup() {
	if w.currentData == nil || len(w.currentData.TodayEvents) == 0 {
		return
	}

	glib.IdleAdd(func() {
		if w.hoverPopup != nil {
			return // Already showing
		}

		w.hoverPopup = gtk.NewWindow()
		w.hoverPopup.SetTitle("Today's Meetings")
		w.hoverPopup.SetDecorated(false)
		w.hoverPopup.SetModal(false)
		w.hoverPopup.SetResizable(false)
		w.hoverPopup.SetDefaultSize(350, -1)

		// Use layer shell
		layershell.InitForWindow(w.hoverPopup)
		layershell.SetNamespace(w.hoverPopup, "calendar-popup")
		layershell.SetLayer(w.hoverPopup, layershell.LayerShellLayerOverlay)

		// Position popup above the widget
		w.positionPopupAboveWidget()

		// Create content
		vbox := gtk.NewBox(gtk.OrientationVertical, 8)
		vbox.SetMarginTop(12)
		vbox.SetMarginBottom(12)
		vbox.SetMarginStart(12)
		vbox.SetMarginEnd(12)
		vbox.AddCSSClass("calendar-popup")

		// Title
		title := gtk.NewLabel("Today's Meetings")
		title.AddCSSClass("calendar-popup-title")
		title.SetHAlign(gtk.AlignStart)
		vbox.Append(title)

		// Separator
		sep := gtk.NewSeparator(gtk.OrientationHorizontal)
		vbox.Append(sep)

		// Events list
		now := time.Now()
		hasEvents := false

		for _, event := range w.currentData.TodayEvents {
			if event.IsAllDay {
				// All-day event
				eventBox := w.createEventRow(event, now, true)
				vbox.Append(eventBox)
				hasEvents = true
			}
		}

		for _, event := range w.currentData.TodayEvents {
			if !event.IsAllDay {
				eventBox := w.createEventRow(event, now, false)
				vbox.Append(eventBox)
				hasEvents = true
			}
		}

		if !hasEvents {
			noEvents := gtk.NewLabel("No meetings today")
			noEvents.SetHAlign(gtk.AlignCenter)
			vbox.Append(noEvents)
		}

		w.hoverPopup.SetChild(vbox)
		w.hoverPopup.Show()
	})
}

func (w *CalendarWidget) createEventRow(event datasource.CalendarEvent, now time.Time, isAllDay bool) *gtk.Box {
	row := gtk.NewBox(gtk.OrientationHorizontal, 8)
	row.SetMarginTop(4)
	row.SetMarginBottom(4)

	var timeStr string
	var statusIcon string

	if isAllDay {
		timeStr = "All day"
		statusIcon = "  "
	} else {
		timeStr = event.StartTime.Format("15:04")

		// Determine status
		if now.After(event.EndTime) {
			statusIcon = "  " // Past
			row.AddCSSClass("calendar-event-past")
		} else if now.After(event.StartTime) {
			statusIcon = ">" // Ongoing
			row.AddCSSClass("calendar-event-ongoing")
		} else if event.StartTime.Sub(now) <= 15*time.Minute {
			statusIcon = "!" // Soon
			row.AddCSSClass("calendar-event-soon")
		} else {
			statusIcon = " " // Future
		}
	}

	// Status indicator
	statusLabel := gtk.NewLabel(statusIcon)
	statusLabel.SetHAlign(gtk.AlignStart)
	row.Append(statusLabel)

	// Time
	timeLabel := gtk.NewLabel(timeStr)
	timeLabel.SetHAlign(gtk.AlignStart)
	timeLabel.SetSizeRequest(50, -1)
	row.Append(timeLabel)

	// Title
	titleLabel := gtk.NewLabel(truncateEventTitle(event.Title, 35))
	titleLabel.SetHAlign(gtk.AlignStart)
	titleLabel.SetHExpand(true)
	row.Append(titleLabel)

	return row
}

func truncateEventTitle(title string, maxLen int) string {
	if len(title) <= maxLen {
		return title
	}
	return title[:maxLen-2] + ".."
}

func (w *CalendarWidget) hideHoverPopup() {
	glib.IdleAdd(func() {
		if w.hoverPopup != nil {
			w.hoverPopup.Close()
			w.hoverPopup = nil
		}
	})
}

func (w *CalendarWidget) positionPopupAboveWidget() {
	if w.hoverPopup == nil {
		return
	}

	widget := &w.container.Widget
	native := widget.Native()
	if native == nil {
		return
	}

	surface := native.Surface()
	if surface == nil {
		return
	}

	display := w.container.Display()
	if display == nil {
		return
	}

	monitor := display.MonitorAtSurface(surface)
	if monitor == nil {
		return
	}

	layershell.SetMonitor(w.hoverPopup, monitor)

	// Get monitor geometry
	monitorGeom := monitor.Geometry()
	monitorWidth := monitorGeom.Width()

	// Get widget position - for layer-shell bar, compute bounds relative to native surface
	bounds, ok := widget.ComputeBounds(&native.Widget)
	if !ok {
		return
	}

	widgetX := int(bounds.X())
	widgetWidth := int(bounds.Width())

	// Calculate right margin to center popup on widget
	popupWidth := 350
	widgetCenterX := widgetX + widgetWidth/2
	popupLeftX := widgetCenterX - popupWidth/2

	// Right margin = screen width - popup right edge
	rightMargin := monitorWidth - (popupLeftX + popupWidth)
	if rightMargin < 0 {
		rightMargin = 0
	}

	// Anchor to bottom-right and use margins to position
	layershell.SetAnchor(w.hoverPopup, layershell.LayerShellEdgeBottom, true)
	layershell.SetAnchor(w.hoverPopup, layershell.LayerShellEdgeRight, true)
	layershell.SetMargin(w.hoverPopup, layershell.LayerShellEdgeBottom, 40) // Above the bar
	layershell.SetMargin(w.hoverPopup, layershell.LayerShellEdgeRight, rightMargin)
}
