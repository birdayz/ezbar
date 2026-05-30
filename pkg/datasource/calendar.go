package datasource

import (
	"context"
	"fmt"
	"log/slog"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/apognu/gocal"
)

type GoogleCalendarDataSource struct {
	mu          sync.RWMutex
	currentData CalendarData
	callbacks   []func(value any)
	client      *http.Client
	icalURL     string
	logger      *slog.Logger
}

func NewGoogleCalendarDataSource() *GoogleCalendarDataSource {
	return &GoogleCalendarDataSource{
		currentData: CalendarData{
			DisplayText:   "Loading...",
			TimeUntilNext: "",
		},
		callbacks: make([]func(value any), 0),
		client:    &http.Client{Timeout: 15 * time.Second},
		logger:    NewCalendarLogger(),
	}
}

func NewCalendarLogger() *slog.Logger {
	handler := NewColoredHandler(os.Stderr, &slog.HandlerOptions{
		Level: slog.LevelInfo,
	})
	return slog.New(handler).With("component", "calendar")
}

func (c *GoogleCalendarDataSource) loadConfig() error {
	// Check env var first
	if url := os.Getenv("GOOGLE_CALENDAR_ICAL_URL"); url != "" {
		c.icalURL = url
		return nil
	}

	homeDir, err := os.UserHomeDir()
	if err != nil {
		return err
	}

	// Read from config file
	configPath := filepath.Join(homeDir, ".config", "ezbar", "calendar_url")
	data, err := os.ReadFile(configPath)
	if err != nil {
		return fmt.Errorf("calendar URL not configured. Get your secret iCal URL from Google Calendar settings and save it to ~/.config/ezbar/calendar_url")
	}

	c.icalURL = strings.TrimSpace(string(data))
	if c.icalURL == "" {
		return fmt.Errorf("calendar_url file is empty")
	}

	return nil
}

func (c *GoogleCalendarDataSource) getEvents() (CalendarData, error) {
	if c.icalURL == "" {
		if err := c.loadConfig(); err != nil {
			return CalendarData{}, err
		}
	}

	req, err := http.NewRequest("GET", c.icalURL, nil)
	if err != nil {
		return CalendarData{}, fmt.Errorf("creating request: %w", err)
	}
	req.Header.Set("Cache-Control", "no-cache")

	resp, err := c.client.Do(req)
	if err != nil {
		return CalendarData{}, fmt.Errorf("network error: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != 200 {
		return CalendarData{}, fmt.Errorf("HTTP %d fetching calendar", resp.StatusCode)
	}

	now := time.Now()

	// Filter to today's events using gocal
	startOfDay := time.Date(now.Year(), now.Month(), now.Day(), 0, 0, 0, 0, now.Location())
	endOfDay := startOfDay.Add(24 * time.Hour)

	parser := gocal.NewParser(resp.Body)
	parser.Start, parser.End = &startOfDay, &endOfDay
	if err := parser.Parse(); err != nil {
		return CalendarData{}, fmt.Errorf("parsing ical: %w", err)
	}

	var todayEvents []CalendarEvent
	localLoc := now.Location()
	for _, ev := range parser.Events {
		event := CalendarEvent{
			Title:    ev.Summary,
			Location: ev.Location,
		}
		if ev.Start != nil {
			// Convert to local timezone for consistent display
			event.StartTime = ev.Start.In(localLoc)
		}
		if ev.End != nil {
			event.EndTime = ev.End.In(localLoc)
		}
		// All-day events have no time component (midnight to midnight)
		if ev.RawStart.Value != "" && len(ev.RawStart.Value) == 8 {
			event.IsAllDay = true
		}
		c.logger.Info("Event", "title", event.Title, "start", event.StartTime, "end", event.EndTime)
		todayEvents = append(todayEvents, event)
	}

	// Sort events by start time
	sort.Slice(todayEvents, func(i, j int) bool {
		return todayEvents[i].StartTime.Before(todayEvents[j].StartTime)
	})

	// Find next upcoming or ongoing event (non-all-day)
	var nextEvent *CalendarEvent
	for i := range todayEvents {
		// Include if event hasn't ended yet (ongoing or future)
		if !todayEvents[i].IsAllDay && todayEvents[i].EndTime.After(now) {
			if nextEvent == nil || todayEvents[i].StartTime.Before(nextEvent.StartTime) {
				nextEvent = &todayEvents[i]
			}
		}
	}

	data := CalendarData{
		Events:      todayEvents,
		TodayEvents: todayEvents,
		NextEvent:   nextEvent,
	}

	if nextEvent == nil {
		data.DisplayText = "No meetings"
		data.TimeUntilNext = ""
	} else {
		timeUntil := time.Until(nextEvent.StartTime)

		if timeUntil < 0 {
			if now.Before(nextEvent.EndTime) {
				data.IsOverdue = true
				data.DisplayText = fmt.Sprintf("NOW: %s", truncateTitle(nextEvent.Title, 20))
				data.TimeUntilNext = "ongoing"
			} else {
				// Find actually next one
				var actualNext *CalendarEvent
				for i := range todayEvents {
					if !todayEvents[i].IsAllDay && todayEvents[i].StartTime.After(now) {
						if actualNext == nil || todayEvents[i].StartTime.Before(actualNext.StartTime) {
							actualNext = &todayEvents[i]
						}
					}
				}
				if actualNext != nil {
					nextEvent = actualNext
					data.NextEvent = actualNext
					timeUntil = time.Until(actualNext.StartTime)
				} else {
					data.DisplayText = "No more meetings"
					data.TimeUntilNext = ""
					data.NextEvent = nil
					return data, nil
				}
			}
		}

		if !data.IsOverdue {
			if timeUntil <= 5*time.Minute {
				data.IsUrgent = true
				data.DisplayText = fmt.Sprintf("SOON: %s", truncateTitle(nextEvent.Title, 18))
			} else if timeUntil <= 15*time.Minute {
				data.IsUrgent = true
				data.DisplayText = truncateTitle(nextEvent.Title, 25)
			} else {
				data.DisplayText = truncateTitle(nextEvent.Title, 25)
			}

			if timeUntil < time.Hour {
				data.TimeUntilNext = fmt.Sprintf("%dm", int(timeUntil.Minutes()))
			} else {
				hours := int(timeUntil.Hours())
				mins := int(timeUntil.Minutes()) % 60
				if mins > 0 {
					data.TimeUntilNext = fmt.Sprintf("%dh%dm", hours, mins)
				} else {
					data.TimeUntilNext = fmt.Sprintf("%dh", hours)
				}
			}
		}
	}

	return data, nil
}

func truncateTitle(title string, maxLen int) string {
	if len(title) <= maxLen {
		return title
	}
	return title[:maxLen-2] + ".."
}

func (c *GoogleCalendarDataSource) Start(ctx context.Context) {
	go func() {
		data, err := c.getEvents()
		if err != nil {
			c.logger.Warn("Calendar error", "error", err)
			data = CalendarData{
				DisplayText: "Setup: see ~/.config/ezbar/calendar_url",
			}
		}

		c.mu.Lock()
		c.currentData = data
		callbacks := make([]func(value any), len(c.callbacks))
		copy(callbacks, c.callbacks)
		c.mu.Unlock()

		for _, callback := range callbacks {
			callback(data)
		}

		ticker := time.NewTicker(60 * time.Second)
		defer ticker.Stop()

		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				data, err := c.getEvents()
				if err != nil {
					c.logger.Warn("Calendar error", "error", err)
					continue
				}

				c.mu.Lock()
				c.currentData = data
				callbacks := make([]func(value any), len(c.callbacks))
				copy(callbacks, c.callbacks)
				c.mu.Unlock()

				for _, callback := range callbacks {
					callback(data)
				}
			}
		}
	}()
}

func (c *GoogleCalendarDataSource) Subscribe(callback func(value any)) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.callbacks = append(c.callbacks, callback)
}

func (c *GoogleCalendarDataSource) GetCurrentValue() any {
	c.mu.RLock()
	defer c.mu.RUnlock()
	return c.currentData
}

func (c *GoogleCalendarDataSource) HandleClick() {
	// No-op - no auth needed with iCal
}

func (c *GoogleCalendarDataSource) GetTodayEvents() []CalendarEvent {
	c.mu.RLock()
	defer c.mu.RUnlock()
	return c.currentData.TodayEvents
}
