package widget

import "github.com/diamondburned/gotk4/pkg/gtk/v4"

// Widget interface defines how UI components work
type Widget interface {
	Update(value interface{})
	GetGTKWidget() *gtk.Widget
}