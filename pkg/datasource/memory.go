package datasource

import (
	"context"
	"sync"
	"time"
)

type MemoryDataSource struct {
	mu          sync.RWMutex
	currentData MemoryData
	callbacks   []func(value interface{})
}

func NewMemoryDataSource() *MemoryDataSource {
	return &MemoryDataSource{
		currentData: MemoryData{Usage: 0, UsageString: "💾 --"},
		callbacks:   make([]func(value interface{}), 0),
	}
}

func (m *MemoryDataSource) Start(ctx context.Context) {
	go func() {
		// Immediate fetch at startup
		usageStr := getMemoryUsage()
		usage := extractMemoryUsageValue(usageStr)
		
		data := MemoryData{
			Usage:       usage,
			UsageString: usageStr,
		}
		
		m.mu.Lock()
		m.currentData = data
		callbacks := make([]func(value interface{}), len(m.callbacks))
		copy(callbacks, m.callbacks)
		m.mu.Unlock()
		
		for _, callback := range callbacks {
			callback(data)
		}
		
		// Continue with regular timer updates
		ticker := time.NewTicker(3 * time.Second)
		defer ticker.Stop()
		
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				usageStr := getMemoryUsage()
				usage := extractMemoryUsageValue(usageStr)
				
				data := MemoryData{
					Usage:       usage,
					UsageString: usageStr,
				}
				
				m.mu.Lock()
				m.currentData = data
				callbacks := make([]func(value interface{}), len(m.callbacks))
				copy(callbacks, m.callbacks)
				m.mu.Unlock()
				
				for _, callback := range callbacks {
					callback(data)
				}
			}
		}
	}()
}

func (m *MemoryDataSource) Subscribe(callback func(value interface{})) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.callbacks = append(m.callbacks, callback)
}

func (m *MemoryDataSource) GetCurrentValue() interface{} {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.currentData
}