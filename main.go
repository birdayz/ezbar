package main

import (
	"context"
	"fmt"
	"log"
	"time"

	"github.com/dlasky/gotk3-layershell/layershell"
	"github.com/gotk3/gotk3/gtk"
	"github.com/joshuarubin/go-sway"
)

var workspaceLabel *gtk.Label
var timeLabel *gtk.Label

func main() {
	gtk.Init(nil)

	window, err := gtk.WindowNew(gtk.WINDOW_POPUP)
	if err != nil {
		log.Fatal(err)
	}
	window.SetTitle("ezbar")
	window.SetDecorated(false)
	window.SetBorderWidth(0)
	window.SetName("bar-window")

	mainBox, err := gtk.BoxNew(gtk.ORIENTATION_HORIZONTAL, 0)
	if err != nil {
		log.Fatal(err)
	}
	window.Add(mainBox)

	timeLabel, err = gtk.LabelNew("Loading…")
	if err != nil {
		log.Fatal(err)
	}

	workspaceLabel, err = gtk.LabelNew("Starting...")
	if err != nil {
		log.Fatal(err)
	}

	workspaceLabel.SetHAlign(gtk.ALIGN_CENTER)
	workspaceLabel.SetVAlign(gtk.ALIGN_CENTER)
	workspaceLabel.SetMarginStart(0)
	workspaceLabel.SetMarginEnd(0)
	workspaceLabel.SetMarginTop(0)
	workspaceLabel.SetMarginBottom(0)

	workspaceLabel.SetLineWrap(false)
	workspaceLabel.SetSingleLineMode(true)

	// Create the label you want to center
	centerLabel, err := gtk.LabelNew("Centered Label")
	if err != nil {
		log.Fatal(err)
	}

	// Left box
	leftBox, _ := gtk.BoxNew(gtk.ORIENTATION_HORIZONTAL, 0)
	leftBox.SetHAlign(gtk.ALIGN_START)
	leftBox.PackStart(workspaceLabel, false, false, 10)

	// Center box
	centerBox, _ := gtk.BoxNew(gtk.ORIENTATION_HORIZONTAL, 0)
	centerBox.SetHAlign(gtk.ALIGN_CENTER)
	centerBox.PackStart(centerLabel, false, false, 0)

	// Right box
	rightBox, _ := gtk.BoxNew(gtk.ORIENTATION_HORIZONTAL, 0)
	rightBox.SetHAlign(gtk.ALIGN_END)
	rightBox.PackStart(timeLabel, false, false, 10)

	mainBox.PackStart(leftBox, true, true, 0)
	mainBox.PackStart(centerBox, true, true, 0)
	mainBox.PackStart(rightBox, true, true, 0)

	screen := window.GetScreen()
	visual, _ := screen.GetRGBAVisual()
	if visual != nil {
		window.SetVisual(visual)
	}

	// CSS Styling
	cssProvider, err := gtk.CssProviderNew()
	if err != nil {
		log.Fatal(err)
	}
	css := `
#bar-window {
	background-color: rgba(20, 20, 20, 0.8);
}
label {
	font-size: 14px;
	font-family: monospace;
	color: #ffffff;
	padding: 5px;
	margin: 0;
}
`
	cssProvider.LoadFromData(css)
	gtk.AddProviderForScreen(screen, cssProvider, gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)

	// LayerShell setup
	// Makes it possible to have this "sticky", reserved area for the bar, and other windows are pushed.
	layershell.InitForWindow(window)
	layershell.SetNamespace(window, "gtk-layer-shell")
	layershell.SetAnchor(window, layershell.LAYER_SHELL_EDGE_LEFT, true)
	layershell.SetAnchor(window, layershell.LAYER_SHELL_EDGE_RIGHT, true)
	layershell.SetAnchor(window, layershell.LAYER_SHELL_EDGE_BOTTOM, true)
	layershell.SetLayer(window, layershell.LAYER_SHELL_LAYER_TOP)
	layershell.AutoExclusiveZoneEnable(window)

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
				workspaceLabel.SetMarkup(text)

			}

			tree, err := client.GetTree(ctx)
			if err != nil {
				log.Printf("Failed to get tree: %v", err)
				time.Sleep(1 * time.Second)
				continue
			}
			if tree.FocusedNode() != nil {
				centerLabel.SetText(tree.FocusedNode().Name)
			}

			time.Sleep(time.Millisecond * 100)
		}
	}()

	go func() {
		for {
			now := time.Now()
			formatted := fmt.Sprintf("%02d:%02d:%02d", now.Hour(), now.Minute(), now.Second())
			timeLabel.SetText(fmt.Sprintf("%v", formatted))
			time.Sleep(time.Millisecond * 100)
		}
	}()

	window.ShowAll()
	gtk.Main()
}
