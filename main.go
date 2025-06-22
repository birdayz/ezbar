package main

import (
	"cmp"
	"context"
	"fmt"
	"html"
	"io/ioutil"
	"log/slog"
	"os"
	"os/exec"
	"os/signal"
	"slices"
	"strconv"
	"strings"
	"time"

	layershell "github.com/diamondburned/gotk4-layer-shell/pkg/gtk4layershell"
	"github.com/diamondburned/gotk4/pkg/gdk/v4"
	"github.com/diamondburned/gotk4/pkg/gio/v2"
	"github.com/diamondburned/gotk4/pkg/glib/v2"
	"github.com/diamondburned/gotk4/pkg/gtk/v4"
	"github.com/joshuarubin/go-sway"
)

var workspaceLabel *gtk.Label
var timeLabel *gtk.Label
var batteryLabel *gtk.Label
var batterySeparator *gtk.Label

func main() {
	log := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{}))

	if os.Getenv("EZBAR_CHILD") == "1" {
		run(log.With("component", "ezbar"))
		return
	}

	log = log.With("component", "launcher")
	restartLoop(log)
}

func restartLoop(log *slog.Logger) {
	for {
		cmd := exec.Command(os.Args[0])
		cmd.Env = append(os.Environ(), "EZBAR_CHILD=1")
		cmd.Stdout = os.Stdout
		cmd.Stderr = os.Stderr

		log.Info("Spawning new bar subprocess...")
		if err := cmd.Run(); err != nil {
			log.Error("Child process crashed:", "err", err)
		} else {
			log.Info("Child exited cleanly")
		}
	}

}

func run(log *slog.Logger) {
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt)
	defer cancel()

	app := gtk.NewApplication("de.nerden.ezbar", gio.ApplicationNonUnique)
	app.ConnectActivate(func() {
		w := activate(log, app)
		w.Show()
	})

	go func() {
		<-ctx.Done()
		glib.IdleAdd(app.Quit)
	}()

	if code := app.Run(os.Args); code > 0 {
		cancel()
		os.Exit(code)
	}
}

func activate(log *slog.Logger, app *gtk.Application) *gtk.Window {
	window := gtk.NewApplicationWindow(app)
	window.SetTitle("ezbar")
	window.SetDecorated(false)
	window.SetName("bar-window")

	mainBox := gtk.NewBox(gtk.OrientationHorizontal, 0)
	window.SetChild(mainBox)

	timeLabel = gtk.NewLabel("Loading…")
	batteryLabel = gtk.NewLabel("🔋 --")

	workspaceLabel = gtk.NewLabel("Starting...")
	workspaceLabel.AddCSSClass("bar-label")

	workspaceLabel.SetHAlign(gtk.AlignCenter)
	workspaceLabel.SetVAlign(gtk.AlignCenter)
	workspaceLabel.SetMarginStart(0)
	workspaceLabel.SetMarginEnd(0)
	workspaceLabel.SetMarginTop(0)
	workspaceLabel.SetMarginBottom(0)

	workspaceLabel.SetSingleLineMode(true)

	centerLabel := gtk.NewLabel("Centered Label")

	leftBox := gtk.NewBox(gtk.OrientationHorizontal, 0)
	leftBox.SetHAlign(gtk.AlignStart)
	leftBox.Append(workspaceLabel)

	// Center box
	centerBox := gtk.NewBox(gtk.OrientationHorizontal, 0)
	centerBox.SetHAlign(gtk.AlignCenter)
	centerBox.Append(centerLabel)

	// Right box
	rightBox := gtk.NewBox(gtk.OrientationHorizontal, 0)
	rightBox.SetHAlign(gtk.AlignEnd)
	
	batterySeparator = gtk.NewLabel("|")
	
	// Only add battery components if battery exists
	if hasBattery() {
		rightBox.Append(batteryLabel)
		rightBox.Append(batterySeparator)
	}
	
	rightBox.Append(timeLabel)

	mainBox.Append(leftBox)
	mainBox.Append(centerBox)
	mainBox.Append(rightBox)

	leftBox.SetHExpand(true)
	leftBox.SetHAlign(gtk.AlignStart)

	centerBox.SetHExpand(true)
	centerBox.SetHAlign(gtk.AlignCenter)

	rightBox.SetHExpand(true)
	rightBox.SetHAlign(gtk.AlignEnd)
	workspaceLabel.AddCSSClass("workspace-label")

	css := gtk.NewCSSProvider()
	css.LoadFromData(`
window {
	background-color: rgba(0, 0, 0, 0.8); /* Fully opaque window background */
}
.emoji {
  font-family: "Noto Color Emoji", "Twemoji", sans-serif; /* Use emoji font for emojis */
}

label {
  font: 14px "Monospace";
  color: #ffffff;
  padding: 5px;
  margin: 0;
}


`)
	styleContext := window.StyleContext()
	gtk.StyleContextAddProviderForDisplay(styleContext.Display(), css, gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)

	layershell.InitForWindow(&window.Window)
	layershell.SetNamespace(&window.Window, "gtk-layer-shell")
	layershell.SetAnchor(&window.Window, layershell.LayerShellEdgeLeft, true)
	layershell.SetAnchor(&window.Window, layershell.LayerShellEdgeRight, true)
	layershell.SetAnchor(&window.Window, layershell.LayerShellEdgeBottom, true)
	layershell.SetLayer(&window.Window, layershell.LayerShellLayerTop)
	layershell.AutoExclusiveZoneEnable(&window.Window)

	go func() {
		ctx := context.Background()
		client, err := sway.New(ctx)
		if err != nil {
			log.Warn("Sway IPC error: %v", "err", err)
			return
		}

		wss := &workspaceState{}
		// Fetch initial workspace state
		workspaces, err := client.GetWorkspaces(ctx)
		if err != nil {
			log.Error("failed to get initial list of workspaces", "error", err)
			app.Quit()
		}

		for _, ws := range workspaces {
			wss.workspaces = append(wss.workspaces, &workspace{
				name:    ws.Name,
				focused: ws.Focused,
			})
		}

		// Do initial draw
		drawWorkspace(wss)

		go func() {
			log.Info("Listening to sway events")
			if err := sway.Subscribe(context.Background(), &eh{wss: wss}, sway.EventTypeWorkspace); err != nil {
				log.Error("failed to subscribe to sway", "error", err)
				app.Quit()
			}
		}()

		for {
			tree, err := client.GetTree(ctx)
			if err != nil {
				log.Warn("Failed to get tree: %v", "err", err)
				time.Sleep(1 * time.Second)
				continue
			}
			if tree.FocusedNode() != nil {

				glib.IdleAdd(func() {
					nodeName := tree.FocusedNode().Name

					formattedText := fmt.Sprintf(
						"%s",
						html.EscapeString(nodeName),
					)

					centerLabel.SetMarkup(formattedText)
				})
			}

			time.Sleep(time.Millisecond * 200)
		}
	}()

	go func() {
		for {
			now := time.Now()
			formatted := now.Format("2006-01-02 15:04:05")
			glib.IdleAdd(func() {
				timeLabel.SetText(fmt.Sprintf("%v", formatted))
			})
			time.Sleep(time.Millisecond * 200)
		}
	}()

	if hasBattery() {
		go func() {
			for {
				batteryStatus := getBatteryStatus()
				glib.IdleAdd(func() {
					batteryLabel.SetText(batteryStatus)
				})
				time.Sleep(5 * time.Second)
			}
		}()
	}

	glib.IdleAdd(func() {
		window.Show()
	})
	window.Show()

	display := gdk.DisplayGetDefault()
	if display != nil {
		monitors := display.Monitors()

		if monitors != nil {
			monitors.ConnectItemsChanged(func(position, removed, added uint) {
				log.Info("Monitor change detected. Exiting", "position", position, "removed", removed, "added", added)
				os.Stdout.Sync()
				app.Quit()
			})
			log.Info("Listening for Monitor change events")
		}
	}

	return &window.Window

}

func getBatteryStatus() string {
	capacity, err := ioutil.ReadFile("/sys/class/power_supply/BAT0/capacity")
	if err != nil {
		return "🔋 --"
	}

	status, err := ioutil.ReadFile("/sys/class/power_supply/BAT0/status")
	if err != nil {
		return "🔋 --"
	}

	capacityStr := strings.TrimSpace(string(capacity))
	statusStr := strings.TrimSpace(string(status))

	var icon string
	switch statusStr {
	case "Charging":
		icon = "🔌"
	case "Discharging":
		icon = "🔋"
	case "Full":
		icon = "🔋"
	default:
		icon = "🔋"
	}

	return fmt.Sprintf("%s %s%%", icon, capacityStr)
}

func hasBattery() bool {
	_, err := os.Stat("/sys/class/power_supply/BAT0")
	return err == nil
}

type eh struct {
	wss *workspaceState
}

func (eh *eh) Workspace(ctx context.Context, e sway.WorkspaceEvent) {
	if e.Change == sway.WorkspaceInit {
		eh.wss.workspaces = append(eh.wss.workspaces, &workspace{
			name:    e.Current.Name,
			focused: e.Current.Focused,
		})
	}
	if e.Change == sway.WorkspaceEmpty {
		eh.wss.workspaces = slices.DeleteFunc(eh.wss.workspaces, func(ws *workspace) bool {
			return ws.name == e.Current.Name
		})
	}

	if e.Change == sway.WorkspaceFocus {
		newFocus := e.Current.Name
		oldFocus := e.Old.Name

		for _, ws := range eh.wss.workspaces {
			if ws.name == newFocus {
				ws.focused = true
			}

			if ws.name == oldFocus {
				ws.focused = false
			}

		}
	}

	slices.SortFunc(eh.wss.workspaces, func(a, b *workspace) int {
		aInt, errA := strconv.Atoi(a.name)
		bInt, errB := strconv.Atoi(b.name)
		if errA == nil && errB == nil {
			return cmp.Compare(aInt, bInt)
		}
		return cmp.Compare(a.name, b.name)
	})

	drawWorkspace(eh.wss)
}
func (eh *eh) Mode(context.Context, sway.ModeEvent)                       {}
func (eh *eh) Window(context.Context, sway.WindowEvent)                   {}
func (eh *eh) BarConfigUpdate(context.Context, sway.BarConfigUpdateEvent) {}
func (eh *eh) Binding(context.Context, sway.BindingEvent)                 {}
func (eh *eh) Shutdown(context.Context, sway.ShutdownEvent)               {}
func (eh *eh) Tick(context.Context, sway.TickEvent)                       {}
func (eh *eh) BarStateUpdate(context.Context, sway.BarStateUpdateEvent)   {}
func (eh *eh) BarStatusUpdate(context.Context, sway.BarStatusUpdateEvent) {}
func (eh *eh) Input(context.Context, sway.InputEvent)                     {}

type workspaceState struct {
	workspaces []*workspace
}

type workspace struct {
	name    string
	focused bool
}

func drawWorkspace(wss *workspaceState) error {
	text := "<span>"
	for _, ws := range wss.workspaces {
		if ws.focused {
			text += fmt.Sprintf("[%s]", ws.name)
		} else {
			text += fmt.Sprintf(" %s ", ws.name)
		}
	}
	text += "</span>"
	glib.IdleAdd(func() {
		workspaceLabel.SetMarkup(text)
	})
	return nil
}
