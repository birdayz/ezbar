package datasource

import (
	"context"
	"fmt"
	"os/exec"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"time"
)

type PingDataSource struct {
	mu          sync.RWMutex
	currentData PingData
	callbacks   []func(value interface{})
	target      string
}

func NewPingDataSource(target string) *PingDataSource {
	if target == "" {
		target = "8.8.8.8"
	}
	return &PingDataSource{
		target:      target,
		currentData: PingData{Latency: 0, PingString: "🏓 --", IsUp: false},
		callbacks:   make([]func(value interface{}), 0),
	}
}

func (p *PingDataSource) Start(ctx context.Context) {
	go func() {
		// Immediate ping at startup
		data := p.performPing()
		
		p.mu.Lock()
		p.currentData = data
		callbacks := make([]func(value interface{}), len(p.callbacks))
		copy(callbacks, p.callbacks)
		p.mu.Unlock()
		
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
				data := p.performPing()
				
				p.mu.Lock()
				p.currentData = data
				callbacks := make([]func(value interface{}), len(p.callbacks))
				copy(callbacks, p.callbacks)
				p.mu.Unlock()
				
				for _, callback := range callbacks {
					callback(data)
				}
			}
		}
	}()
}

func (p *PingDataSource) Subscribe(callback func(value interface{})) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.callbacks = append(p.callbacks, callback)
}

func (p *PingDataSource) GetCurrentValue() interface{} {
	p.mu.RLock()
	defer p.mu.RUnlock()
	return p.currentData
}

func (p *PingDataSource) performPing() PingData {
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	
	cmd := exec.CommandContext(ctx, "ping", "-c", "1", "-W", "2", p.target)
	output, err := cmd.Output()
	
	if err != nil {
		return PingData{
			Latency:    0,
			PingString: "🏓 DOWN",
			IsUp:       false,
		}
	}
	
	latency, err := extractPingLatency(string(output))
	if err != nil {
		return PingData{
			Latency:    0,
			PingString: "🏓 ERROR",
			IsUp:       false,
		}
	}
	
	return PingData{
		Latency:    latency,
		PingString: fmt.Sprintf("🏓 %.1fms", latency),
		IsUp:       true,
	}
}

func extractPingLatency(output string) (float64, error) {
	// Look for patterns like "time=1.23 ms" or "time=1.23ms"
	re := regexp.MustCompile(`time=([0-9.]+)\s*ms`)
	matches := re.FindStringSubmatch(output)
	
	if len(matches) < 2 {
		return 0, fmt.Errorf("could not parse ping output")
	}
	
	latencyStr := strings.TrimSpace(matches[1])
	latency, err := strconv.ParseFloat(latencyStr, 64)
	if err != nil {
		return 0, fmt.Errorf("could not parse latency value: %v", err)
	}
	
	return latency, nil
}