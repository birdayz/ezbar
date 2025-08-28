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

	"github.com/birdayz/ezbar/pkg/datasource"
	"github.com/birdayz/ezbar/pkg/widget"
)

var (
	workspaceLabel   *gtk.Label
	batteryLabel     *gtk.Label
	batterySeparator *gtk.Label
)

// New widget-based components
var (
	kubectlLabelWidget *widget.LabelWidget
	kubectlPopup       *widget.KubectlPopup
	cpuLabelWidget     *widget.LabelWidget
	cpuGraphWidget     *widget.GraphWidget
	tempLabelWidget    *widget.LabelWidget
	tempGraphWidget    *widget.GraphWidget
	memoryLabelWidget  *widget.LabelWidget
	memoryGraphWidget  *widget.GraphWidget
	pingLabelWidget    *widget.LabelWidget
	pingGraphWidget    *widget.GraphWidget
	timeLabelWidget    *widget.LabelWidget
	spotifyWidget      *widget.SpotifyNativeWidget
	stockWidget        *widget.StockWidget
	volumeLabelWidget  *widget.LabelWidget
)

// Data sources
var (
	kubectlDataSource     *datasource.KubectlDataSource
	cpuDataSource         *datasource.CPUDataSource
	memoryDataSource      *datasource.MemoryDataSource
	temperatureDataSource *datasource.TemperatureDataSource
	pingDataSource        *datasource.PingDataSource
	timeDataSource        *datasource.TimeDataSource
	spotifyDataSource     *datasource.SpotifyDataSource
	stockDataSource       *datasource.StockDataSource
	volumeDataSource      *datasource.VolumeDataSource
)

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

	batteryLabel = gtk.NewLabel("🔋 --")

	// Create new architecture widgets and data sources
	kubectlLabelWidget = widget.NewLabelWidget("⚙️ --")
	cpuLabelWidget = widget.NewLabelWidget("🖥️ --")
	cpuGraphWidget = widget.NewGraphWidget("cpu", 30)
	tempLabelWidget = widget.NewLabelWidget("🌡️ --")
	tempGraphWidget = widget.NewGraphWidget("temperature", 60)
	memoryLabelWidget = widget.NewLabelWidget("💾 --")
	memoryGraphWidget = widget.NewGraphWidget("memory", 20)
	pingLabelWidget = widget.NewLabelWidget("🏓 --")
	pingGraphWidget = widget.NewGraphWidget("ping", 40)
	timeLabelWidget = widget.NewLabelWidget("Loading…")
	spotifyWidget = widget.NewSpotifyNativeWidget()
	stockWidget = widget.NewStockWidget()
	volumeLabelWidget = widget.NewLabelWidget("🔇 --")

	// Create data sources
	kubectlDataSource = datasource.NewKubectlDataSource()
	cpuDataSource = datasource.NewCPUDataSource()
	memoryDataSource = datasource.NewMemoryDataSource()
	temperatureDataSource = datasource.NewTemperatureDataSource()
	pingDataSource = datasource.NewPingDataSource("8.8.8.8")
	timeDataSource = datasource.NewTimeDataSource()
	spotifyDataSource = datasource.NewSpotifyDataSource()
	volumeDataSource = datasource.NewVolumeDataSource()

	// Create stock data source (configurable via environment variables)
	stockSymbol := os.Getenv("EZBAR_STOCK_SYMBOL")
	if stockSymbol == "" {
		stockSymbol = "NQ=F" // Default to Apple
	}
	stockApiKey := os.Getenv("EZBAR_STOCK_API_KEY") // Optional for free APIs
	stockDataSource = datasource.NewNASDAQStockDataSource(stockSymbol, stockApiKey)

	// Associate labels with their graphs for click-to-toggle
	cpuLabelWidget.SetAssociatedGraph(cpuGraphWidget)
	tempLabelWidget.SetAssociatedGraph(tempGraphWidget)
	memoryLabelWidget.SetAssociatedGraph(memoryGraphWidget)
	pingLabelWidget.SetAssociatedGraph(pingGraphWidget)

	// Connect data sources to widgets
	kubectlDataSource.Subscribe(kubectlLabelWidget.Update)

	// Set kubectl click handler to clear context
	kubectlLabelWidget.SetClickHandler(func() {
		kubectlDataSource.ClearContext()
	})
	
	// Create kubectl context selection popup
	kubectlPopup = widget.NewKubectlPopup(kubectlDataSource, func(context string) {
		kubectlDataSource.SetContext(context)
	})
	
	// Set kubectl right-click handler to show context selection popup
	kubectlLabelWidget.SetRightClickHandler(func() {
		widget := kubectlLabelWidget.GetGTKWidget()
		kubectlPopup.Show(widget, &window.Window)
	})

	cpuDataSource.Subscribe(cpuLabelWidget.Update)
	cpuDataSource.Subscribe(cpuGraphWidget.Update)

	memoryDataSource.Subscribe(memoryLabelWidget.Update)
	memoryDataSource.Subscribe(memoryGraphWidget.Update)

	temperatureDataSource.Subscribe(tempLabelWidget.Update)
	temperatureDataSource.Subscribe(tempGraphWidget.Update)

	pingDataSource.Subscribe(pingLabelWidget.Update)
	pingDataSource.Subscribe(pingGraphWidget.Update)

	timeDataSource.Subscribe(timeLabelWidget.Update)

	spotifyDataSource.Subscribe(spotifyWidget.Update)

	stockDataSource.Subscribe(stockWidget.Update)

	volumeDataSource.Subscribe(volumeLabelWidget.Update)

	// Connect volume click to toggle mute
	volumeLabelWidget.SetClickHandler(func() {
		volumeDataSource.ToggleMute()
	})

	// Connect volume scroll to change volume
	volumeLabelWidget.SetScrollHandler(func(direction int) {
		volumeDataSource.ChangeVolume(direction)
	})

	// Connect Spotify click to OAuth trigger and scroll controls
	spotifyWidget.SetClickHandler(func() {
		spotifyDataSource.HandleClick()
	})
	spotifyWidget.SetSpotifyDataSource(spotifyDataSource)

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

	// Add new architecture widgets to UI
	rightBox.Append(kubectlLabelWidget.GetGTKWidget())
	rightBox.Append(gtk.NewLabel("|"))
	rightBox.Append(cpuLabelWidget.GetGTKWidget())
	rightBox.Append(cpuGraphWidget.GetGTKWidget())
	rightBox.Append(gtk.NewLabel("|"))
	rightBox.Append(tempLabelWidget.GetGTKWidget())
	rightBox.Append(tempGraphWidget.GetGTKWidget())
	rightBox.Append(gtk.NewLabel("|"))
	rightBox.Append(memoryLabelWidget.GetGTKWidget())
	rightBox.Append(memoryGraphWidget.GetGTKWidget())
	rightBox.Append(gtk.NewLabel("|"))
	rightBox.Append(pingLabelWidget.GetGTKWidget())
	rightBox.Append(pingGraphWidget.GetGTKWidget())
	rightBox.Append(gtk.NewLabel("|"))
	rightBox.Append(spotifyWidget.GetGTKWidget())
	rightBox.Append(gtk.NewLabel("|"))
	rightBox.Append(stockWidget.GetGTKWidget())
	rightBox.Append(gtk.NewLabel("|"))
	rightBox.Append(volumeLabelWidget.GetGTKWidget())
	rightBox.Append(gtk.NewLabel("|"))

	// Only add battery components if battery exists
	if hasBattery() {
		rightBox.Append(batteryLabel)
		rightBox.Append(batterySeparator)
	}

	rightBox.Append(timeLabelWidget.GetGTKWidget())

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

.spotify-scroll scrollbar {
  opacity: 0;
}

.spotify-scroll scrollbar:hover {
  opacity: 0;
}

@keyframes production-alert {
  0% { color: #ff0000; background-color: rgba(255, 0, 0, 0.2); }
  50% { color: #ffffff; background-color: rgba(255, 0, 0, 0.8); }
  100% { color: #ff0000; background-color: rgba(255, 0, 0, 0.2); }
}

.production-context {
  animation: production-alert 1s ease-in-out infinite;
  font-weight: bold;
  border-radius: 3px;
  padding: 3px 6px;
}

.production-context-row {
  color: #ff9999;
  font-weight: bold;
  background-color: rgba(255, 0, 0, 0.2);
}

.kubectl-popup .production-context-row {
  color: #ff6666;
  font: bold 14px "Monospace";
  background-color: rgba(255, 0, 0, 0.2);
}

.kubectl-popup {
  background-color: rgba(0, 0, 0, 0.8);
  border: 1px solid #333;
  border-radius: 8px;
  box-shadow: 0 4px 20px rgba(0, 0, 0, 0.5);
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
	layershell.SetKeyboardMode(&window.Window, layershell.LayerShellKeyboardModeOnDemand)
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

	// Start data sources with context
	ctx := context.Background()
	kubectlDataSource.Start(ctx)
	cpuDataSource.Start(ctx)
	memoryDataSource.Start(ctx)
	temperatureDataSource.Start(ctx)
	pingDataSource.Start(ctx)
	timeDataSource.Start(ctx)
	spotifyDataSource.Start(ctx)
	stockDataSource.Start(ctx)
	volumeDataSource.Start(ctx)

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
		icon = "⚡" // Lightning bolt for active charging
	case "Not charging":
		icon = "🔌" // Plugged in but not actively charging (battery full/nearly full)
	case "Discharging":
		icon = "🔋"
	case "Full":
		icon = "🔌" // Full but still plugged in
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

