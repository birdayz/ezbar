package datasource

import (
	"strings"
	"testing"
	"time"

	"github.com/apognu/gocal"
)

func TestParseICal(t *testing.T) {
	ical := `BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VEVENT
UID:1@test
DTSTAMP:20250203T000000Z
SUMMARY:Team Standup
DTSTART:20250203T090000Z
DTEND:20250203T093000Z
LOCATION:Zoom
END:VEVENT
BEGIN:VEVENT
UID:2@test
DTSTAMP:20250203T000000Z
SUMMARY:1:1 with Manager
DTSTART:20250203T140000Z
DTEND:20250203T150000Z
END:VEVENT
END:VCALENDAR`

	start := time.Date(2025, 2, 3, 0, 0, 0, 0, time.UTC)
	end := start.Add(24 * time.Hour)
	parser := gocal.NewParser(strings.NewReader(ical))
	parser.Start, parser.End = &start, &end
	if err := parser.Parse(); err != nil {
		t.Fatalf("gocal parse failed: %v", err)
	}

	if len(parser.Events) != 2 {
		t.Fatalf("expected 2 events, got %d", len(parser.Events))
	}

	// Check events are parsed correctly
	found := make(map[string]bool)
	for _, ev := range parser.Events {
		found[ev.Summary] = true
	}

	if !found["Team Standup"] {
		t.Error("expected 'Team Standup' event")
	}
	if !found["1:1 with Manager"] {
		t.Error("expected '1:1 with Manager' event")
	}

	t.Logf("Parsed %d events:", len(parser.Events))
	for i, ev := range parser.Events {
		t.Logf("  [%d] %s @ %s", i, ev.Summary, ev.Start.Format("15:04"))
	}
}

func TestCalendarWithURL(t *testing.T) {
	ds := NewGoogleCalendarDataSource()

	// Try loading config
	err := ds.loadConfig()
	if err != nil {
		t.Skipf("No calendar URL configured: %v", err)
	}

	data, err := ds.getEvents()
	if err != nil {
		t.Fatalf("getEvents failed: %v", err)
	}

	t.Logf("Display: %s", data.DisplayText)
	t.Logf("Time until: %s", data.TimeUntilNext)
	t.Logf("Today's events: %d", len(data.TodayEvents))

	for i, ev := range data.TodayEvents {
		t.Logf("  [%d] %s @ %s (all-day: %v)", i, ev.Title, ev.StartTime.Format("15:04"), ev.IsAllDay)
	}
}
