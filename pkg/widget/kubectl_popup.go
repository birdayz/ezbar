package widget

import (
	"strings"

	"github.com/diamondburned/gotk4/pkg/gdk/v4"
	"github.com/diamondburned/gotk4/pkg/glib/v2"
	"github.com/diamondburned/gotk4/pkg/gtk/v4"

	"github.com/birdayz/ezbar/pkg/datasource"
)

type KubectlPopup struct {
	popover      *gtk.Popover
	searchEntry  *gtk.SearchEntry
	listBox      *gtk.ListBox
	scrolled     *gtk.ScrolledWindow
	contexts     []string
	filteredRows []*gtk.ListBoxRow
	onSelect     func(context string)
	datasource   *datasource.KubectlDataSource
	isVisible    bool
	parentWindow *gtk.Window
}

func NewKubectlPopup(datasource *datasource.KubectlDataSource, onSelect func(context string)) *KubectlPopup {
	popup := &KubectlPopup{
		datasource: datasource,
		onSelect:   onSelect,
		isVisible:  false,
	}
	
	popup.setupUI()
	return popup
}

func (p *KubectlPopup) setupUI() {
	// Create popover
	p.popover = gtk.NewPopover()
	p.popover.SetPosition(gtk.PosTop)
	p.popover.SetSizeRequest(300, 200)
	p.popover.SetCanFocus(true)
	p.popover.SetFocusable(true)
	p.popover.AddCSSClass("kubectl-popup")
	
	// Create main container
	vbox := gtk.NewBox(gtk.OrientationVertical, 3)
	vbox.SetMarginTop(5)
	vbox.SetMarginBottom(5)
	vbox.SetMarginStart(8)
	vbox.SetMarginEnd(8)
	p.popover.SetChild(vbox)
	
	// Create search entry
	p.searchEntry = gtk.NewSearchEntry()
	p.searchEntry.SetPlaceholderText("Search contexts...")
	p.searchEntry.SetHExpand(true)
	p.searchEntry.SetCanFocus(true)
	p.searchEntry.SetFocusable(true)
	
	// Add mouse enter event to auto-focus search entry
	motionController := gtk.NewEventControllerMotion()
	motionController.ConnectEnter(func(x, y float64) {
		p.searchEntry.GrabFocus()
	})
	p.searchEntry.AddController(motionController)
	
	vbox.Append(p.searchEntry)
	
	// Create scrolled window for list
	p.scrolled = gtk.NewScrolledWindow()
	p.scrolled.SetPolicy(gtk.PolicyNever, gtk.PolicyAutomatic)
	p.scrolled.SetHExpand(true)
	p.scrolled.SetVExpand(true)
	p.scrolled.SetMarginTop(2)
	vbox.Append(p.scrolled)
	
	// Create list box
	p.listBox = gtk.NewListBox()
	p.listBox.SetSelectionMode(gtk.SelectionSingle)
	p.listBox.SetVExpand(true)
	p.scrolled.SetChild(p.listBox)
	
	// Connect search entry to filter function
	p.searchEntry.ConnectSearchChanged(func() {
		p.filterContexts()
	})
	
	// Connect list box selection
	p.listBox.ConnectRowActivated(func(row *gtk.ListBoxRow) {
		label := row.Child().(*gtk.Label)
		context := label.Text()
		if p.onSelect != nil {
			p.onSelect(context)
		}
		p.Hide()
	})
	
	// Handle escape key to close popup - add to both popover and search entry
	keyController := gtk.NewEventControllerKey()
	keyController.ConnectKeyPressed(func(keyval uint, keycode uint, state gdk.ModifierType) bool {
		if keyval == gdk.KEY_Escape {
			p.Hide()
			return true
		}
		return false
	})
	p.popover.AddController(keyController)
	
	// Also add escape handler to search entry
	searchKeyController := gtk.NewEventControllerKey()
	searchKeyController.ConnectKeyPressed(func(keyval uint, keycode uint, state gdk.ModifierType) bool {
		if keyval == gdk.KEY_Escape {
			p.Hide()
			return true
		}
		return false
	})
	p.searchEntry.AddController(searchKeyController)
	
	// Connect to show/hide signals to track visibility
	p.popover.ConnectShow(func() {
		p.isVisible = true
		
		// Just try to focus the search entry
		glib.TimeoutAdd(10, func() {
			p.searchEntry.GrabFocus()
		})
	})
	
	p.popover.ConnectHide(func() {
		p.isVisible = false
	})
}

func (p *KubectlPopup) Show(parentWidget *gtk.Widget, parentWindow *gtk.Window) {
	// Store parent window for key forwarding
	p.parentWindow = parentWindow
	
	// Load contexts
	p.contexts = p.datasource.GetAllContexts()
	p.populateList()
	
	// Set parent widget for the popover
	p.popover.SetParent(parentWidget)
	
	// Show popover and focus search entry
	p.popover.Popup()
	
	// Multiple attempts to grab focus properly
	glib.IdleAdd(func() {
		p.popover.GrabFocus()
		p.searchEntry.GrabFocus()
	})
	
	// Backup focus attempt with timeout
	glib.TimeoutAdd(50, func() {
		p.searchEntry.GrabFocus()
	})
}

func (p *KubectlPopup) Hide() {
	p.popover.Popdown()
}

func (p *KubectlPopup) IsVisible() bool {
	return p.isVisible
}

func (p *KubectlPopup) ForwardKeyEvent(keyval uint, keycode uint, state gdk.ModifierType) bool {
	if !p.isVisible {
		return false
	}
	
	// Handle escape key
	if keyval == gdk.KEY_Escape {
		p.Hide()
		return true
	}
	
	// Handle enter key - select first visible item
	if keyval == gdk.KEY_Return || keyval == gdk.KEY_KP_Enter {
		if len(p.filteredRows) > 0 {
			row := p.filteredRows[0]
			label := row.Child().(*gtk.Label)
			context := label.Text()
			if p.onSelect != nil {
				p.onSelect(context)
			}
			p.Hide()
			return true
		}
	}
	
	// Handle printable characters - forward to search entry
	if keyval >= 32 && keyval <= 126 { // Printable ASCII range
		// Get current text and append new character
		currentText := p.searchEntry.Text()
		newChar := string(rune(keyval))
		
		// Handle shift for uppercase
		if (state & gdk.ShiftMask) != 0 {
			newChar = strings.ToUpper(newChar)
		} else {
			newChar = strings.ToLower(newChar)
		}
		
		p.searchEntry.SetText(currentText + newChar)
		// Position cursor at end
		p.searchEntry.SetPosition(-1)
		return true
	}
	
	// Handle backspace
	if keyval == gdk.KEY_BackSpace {
		currentText := p.searchEntry.Text()
		if len(currentText) > 0 {
			p.searchEntry.SetText(currentText[:len(currentText)-1])
			p.searchEntry.SetPosition(-1)
		}
		return true
	}
	
	return false
}

func (p *KubectlPopup) populateList() {
	// Clear existing rows
	for len(p.filteredRows) > 0 {
		p.listBox.Remove(p.filteredRows[0])
		p.filteredRows = p.filteredRows[1:]
	}
	p.filteredRows = nil
	
	// Add all contexts
	for _, context := range p.contexts {
		p.addContextRow(context)
	}
}

func (p *KubectlPopup) addContextRow(context string) {
	label := gtk.NewLabel(context)
	label.SetHAlign(gtk.AlignStart)
	label.SetMarginStart(8)
	label.SetMarginEnd(8)
	label.SetMarginTop(3)
	label.SetMarginBottom(3)
	
	// Check if this is a production context and style accordingly
	if isProductionContextLocal(context) {
		label.AddCSSClass("production-context-row")
	}
	
	row := gtk.NewListBoxRow()
	row.SetChild(label)
	
	p.listBox.Append(row)
	p.filteredRows = append(p.filteredRows, row)
}

func (p *KubectlPopup) filterContexts() {
	searchText := strings.ToLower(p.searchEntry.Text())
	
	// Clear existing rows
	for len(p.filteredRows) > 0 {
		p.listBox.Remove(p.filteredRows[0])
		p.filteredRows = p.filteredRows[1:]
	}
	p.filteredRows = nil
	
	// Add filtered contexts
	for _, context := range p.contexts {
		if searchText == "" || strings.Contains(strings.ToLower(context), searchText) {
			p.addContextRow(context)
		}
	}
}

func isProductionContextLocal(context string) bool {
	contextLower := strings.ToLower(context)
	return strings.Contains(contextLower, "prod") || strings.Contains(contextLower, "prd")
}

