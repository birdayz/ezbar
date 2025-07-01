package datasource

import (
	"context"
	"sync"
	"time"
)

type TimeDataSource struct {
	mu          sync.RWMutex
	currentData TimeData
	callbacks   []func(value interface{})
}

func NewTimeDataSource() *TimeDataSource {
	return &TimeDataSource{
		currentData: TimeData{Time: time.Now(), TimeString: "Loading…"},
		callbacks:   make([]func(value interface{}), 0),
	}
}

func (td *TimeDataSource) Start(ctx context.Context) {
	go func() {
		// Immediate fetch at startup
		now := time.Now()
		timeStr := now.Format("2006-01-02 15:04:05")
		
		data := TimeData{
			Time:       now,
			TimeString: timeStr,
		}
		
		td.mu.Lock()
		td.currentData = data
		callbacks := make([]func(value interface{}), len(td.callbacks))
		copy(callbacks, td.callbacks)
		td.mu.Unlock()
		
		for _, callback := range callbacks {
			callback(data)
		}
		
		// Continue with regular timer updates
		ticker := time.NewTicker(200 * time.Millisecond)
		defer ticker.Stop()
		
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				now := time.Now()
				timeStr := now.Format("2006-01-02 15:04:05")
				
				data := TimeData{
					Time:       now,
					TimeString: timeStr,
				}
				
				td.mu.Lock()
				td.currentData = data
				callbacks := make([]func(value interface{}), len(td.callbacks))
				copy(callbacks, td.callbacks)
				td.mu.Unlock()
				
				for _, callback := range callbacks {
					callback(data)
				}
			}
		}
	}()
}

func (td *TimeDataSource) Subscribe(callback func(value interface{})) {
	td.mu.Lock()
	defer td.mu.Unlock()
	td.callbacks = append(td.callbacks, callback)
}

func (td *TimeDataSource) GetCurrentValue() interface{} {
	td.mu.RLock()
	defer td.mu.RUnlock()
	return td.currentData
}