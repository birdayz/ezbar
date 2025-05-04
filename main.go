package main

import (
	"context"
	"fmt"
	"html"
	"log"
	"os"
	"os/exec"
	"os/signal"
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

func main() {
	if os.Getenv("EZBAR_CHILD") == "1" {
		run()
		return
	}

	for {
		cmd := exec.Command(os.Args[0])
		cmd.Env = append(os.Environ(), "EZBAR_CHILD=1")
		cmd.Stdout = os.Stdout
		cmd.Stderr = os.Stderr

		fmt.Println("[ezbar launcher] Spawning new bar subprocess...")
		if err := cmd.Run(); err != nil {
			fmt.Println("[ezbar launcher] Child process crashed:", err)
		} else {
			fmt.Println("[ezbar launcher] Child exited cleanly.")
		}

		// Optional: tiny delay to avoid tight respawn loops
		time.Sleep(1 * time.Second)
	}
}

func run() {
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt)
	defer cancel()

	app := gtk.NewApplication("de.nerden.ezbar", gio.ApplicationNonUnique)
	app.ConnectActivate(func() {
		w := activate(app)
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

func activate(app *gtk.Application) *gtk.Window {
	// window, err := gtk.New(gtk.WINDOW_POPUP)
	window := gtk.NewApplicationWindow(app)
	window.SetTitle("ezbar")
	window.SetDecorated(false)
	// window.SetBorderWidth(0)
	// window.Bor
	window.SetName("bar-window")

	// window.get

	mainBox := gtk.NewBox(gtk.OrientationHorizontal, 0)
	window.SetChild(mainBox)

	timeLabel = gtk.NewLabel("Loading…")

	workspaceLabel = gtk.NewLabel("Starting...")
	workspaceLabel.AddCSSClass("bar-label")

	workspaceLabel.SetHAlign(gtk.AlignCenter)
	workspaceLabel.SetVAlign(gtk.AlignCenter)
	workspaceLabel.SetMarginStart(0)
	workspaceLabel.SetMarginEnd(0)
	workspaceLabel.SetMarginTop(0)
	workspaceLabel.SetMarginBottom(0)

	// workspaceLabel.SetLi(false)
	workspaceLabel.SetSingleLineMode(true)

	// Create the label you want to center
	centerLabel := gtk.NewLabel("Centered Label")

	// Left box
	leftBox := gtk.NewBox(gtk.OrientationHorizontal, 0)
	leftBox.SetHAlign(gtk.AlignStart)
	// leftBox.PackStart(workspaceLabel, false, false, 10)
	leftBox.Append(workspaceLabel)

	// Center box
	centerBox := gtk.NewBox(gtk.OrientationHorizontal, 0)
	centerBox.SetHAlign(gtk.AlignCenter)
	// centerBox.PackStart(centerLabel, false, false, 0)
	centerBox.Append(centerLabel)

	// Right box
	rightBox := gtk.NewBox(gtk.OrientationHorizontal, 0)
	rightBox.SetHAlign(gtk.AlignEnd)
	// rightBox.PackStart(timeLabel, false, false, 10)
	rightBox.Append(timeLabel)

	// mainBox.PackStart(leftBox, true, true, 0)
	// mainBox.PackStart(centerBox, true, true, 0)
	// mainBox.PackStart(rightBox, true, true, 0)

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

	// LayerShell setup
	// Makes it possible to have this "sticky", reserved area for the bar, and other windows are pushed.
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
			log.Printf("Sway IPC error: %v", err)
			return
		}
		for {

			workspaces, err := client.GetWorkspaces(ctx)
			if err == nil {
				text := "<span>"
				for _, ws := range workspaces {
					if ws.Focused {
						text += fmt.Sprintf("[%d]", ws.Num)
					} else {
						text += fmt.Sprintf(" %d ", ws.Num)
					}
				}
				text += "</span>"
				glib.IdleAdd(func() {
					workspaceLabel.SetMarkup(text)
				})
			}

			tree, err := client.GetTree(ctx)
			if err != nil {
				log.Printf("Failed to get tree: %v", err)
				time.Sleep(1 * time.Second)
				continue
			}
			if tree.FocusedNode() != nil {

				glib.IdleAdd(func() {
					nodeName := tree.FocusedNode().Name

					// If you want to add emoji support, you can format the text with HTML-like markup
					// For example, if nodeName contains an emoji, apply the appropriate font to it
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

	glib.IdleAdd(func() {
		window.Show()
	})
	window.Show()

	display := gdk.DisplayGetDefault()
	if display != nil {
		monitors := display.Monitors()

		if monitors != nil {
			monitors.ConnectItemsChanged(func(position, removed, added uint) {
				fmt.Printf("[ezbar] Monitors changed: position=%d, removed=%d, added=%d\n", position, removed, added)
				os.Stdout.Sync()
				app.Quit()
			})
			fmt.Println("[ezbar] Listening for Monitor change events")
		}
	}

	return &window.Window

}
