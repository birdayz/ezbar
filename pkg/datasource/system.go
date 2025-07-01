package datasource

import (
	"fmt"
	"io/ioutil"
	"strconv"
	"strings"
	"time"
)

// System monitoring functions

// humanizeWithDecimals formats bytes with decimal places
func humanizeWithDecimals(bytes uint64) string {
	const unit = 1024
	if bytes < unit {
		return fmt.Sprintf("%d B", bytes)
	}
	
	div, exp := uint64(unit), 0
	for n := bytes / unit; n >= unit; n /= unit {
		div *= unit
		exp++
	}
	
	val := float64(bytes) / float64(div)
	units := []string{"KB", "MB", "GB", "TB", "PB", "EB", "ZB", "YB"}
	
	if exp >= len(units) {
		exp = len(units) - 1
	}
	
	return fmt.Sprintf("%.1f%s", val, units[exp])
}

func getCPUUsage() string {
	stat1, err := ioutil.ReadFile("/proc/stat")
	if err != nil {
		return "🖥️ --"
	}

	time.Sleep(100 * time.Millisecond)

	stat2, err := ioutil.ReadFile("/proc/stat")
	if err != nil {
		return "🖥️ --"
	}

	cpu1 := parseCPUStat(string(stat1))
	cpu2 := parseCPUStat(string(stat2))

	if cpu1 == nil || cpu2 == nil {
		return "🖥️ --"
	}

	idle := cpu2[3] - cpu1[3]
	total := (cpu2[0] + cpu2[1] + cpu2[2] + cpu2[3]) - (cpu1[0] + cpu1[1] + cpu1[2] + cpu1[3])

	if total == 0 {
		return "🖥️ 0%"
	}

	usage := 100 - (idle*100)/total
	return "🖥️ " + strconv.FormatInt(usage, 10) + "%"
}

func parseCPUStat(stat string) []int64 {
	lines := strings.Split(stat, "\n")
	if len(lines) == 0 {
		return nil
	}

	fields := strings.Fields(lines[0])
	if len(fields) < 5 || fields[0] != "cpu" {
		return nil
	}

	values := make([]int64, 4)
	for i := 0; i < 4; i++ {
		val, err := strconv.ParseInt(fields[i+1], 10, 64)
		if err != nil {
			return nil
		}
		values[i] = val
	}

	return values
}

func getMemoryUsage() string {
	meminfo, err := ioutil.ReadFile("/proc/meminfo")
	if err != nil {
		return "💾 --"
	}

	lines := strings.Split(string(meminfo), "\n")
	var memTotal, memAvailable int64

	for _, line := range lines {
		fields := strings.Fields(line)
		if len(fields) < 2 {
			continue
		}

		switch fields[0] {
		case "MemTotal:":
			if val, err := strconv.ParseInt(fields[1], 10, 64); err == nil {
				memTotal = val
			}
		case "MemAvailable:":
			if val, err := strconv.ParseInt(fields[1], 10, 64); err == nil {
				memAvailable = val
			}
		}
	}

	if memTotal == 0 {
		return "💾 --"
	}

	// Calculate used memory like free -h does: total - available
	memUsed := memTotal - memAvailable
	
	// Convert from KB to bytes for humanize
	usedBytes := uint64(memUsed * 1024)
	totalBytes := uint64(memTotal * 1024)
	
	// Use custom humanize function with decimal places
	usedStr := humanizeWithDecimals(usedBytes)
	totalStr := humanizeWithDecimals(totalBytes)
	
	return fmt.Sprintf("💾 %s/%s", usedStr, totalStr)
}

func getCPUTemperature() string {
	// Try to find CPU temperature from various sources
	tempPaths := []string{
		"/sys/class/thermal/thermal_zone0/temp",
		"/sys/class/thermal/thermal_zone1/temp",
		"/sys/devices/platform/thinkpad_hwmon/hwmon/hwmon7/temp1_input",
		"/sys/devices/platform/coretemp.0/hwmon/hwmon*/temp1_input",
	}
	
	for _, path := range tempPaths {
		if temp, err := ioutil.ReadFile(path); err == nil {
			tempStr := strings.TrimSpace(string(temp))
			if tempVal, err := strconv.ParseFloat(tempStr, 64); err == nil {
				// Temperature is usually in millidegrees Celsius
				tempCelsius := tempVal / 1000.0
				return "🌡️ " + strconv.FormatFloat(tempCelsius, 'f', 0, 64) + "°C"
			}
		}
	}
	
	// Try to find temperature from hwmon sensors
	hwmonFiles, err := ioutil.ReadDir("/sys/class/hwmon")
	if err == nil {
		for _, hwmon := range hwmonFiles {
			tempPath := "/sys/class/hwmon/" + hwmon.Name() + "/temp1_input"
			if temp, err := ioutil.ReadFile(tempPath); err == nil {
				tempStr := strings.TrimSpace(string(temp))
				if tempVal, err := strconv.ParseFloat(tempStr, 64); err == nil {
					tempCelsius := tempVal / 1000.0
					return "🌡️ " + strconv.FormatFloat(tempCelsius, 'f', 0, 64) + "°C"
				}
			}
		}
	}
	
	return "🌡️ --"
}

func extractCPUUsageValue(cpuStr string) float64 {
	// Extract CPU usage value from strings like "🖥️ 45%"
	if strings.Contains(cpuStr, "%") {
		parts := strings.Split(cpuStr, " ")
		for _, part := range parts {
			if strings.Contains(part, "%") {
				cpuPart := strings.Replace(part, "%", "", -1)
				if cpu, err := strconv.ParseFloat(cpuPart, 64); err == nil {
					return cpu
				}
			}
		}
	}
	return 0
}

func extractMemoryUsageValue(memStr string) float64 {
	// Extract memory usage percentage from strings like "💾 8.2GB/16GB"
	if strings.Contains(memStr, "/") {
		parts := strings.Split(memStr, " ")
		for _, part := range parts {
			if strings.Contains(part, "/") {
				fractionParts := strings.Split(part, "/")
				if len(fractionParts) == 2 {
					used := parseMemorySize(fractionParts[0])
					total := parseMemorySize(fractionParts[1])
					if used > 0 && total > 0 {
						return (used / total) * 100
					}
				}
			}
		}
	}
	return 0
}

func parseMemorySize(sizeStr string) float64 {
	// Parse sizes like "8.2GB", "512MB", etc.
	sizeStr = strings.TrimSpace(sizeStr)
	if strings.HasSuffix(sizeStr, "GB") {
		if val, err := strconv.ParseFloat(strings.TrimSuffix(sizeStr, "GB"), 64); err == nil {
			return val * 1024 * 1024 * 1024
		}
	} else if strings.HasSuffix(sizeStr, "MB") {
		if val, err := strconv.ParseFloat(strings.TrimSuffix(sizeStr, "MB"), 64); err == nil {
			return val * 1024 * 1024
		}
	} else if strings.HasSuffix(sizeStr, "KB") {
		if val, err := strconv.ParseFloat(strings.TrimSuffix(sizeStr, "KB"), 64); err == nil {
			return val * 1024
		}
	}
	return 0
}

func extractTemperatureValue(tempStr string) float64 {
	// Extract temperature value from strings like "🌡️ 45°C"
	if strings.Contains(tempStr, "°C") {
		parts := strings.Split(tempStr, " ")
		for _, part := range parts {
			if strings.Contains(part, "°C") {
				tempPart := strings.Replace(part, "°C", "", -1)
				if temp, err := strconv.ParseFloat(tempPart, 64); err == nil {
					return temp
				}
			}
		}
	}
	return 0
}