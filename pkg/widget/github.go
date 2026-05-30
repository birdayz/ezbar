package widget

import (
	"fmt"
	"os/exec"
	"time"


	layershell "github.com/diamondburned/gotk4-layer-shell/pkg/gtk4layershell"
	"github.com/diamondburned/gotk4/pkg/gdk/v4"
	"github.com/diamondburned/gotk4/pkg/glib/v2"
	"github.com/diamondburned/gotk4/pkg/gtk/v4"

	"github.com/birdayz/ezbar/pkg/datasource"
)

type GitHubWidget struct {
	container       *gtk.Box
	textLabel       *gtk.Label
	currentData     *datasource.GitHubData
	dataSource      *datasource.GitHubDataSource
	hoverPopup      *gtk.Window
	hoverController *gtk.EventControllerMotion
	closeTimer      *time.Timer
	lastCount       int
	blinkTimer      *time.Timer
	blinkState      bool
	blinkMuted      bool
	blinkStart      time.Time
}

func (w *GitHubWidget) SetDataSource(ds *datasource.GitHubDataSource) {
	w.dataSource = ds
}

func NewGitHubWidget() *GitHubWidget {
	w := &GitHubWidget{}

	w.container = gtk.NewBox(gtk.OrientationHorizontal, 4)
	w.container.SetHAlign(gtk.AlignStart)
	w.container.SetVAlign(gtk.AlignCenter)
	w.container.SetMarginStart(4)
	w.container.SetMarginEnd(4)

	w.textLabel = gtk.NewLabel("GH ...")
	w.textLabel.SetHAlign(gtk.AlignStart)
	w.textLabel.SetSingleLineMode(true)
	w.container.Append(w.textLabel)

	// Hover for popup
	w.hoverController = gtk.NewEventControllerMotion()
	w.hoverController.ConnectEnter(func(x, y float64) {
		w.cancelClose()
		w.showHoverPopup()
	})
	w.hoverController.ConnectLeave(func() {
		w.scheduleClose()
	})
	w.container.AddController(w.hoverController)

	// Click to mute blinking
	clickGesture := gtk.NewGestureClick()
	clickGesture.SetButton(1)
	clickGesture.ConnectPressed(func(n int, x, y float64) {
		w.muteBlinking()
	})
	w.container.AddController(clickGesture)

	return w
}

func (w *GitHubWidget) scheduleClose() {
	w.cancelClose()
	w.closeTimer = time.AfterFunc(300*time.Millisecond, func() {
		glib.IdleAdd(func() {
			w.doClose()
		})
	})
}

func (w *GitHubWidget) cancelClose() {
	if w.closeTimer != nil {
		w.closeTimer.Stop()
		w.closeTimer = nil
	}
}

func (w *GitHubWidget) doClose() {
	if w.hoverPopup != nil {
		w.hoverPopup.Close()
		w.hoverPopup = nil
	}
}

func (w *GitHubWidget) Update(value interface{}) {
	if data, ok := value.(datasource.GitHubData); ok {
		newItems := data.Count > w.lastCount && w.lastCount >= 0
		w.lastCount = data.Count
		w.currentData = &data
		glib.IdleAdd(func() {
			w.textLabel.SetText(data.DisplayText)

			w.textLabel.RemoveCSSClass("github-has-notifications")
			w.textLabel.RemoveCSSClass("github-new-notifications")
			if data.Count > 0 {
				w.textLabel.AddCSSClass("github-has-notifications")
			}

			if newItems {
				w.blinkMuted = false
				w.textLabel.AddCSSClass("github-new-notifications")
				w.startBlinking()
			}
		})
	}
}

func (w *GitHubWidget) startBlinking() {
	if w.blinkTimer != nil {
		w.blinkTimer.Stop()
	}
	w.blinkState = true
	w.blinkStart = time.Now()
	w.blinkTimer = time.AfterFunc(500*time.Millisecond, w.toggleBlink)
}

func (w *GitHubWidget) toggleBlink() {
	if w.blinkTimer == nil || w.blinkMuted {
		return
	}
	// Stop blinking after 30s, keep red/bold
	if time.Since(w.blinkStart) > 30*time.Second {
		w.blinkTimer = nil
		glib.IdleAdd(func() {
			w.container.SetOpacity(1.0)
		})
		return
	}
	w.blinkState = !w.blinkState
	glib.IdleAdd(func() {
		if w.blinkState {
			w.container.SetOpacity(1.0)
		} else {
			w.container.SetOpacity(0.4)
		}
	})
	w.blinkTimer = time.AfterFunc(500*time.Millisecond, w.toggleBlink)
}

func (w *GitHubWidget) muteBlinking() {
	if w.blinkTimer != nil {
		w.blinkTimer.Stop()
		w.blinkTimer = nil
	}
	w.blinkMuted = true
	glib.IdleAdd(func() {
		w.container.SetOpacity(1.0)
		w.textLabel.RemoveCSSClass("github-new-notifications")
	})
}

func (w *GitHubWidget) GetGTKWidget() *gtk.Widget {
	return &w.container.Widget
}

func (w *GitHubWidget) showHoverPopup() {
	if w.currentData == nil || len(w.currentData.Notifications) == 0 {
		return
	}

	glib.IdleAdd(func() {
		if w.hoverPopup != nil {
			return
		}

		w.hoverPopup = gtk.NewWindow()
		w.hoverPopup.SetTitle("GitHub Notifications")
		w.hoverPopup.SetDecorated(false)
		w.hoverPopup.SetModal(false)
		w.hoverPopup.SetResizable(false)
		w.hoverPopup.SetDefaultSize(450, -1)

		layershell.InitForWindow(w.hoverPopup)
		layershell.SetNamespace(w.hoverPopup, "github-popup")
		layershell.SetLayer(w.hoverPopup, layershell.LayerShellLayerOverlay)

		w.positionPopup()

		// Keep popup open when hovering over it
		popupHover := gtk.NewEventControllerMotion()
		popupHover.ConnectEnter(func(x, y float64) {
			w.cancelClose()
		})
		popupHover.ConnectLeave(func() {
			w.scheduleClose()
		})

		vbox := gtk.NewBox(gtk.OrientationVertical, 4)
		vbox.SetMarginTop(12)
		vbox.SetMarginBottom(12)
		vbox.SetMarginStart(12)
		vbox.SetMarginEnd(12)
		vbox.AddCSSClass("github-popup")
		vbox.AddController(popupHover)

		// Header with title and mark-all-read button
		headerBox := gtk.NewBox(gtk.OrientationHorizontal, 8)
		title := gtk.NewLabel(fmt.Sprintf("GitHub Notifications (%d)", w.currentData.Count))
		title.AddCSSClass("github-popup-title")
		title.SetHAlign(gtk.AlignStart)
		title.SetHExpand(true)
		headerBox.Append(title)

		if w.dataSource != nil {
			markAllBtn := gtk.NewLabel("[clear all]")
			markAllBtn.AddCSSClass("github-mark-all-read")
			markAllBtn.SetHAlign(gtk.AlignEnd)
			markAllGesture := gtk.NewGestureClick()
			markAllGesture.SetButton(1)
			markAllGesture.ConnectPressed(func(n int, x, y float64) {
				w.dataSource.MarkAllAsRead()
				w.doClose()
			})
			markAllBtn.AddController(markAllGesture)
			markAllBtn.SetCursor(gdk.NewCursorFromName("pointer", nil))
			headerBox.Append(markAllBtn)
		}
		vbox.Append(headerBox)

		sep := gtk.NewSeparator(gtk.OrientationHorizontal)
		vbox.Append(sep)

		// Group by reason
		byReason := make(map[string][]datasource.GitHubNotification)
		for _, n := range w.currentData.Notifications {
			byReason[n.Reason] = append(byReason[n.Reason], n)
		}

		reasonOrder := []string{"review_requested", "mention", "assign", "author", "comment", "state_change", "manual", "subscribed"}
		for _, reason := range reasonOrder {
			notifications, ok := byReason[reason]
			if !ok {
				continue
			}

			reasonLabel := gtk.NewLabel(fmt.Sprintf("%s (%d)", reasonDisplayName(reason), len(notifications)))
			reasonLabel.AddCSSClass("github-reason-header")
			reasonLabel.SetHAlign(gtk.AlignStart)
			vbox.Append(reasonLabel)

			limit := 10
			if len(notifications) < limit {
				limit = len(notifications)
			}
			for _, n := range notifications[:limit] {
				row := w.createNotificationRow(n)
				vbox.Append(row)
			}
			if len(notifications) > 10 {
				more := gtk.NewLabel(fmt.Sprintf("  ... and %d more", len(notifications)-10))
				more.SetHAlign(gtk.AlignStart)
				more.AddCSSClass("github-more")
				vbox.Append(more)
			}
		}

		w.hoverPopup.SetChild(vbox)
		w.hoverPopup.Show()
	})
}

func (w *GitHubWidget) createNotificationRow(n datasource.GitHubNotification) *gtk.Box {
	row := gtk.NewBox(gtk.OrientationHorizontal, 8)
	row.SetMarginTop(2)
	row.SetMarginBottom(2)

	// Type icon
	var icon string
	switch n.Type {
	case "PullRequest":
		icon = "PR"
	case "Issue":
		icon = "IS"
	case "Release":
		icon = "RE"
	default:
		icon = "  "
	}

	iconLabel := gtk.NewLabel(icon)
	iconLabel.AddCSSClass("github-type-icon")
	iconLabel.SetHAlign(gtk.AlignStart)
	row.Append(iconLabel)

	// Repo (short)
	repoName := n.RepoName
	for i := len(repoName) - 1; i >= 0; i-- {
		if repoName[i] == '/' {
			repoName = repoName[i+1:]
			break
		}
	}
	if len(repoName) > 15 {
		repoName = repoName[:13] + ".."
	}

	repoLabel := gtk.NewLabel(repoName)
	repoLabel.AddCSSClass("github-repo")
	repoLabel.SetHAlign(gtk.AlignStart)
	repoLabel.SetSizeRequest(110, -1)
	row.Append(repoLabel)

	// Title
	titleText := n.Title
	if len(titleText) > 45 {
		titleText = titleText[:43] + ".."
	}
	titleLabel := gtk.NewLabel(titleText)
	titleLabel.SetHAlign(gtk.AlignStart)
	titleLabel.SetHExpand(true)
	row.Append(titleLabel)

	// Time ago
	agoLabel := gtk.NewLabel(timeAgo(n.UpdatedAt))
	agoLabel.AddCSSClass("github-time-ago")
	agoLabel.SetHAlign(gtk.AlignEnd)
	row.Append(agoLabel)

	// Left click: open in browser + mark as read
	if n.HTMLURL != "" {
		url := n.HTMLURL
		id := n.ID
		leftClick := gtk.NewGestureClick()
		leftClick.SetButton(1)
		leftClick.ConnectPressed(func(nPress int, x, y float64) {
			exec.Command("xdg-open", url).Start()
			if w.dataSource != nil {
				w.dataSource.MarkAsRead(id)
			}
		})
		row.AddController(leftClick)
	}

	// Middle click: mark as read (dismiss) without opening
	if w.dataSource != nil {
		id := n.ID
		midClick := gtk.NewGestureClick()
		midClick.SetButton(2)
		midClick.ConnectPressed(func(nPress int, x, y float64) {
			w.dataSource.MarkAsRead(id)
		})
		row.AddController(midClick)
	}

	row.SetCursor(gdk.NewCursorFromName("pointer", nil))
	row.AddCSSClass("github-row-clickable")

	return row
}

func (w *GitHubWidget) positionPopup() {
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

	monitorGeom := monitor.Geometry()
	monitorWidth := monitorGeom.Width()

	bounds, ok := widget.ComputeBounds(&native.Widget)
	if !ok {
		return
	}

	widgetX := int(bounds.X())
	widgetWidth := int(bounds.Width())

	popupWidth := 450
	widgetCenterX := widgetX + widgetWidth/2
	popupLeftX := widgetCenterX - popupWidth/2

	rightMargin := monitorWidth - (popupLeftX + popupWidth)
	if rightMargin < 0 {
		rightMargin = 0
	}

	layershell.SetAnchor(w.hoverPopup, layershell.LayerShellEdgeBottom, true)
	layershell.SetAnchor(w.hoverPopup, layershell.LayerShellEdgeRight, true)
	layershell.SetMargin(w.hoverPopup, layershell.LayerShellEdgeBottom, 40)
	layershell.SetMargin(w.hoverPopup, layershell.LayerShellEdgeRight, rightMargin)
}

func timeAgo(t time.Time) string {
	d := time.Since(t)
	switch {
	case d < time.Minute:
		return "now"
	case d < time.Hour:
		return fmt.Sprintf("%dm", int(d.Minutes()))
	case d < 24*time.Hour:
		return fmt.Sprintf("%dh", int(d.Hours()))
	default:
		return fmt.Sprintf("%dd", int(d.Hours()/24))
	}
}

func reasonDisplayName(reason string) string {
	switch reason {
	case "review_requested":
		return "Review Requested"
	case "mention":
		return "Mentioned"
	case "assign":
		return "Assigned"
	case "author":
		return "Your PRs/Issues"
	case "comment":
		return "Comments"
	case "state_change":
		return "State Changes"
	case "manual":
		return "Subscribed (manual)"
	case "subscribed":
		return "Watching"
	default:
		return reason
	}
}
