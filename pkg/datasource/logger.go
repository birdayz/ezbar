package datasource

import (
	"context"
	"fmt"
	"io"
	"log/slog"
	"os"
)

// ColoredHandler wraps slog.TextHandler with ANSI color codes
type ColoredHandler struct {
	handler slog.Handler
	w       io.Writer
}

func NewColoredHandler(w io.Writer, opts *slog.HandlerOptions) *ColoredHandler {
	if opts == nil {
		opts = &slog.HandlerOptions{}
	}
	return &ColoredHandler{
		handler: slog.NewTextHandler(w, opts),
		w:       w,
	}
}

func (h *ColoredHandler) Enabled(ctx context.Context, level slog.Level) bool {
	return h.handler.Enabled(ctx, level)
}

func (h *ColoredHandler) WithAttrs(attrs []slog.Attr) slog.Handler {
	return &ColoredHandler{
		handler: h.handler.WithAttrs(attrs),
		w:       h.w,
	}
}

func (h *ColoredHandler) WithGroup(name string) slog.Handler {
	return &ColoredHandler{
		handler: h.handler.WithGroup(name),
		w:       h.w,
	}
}

func (h *ColoredHandler) Handle(ctx context.Context, r slog.Record) error {
	// Color codes
	const (
		colorReset  = "\033[0m"
		colorRed    = "\033[31m"
		colorYellow = "\033[33m"
		colorBlue   = "\033[34m"
		colorGreen  = "\033[32m"
		colorCyan   = "\033[36m"
		colorPurple = "\033[35m"
		colorBold   = "\033[1m"
	)

	var levelColor string
	var prefix string
	switch r.Level {
	case slog.LevelDebug:
		levelColor = colorCyan
		prefix = "🔍"
	case slog.LevelInfo:
		levelColor = colorGreen
		prefix = "🎵"
	case slog.LevelWarn:
		levelColor = colorYellow
		prefix = "⚠️"
	case slog.LevelError:
		levelColor = colorRed
		prefix = "❌"
	default:
		levelColor = colorReset
		prefix = "📝"
	}

	// Format: [emoji] [TIME] [LEVEL] message [attributes]
	timestamp := r.Time.Format("15:04:05")
	
	fmt.Fprintf(h.w, "%s %s[%s]%s %s%s[%s]%s %s",
		prefix,
		colorCyan, timestamp, colorReset,
		levelColor+colorBold, r.Level.String(), colorReset,
		" ",
		r.Message,
	)

	// Add attributes
	r.Attrs(func(a slog.Attr) bool {
		fmt.Fprintf(h.w, " %s%s%s=%v", colorPurple, a.Key, colorReset, a.Value)
		return true
	})

	fmt.Fprintln(h.w)
	return nil
}

func NewSpotifyLogger() *slog.Logger {
	handler := NewColoredHandler(os.Stderr, &slog.HandlerOptions{
		Level: slog.LevelInfo,
	})
	return slog.New(handler).With("component", "spotify")
}