package datasource

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestSpotifyCredentialsLoading(t *testing.T) {
	// Create a temporary credentials file for testing
	tmpDir := t.TempDir()
	credPath := filepath.Join(tmpDir, "credentials.json")
	
	testCreds := SpotifyCredentials{
		Username: "testuser",
		AuthType: 1,
		AuthData: "dGVzdF90b2tlbg==", // base64 encoded "test_token"
	}
	
	credData, err := json.Marshal(testCreds)
	if err != nil {
		t.Fatalf("Failed to marshal test credentials: %v", err)
	}
	
	if err := os.WriteFile(credPath, credData, 0644); err != nil {
		t.Fatalf("Failed to write test credentials: %v", err)
	}
	
	// Test loading credentials
	ds := NewSpotifyDataSource()
	
	// Mock the credential path by temporarily setting HOME
	originalHome := os.Getenv("HOME")
	defer os.Setenv("HOME", originalHome)
	
	// Create the expected directory structure
	mockHome := tmpDir
	mockCacheDir := filepath.Join(mockHome, ".cache", "spotifyd", "oauth")
	if err := os.MkdirAll(mockCacheDir, 0755); err != nil {
		t.Fatalf("Failed to create mock cache directory: %v", err)
	}
	
	// Copy the test credentials to the expected location
	expectedCredPath := filepath.Join(mockCacheDir, "credentials.json")
	if err := os.WriteFile(expectedCredPath, credData, 0644); err != nil {
		t.Fatalf("Failed to write credentials to mock location: %v", err)
	}
	
	os.Setenv("HOME", mockHome)
	
	// Test credential loading
	err = ds.loadCredentials()
	if err != nil {
		t.Fatalf("Failed to load credentials: %v", err)
	}
	
	if ds.accessToken != "test_token" {
		t.Errorf("Expected access token 'test_token', got '%s'", ds.accessToken)
	}
}

func TestSpotifyCredentialsLoadingRealFile(t *testing.T) {
	// Test with the actual credentials file
	ds := NewSpotifyDataSource()
	
	err := ds.loadCredentials()
	if err != nil {
		t.Logf("Failed to load real credentials: %v", err)
		t.Logf("This is expected if credentials file doesn't exist or is invalid")
		
		// Check if the file exists
		homeDir, _ := os.UserHomeDir()
		credPath := filepath.Join(homeDir, ".cache", "spotifyd", "oauth", "credentials.json")
		if _, err := os.Stat(credPath); os.IsNotExist(err) {
			t.Logf("Credentials file does not exist at: %s", credPath)
		} else {
			// File exists, let's examine its contents
			data, readErr := os.ReadFile(credPath)
			if readErr != nil {
				t.Logf("Failed to read credentials file: %v", readErr)
			} else {
				t.Logf("Credentials file contents: %s", string(data))
				
				// Try to parse it
				var creds SpotifyCredentials
				if parseErr := json.Unmarshal(data, &creds); parseErr != nil {
					t.Logf("Failed to parse credentials JSON: %v", parseErr)
				} else {
					t.Logf("Parsed credentials - Username: %s, AuthType: %d, AuthData length: %d", 
						creds.Username, creds.AuthType, len(creds.AuthData))
				}
			}
		}
		return
	}
	
	t.Logf("Successfully loaded credentials, access token length: %d", len(ds.accessToken))
}

func TestSpotifyGetCurrentTrack(t *testing.T) {
	ds := NewSpotifyDataSource()
	
	// First try to load real credentials
	err := ds.loadCredentials()
	if err != nil {
		t.Skipf("Skipping API test - failed to load credentials: %v", err)
	}
	
	t.Logf("Access token (first 20 chars): %s...", ds.accessToken[:20])
	t.Logf("Access token length: %d", len(ds.accessToken))
	
	// Test getting current track
	data, err := ds.getCurrentTrack()
	if err != nil {
		t.Logf("Failed to get current track: %v", err)
		t.Logf("This might be expected if no track is playing or token is invalid")
		t.Logf("Returned data: %+v", data)
		
		// Let's also test a simple API call to verify the token
		testErr := testSpotifyToken(ds.accessToken)
		t.Logf("Token test result: %v", testErr)
		return
	}
	
	t.Logf("Successfully got current track data: %+v", data)
}

func testSpotifyToken(token string) error {
	client := &http.Client{Timeout: 10 * time.Second}
	
	// Try a simple API call to get user profile
	req, err := http.NewRequest("GET", "https://api.spotify.com/v1/me", nil)
	if err != nil {
		return err
	}
	
	req.Header.Set("Authorization", "Bearer "+token)
	
	resp, err := client.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	
	body, _ := io.ReadAll(resp.Body)
	
	if resp.StatusCode != 200 {
		return fmt.Errorf("API call failed with status %d: %s", resp.StatusCode, string(body))
	}
	
	return nil
}

func TestSpotifyDataSourceIntegration(t *testing.T) {
	ds := NewSpotifyDataSource()
	
	// Test that we can create and start the datasource
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	
	var receivedData interface{}
	ds.Subscribe(func(value interface{}) {
		receivedData = value
		t.Logf("Received data update: %+v", value)
	})
	
	// Start the datasource
	ds.Start(ctx)
	
	// Wait a bit for the first update
	time.Sleep(2 * time.Second)
	
	// Check that we got an update
	if receivedData == nil {
		t.Error("Expected to receive data update, but got nil")
	} else {
		if spotifyData, ok := receivedData.(SpotifyData); ok {
			t.Logf("Received SpotifyData: Track='%s', Artist='%s', TrackString='%s', IsPlaying=%v", 
				spotifyData.Track, spotifyData.Artist, spotifyData.TrackString, spotifyData.IsPlaying)
		} else {
			t.Errorf("Received data is not SpotifyData type: %T", receivedData)
		}
	}
	
	// Test GetCurrentValue
	currentValue := ds.GetCurrentValue()
	if currentValue == nil {
		t.Error("GetCurrentValue returned nil")
	} else {
		t.Logf("GetCurrentValue returned: %+v", currentValue)
	}
}

func TestSpotifyDataSourceErrorHandling(t *testing.T) {
	ds := NewSpotifyDataSource()
	
	// Test with invalid token
	ds.accessToken = "invalid_token"
	
	data, err := ds.getCurrentTrack()
	if err == nil {
		t.Logf("Expected error with invalid token, but got data: %+v", data)
	} else {
		t.Logf("Got expected error with invalid token: %v", err)
	}
	
	// The data should still be valid (error state)
	if data.TrackString == "" {
		t.Error("Expected non-empty TrackString even on error")
	}
}