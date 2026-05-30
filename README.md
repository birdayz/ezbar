# ezbar

ezbar is my extremely simple statusbar for sway.

## Widgets

### Google Calendar

Shows your next meeting with countdown. Hover to see today's schedule. Blinks when a meeting is imminent or ongoing.

**Setup:**

1. Go to [Google Calendar Settings](https://calendar.google.com/calendar/r/settings)
2. Click your calendar under "Settings for my calendars"
3. Scroll to "Integrate calendar"
4. Copy the **"Secret address in iCal format"** URL
5. Save it:

```bash
echo "YOUR_ICAL_URL" > ~/.config/ezbar/calendar_url
```

### Spotify

Shows currently playing track. Click to play/pause, scroll to skip tracks.

**Setup:**

1. Create app at [Spotify Developer Dashboard](https://developer.spotify.com/dashboard)
2. Add `http://127.0.0.1:8888/callback` as redirect URI
3. Create config:

```bash
cat > ~/.config/ezbar/spotify_config.json << 'EOF'
{"client_id": "YOUR_CLIENT_ID", "client_secret": "YOUR_CLIENT_SECRET"}
EOF
```

4. Click the widget to authorize
