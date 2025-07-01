package datasource

import (
	"context"
	"os/exec"
	"strings"
	"time"
)

type KubectlDataSource struct {
	callbacks     []func(interface{})
	currentValue  interface{}
	updateChannel chan KubectlData
}

func NewKubectlDataSource() *KubectlDataSource {
	return &KubectlDataSource{
		callbacks:     make([]func(interface{}), 0),
		updateChannel: make(chan KubectlData, 1),
	}
}

func (k *KubectlDataSource) Start(ctx context.Context) {
	go func() {
		ticker := time.NewTicker(5 * time.Second)
		defer ticker.Stop()

		// Initial fetch
		k.updateContext()

		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				k.updateContext()
			case data := <-k.updateChannel:
				k.currentValue = data
				for _, callback := range k.callbacks {
					callback(data)
				}
			}
		}
	}()
}

func (k *KubectlDataSource) Subscribe(callback func(value interface{})) {
	k.callbacks = append(k.callbacks, callback)
}

func (k *KubectlDataSource) GetCurrentValue() interface{} {
	return k.currentValue
}

func (k *KubectlDataSource) ClearContext() {
	// Clear the current context by unsetting it
	cmd := exec.Command("kubectl", "config", "unset", "current-context")
	cmd.Run() // Ignore errors since context might already be unset
	
	// Immediately update to reflect the change
	k.updateContext()
}

func (k *KubectlDataSource) GetAllContexts() []string {
	cmd := exec.Command("kubectl", "config", "get-contexts", "-o", "name")
	output, err := cmd.Output()
	if err != nil {
		return []string{}
	}

	contexts := strings.Split(strings.TrimSpace(string(output)), "\n")
	var validContexts []string
	for _, context := range contexts {
		context = strings.TrimSpace(context)
		if context != "" {
			validContexts = append(validContexts, context)
		}
	}
	
	return validContexts
}

func (k *KubectlDataSource) SetContext(context string) {
	cmd := exec.Command("kubectl", "config", "use-context", context)
	cmd.Run() // Ignore errors
	
	// Immediately update to reflect the change
	k.updateContext()
}

func (k *KubectlDataSource) updateContext() {
	context := getKubectlContext()
	isProduction := isProductionContext(context)
	data := KubectlData{
		Context:       context,
		ContextString: "⚙️ " + context,
		IsProduction:  isProduction,
	}

	select {
	case k.updateChannel <- data:
	default:
		// Channel full, skip update
	}
}

func getKubectlContext() string {
	cmd := exec.Command("kubectl", "config", "current-context")
	output, err := cmd.Output()
	if err != nil {
		return "--"
	}

	context := strings.TrimSpace(string(output))
	if context == "" {
		return "--"
	}

	return context
}

func isProductionContext(context string) bool {
	contextLower := strings.ToLower(context)
	return strings.Contains(contextLower, "prod") || strings.Contains(contextLower, "prd")
}