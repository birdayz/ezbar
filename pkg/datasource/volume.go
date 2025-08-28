package datasource

import (
	"context"
	"fmt"
	"os/exec"
	"strconv"
	"strings"
	"time"
)

type VolumeDataSource struct {
	callbacks []func(interface{})
	currentValue VolumeData
}

func NewVolumeDataSource() *VolumeDataSource {
	return &VolumeDataSource{
		callbacks: make([]func(interface{}), 0),
		currentValue: VolumeData{
			Volume:       0,
			VolumeString: "🔇 --",
			IsMuted:      false,
		},
	}
}

func (ds *VolumeDataSource) Start(ctx context.Context) {
	go func() {
		ticker := time.NewTicker(1 * time.Second)
		defer ticker.Stop()
		
		// Get initial value
		ds.updateVolume()
		
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				ds.updateVolume()
			}
		}
	}()
}

func (ds *VolumeDataSource) Subscribe(callback func(interface{})) {
	ds.callbacks = append(ds.callbacks, callback)
}

func (ds *VolumeDataSource) GetCurrentValue() interface{} {
	return ds.currentValue
}

func (ds *VolumeDataSource) ToggleMute() {
	toggleMute()
	ds.updateVolume()
}

func (ds *VolumeDataSource) ChangeVolume(direction int) {
	changeVolume(direction)
	ds.updateVolume()
}

func (ds *VolumeDataSource) updateVolume() {
	volume, isMuted := getVolumeInfo()
	
	var volumeString string
	if isMuted {
		volumeString = "🔇 --%"
	} else {
		var icon string
		if volume == 0 {
			icon = "🔇"
		} else if volume < 33 {
			icon = "🔈"
		} else if volume < 66 {
			icon = "🔉"
		} else {
			icon = "🔊"
		}
		volumeString = fmt.Sprintf("%s %d%%", icon, volume)
	}
	
	newValue := VolumeData{
		Volume:       volume,
		VolumeString: volumeString,
		IsMuted:      isMuted,
	}
	
	ds.currentValue = newValue
	
	// Notify all subscribers
	for _, callback := range ds.callbacks {
		callback(newValue)
	}
}

func getVolumeInfo() (int, bool) {
	// Try PulseAudio first
	if volume, isMuted, err := getPulseAudioVolume(); err == nil {
		return volume, isMuted
	}
	
	// Try ALSA as fallback
	if volume, isMuted, err := getALSAVolume(); err == nil {
		return volume, isMuted
	}
	
	return 0, false
}

func getPulseAudioVolume() (int, bool, error) {
	// Get volume
	cmd := exec.Command("pactl", "get-sink-volume", "@DEFAULT_SINK@")
	output, err := cmd.Output()
	if err != nil {
		return 0, false, err
	}
	
	// Parse volume from output like "Volume: front-left: 32768 /  50% / -18.06 dB"
	lines := strings.Split(string(output), "\n")
	for _, line := range lines {
		if strings.Contains(line, "Volume:") {
			parts := strings.Split(line, "/")
			if len(parts) >= 2 {
				volumePart := strings.TrimSpace(parts[1])
				volumePart = strings.TrimSuffix(volumePart, "%")
				if volume, err := strconv.Atoi(volumePart); err == nil {
					// Check if muted
					cmd := exec.Command("pactl", "get-sink-mute", "@DEFAULT_SINK@")
					muteOutput, err := cmd.Output()
					if err != nil {
						return volume, false, nil
					}
					
					isMuted := strings.Contains(string(muteOutput), "Mute: yes")
					return volume, isMuted, nil
				}
			}
		}
	}
	
	return 0, false, fmt.Errorf("could not parse volume")
}

func getALSAVolume() (int, bool, error) {
	// Try to get volume from ALSA
	cmd := exec.Command("amixer", "get", "Master")
	output, err := cmd.Output()
	if err != nil {
		return 0, false, err
	}
	
	lines := strings.Split(string(output), "\n")
	for _, line := range lines {
		if strings.Contains(line, "[") && strings.Contains(line, "%") {
			// Parse line like "  Front Left: Playbook 32768 [50%] [18.06dB] [on]"
			start := strings.Index(line, "[")
			end := strings.Index(line, "%]")
			if start != -1 && end != -1 {
				volumeStr := line[start+1 : end]
				if volume, err := strconv.Atoi(volumeStr); err == nil {
					// Check if muted (look for [off])
					isMuted := strings.Contains(line, "[off]")
					return volume, isMuted, nil
				}
			}
		}
	}
	
	return 0, false, fmt.Errorf("could not parse ALSA volume")
}

func toggleMute() {
	// Try PulseAudio first
	cmd := exec.Command("pactl", "set-sink-mute", "@DEFAULT_SINK@", "toggle")
	if err := cmd.Run(); err != nil {
		// Try ALSA as fallback
		cmd = exec.Command("amixer", "set", "Master", "toggle")
		cmd.Run()
	}
}

func changeVolume(direction int) {
	volumeChange := direction * 5 // Change by 5%
	
	// Try PulseAudio first
	var sign string
	if volumeChange > 0 {
		sign = "+"
	} else {
		sign = ""
	}
	
	cmd := exec.Command("pactl", "set-sink-volume", "@DEFAULT_SINK@", fmt.Sprintf("%s%d%%", sign, volumeChange))
	if err := cmd.Run(); err != nil {
		// Try ALSA as fallback
		var alsaDirection string
		if volumeChange > 0 {
			alsaDirection = fmt.Sprintf("%d%%+", volumeChange)
		} else {
			alsaDirection = fmt.Sprintf("%d%%-", -volumeChange)
		}
		cmd = exec.Command("amixer", "set", "Master", alsaDirection)
		cmd.Run()
	}
}