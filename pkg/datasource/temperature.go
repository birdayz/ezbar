package datasource

import (
	"context"
	"sync"
	"time"
)

type TemperatureDataSource struct {
	mu          sync.RWMutex
	currentData TemperatureData
	callbacks   []func(value interface{})
}

func NewTemperatureDataSource() *TemperatureDataSource {
	return &TemperatureDataSource{
		currentData: TemperatureData{Temperature: 0, TempString: "🌡️ --"},
		callbacks:   make([]func(value interface{}), 0),
	}
}

func (t *TemperatureDataSource) Start(ctx context.Context) {
	go func() {
		// Immediate fetch at startup
		tempStr := getCPUTemperature()
		temp := extractTemperatureValue(tempStr)
		
		data := TemperatureData{
			Temperature: temp,
			TempString:  tempStr,
		}
		
		t.mu.Lock()
		t.currentData = data
		callbacks := make([]func(value interface{}), len(t.callbacks))
		copy(callbacks, t.callbacks)
		t.mu.Unlock()
		
		for _, callback := range callbacks {
			callback(data)
		}
		
		// Continue with regular timer updates
		ticker := time.NewTicker(2 * time.Second)
		defer ticker.Stop()
		
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				tempStr := getCPUTemperature()
				temp := extractTemperatureValue(tempStr)
				
				data := TemperatureData{
					Temperature: temp,
					TempString:  tempStr,
				}
				
				t.mu.Lock()
				t.currentData = data
				callbacks := make([]func(value interface{}), len(t.callbacks))
				copy(callbacks, t.callbacks)
				t.mu.Unlock()
				
				for _, callback := range callbacks {
					callback(data)
				}
			}
		}
	}()
}

func (t *TemperatureDataSource) Subscribe(callback func(value interface{})) {
	t.mu.Lock()
	defer t.mu.Unlock()
	t.callbacks = append(t.callbacks, callback)
}

func (t *TemperatureDataSource) GetCurrentValue() interface{} {
	t.mu.RLock()
	defer t.mu.RUnlock()
	return t.currentData
}