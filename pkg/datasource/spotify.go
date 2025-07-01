package datasource

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"time"
)

type SpotifyCredentials struct {
	Username string `json:"username"`
	AuthType int    `json:"auth_type"`
	AuthData string `json:"auth_data"`
}

type SpotifyWebAPIToken struct {
	AccessToken  string `json:"access_token"`
	RefreshToken string `json:"refresh_token"`
	ExpiresIn    int    `json:"expires_in"`
	TokenType    string `json:"token_type"`
	Scope        string `json:"scope"`
	ExpiresAt    int64  `json:"expires_at"` // Unix timestamp when token expires
}

type SpotifyConfig struct {
	ClientID     string `json:"client_id"`
	ClientSecret string `json:"client_secret"`
}

type SpotifyCurrentTrack struct {
	Item struct {
		Name    string `json:"name"`
		Artists []struct {
			Name string `json:"name"`
		} `json:"artists"`
		Album struct {
			Name string `json:"name"`
		} `json:"album"`
	} `json:"item"`
	IsPlaying bool `json:"is_playing"`
}

type SpotifyDataSource struct {
	mu                sync.RWMutex
	currentData       SpotifyData
	callbacks         []func(value interface{})
	client            *http.Client
	accessToken       string
	needsAuth         bool
	authInProgress    bool
	clickCallbacks    []func()
	logger            *slog.Logger
}

func NewSpotifyDataSource() *SpotifyDataSource {
	return &SpotifyDataSource{
		currentData: SpotifyData{
			Track:       "",
			Artist:      "",
			Album:       "",
			TrackString: "🎵 --",
			Icon:        "🎵",
			ScrollText:  "--",
			IsPlaying:   false,
		},
		callbacks:      make([]func(value interface{}), 0),
		clickCallbacks: make([]func(), 0),
		client:         &http.Client{Timeout: 10 * time.Second},
		needsAuth:      false,
		authInProgress: false,
		logger:         NewSpotifyLogger(),
	}
}

func (s *SpotifyDataSource) loadCredentials() error {
	// First try environment variable
	if token := os.Getenv("SPOTIFY_ACCESS_TOKEN"); token != "" {
		s.accessToken = token
		return nil
	}

	homeDir, err := os.UserHomeDir()
	if err != nil {
		return fmt.Errorf("failed to get home directory: %w", err)
	}

	// Try loading existing Web API token
	tokenPath := filepath.Join(homeDir, ".config", "ezbar", "spotify_web_token.json")
	if tokenData, err := os.ReadFile(tokenPath); err == nil {
		var webToken SpotifyWebAPIToken
		if err := json.Unmarshal(tokenData, &webToken); err == nil {
			// Check if token is still valid
			if time.Now().Unix() < webToken.ExpiresAt {
				s.accessToken = webToken.AccessToken
				return nil
			}
			// Token expired, try to refresh
			if webToken.RefreshToken != "" {
				if err := s.refreshToken(&webToken); err == nil {
					s.accessToken = webToken.AccessToken
					return nil
				}
			}
		}
	}

	// Try to get new token using stored config
	configPath := filepath.Join(homeDir, ".config", "ezbar", "spotify_config.json")
	if configData, err := os.ReadFile(configPath); err == nil {
		var config SpotifyConfig
		if err := json.Unmarshal(configData, &config); err == nil && config.ClientID != "" {
			// Mark that we need authentication but don't auto-trigger
			s.mu.Lock()
			s.needsAuth = true
			s.mu.Unlock()
			return fmt.Errorf("needs_auth")
		}
	}

	return fmt.Errorf("no Spotify Web API credentials found. Please create ~/.config/ezbar/spotify_config.json with your client_id and client_secret, then run authorization. Or set SPOTIFY_ACCESS_TOKEN environment variable.\n\nTo set up:\n1. Go to https://developer.spotify.com/dashboard\n2. Create an app and get client_id/client_secret\n3. Create ~/.config/ezbar/spotify_config.json:\n   {\"client_id\": \"your_id\", \"client_secret\": \"your_secret\"}")
}

func (s *SpotifyDataSource) refreshToken(webToken *SpotifyWebAPIToken) error {
	homeDir, err := os.UserHomeDir()
	if err != nil {
		return err
	}

	configPath := filepath.Join(homeDir, ".config", "ezbar", "spotify_config.json")
	configData, err := os.ReadFile(configPath)
	if err != nil {
		return fmt.Errorf("config file not found: %w", err)
	}

	var config SpotifyConfig
	if err := json.Unmarshal(configData, &config); err != nil {
		return fmt.Errorf("failed to parse config: %w", err)
	}

	// Refresh the token
	data := url.Values{}
	data.Set("grant_type", "refresh_token")
	data.Set("refresh_token", webToken.RefreshToken)

	req, err := http.NewRequest("POST", "https://accounts.spotify.com/api/token", strings.NewReader(data.Encode()))
	if err != nil {
		return err
	}

	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	auth := base64.StdEncoding.EncodeToString([]byte(config.ClientID + ":" + config.ClientSecret))
	req.Header.Set("Authorization", "Basic "+auth)

	resp, err := s.client.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()

	if resp.StatusCode != 200 {
		body, _ := io.ReadAll(resp.Body)
		return fmt.Errorf("token refresh failed: %d - %s", resp.StatusCode, string(body))
	}

	var tokenResp SpotifyWebAPIToken
	if err := json.NewDecoder(resp.Body).Decode(&tokenResp); err != nil {
		return err
	}

	// Update the token
	webToken.AccessToken = tokenResp.AccessToken
	if tokenResp.RefreshToken != "" {
		webToken.RefreshToken = tokenResp.RefreshToken
	}
	webToken.ExpiresIn = tokenResp.ExpiresIn
	webToken.ExpiresAt = time.Now().Unix() + int64(tokenResp.ExpiresIn)

	// Save the refreshed token
	tokenPath := filepath.Join(homeDir, ".config", "ezbar", "spotify_web_token.json")
	os.MkdirAll(filepath.Dir(tokenPath), 0755)
	tokenData, _ := json.Marshal(webToken)
	return os.WriteFile(tokenPath, tokenData, 0600)
}

func (s *SpotifyDataSource) performHeadlessAuth(config *SpotifyConfig) error {
	// Try to start a local server OAuth flow
	return s.performLocalServerAuth(config)
}

func (s *SpotifyDataSource) performLocalServerAuth(config *SpotifyConfig) error {
	// Start local HTTP server for OAuth callback
	redirectURI := "http://127.0.0.1:8888/callback"
	
	// Generate authorization URL
	authURL := fmt.Sprintf(
		"https://accounts.spotify.com/authorize?response_type=code&client_id=%s&scope=%s&redirect_uri=%s&show_dialog=true",
		config.ClientID,
		url.QueryEscape("user-read-currently-playing user-read-playback-state user-modify-playback-state"),
		url.QueryEscape(redirectURI),
	)
	
	// Set up channels for communication
	codeChan := make(chan string, 1)
	errChan := make(chan error, 1)
	
	// Set up HTTP server
	mux := http.NewServeMux()
	mux.HandleFunc("/callback", func(w http.ResponseWriter, r *http.Request) {
		code := r.URL.Query().Get("code")
		if code == "" {
			errChan <- fmt.Errorf("no authorization code received")
			return
		}
		
		w.Header().Set("Content-Type", "text/html")
		w.Write([]byte(`
			<html>
			<head><title>Spotify Authorization</title></head>
			<body style="font-family: Arial, sans-serif; text-align: center; margin-top: 100px;">
				<h1>🎵 Authorization Successful!</h1>
				<p>You can close this window now.</p>
				<p>Your ezbar should start showing Spotify tracks shortly.</p>
			</body>
			</html>
		`))
		
		codeChan <- code
	})
	
	server := &http.Server{
		Addr:    ":8888",
		Handler: mux,
	}
	
	// Start the server
	go func() {
		if err := server.ListenAndServe(); err != nil && err != http.ErrServerClosed {
			errChan <- fmt.Errorf("server error: %w", err)
		}
	}()
	
	// Give server time to start
	time.Sleep(500 * time.Millisecond)
	
	// Open browser
	s.openBrowser(authURL)
	
	s.logger.Info("Spotify authorization started", "url", authURL)
	fmt.Printf("\n🎵 Spotify Authorization Started\n")
	fmt.Printf("1. Browser should open automatically\n")
	fmt.Printf("2. Authorize the application in Spotify\n")
	fmt.Printf("3. You'll be redirected back automatically\n")
	fmt.Printf("4. Wait for completion...\n")
	
	// Wait for authorization code or timeout
	var authCode string
	select {
	case authCode = <-codeChan:
		s.logger.Info("Authorization code received successfully")
		fmt.Println("✅ Authorization code received!")
	case err := <-errChan:
		server.Close()
		return err
	case <-time.After(5 * time.Minute):
		server.Close()
		return fmt.Errorf("authorization timeout (5 minutes)")
	}
	
	// Shutdown the server
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	server.Shutdown(ctx)
	
	// Exchange code for token
	return s.exchangeCodeForToken(config, authCode, redirectURI)
}


func (s *SpotifyDataSource) openBrowser(url string) {
	var cmd *exec.Cmd
	
	// Try to detect the OS and open browser
	switch {
	case fileExists("/usr/bin/xdg-open"):
		cmd = exec.Command("xdg-open", url)
	case fileExists("/usr/bin/open"):
		cmd = exec.Command("open", url)
	case fileExists("/mnt/c/Windows/System32/cmd.exe"):
		cmd = exec.Command("cmd", "/c", "start", url)
	default:
		fmt.Printf("Could not auto-open browser. Please open manually: %s\n", url)
		return
	}
	
	go func() {
		cmd.Run()
	}()
}

func fileExists(filename string) bool {
	_, err := os.Stat(filename)
	return err == nil
}

func (s *SpotifyDataSource) exchangeCodeForToken(config *SpotifyConfig, code, redirectURI string) error {
	data := url.Values{}
	data.Set("grant_type", "authorization_code")
	data.Set("code", code)
	data.Set("redirect_uri", redirectURI)
	
	req, err := http.NewRequest("POST", "https://accounts.spotify.com/api/token", strings.NewReader(data.Encode()))
	if err != nil {
		return err
	}
	
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	auth := base64.StdEncoding.EncodeToString([]byte(config.ClientID + ":" + config.ClientSecret))
	req.Header.Set("Authorization", "Basic "+auth)
	
	resp, err := s.client.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	
	if resp.StatusCode != 200 {
		body, _ := io.ReadAll(resp.Body)
		return fmt.Errorf("token exchange failed: %d - %s", resp.StatusCode, string(body))
	}
	
	var tokenResp SpotifyWebAPIToken
	if err := json.NewDecoder(resp.Body).Decode(&tokenResp); err != nil {
		return err
	}
	
	// Calculate expiration time
	tokenResp.ExpiresAt = time.Now().Unix() + int64(tokenResp.ExpiresIn)
	
	// Save the token
	homeDir, err := os.UserHomeDir()
	if err != nil {
		return err
	}
	
	tokenPath := filepath.Join(homeDir, ".config", "ezbar", "spotify_web_token.json")
	os.MkdirAll(filepath.Dir(tokenPath), 0755)
	tokenData, err := json.Marshal(tokenResp)
	if err != nil {
		return err
	}
	
	if err := os.WriteFile(tokenPath, tokenData, 0600); err != nil {
		return err
	}
	
	// Set the access token for immediate use
	s.accessToken = tokenResp.AccessToken
	
	s.logger.Info("Authorization successful - token saved")
	fmt.Println("Authorization successful! Token saved.")
	return nil
}

func (s *SpotifyDataSource) getCurrentTrack() (SpotifyData, error) {
	if s.accessToken == "" {
		if err := s.loadCredentials(); err != nil {
			if err.Error() == "needs_auth" {
				return SpotifyData{
					Track:       "",
					Artist:      "",
					Album:       "",
					TrackString: "🎵 Click to authorize",
					Icon:        "🎵",
					ScrollText:  "Click to authorize",
					IsPlaying:   false,
				}, nil
			}
			return SpotifyData{}, err
		}
	}

	req, err := http.NewRequest("GET", "https://api.spotify.com/v1/me/player/currently-playing", nil)
	if err != nil {
		return SpotifyData{}, err
	}

	req.Header.Set("Authorization", "Bearer "+s.accessToken)

	resp, err := s.client.Do(req)
	if err != nil {
		s.logger.Warn("HTTP request failed", "error", err)
		return SpotifyData{}, fmt.Errorf("network error: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode == 204 {
		// No content - nothing is playing
		return SpotifyData{
			Track:       "",
			Artist:      "",
			Album:       "",
			TrackString: "🎵 Nothing playing",
			Icon:        "🎵",
			ScrollText:  "Nothing playing",
			IsPlaying:   false,
		}, nil
	}

	if resp.StatusCode == 401 {
		// Unauthorized - token is invalid or expired
		return SpotifyData{}, fmt.Errorf("spotify API error: 401 (token invalid/expired)")
	}

	if resp.StatusCode != 200 {
		// Read the response body for more details
		body, _ := io.ReadAll(resp.Body)
		return SpotifyData{}, fmt.Errorf("spotify API error: %d - %s", resp.StatusCode, string(body))
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return SpotifyData{}, err
	}

	var track SpotifyCurrentTrack
	if err := json.Unmarshal(body, &track); err != nil {
		return SpotifyData{}, err
	}

	artistName := ""
	if len(track.Item.Artists) > 0 {
		artistName = track.Item.Artists[0].Name
	}

	var icon string
	var scrollText string
	
	if track.IsPlaying {
		icon = "🎵"
	} else {
		icon = "⏸️"
	}
	
	scrollText = fmt.Sprintf("%s - %s", track.Item.Name, artistName)
	trackString := fmt.Sprintf("%s %s", icon, scrollText)

	return SpotifyData{
		Track:       track.Item.Name,
		Artist:      artistName,
		Album:       track.Item.Album.Name,
		TrackString: trackString,
		Icon:        icon,
		ScrollText:  scrollText,
		IsPlaying:   track.IsPlaying,
	}, nil
}

func (s *SpotifyDataSource) Start(ctx context.Context) {
	go func() {
		// Immediate fetch at startup
		data, err := s.getCurrentTrack()
		if err != nil {
			// Check if this is a token-related error
			if strings.Contains(err.Error(), "401") {
				s.logger.Info("Access token expired at startup, attempting refresh")
				// Clear the current token to trigger refresh
				s.mu.Lock()
				s.accessToken = ""
				s.mu.Unlock()
				
				// Try to get track again (will attempt token refresh)
				if retryData, retryErr := s.getCurrentTrack(); retryErr == nil {
					data = retryData
				} else {
					s.logger.Error("Token refresh failed at startup", "error", retryErr)
					data = SpotifyData{
						Track:       "",
						Artist:      "",
						Album:       "",
						TrackString: "🎵 Token expired - click to reauth",
						Icon:        "🎵",
						ScrollText:  "Token expired - click to reauth",
						IsPlaying:   false,
					}
				}
			} else if strings.Contains(err.Error(), "network error") {
				s.logger.Warn("Network error at startup", "error", err)
				data = SpotifyData{
					Track:       "",
					Artist:      "",
					Album:       "",
					TrackString: "🎵 Network error",
					Icon:        "🎵",
					ScrollText:  "Network error",
					IsPlaying:   false,
				}
			} else {
				s.logger.Warn("Spotify API error at startup", "error", err)
				data = SpotifyData{
					Track:       "",
					Artist:      "",
					Album:       "",
					TrackString: "🎵 Error loading",
					Icon:        "🎵",
					ScrollText:  "Error loading",
					IsPlaying:   false,
				}
			}
		}

		s.mu.Lock()
		s.currentData = data
		callbacks := make([]func(value interface{}), len(s.callbacks))
		copy(callbacks, s.callbacks)
		s.mu.Unlock()

		for _, callback := range callbacks {
			callback(data)
		}

		// Continue with regular timer updates
		ticker := time.NewTicker(5 * time.Second)
		defer ticker.Stop()

		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				data, err := s.getCurrentTrack()
				if err != nil {
					// Check if this is a token-related error
					if strings.Contains(err.Error(), "401") {
						s.logger.Info("Access token expired, attempting refresh")
						// Clear the current token to trigger refresh
						s.mu.Lock()
						s.accessToken = ""
						s.mu.Unlock()
						
						// Try to get track again (will attempt token refresh)
						if retryData, retryErr := s.getCurrentTrack(); retryErr == nil {
							data = retryData
						} else {
							s.logger.Error("Token refresh failed", "error", retryErr)
							data = SpotifyData{
								Track:       "",
								Artist:      "",
								Album:       "",
								TrackString: "🎵 Token expired - click to reauth",
								Icon:        "🎵",
								ScrollText:  "Token expired - click to reauth",
								IsPlaying:   false,
							}
						}
					} else if strings.Contains(err.Error(), "network error") {
						s.logger.Warn("Network error, will retry next cycle", "error", err)
						data = SpotifyData{
							Track:       "",
							Artist:      "",
							Album:       "",
							TrackString: "🎵 Network error",
							Icon:        "🎵",
							ScrollText:  "Network error",
							IsPlaying:   false,
						}
					} else {
						s.logger.Warn("Spotify API error", "error", err)
						data = SpotifyData{
							Track:       "",
							Artist:      "",
							Album:       "",
							TrackString: "🎵 Error loading",
							Icon:        "🎵",
							ScrollText:  "Error loading",
							IsPlaying:   false,
						}
					}
				}

				s.mu.Lock()
				s.currentData = data
				callbacks := make([]func(value interface{}), len(s.callbacks))
				copy(callbacks, s.callbacks)
				s.mu.Unlock()

				for _, callback := range callbacks {
					callback(data)
				}
			}
		}
	}()
}

func (s *SpotifyDataSource) Subscribe(callback func(value interface{})) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.callbacks = append(s.callbacks, callback)
}

func (s *SpotifyDataSource) GetCurrentValue() interface{} {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.currentData
}

func (s *SpotifyDataSource) OnClick(callback func()) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.clickCallbacks = append(s.clickCallbacks, callback)
}

func (s *SpotifyDataSource) HandleClick() {
	s.mu.RLock()
	needsAuth := s.needsAuth
	authInProgress := s.authInProgress
	hasToken := s.accessToken != ""
	isPlaying := s.currentData.IsPlaying
	callbacks := make([]func(), len(s.clickCallbacks))
	copy(callbacks, s.clickCallbacks)
	s.mu.RUnlock()
	
	if needsAuth && !authInProgress {
		go s.triggerOAuth()
	} else if hasToken {
		// Toggle play/pause
		go s.togglePlayback(isPlaying)
	}
	
	for _, callback := range callbacks {
		callback()
	}
}

func (s *SpotifyDataSource) triggerOAuth() {
	s.mu.Lock()
	s.authInProgress = true
	s.mu.Unlock()
	
	// Update display to show auth in progress
	data := SpotifyData{
		Track:       "",
		Artist:      "",
		Album:       "",
		TrackString: "🎵 Authorizing...",
		Icon:        "🎵",
		ScrollText:  "Authorizing...",
		IsPlaying:   false,
	}
	
	s.mu.Lock()
	s.currentData = data
	callbacks := make([]func(value interface{}), len(s.callbacks))
	copy(callbacks, s.callbacks)
	s.mu.Unlock()
	
	for _, callback := range callbacks {
		callback(data)
	}
	
	// Load config and perform OAuth
	homeDir, err := os.UserHomeDir()
	if err != nil {
		s.setAuthError("Failed to get home directory")
		return
	}
	
	configPath := filepath.Join(homeDir, ".config", "ezbar", "spotify_config.json")
	configData, err := os.ReadFile(configPath)
	if err != nil {
		s.setAuthError("Config file not found")
		return
	}
	
	var config SpotifyConfig
	if err := json.Unmarshal(configData, &config); err != nil {
		s.setAuthError("Failed to parse config")
		return
	}
	
	if err := s.performLocalServerAuth(&config); err != nil {
		s.setAuthError("Authorization failed: " + err.Error())
		return
	}
	
	// Success - clear auth flags and retry getting track
	s.mu.Lock()
	s.needsAuth = false
	s.authInProgress = false
	s.mu.Unlock()
	
	// Trigger immediate update
	if trackData, err := s.getCurrentTrack(); err == nil {
		s.mu.Lock()
		s.currentData = trackData
		callbacks := make([]func(value interface{}), len(s.callbacks))
		copy(callbacks, s.callbacks)
		s.mu.Unlock()
		
		for _, callback := range callbacks {
			callback(trackData)
		}
	}
}

func (s *SpotifyDataSource) setAuthError(message string) {
	data := SpotifyData{
		Track:       "",
		Artist:      "",
		Album:       "",
		TrackString: "🎵 Auth failed - click to retry",
		Icon:        "🎵",
		ScrollText:  "Auth failed - click to retry",
		IsPlaying:   false,
	}
	
	s.mu.Lock()
	s.currentData = data
	s.authInProgress = false
	callbacks := make([]func(value interface{}), len(s.callbacks))
	copy(callbacks, s.callbacks)
	s.mu.Unlock()
	
	for _, callback := range callbacks {
		callback(data)
	}
	
	s.logger.Error("Spotify authorization failed", "error", message)
}

func (s *SpotifyDataSource) togglePlayback(isCurrentlyPlaying bool) {
	var endpoint string
	var action string
	
	if isCurrentlyPlaying {
		endpoint = "https://api.spotify.com/v1/me/player/pause"
		action = "pause"
	} else {
		endpoint = "https://api.spotify.com/v1/me/player/play"
		action = "resume"
	}
	
	req, err := http.NewRequest("PUT", endpoint, nil)
	if err != nil {
		fmt.Printf("Failed to create %s request: %v\n", action, err)
		return
	}
	
	req.Header.Set("Authorization", "Bearer "+s.accessToken)
	
	resp, err := s.client.Do(req)
	if err != nil {
		fmt.Printf("Failed to %s playback: %v\n", action, err)
		return
	}
	defer resp.Body.Close()
	
	if resp.StatusCode == 200 || resp.StatusCode == 204 {
		// Success (200 OK or 204 No Content) - trigger immediate update to reflect the change
		s.logger.Info("Playback control successful", "action", action, "status", resp.StatusCode)
		go func() {
			// Wait a moment for Spotify to update, then refresh
			time.Sleep(500 * time.Millisecond)
			if trackData, err := s.getCurrentTrack(); err == nil {
				s.mu.Lock()
				s.currentData = trackData
				callbacks := make([]func(value interface{}), len(s.callbacks))
				copy(callbacks, s.callbacks)
				s.mu.Unlock()
				
				for _, callback := range callbacks {
					callback(trackData)
				}
			}
		}()
	} else if resp.StatusCode == 401 {
		s.logger.Error("Permissions missing for playback control - token needs re-authorization", "action", action)
		s.logger.Info("To fix: Delete ~/.config/ezbar/spotify_web_token.json and click widget to re-authorize")
	} else if resp.StatusCode == 403 {
		s.logger.Warn("Playback control failed - Premium required or device not active", "action", action)
	} else if resp.StatusCode == 404 {
		s.logger.Warn("Playback control failed - No active device found", "action", action)
	} else {
		body, _ := io.ReadAll(resp.Body)
		s.logger.Error("Playback control failed", "action", action, "status", resp.StatusCode, "response", string(body))
	}
}

func (s *SpotifyDataSource) NextTrack() {
	s.skipTrack("next")
}

func (s *SpotifyDataSource) PreviousTrack() {
	s.skipTrack("previous")
}

func (s *SpotifyDataSource) skipTrack(direction string) {
	var endpoint string
	
	if direction == "next" {
		endpoint = "https://api.spotify.com/v1/me/player/next"
	} else {
		endpoint = "https://api.spotify.com/v1/me/player/previous"
	}
	
	req, err := http.NewRequest("POST", endpoint, nil)
	if err != nil {
		fmt.Printf("Failed to create %s track request: %v\n", direction, err)
		return
	}
	
	req.Header.Set("Authorization", "Bearer "+s.accessToken)
	
	resp, err := s.client.Do(req)
	if err != nil {
		fmt.Printf("Failed to skip to %s track: %v\n", direction, err)
		return
	}
	defer resp.Body.Close()
	
	if resp.StatusCode == 200 || resp.StatusCode == 204 {
		s.logger.Info("Track skip successful", "direction", direction, "status", resp.StatusCode)
		go func() {
			// Wait a moment for Spotify to update, then refresh
			time.Sleep(500 * time.Millisecond)
			if trackData, err := s.getCurrentTrack(); err == nil {
				s.mu.Lock()
				s.currentData = trackData
				callbacks := make([]func(value interface{}), len(s.callbacks))
				copy(callbacks, s.callbacks)
				s.mu.Unlock()
				
				for _, callback := range callbacks {
					callback(trackData)
				}
			}
		}()
	} else if resp.StatusCode == 401 {
		s.logger.Error("Permissions missing for track control - token needs re-authorization", "direction", direction)
		s.logger.Info("To fix: Delete ~/.config/ezbar/spotify_web_token.json and click widget to re-authorize")
	} else if resp.StatusCode == 403 {
		s.logger.Warn("Track control failed - Premium required or device not active", "direction", direction)
	} else if resp.StatusCode == 404 {
		s.logger.Warn("Track control failed - No active device found", "direction", direction)
	} else {
		body, _ := io.ReadAll(resp.Body)
		s.logger.Error("Track control failed", "direction", direction, "status", resp.StatusCode, "response", string(body))
	}
}

func (s *SpotifyDataSource) VolumeUp() {
	s.adjustVolume(5) // Increase by 5% (smaller steps)
}

func (s *SpotifyDataSource) VolumeDown() {
	s.adjustVolume(-5) // Decrease by 5% (smaller steps)
}

func (s *SpotifyDataSource) AdjustVolumeBy(delta int) {
	s.adjustVolume(delta) // Allow custom volume adjustments
}

func (s *SpotifyDataSource) adjustVolume(delta int) {
	// First get current volume
	req, err := http.NewRequest("GET", "https://api.spotify.com/v1/me/player", nil)
	if err != nil {
		fmt.Printf("Failed to create get player request: %v\n", err)
		return
	}
	
	req.Header.Set("Authorization", "Bearer "+s.accessToken)
	
	resp, err := s.client.Do(req)
	if err != nil {
		fmt.Printf("Failed to get current player state: %v\n", err)
		return
	}
	defer resp.Body.Close()
	
	if resp.StatusCode != 200 {
		s.logger.Warn("Failed to get player state for volume control", "status", resp.StatusCode)
		return
	}
	
	var playerState struct {
		Device struct {
			VolumePercent int `json:"volume_percent"`
		} `json:"device"`
	}
	
	if err := json.NewDecoder(resp.Body).Decode(&playerState); err != nil {
		s.logger.Error("Failed to decode player state", "error", err)
		return
	}
	
	// Calculate new volume
	newVolume := playerState.Device.VolumePercent + delta
	if newVolume < 0 {
		newVolume = 0
	}
	if newVolume > 100 {
		newVolume = 100
	}
	
	// Set new volume
	endpoint := fmt.Sprintf("https://api.spotify.com/v1/me/player/volume?volume_percent=%d", newVolume)
	req, err = http.NewRequest("PUT", endpoint, nil)
	if err != nil {
		fmt.Printf("Failed to create volume request: %v\n", err)
		return
	}
	
	req.Header.Set("Authorization", "Bearer "+s.accessToken)
	
	resp, err = s.client.Do(req)
	if err != nil {
		fmt.Printf("Failed to adjust volume: %v\n", err)
		return
	}
	defer resp.Body.Close()
	
	if resp.StatusCode == 200 || resp.StatusCode == 204 {
		s.logger.Info("Volume adjustment successful", "old_volume", playerState.Device.VolumePercent, "new_volume", newVolume, "status", resp.StatusCode)
	} else if resp.StatusCode == 401 {
		s.logger.Error("Permissions missing for volume control - token needs re-authorization")
		s.logger.Info("To fix: Delete ~/.config/ezbar/spotify_web_token.json and click widget to re-authorize")
	} else if resp.StatusCode == 403 {
		s.logger.Warn("Volume control failed - Premium required or device not active")
	} else if resp.StatusCode == 404 {
		s.logger.Warn("Volume control failed - No active device found")
	} else {
		body, _ := io.ReadAll(resp.Body)
		s.logger.Error("Volume control failed", "status", resp.StatusCode, "response", string(body))
	}
}