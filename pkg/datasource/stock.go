package datasource

import (
	"context"
	"encoding/json"
	"fmt"
	"io/ioutil"
	"net/http"
	"strconv"
	"strings"
	"time"
)

type StockDataSource struct {
	symbol          string
	apiKey          string
	updateInterval  time.Duration
	currentData     StockData
	subscribers     []func(value interface{})
	ticker          *time.Ticker
	isRunning       bool
}

// AlphaVantageResponse represents the API response structure
type AlphaVantageResponse struct {
	GlobalQuote struct {
		Symbol           string `json:"01. symbol"`
		Open             string `json:"02. open"`
		High             string `json:"03. high"`
		Low              string `json:"04. low"`
		Price            string `json:"05. price"`
		Volume           string `json:"06. volume"`
		LatestTradingDay string `json:"07. latest trading day"`
		PreviousClose    string `json:"08. previous close"`
		Change           string `json:"09. change"`
		ChangePercent    string `json:"10. change percent"`
	} `json:"Global Quote"`
}

// FinnhubResponse represents Finnhub API response
type FinnhubResponse struct {
	C  float64 `json:"c"`  // Current price
	D  float64 `json:"d"`  // Change
	DP float64 `json:"dp"` // Percent change
	H  float64 `json:"h"`  // High price of the day
	L  float64 `json:"l"`  // Low price of the day
	O  float64 `json:"o"`  // Open price of the day
	PC float64 `json:"pc"` // Previous close price
}

func NewStockDataSource(symbol string, apiKey string) *StockDataSource {
	return &StockDataSource{
		symbol:         strings.ToUpper(symbol),
		apiKey:         apiKey,
		updateInterval: 5 * time.Minute, // Update every 5 minutes to avoid rate limits
		subscribers:    make([]func(value interface{}), 0),
		currentData: StockData{
			Symbol:      strings.ToUpper(symbol),
			DisplayText: fmt.Sprintf("📈 %s: Loading...", strings.ToUpper(symbol)),
		},
	}
}

func (s *StockDataSource) Start(ctx context.Context) {
	if s.isRunning {
		return
	}
	s.isRunning = true

	// Initial fetch
	s.fetchStockData()

	// Start periodic updates
	s.ticker = time.NewTicker(s.updateInterval)
	go func() {
		for {
			select {
			case <-ctx.Done():
				if s.ticker != nil {
					s.ticker.Stop()
				}
				s.isRunning = false
				return
			case <-s.ticker.C:
				s.fetchStockData()
			}
		}
	}()
}

func (s *StockDataSource) Subscribe(callback func(value interface{})) {
	s.subscribers = append(s.subscribers, callback)
}

func (s *StockDataSource) GetCurrentValue() interface{} {
	return s.currentData
}

func (s *StockDataSource) fetchStockData() {
	fmt.Printf("Fetching stock data for %s...\n", s.symbol)
	
	// Try free Yahoo Finance API first
	data, err := s.fetchFromYahooFinance()
	if err != nil {
		fmt.Printf("Yahoo Finance error: %v, trying Finnhub...\n", err)
		
		// Try Finnhub if API key is available
		data, err = s.fetchFromFinnhub()
		if err != nil {
			fmt.Printf("Finnhub error: %v, trying Alpha Vantage...\n", err)
			// Fallback to Alpha Vantage if available
			data, err = s.fetchFromAlphaVantage()
			if err != nil {
				fmt.Printf("Alpha Vantage error: %v\n", err)
				s.currentData = StockData{
					Symbol:      s.symbol,
					DisplayText: fmt.Sprintf("📈 %s: Error fetching data", s.symbol),
					PriceString: "---",
					ChangeString: "---",
				}
			} else {
				s.currentData = *data
			}
		} else {
			s.currentData = *data
		}
	} else {
		s.currentData = *data
	}

	fmt.Printf("Stock data updated: %s\n", s.currentData.DisplayText)
	
	// Notify subscribers
	for _, callback := range s.subscribers {
		callback(s.currentData)
	}
}

func (s *StockDataSource) fetchFromFinnhub() (*StockData, error) {
	// Finnhub requires a real API key now
	if s.apiKey == "" {
		return nil, fmt.Errorf("Finnhub API key required")
	}
	url := fmt.Sprintf("https://finnhub.io/api/v1/quote?symbol=%s&token=%s", s.symbol, s.apiKey)
	
	client := &http.Client{Timeout: 10 * time.Second}
	resp, err := client.Get(url)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	body, err := ioutil.ReadAll(resp.Body)
	if err != nil {
		return nil, err
	}

	var finnhubData FinnhubResponse
	if err := json.Unmarshal(body, &finnhubData); err != nil {
		return nil, err
	}

	// Check if we got valid data
	if finnhubData.C == 0 {
		return nil, fmt.Errorf("no data available for symbol %s", s.symbol)
	}

	return s.formatStockData(finnhubData.C, finnhubData.D, finnhubData.DP), nil
}

func (s *StockDataSource) fetchFromAlphaVantage() (*StockData, error) {
	if s.apiKey == "" {
		return nil, fmt.Errorf("Alpha Vantage API key not provided")
	}
	
	url := fmt.Sprintf("https://www.alphavantage.co/query?function=GLOBAL_QUOTE&symbol=%s&apikey=%s", s.symbol, s.apiKey)
	
	client := &http.Client{Timeout: 10 * time.Second}
	resp, err := client.Get(url)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	body, err := ioutil.ReadAll(resp.Body)
	if err != nil {
		return nil, err
	}

	var alphaData AlphaVantageResponse
	if err := json.Unmarshal(body, &alphaData); err != nil {
		return nil, err
	}

	if alphaData.GlobalQuote.Symbol == "" {
		return nil, fmt.Errorf("no data available for symbol %s", s.symbol)
	}

	price, _ := strconv.ParseFloat(alphaData.GlobalQuote.Price, 64)
	change, _ := strconv.ParseFloat(alphaData.GlobalQuote.Change, 64)
	
	// Parse change percent (remove % sign)
	changePercentStr := strings.TrimSuffix(alphaData.GlobalQuote.ChangePercent, "%")
	changePercent, _ := strconv.ParseFloat(changePercentStr, 64)

	return s.formatStockData(price, change, changePercent), nil
}

func (s *StockDataSource) formatStockData(price, change, changePercent float64) *StockData {
	isPositive := change >= 0
	isNegative := change < 0
	
	var trendEmoji string
	var changeColor string
	
	if isPositive {
		trendEmoji = "📈"
		changeColor = "+"
	} else {
		trendEmoji = "📉"
		changeColor = ""
	}

	priceStr := fmt.Sprintf("$%.2f", price)
	changeStr := fmt.Sprintf("%s%.2f (%.2f%%)", changeColor, change, changePercent)
	
	displayText := fmt.Sprintf("%s %s: %s %s", trendEmoji, s.symbol, priceStr, changeStr)
	
	return &StockData{
		Symbol:        s.symbol,
		Price:         price,
		Change:        change,
		ChangePercent: changePercent,
		DisplayText:   displayText,
		PriceString:   priceStr,
		ChangeString:  changeStr,
		IsPositive:    isPositive,
		IsNegative:    isNegative,
		TrendEmoji:    trendEmoji,
	}
}

func (s *StockDataSource) fetchFromYahooFinance() (*StockData, error) {
	// Use Yahoo Finance query API (free, no key required)
	url := fmt.Sprintf("https://query1.finance.yahoo.com/v8/finance/chart/%s", s.symbol)
	
	client := &http.Client{Timeout: 15 * time.Second}
	req, err := http.NewRequest("GET", url, nil)
	if err != nil {
		return nil, err
	}
	
	// Add headers to look like a regular browser request
	req.Header.Set("User-Agent", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36")
	req.Header.Set("Accept", "application/json")
	
	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	body, err := ioutil.ReadAll(resp.Body)
	if err != nil {
		return nil, err
	}

	var yahooData struct {
		Chart struct {
			Result []struct {
				Meta struct {
					Currency                string  `json:"currency"`
					Symbol                  string  `json:"symbol"`
					RegularMarketPrice      float64 `json:"regularMarketPrice"`
					PreviousClose           float64 `json:"previousClose"`
					ChartPreviousClose      float64 `json:"chartPreviousClose"`
				} `json:"meta"`
			} `json:"result"`
		} `json:"chart"`
	}

	if err := json.Unmarshal(body, &yahooData); err != nil {
		return nil, err
	}

	if len(yahooData.Chart.Result) == 0 {
		return nil, fmt.Errorf("no data available for symbol %s", s.symbol)
	}

	meta := yahooData.Chart.Result[0].Meta
	if meta.RegularMarketPrice == 0 {
		return nil, fmt.Errorf("invalid price data for symbol %s", s.symbol)
	}

	price := meta.RegularMarketPrice
	previousClose := meta.PreviousClose
	if previousClose == 0 {
		previousClose = meta.ChartPreviousClose
	}
	
	change := price - previousClose
	changePercent := 0.0
	if previousClose != 0 {
		changePercent = (change / previousClose) * 100
	}

	return s.formatStockData(price, change, changePercent), nil
}

// Convenience function to create NASDAQ stock data source
func NewNASDAQStockDataSource(symbol string, apiKey string) *StockDataSource {
	return NewStockDataSource(symbol, apiKey)
}