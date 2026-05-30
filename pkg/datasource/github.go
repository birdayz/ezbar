package datasource

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"net/http"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	ghauth "github.com/cli/go-gh/v2/pkg/auth"
	"github.com/google/go-github/v72/github"
)

type GitHubNotification struct {
	ID        string
	Reason    string
	Title     string
	Type      string // PullRequest, Issue, Release, etc.
	RepoName  string
	HTMLURL   string
	UpdatedAt time.Time
	Unread    bool
}

type GitHubData struct {
	Notifications []GitHubNotification
	Count         int
	DisplayText   string
}

type GitHubConfig struct {
	// Reasons to include. Empty means all.
	Reasons []string `json:"reasons"`
	// Repos to exclude
	ExcludeRepos []string `json:"exclude_repos"`
}

type GitHubDataSource struct {
	mu           sync.RWMutex
	currentData  GitHubData
	callbacks    []func(value any)
	config       GitHubConfig
	client       *github.Client
	logger       *slog.Logger
	lastModified string        // Last-Modified header for conditional requests
	pollInterval time.Duration // From X-Poll-Interval header
}

func NewGitHubDataSource() *GitHubDataSource {
	ds := &GitHubDataSource{
		currentData: GitHubData{DisplayText: "GH ..."},
		callbacks:   make([]func(value any), 0),
		logger: slog.New(NewColoredHandler(os.Stderr, &slog.HandlerOptions{
			Level: slog.LevelInfo,
		})).With("component", "github"),
	}
	ds.loadConfig()
	return ds
}

func (g *GitHubDataSource) loadConfig() {
	homeDir, err := os.UserHomeDir()
	if err != nil {
		return
	}
	configPath := filepath.Join(homeDir, ".config", "ezbar", "github_config.json")
	data, err := os.ReadFile(configPath)
	if err != nil {
		g.config = GitHubConfig{
			Reasons: []string{"review_requested", "mention", "assign", "author"},
		}
		return
	}
	if err := json.Unmarshal(data, &g.config); err != nil {
		g.logger.Warn("Failed to parse github config", "error", err)
		g.config = GitHubConfig{
			Reasons: []string{"review_requested", "mention", "assign", "author"},
		}
	}
}

func (g *GitHubDataSource) initClient() error {
	if g.client != nil {
		return nil
	}

	token, _ := ghauth.TokenForHost("github.com")
	if token == "" {
		return fmt.Errorf("no GitHub token found (run 'gh auth login' first)")
	}

	g.client = github.NewClient(nil).WithAuthToken(token)
	return nil
}

// errNotModified is returned when the API returns 304 Not Modified.
var errNotModified = fmt.Errorf("not modified")

func (g *GitHubDataSource) fetchNotifications(ctx context.Context) (GitHubData, error) {
	if err := g.initClient(); err != nil {
		return GitHubData{}, err
	}

	var allNotifications []*github.Notification
	opts := &github.NotificationListOptions{
		ListOptions: github.ListOptions{PerPage: 50},
	}

	firstPage := true
	for {
		// Build request manually to support If-Modified-Since
		urlStr := fmt.Sprintf("notifications?per_page=%d&page=%d", opts.PerPage, opts.Page)
		req, err := g.client.NewRequest("GET", urlStr, nil)
		if err != nil {
			return GitHubData{}, fmt.Errorf("creating request: %w", err)
		}

		if firstPage && g.lastModified != "" {
			req.Header.Set("If-Modified-Since", g.lastModified)
		}

		var notifications []*github.Notification
		resp, err := g.client.Do(ctx, req, &notifications)
		if err != nil {
			// Check for 304 Not Modified
			if resp != nil && resp.StatusCode == http.StatusNotModified {
				g.updatePollInterval(resp)
				return GitHubData{}, errNotModified
			}
			return GitHubData{}, fmt.Errorf("listing notifications: %w", err)
		}

		if firstPage {
			g.updatePollInterval(resp)
			if lm := resp.Header.Get("Last-Modified"); lm != "" {
				g.lastModified = lm
			}
			firstPage = false
		}

		allNotifications = append(allNotifications, notifications...)
		if resp.NextPage == 0 {
			break
		}
		opts.Page = resp.NextPage
	}

	var converted []GitHubNotification
	for _, n := range allNotifications {
		gn := GitHubNotification{
			ID:     n.GetID(),
			Reason: n.GetReason(),
			Unread: n.GetUnread(),
		}
		if n.Subject != nil {
			gn.Title = n.Subject.GetTitle()
			gn.Type = n.Subject.GetType()
			gn.HTMLURL = apiURLToHTMLURL(n.Subject.GetURL(), n.Subject.GetType())
		}
		if n.Repository != nil {
			gn.RepoName = n.Repository.GetFullName()
		}
		gn.UpdatedAt = n.GetUpdatedAt().Time
		converted = append(converted, gn)
	}

	// Filter out merged/closed PRs for review_requested notifications
	converted = g.filterMergedPRs(ctx, converted)

	filtered := g.filterNotifications(converted)

	// Sort by updated_at descending (newest first)
	sort.Slice(filtered, func(i, j int) bool {
		return filtered[i].UpdatedAt.After(filtered[j].UpdatedAt)
	})

	data := GitHubData{
		Notifications: filtered,
		Count:         len(filtered),
	}

	if data.Count == 0 {
		data.DisplayText = "GH 0"
	} else {
		data.DisplayText = fmt.Sprintf("GH %d", data.Count)
	}

	return data, nil
}

func (g *GitHubDataSource) updatePollInterval(resp *github.Response) {
	if pi := resp.Header.Get("X-Poll-Interval"); pi != "" {
		if secs, err := strconv.Atoi(pi); err == nil && secs > 0 {
			g.pollInterval = time.Duration(secs) * time.Second
			g.logger.Info("GitHub poll interval", "seconds", secs)
		}
	}
}

func (g *GitHubDataSource) filterNotifications(notifications []GitHubNotification) []GitHubNotification {
	reasonSet := make(map[string]bool)
	for _, r := range g.config.Reasons {
		reasonSet[r] = true
	}

	excludeRepoSet := make(map[string]bool)
	for _, r := range g.config.ExcludeRepos {
		excludeRepoSet[r] = true
	}

	var filtered []GitHubNotification
	for _, n := range notifications {
		if !n.Unread {
			continue
		}
		if len(reasonSet) > 0 && !reasonSet[n.Reason] {
			continue
		}
		if excludeRepoSet[n.RepoName] {
			continue
		}
		filtered = append(filtered, n)
	}
	return filtered
}

var prURLPattern = regexp.MustCompile(`github\.com/([^/]+)/([^/]+)/pull/(\d+)$`)

func (g *GitHubDataSource) filterMergedPRs(ctx context.Context, notifications []GitHubNotification) []GitHubNotification {
	// Find review_requested PR notifications that need checking
	type checkResult struct {
		index    int
		excluded bool
	}

	var wg sync.WaitGroup
	results := make(chan checkResult, len(notifications))

	// Limit concurrency
	sem := make(chan struct{}, 10)

	for i, n := range notifications {
		if n.Reason != "review_requested" || n.Type != "PullRequest" {
			continue
		}

		// Parse owner/repo/number from HTMLURL
		matches := prURLPattern.FindStringSubmatch(n.HTMLURL)
		if matches == nil {
			continue
		}
		owner, repo := matches[1], matches[2]
		number, _ := strconv.Atoi(matches[3])

		wg.Add(1)
		go func(idx int) {
			defer wg.Done()
			sem <- struct{}{}
			defer func() { <-sem }()

			pr, _, err := g.client.PullRequests.Get(ctx, owner, repo, number)
			if err != nil {
				return // keep the notification on error
			}
			if pr.GetMerged() || pr.GetState() == "closed" {
				results <- checkResult{index: idx, excluded: true}
			}
		}(i)
	}

	go func() {
		wg.Wait()
		close(results)
	}()

	excluded := make(map[int]bool)
	for r := range results {
		if r.excluded {
			excluded[r.index] = true
		}
	}

	var filtered []GitHubNotification
	for i, n := range notifications {
		if !excluded[i] {
			filtered = append(filtered, n)
		}
	}
	return filtered
}

func (g *GitHubDataSource) notify(data GitHubData) {
	g.mu.Lock()
	g.currentData = data
	callbacks := make([]func(value any), len(g.callbacks))
	copy(callbacks, g.callbacks)
	g.mu.Unlock()

	for _, cb := range callbacks {
		cb(data)
	}
}

func (g *GitHubDataSource) Start(ctx context.Context) {
	go func() {
		data, err := g.fetchNotifications(ctx)
		if err != nil {
			g.logger.Warn("GitHub error", "error", err)
			data = GitHubData{DisplayText: "GH ?"}
		}
		g.notify(data)

		for {
			// Use poll interval from API, default to 60s
			interval := g.pollInterval
			if interval == 0 {
				interval = 60 * time.Second
			}

			select {
			case <-ctx.Done():
				return
			case <-time.After(interval):
				data, err := g.fetchNotifications(ctx)
				if err == errNotModified {
					continue // Nothing changed
				}
				if err != nil {
					g.logger.Warn("GitHub error", "error", err)
					continue
				}
				g.notify(data)
			}
		}
	}()
}

func (g *GitHubDataSource) Subscribe(callback func(value any)) {
	g.mu.Lock()
	defer g.mu.Unlock()
	g.callbacks = append(g.callbacks, callback)
}

func (g *GitHubDataSource) GetCurrentValue() any {
	g.mu.RLock()
	defer g.mu.RUnlock()
	return g.currentData
}

// MarkAsRead marks a single notification as read and removes it from the local list.
func (g *GitHubDataSource) MarkAsRead(id string) {
	go func() {
		if g.client == nil {
			return
		}
		_, err := g.client.Activity.MarkThreadRead(context.Background(), id)
		if err != nil {
			g.logger.Warn("Failed to mark notification read", "id", id, "error", err)
		}
	}()

	// Immediately update local state
	g.mu.Lock()
	var remaining []GitHubNotification
	for _, n := range g.currentData.Notifications {
		if n.ID != id {
			remaining = append(remaining, n)
		}
	}
	g.currentData.Notifications = remaining
	g.currentData.Count = len(remaining)
	if g.currentData.Count == 0 {
		g.currentData.DisplayText = "GH 0"
	} else {
		g.currentData.DisplayText = fmt.Sprintf("GH %d", g.currentData.Count)
	}
	data := g.currentData
	callbacks := make([]func(value any), len(g.callbacks))
	copy(callbacks, g.callbacks)
	g.mu.Unlock()

	for _, cb := range callbacks {
		cb(data)
	}
}

// MarkAllAsRead marks all notifications as read.
func (g *GitHubDataSource) MarkAllAsRead() {
	go func() {
		if g.client == nil {
			return
		}
		_, err := g.client.Activity.MarkNotificationsRead(context.Background(), github.Timestamp{Time: time.Now()})
		if err != nil {
			g.logger.Warn("Failed to mark all notifications read", "error", err)
		}
	}()

	// Immediately update local state
	g.mu.Lock()
	g.currentData.Notifications = nil
	g.currentData.Count = 0
	g.currentData.DisplayText = "GH 0"
	data := g.currentData
	callbacks := make([]func(value any), len(g.callbacks))
	copy(callbacks, g.callbacks)
	g.mu.Unlock()

	for _, cb := range callbacks {
		cb(data)
	}
}

// apiURLToHTMLURL converts GitHub API URLs to browser-friendly HTML URLs.
// e.g. https://api.github.com/repos/owner/repo/pulls/123 -> https://github.com/owner/repo/pull/123
func apiURLToHTMLURL(apiURL, subjectType string) string {
	if apiURL == "" {
		return ""
	}
	// Strip API prefix
	htmlURL := strings.Replace(apiURL, "https://api.github.com/repos/", "https://github.com/", 1)
	// Fix path: "pulls/N" -> "pull/N", "issues/N" stays
	if subjectType == "PullRequest" {
		htmlURL = strings.Replace(htmlURL, "/pulls/", "/pull/", 1)
	}
	return htmlURL
}
