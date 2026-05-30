package datasource

import (
	"context"
	"time"
)

// DataSource interface defines how data producers work
type DataSource interface {
	Start(ctx context.Context)
	Subscribe(callback func(value interface{}))
	GetCurrentValue() interface{}
}

// Data value types
type CPUData struct {
	Usage       float64
	UsageString string
}

type MemoryData struct {
	Usage       float64
	UsageString string
}

type TemperatureData struct {
	Temperature float64
	TempString  string
}

type TimeData struct {
	Time       time.Time
	TimeString string
}

type PingData struct {
	Latency    float64 // in milliseconds
	PingString string
	IsUp       bool
}

type SpotifyData struct {
	Track       string
	Artist      string
	Album       string
	TrackString string
	Icon        string
	ScrollText  string
	IsPlaying   bool
}

type StockData struct {
	Symbol         string
	Price          float64
	Change         float64
	ChangePercent  float64
	Volume         int64
	MarketCap      int64
	DisplayText    string
	PriceString    string
	ChangeString   string
	IsPositive     bool
	IsNegative     bool
	TrendEmoji     string
}

type KubectlData struct {
	Context       string
	ContextString string
	IsProduction  bool
}

type VolumeData struct {
	Volume       int
	VolumeString string
	IsMuted      bool
}

type CalendarEvent struct {
	Title     string
	StartTime time.Time
	EndTime   time.Time
	IsAllDay  bool
	Location  string
}

type CalendarData struct {
	Events         []CalendarEvent
	NextEvent      *CalendarEvent
	DisplayText    string
	TimeUntilNext  string
	IsUrgent       bool // Meeting starting within 5 minutes
	IsOverdue      bool // Meeting already started
	NeedsAuth      bool
	TodayEvents    []CalendarEvent
}