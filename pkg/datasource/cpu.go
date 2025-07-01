package datasource

import (
	"context"
	"sync"
	"time"
)

type CPUDataSource struct {
	mu          sync.RWMutex
	currentData CPUData
	callbacks   []func(value interface{})
}

func NewCPUDataSource() *CPUDataSource {
	return &CPUDataSource{
		currentData: CPUData{Usage: 0, UsageString: "🖥️ --"},
		callbacks:   make([]func(value interface{}), 0),
	}
}

func (c *CPUDataSource) Start(ctx context.Context) {
	go func() {
		// Immediate fetch at startup
		usageStr := getCPUUsage()
		usage := extractCPUUsageValue(usageStr)
		
		data := CPUData{
			Usage:       usage,
			UsageString: usageStr,
		}
		
		c.mu.Lock()
		c.currentData = data
		callbacks := make([]func(value interface{}), len(c.callbacks))
		copy(callbacks, c.callbacks)
		c.mu.Unlock()
		
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
				usageStr := getCPUUsage()
				usage := extractCPUUsageValue(usageStr)
				
				data := CPUData{
					Usage:       usage,
					UsageString: usageStr,
				}
				
				c.mu.Lock()
				c.currentData = data
				callbacks := make([]func(value interface{}), len(c.callbacks))
				copy(callbacks, c.callbacks)
				c.mu.Unlock()
				
				for _, callback := range callbacks {
					callback(data)
				}
			}
		}
	}()
}

func (c *CPUDataSource) Subscribe(callback func(value interface{})) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.callbacks = append(c.callbacks, callback)
}

func (c *CPUDataSource) GetCurrentValue() interface{} {
	c.mu.RLock()
	defer c.mu.RUnlock()
	return c.currentData
}